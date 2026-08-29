use crate::diagnostics::{
    AdapterKind, AdapterStatus, discover_source_root, inspect_adapter, safe_message,
};
use crate::output::summarize_compose;
use crate::source::{canonicalize_source_path, read_source_documents};
use context_client::ContextClient;
use context_core::{
    ComposeRequest, ReviewMode, ScopeKind, ScopeRef, SetReviewPolicyRequest,
    SourceImportApplyRequest, SourceImportApplyResult, SourceImportKind, SourceImportPreview,
    SourceImportPreviewRequest,
};
use serde::Serialize;
use serde_json::json;
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub(crate) struct SetupOptions {
    pub project_dir: PathBuf,
    pub sources: Vec<PathBuf>,
    pub adapters: Vec<AdapterKind>,
    pub review_mode: Option<ReviewMode>,
    pub apply: bool,
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SetupState {
    Ready,
    NeedsAction,
    Applied,
    Failed,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct SetupReport {
    pub state: SetupState,
    pub mode: &'static str,
    pub project_scope: ScopeRef,
    pub daemon: SetupCheck,
    pub private_paths: Vec<PrivatePathReport>,
    pub policy: SetupPolicyReport,
    pub detected_sources: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub import_preview: Option<SourceImportPreview>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub import_result: Option<SourceImportApplyResult>,
    pub adapters: Vec<AdapterStatus>,
    pub compose_probe: SetupComposeProbe,
    pub next_steps: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct SetupCheck {
    pub ok: bool,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remediation: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct PrivatePathReport {
    pub kind: &'static str,
    pub path: String,
    pub exists: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unix_mode: Option<String>,
    pub private: bool,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct SetupPolicyReport {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current: Option<ReviewMode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub requested: Option<ReviewMode>,
    pub applied: bool,
    pub message: String,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct SetupComposeProbe {
    pub ok: bool,
    pub included_entries: usize,
    pub estimated_tokens: usize,
    pub excluded_entries: usize,
    pub warnings: Vec<String>,
    pub message: String,
}

pub(crate) fn run_setup(client: &ContextClient, options: SetupOptions) -> SetupReport {
    let project_scope = ScopeRef::normalized(
        ScopeKind::Project,
        options.project_dir.display().to_string(),
    )
    .expect("resolved project directory is a valid scope");
    let mut next_steps = Vec::new();

    let daemon = match client.ensure_daemon().and_then(|_| client.ping()) {
        Ok(health) => SetupCheck {
            ok: true,
            message: format!("contextd is reachable with schema v{}.", health.schema_version),
            remediation: None,
        },
        Err(error) => SetupCheck {
            ok: false,
            message: format!("contextd could not be started: {}", safe_message(&error)),
            remediation: Some(
                "Install or build contextd, set CONTEXTD_BIN if needed, then rerun `contextctl setup`."
                    .to_string(),
            ),
        },
    };

    let paths = &client.config().paths;
    let private_paths = vec![
        private_path_report("data", &paths.data_dir, true),
        private_path_report("database", &paths.db_path, false),
        private_path_report("socket", &paths.socket_path, false),
        private_path_report("spool", &paths.spool_dir, true),
    ];
    if private_paths
        .iter()
        .any(|path| path.exists && !path.private)
    {
        next_steps.push(
            "Run `contextctl doctor` to review private-path permission remediation.".to_string(),
        );
    }

    let policy = if daemon.ok {
        setup_policy(client, options.review_mode, options.apply)
    } else {
        SetupPolicyReport {
            ok: false,
            current: None,
            requested: options.review_mode,
            applied: false,
            message: "Review policy was not checked because the daemon is unavailable.".to_string(),
        }
    };

    let (detected_paths, source_collection_failed) =
        match collect_setup_sources(&options.project_dir, &options.sources) {
            Ok(paths) => (paths, false),
            Err(error) => {
                next_steps.push(format!(
                    "Fix source paths and rerun setup: {}",
                    safe_message(&error)
                ));
                (Vec::new(), true)
            }
        };
    let detected_sources = detected_paths
        .iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>();

    let (import_preview, import_result, import_failed, import_needs_action) = if daemon.ok
        && !detected_paths.is_empty()
    {
        match read_source_documents(&detected_paths, Some(&options.project_dir)) {
            Ok(documents) => {
                let preview_request = SourceImportPreviewRequest {
                    source_kind: SourceImportKind::Auto,
                    documents: documents.clone(),
                    destination: project_scope.clone(),
                    pack_name: None,
                    actor: "contextctl-setup".to_string(),
                };
                match client.preview_source_import(preview_request) {
                    Ok(preview) => {
                        let result = if options.apply && preview.apply_allowed {
                            match client.apply_source_import(SourceImportApplyRequest {
                                source_kind: SourceImportKind::Auto,
                                documents,
                                destination: project_scope.clone(),
                                pack_name: None,
                                actor: "contextctl-setup".to_string(),
                                expected_preview_fingerprint: preview.preview_fingerprint.clone(),
                            }) {
                                Ok(result) => Some(result),
                                Err(error) => {
                                    next_steps.push(format!(
                                        "Source import was not applied: {}",
                                        safe_message(&error)
                                    ));
                                    None
                                }
                            }
                        } else {
                            if options.apply && !preview.apply_allowed {
                                next_steps.push(
                                    "Resolve source preview warnings before applying imports."
                                        .to_string(),
                                );
                            } else if !options.apply && !preview.candidates.is_empty() {
                                next_steps.push(
                                    "Review the preview, then rerun with `--apply --yes` to import."
                                        .to_string(),
                                );
                            }
                            None
                        };
                        let (failed, needs_action) =
                            import_apply_state(options.apply, result.as_ref());
                        if let Some(result) = &result {
                            if all_candidates_rejected(result) {
                                next_steps.push(
                                    "All requested source candidates were rejected; inspect item reasons before retrying."
                                        .to_string(),
                                );
                            }
                            for review_id in source_import_review_ids(result) {
                                next_steps.push(format!(
                                    "Review pending import: contextctl review approve {review_id}"
                                ));
                            }
                        }
                        (Some(preview), result, failed, needs_action)
                    }
                    Err(error) => {
                        next_steps.push(format!("Source preview failed: {}", safe_message(&error)));
                        (None, None, true, false)
                    }
                }
            }
            Err(error) => {
                next_steps.push(format!(
                    "Source files could not be read: {}",
                    safe_message(&error)
                ));
                (None, None, true, false)
            }
        }
    } else {
        (None, None, false, false)
    };

    let selected_adapters = if options.adapters.is_empty() {
        vec![AdapterKind::Codex, AdapterKind::ClaudeCode]
    } else {
        let mut adapters = options.adapters;
        adapters.sort_by_key(|adapter| adapter.as_str());
        adapters.dedup();
        adapters
    };
    let source_root = discover_source_root();
    let home = home_directory();
    let adapters = selected_adapters
        .into_iter()
        .map(|adapter| inspect_adapter(adapter, home.as_deref(), source_root.as_deref()))
        .collect::<Vec<_>>();
    for adapter in &adapters {
        if !adapter.configured {
            next_steps.extend(adapter.next_commands.clone());
        }
    }

    let compose_probe = if daemon.ok {
        match client.compose_context(ComposeRequest {
            project_scope_id: Some(project_scope.id.clone()),
            task_scope_id: None,
            include_archived: false,
        }) {
            Ok(response) => {
                let summary = summarize_compose(&response);
                SetupComposeProbe {
                    ok: true,
                    included_entries: summary.included_entries,
                    estimated_tokens: summary.estimated_tokens,
                    excluded_entries: summary.excluded_entries,
                    warnings: response.warnings,
                    message: if summary.has_context {
                        "Compose succeeded with stored project context.".to_string()
                    } else {
                        "Compose succeeded; no active context is stored for this project yet."
                            .to_string()
                    },
                }
            }
            Err(error) => SetupComposeProbe {
                ok: false,
                included_entries: 0,
                estimated_tokens: 0,
                excluded_entries: 0,
                warnings: Vec::new(),
                message: format!("Compose probe failed: {}", safe_message(&error)),
            },
        }
    } else {
        SetupComposeProbe {
            ok: false,
            included_entries: 0,
            estimated_tokens: 0,
            excluded_entries: 0,
            warnings: Vec::new(),
            message: "Compose probe was skipped because the daemon is unavailable.".to_string(),
        }
    };

    if detected_sources.is_empty() {
        next_steps.push(
            "No well-known instruction files were found; add context with `contextctl entry put` or pass `--source <FILE>`."
                .to_string(),
        );
    }
    let mut seen_steps = BTreeSet::new();
    next_steps.retain(|step| seen_steps.insert(step.clone()));

    let failed = !daemon.ok
        || !compose_probe.ok
        || source_collection_failed
        || !policy.ok
        || import_failed
        || (options.review_mode.is_some() && options.apply && !policy.applied)
        || (options.apply
            && import_preview
                .as_ref()
                .is_some_and(|preview| !preview.candidates.is_empty() && import_result.is_none()));
    let needs_action = adapters.iter().any(|adapter| !adapter.configured)
        || private_paths
            .iter()
            .any(|path| path.exists && !path.private)
        || (!options.apply
            && (options.review_mode.is_some()
                || import_preview
                    .as_ref()
                    .is_some_and(|preview| !preview.candidates.is_empty())))
        || import_needs_action;
    let state = setup_state(failed, needs_action, options.apply);

    SetupReport {
        state,
        mode: if options.apply { "apply" } else { "preflight" },
        project_scope,
        daemon,
        private_paths,
        policy,
        detected_sources,
        import_preview,
        import_result,
        adapters,
        compose_probe,
        next_steps,
    }
}

pub(crate) fn format_setup_report(report: &SetupReport) -> String {
    let mut lines = vec![
        format!(
            "Setup {}: {}",
            report.mode,
            match report.state {
                SetupState::Ready => "ready",
                SetupState::NeedsAction => "needs action",
                SetupState::Applied => "applied",
                SetupState::Failed => "failed",
            }
        ),
        format!("Scope: {}", report.project_scope.label()),
        format!(
            "Daemon: {} — {}",
            if report.daemon.ok { "ok" } else { "failed" },
            report.daemon.message
        ),
    ];
    if let Some(remediation) = &report.daemon.remediation {
        lines.push(format!("  Remediation: {remediation}"));
    }
    lines.push("Private data paths:".to_string());
    for path in &report.private_paths {
        let privacy = if !path.exists {
            "missing"
        } else if path.private {
            "private"
        } else {
            "check permissions"
        };
        lines.push(format!(
            "  {}: {} [{}{}]",
            path.kind,
            path.path,
            privacy,
            path.unix_mode
                .as_deref()
                .map(|mode| format!(", mode {mode}"))
                .unwrap_or_default()
        ));
    }
    lines.push(format!("Policy: {}", report.policy.message));
    if report.detected_sources.is_empty() {
        lines.push("Sources: none detected".to_string());
    } else {
        lines.push(format!(
            "Sources: {} instruction file(s) detected",
            report.detected_sources.len()
        ));
        for source in &report.detected_sources {
            lines.push(format!("  {source}"));
        }
    }
    if let Some(preview) = &report.import_preview {
        lines.push(format!(
            "Import preview: {} candidate(s), apply {}",
            preview.candidates.len(),
            if preview.apply_allowed {
                "allowed"
            } else {
                "blocked"
            }
        ));
    }
    if let Some(result) = &report.import_result {
        lines.push(format!(
            "Import applied: {} applied, {} pending, {} rejected",
            result.applied_count, result.pending_count, result.rejected_count
        ));
        for review_id in source_import_review_ids(result) {
            lines.push(format!(
                "  Pending review: contextctl review approve {review_id}"
            ));
        }
    }

    for adapter in &report.adapters {
        lines.push(format!(
            "Adapter {}: installed={}, configured={}",
            adapter.adapter.as_str(),
            adapter.installed,
            adapter.configured
        ));
        for issue in &adapter.issues {
            lines.push(format!("  {}", issue));
        }
    }
    lines.push(format!(
        "Compose probe: {} — {} ({} entries, ~{} tokens)",
        if report.compose_probe.ok {
            "ok"
        } else {
            "failed"
        },
        report.compose_probe.message,
        report.compose_probe.included_entries,
        report.compose_probe.estimated_tokens
    ));
    if !report.next_steps.is_empty() {
        lines.push("Next steps:".to_string());
        for step in &report.next_steps {
            lines.push(format!("  {step}"));
        }
    }
    lines.join("\n")
}

fn all_candidates_rejected(result: &SourceImportApplyResult) -> bool {
    result.candidate_count > 0 && result.rejected_count == result.candidate_count
}

fn source_import_review_ids(result: &SourceImportApplyResult) -> BTreeSet<String> {
    result
        .affected_review_ids
        .iter()
        .cloned()
        .chain(
            result
                .items
                .iter()
                .filter_map(|item| item.review_id.clone()),
        )
        .collect()
}

fn import_apply_state(apply: bool, result: Option<&SourceImportApplyResult>) -> (bool, bool) {
    if !apply {
        return (false, false);
    }
    match result {
        Some(result) => (
            all_candidates_rejected(result),
            result.pending_count > 0 || result.rejected_count > 0,
        ),
        None => (true, false),
    }
}

fn setup_state(failed: bool, needs_action: bool, apply: bool) -> SetupState {
    if failed {
        SetupState::Failed
    } else if needs_action {
        SetupState::NeedsAction
    } else if apply {
        SetupState::Applied
    } else {
        SetupState::Ready
    }
}

pub(crate) fn detect_instruction_files(project: &Path) -> Vec<PathBuf> {
    let mut files = BTreeSet::new();
    add_if_file(&mut files, project.join("AGENTS.md"));
    add_matching_files(project, "CLAUDE", ".md", &mut files);
    add_if_file(&mut files, project.join(".github/copilot-instructions.md"));
    add_matching_files(
        &project.join(".github/instructions"),
        "",
        ".instructions.md",
        &mut files,
    );
    add_matching_files(&project.join(".cursor/rules"), "", ".mdc", &mut files);
    add_if_file(&mut files, project.join(".cursorrules"));
    add_matching_files(&project.join(".continue/rules"), "", ".md", &mut files);
    files.into_iter().collect()
}

fn setup_policy(
    client: &ContextClient,
    requested: Option<ReviewMode>,
    apply: bool,
) -> SetupPolicyReport {
    let current = match client.get_review_policy() {
        Ok(policy) => policy,
        Err(error) => {
            return SetupPolicyReport {
                ok: false,
                current: None,
                requested,
                applied: false,
                message: format!("Review policy could not be read: {}", safe_message(&error)),
            };
        }
    };
    let Some(requested) = requested else {
        return SetupPolicyReport {
            ok: true,
            current: Some(current.mode),
            requested: None,
            applied: false,
            message: format!("Current review mode is {}.", current.mode),
        };
    };
    if requested == current.mode {
        return SetupPolicyReport {
            ok: true,
            current: Some(current.mode),
            requested: Some(requested),
            applied: true,
            message: format!("Review mode is already {requested}."),
        };
    }
    if !apply {
        return SetupPolicyReport {
            ok: true,
            current: Some(current.mode),
            requested: Some(requested),
            applied: false,
            message: format!(
                "Would change review mode from {} to {} with `--apply --yes`.",
                current.mode, requested
            ),
        };
    }
    match client.set_review_policy(SetReviewPolicyRequest {
        mode: requested,
        metadata: json!({}),
        actor: "contextctl-setup".to_string(),
    }) {
        Ok(policy) => SetupPolicyReport {
            ok: true,
            current: Some(current.mode),
            requested: Some(requested),
            applied: true,
            message: format!(
                "Changed review mode from {} to {}.",
                current.mode, policy.mode
            ),
        },
        Err(error) => SetupPolicyReport {
            ok: false,
            current: Some(current.mode),
            requested: Some(requested),
            applied: false,
            message: format!("Review mode was not changed: {}", safe_message(&error)),
        },
    }
}

pub(crate) fn collect_setup_sources(
    project: &Path,
    explicit: &[PathBuf],
) -> anyhow::Result<Vec<PathBuf>> {
    let mut sources = BTreeSet::new();
    for source in detect_instruction_files(project)
        .into_iter()
        .chain(explicit.iter().cloned())
    {
        sources.insert(canonicalize_source_path(&source, project)?);
    }
    Ok(sources.into_iter().collect())
}

fn private_path_report(
    kind: &'static str,
    path: &Path,
    expected_directory: bool,
) -> PrivatePathReport {
    let metadata = fs::metadata(path).ok();
    let exists = metadata.is_some();
    let right_kind = metadata
        .as_ref()
        .is_some_and(|metadata| metadata.is_dir() == expected_directory);
    #[cfg(unix)]
    let (unix_mode, private) = {
        use std::os::unix::fs::PermissionsExt;
        let mode = metadata
            .as_ref()
            .map(|metadata| metadata.permissions().mode() & 0o777);
        (
            mode.map(|mode| format!("{mode:03o}")),
            right_kind && mode.is_some_and(|mode| mode & 0o077 == 0),
        )
    };
    #[cfg(not(unix))]
    let (unix_mode, private) = (None, right_kind);
    PrivatePathReport {
        kind,
        path: path.display().to_string(),
        exists,
        unix_mode,
        private,
    }
}

fn home_directory() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

fn add_if_file(files: &mut BTreeSet<PathBuf>, path: PathBuf) {
    if path.is_file() {
        files.insert(path);
    }
}

fn add_matching_files(directory: &Path, prefix: &str, suffix: &str, files: &mut BTreeSet<PathBuf>) {
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if path.is_file() && name.starts_with(prefix) && name.ends_with(suffix) {
            files.insert(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use context_core::SourceImportApplyResult;
    use tempfile::tempdir;

    #[test]
    fn detects_all_well_known_instruction_patterns() {
        let dir = tempdir().expect("tempdir");
        let files = [
            "AGENTS.md",
            "CLAUDE.md",
            "CLAUDE.local.md",
            ".github/copilot-instructions.md",
            ".github/instructions/rust.instructions.md",
            ".cursor/rules/project.mdc",
            ".cursorrules",
            ".continue/rules/team.md",
            "ordinary.md",
        ];
        for relative in files {
            let path = dir.path().join(relative);
            fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
            fs::write(path, "# Instructions").expect("write");
        }

        let detected = detect_instruction_files(dir.path())
            .into_iter()
            .map(|path| {
                path.strip_prefix(dir.path())
                    .expect("relative")
                    .display()
                    .to_string()
            })
            .collect::<Vec<_>>();
        assert_eq!(detected.len(), 8);
        assert!(detected.contains(&"AGENTS.md".to_string()));
        assert!(detected.contains(&"CLAUDE.local.md".to_string()));
        assert!(detected.contains(&".cursor/rules/project.mdc".to_string()));
        assert!(!detected.contains(&"ordinary.md".to_string()));
    }

    #[test]
    fn all_rejected_apply_is_failed_setup() {
        let result = SourceImportApplyResult {
            request_id: "import-1".to_string(),
            candidate_count: 2,
            imported_count: 0,
            applied_count: 0,
            pending_count: 0,
            skipped_count: 0,
            rejected_count: 2,
            items: Vec::new(),
            affected_entry_ids: Vec::new(),
            affected_review_ids: Vec::new(),
            affected_entry_keys: Vec::new(),
        };
        assert!(all_candidates_rejected(&result));
        let (failed, needs_action) = import_apply_state(true, Some(&result));
        assert!(failed);
        assert!(needs_action);
        assert_eq!(setup_state(failed, needs_action, true), SetupState::Failed);
    }
}
