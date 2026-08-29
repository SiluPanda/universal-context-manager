use anyhow::Result;
use context_client::SpoolRetryReport;
use context_core::{
    CommitWorkResult, ComposeResponse, ContextExportBundle, EntryRecord, HealthReport, PackRecord,
    ReviewItem, ReviewPolicy, RunRecord, ScopeRef, SearchResponse, SourceImportApplyResult,
    SourceImportPreview, StoreStats,
};
use serde::Serialize;
use std::collections::BTreeSet;

pub(crate) struct ComposeSummary {
    pub rendered: String,
    pub included_entries: usize,
    pub estimated_tokens: usize,
    pub rendered_bytes: usize,
    pub excluded_entries: usize,
    pub has_context: bool,
}

pub(crate) fn format_health(report: &HealthReport) -> String {
    format!(
        "Daemon reachable\nSchema: v{}\nStored: {} packs, {} entries, {} reviews, {} runs",
        report.schema_version, report.packs, report.entries, report.reviews, report.runs
    )
}

pub(crate) fn format_stats(stats: &StoreStats) -> String {
    format!(
        "Schema: v{}\nPacks: {}\nEntries: {}\nReviews: {}\nRuns: {}",
        stats.schema_version, stats.packs, stats.entries, stats.reviews, stats.runs
    )
}

pub(crate) fn format_compose(
    response: &ComposeResponse,
    project_scope_id: &str,
    task_scope_id: Option<&str>,
) -> String {
    let summary = summarize_compose(response);
    let mut lines = vec![format!("Scope: project:{project_scope_id}")];
    if let Some(task) = task_scope_id {
        lines.push(format!("Task: task:{task}"));
    }

    if !summary.has_context {
        lines.extend([
            "No active context found for this scope.".to_string(),
            "Add context: contextctl entry put --scope project --key <key> --body <text>"
                .to_string(),
            "Import instructions: contextctl source-import preview <FILE>".to_string(),
        ]);
    } else {
        lines.push(format!(
            "Included {} entries (~{} tokens, {} bytes)",
            summary.included_entries, summary.estimated_tokens, summary.rendered_bytes
        ));
        lines.push(String::new());
        lines.push(summary.rendered);
    }

    if summary.excluded_entries > 0 {
        lines.push(format!(
            "Excluded {} archived or deleted entries",
            summary.excluded_entries
        ));
    }
    for warning in &response.warnings {
        lines.push(format!("Warning: {}", one_line(warning)));
    }
    lines.join("\n")
}

pub(crate) fn summarize_compose(response: &ComposeResponse) -> ComposeSummary {
    let section_entries = response
        .sections
        .iter()
        .map(|section| section.entries.len())
        .sum::<usize>();
    let rendered = if response.rendered_markdown.trim().is_empty() && section_entries > 0 {
        render_sections(response)
    } else {
        response.rendered_markdown.trim().to_string()
    };
    let has_context = !rendered.is_empty() || section_entries > 0;
    let included_entries = if response.metrics.included_entries > 0 {
        response.metrics.included_entries
    } else if section_entries > 0 {
        section_entries
    } else if has_context {
        1
    } else {
        0
    };
    let rendered_bytes = if response.metrics.rendered_bytes > 0 {
        response.metrics.rendered_bytes
    } else {
        rendered.len()
    };
    let estimated_tokens = if response.metrics.estimated_tokens > 0 {
        response.metrics.estimated_tokens
    } else if rendered_bytes > 0 {
        rendered_bytes.div_ceil(4)
    } else {
        0
    };
    ComposeSummary {
        rendered,
        included_entries,
        estimated_tokens,
        rendered_bytes,
        excluded_entries: response
            .metrics
            .excluded_entries
            .max(response.exclusions.len()),
        has_context,
    }
}

fn render_sections(response: &ComposeResponse) -> String {
    let mut rendered = String::new();
    for section in &response.sections {
        if section.entries.is_empty() {
            continue;
        }
        if !rendered.is_empty() {
            rendered.push('\n');
        }
        rendered.push_str(&format!(
            "# {} / {}\n",
            section.scope.label(),
            section.pack_name
        ));
        for entry in &section.entries {
            rendered.push_str(&format!("\n## {} ({})\n", entry.key, entry.kind));
            if let Some(title) = &entry.title {
                rendered.push_str(&format!("\n**{}**\n", title.trim()));
            }
            rendered.push('\n');
            rendered.push_str(entry.value.render_markdown().trim());
            rendered.push('\n');
        }
    }
    rendered.trim().to_string()
}

pub(crate) fn format_search(
    response: &SearchResponse,
    project_scope_id: &str,
    task_scope_id: Option<&str>,
) -> String {
    let mut lines = vec![format!("Scope: project:{project_scope_id}")];
    if let Some(task) = task_scope_id {
        lines.push(format!("Task: task:{task}"));
    }
    if response.hits.is_empty() {
        lines.push(format!("No context matched {:?}.", response.query));
        lines.push("Try a broader query or add context with `contextctl entry put`.".to_string());
        return lines.join("\n");
    }
    let rows = response
        .hits
        .iter()
        .map(|hit| {
            vec![
                format!("{:.3}", hit.score),
                hit.entry.scope.label(),
                hit.entry.pack_name.clone(),
                hit.entry.key.clone(),
                truncate(&hit.snippet, 72),
            ]
        })
        .collect();
    lines.push(table(&["SCORE", "SCOPE", "PACK", "KEY", "MATCH"], rows));
    lines.join("\n")
}

pub(crate) fn format_packs(packs: &[PackRecord]) -> String {
    if packs.is_empty() {
        return [
            "No context packs exist yet.",
            "Create one: contextctl pack create --scope project --name main",
        ]
        .join("\n");
    }
    table(
        &["SCOPE", "NAME", "STATUS", "LOCKED", "DESCRIPTION"],
        packs
            .iter()
            .map(|pack| {
                vec![
                    pack.scope.label(),
                    pack.name.clone(),
                    pack.status.as_str().to_string(),
                    yes_no(pack.locked),
                    truncate(pack.description.as_deref().unwrap_or("-"), 48),
                ]
            })
            .collect(),
    )
}

pub(crate) fn format_pack(pack: &PackRecord, action: &str) -> String {
    format!(
        "{action} pack {} in {}\nStatus: {} | Locked: {} | Revision: {}",
        pack.name,
        pack.scope.label(),
        pack.status.as_str(),
        yes_no(pack.locked),
        pack.revision_no
    )
}

pub(crate) fn format_entries(entries: &[EntryRecord], resolved_scope: Option<&ScopeRef>) -> String {
    let mut lines = Vec::new();
    if let Some(scope) = resolved_scope {
        lines.push(format!("Scope: {}", scope.label()));
    }
    if entries.is_empty() {
        lines.extend([
            "No entries found.".to_string(),
            "Add one: contextctl entry put --scope project --key <key> --body <text>".to_string(),
            "Or preview an import: contextctl source-import preview <FILE>".to_string(),
        ]);
        return lines.join("\n");
    }
    lines.push(table(
        &["SCOPE", "PACK", "KEY", "KIND", "STATUS", "REV", "TITLE"],
        entries
            .iter()
            .map(|entry| {
                vec![
                    entry.scope.label(),
                    entry.pack_name.clone(),
                    entry.key.clone(),
                    entry.kind.clone(),
                    entry.status.as_str().to_string(),
                    entry.revision_no.to_string(),
                    truncate(entry.title.as_deref().unwrap_or("-"), 48),
                ]
            })
            .collect(),
    ));
    lines.join("\n")
}

pub(crate) fn format_entry(entry: &EntryRecord, action: &str) -> String {
    format!(
        "{action} entry {}/{} in {}\nKind: {} | Status: {} | Revision: {} | Locked: {}",
        entry.pack_name,
        entry.key,
        entry.scope.label(),
        entry.kind,
        entry.status.as_str(),
        entry.revision_no,
        yes_no(entry.locked)
    )
}

pub(crate) fn format_entry_detail(entry: &EntryRecord) -> String {
    let mut lines = vec![
        format!("{}/{}", entry.pack_name, entry.key),
        format!("Scope: {}", entry.scope.label()),
        format!(
            "Kind: {} | Status: {} | Revision: {} | Locked: {}",
            entry.kind,
            entry.status.as_str(),
            entry.revision_no,
            yes_no(entry.locked)
        ),
    ];
    if let Some(title) = &entry.title {
        lines.push(format!("Title: {}", one_line(title)));
    }
    if !entry.tags.is_empty() {
        lines.push(format!("Tags: {}", entry.tags.join(", ")));
    }
    lines.push(String::new());
    lines.push(entry.value.render_markdown());
    lines.join("\n")
}

pub(crate) fn format_reviews(reviews: &[ReviewItem]) -> String {
    if reviews.is_empty() {
        return [
            "No review items match this filter.",
            "Pending proposals appear here when policy or conflicts require approval.",
        ]
        .join("\n");
    }
    table(
        &["ID", "STATE", "REASON", "SCOPE", "PACK", "KEY", "TITLE"],
        reviews
            .iter()
            .map(|review| {
                vec![
                    review.id.clone(),
                    review.state.as_str().to_string(),
                    review.reason.as_str().to_string(),
                    review.scope.label(),
                    review.pack_name.clone(),
                    review.entry_key.clone(),
                    truncate(review.proposed_entry.title.as_deref().unwrap_or("-"), 40),
                ]
            })
            .collect(),
    )
}

pub(crate) fn format_review(review: &ReviewItem, action: &str) -> String {
    format!(
        "{action} review {}\nState: {} | Scope: {} | Entry: {}/{} | Revision: {}",
        review.id,
        review.state.as_str(),
        review.scope.label(),
        review.pack_name,
        review.entry_key,
        review.revision_no
    )
}

pub(crate) fn format_runs(runs: &[RunRecord]) -> String {
    if runs.is_empty() {
        return [
            "No recorded runs yet.",
            "Create one: contextctl run create --source <adapter-or-tool>",
        ]
        .join("\n");
    }
    table(
        &["ID", "PROJECT", "TASK", "SOURCE", "STARTED"],
        runs.iter()
            .map(|run| {
                vec![
                    truncate(&run.id, 18),
                    truncate(run.project_scope_id.as_deref().unwrap_or("-"), 42),
                    truncate(run.task_scope_id.as_deref().unwrap_or("-"), 24),
                    run.source.clone(),
                    run.started_at.to_rfc3339(),
                ]
            })
            .collect(),
    )
}

pub(crate) fn format_run(run: &RunRecord, action: &str) -> String {
    let project = run.project_scope_id.as_deref().unwrap_or("-");
    let task = run.task_scope_id.as_deref().unwrap_or("-");
    format!(
        "{action} run {}\nProject: {project}\nTask: {task}\nSource: {}",
        run.id, run.source
    )
}

pub(crate) fn format_commit(result: &CommitWorkResult) -> String {
    let mut lines = vec![
        format!("Commit request: {}", result.request_id),
        format!(
            "Status: {}{}",
            serialized_name(&result.status),
            if result.spooled { " (spooled)" } else { "" }
        ),
    ];
    if let Some(run_id) = &result.run_id {
        lines.push(format!("Run: {run_id}"));
    }
    if result.items.is_empty() {
        lines.push(if result.spooled {
            "No items were delivered yet. Retry with `contextctl retry-spool`.".to_string()
        } else {
            "No proposal items were returned.".to_string()
        });
        return lines.join("\n");
    }
    lines.push(table(
        &["SCOPE", "PACK", "KEY", "RESULT", "REVIEW ID", "REASON"],
        result
            .items
            .iter()
            .map(|item| {
                vec![
                    item.scope.label(),
                    item.pack_name.clone(),
                    item.entry_key.clone(),
                    serialized_name(&item.disposition),
                    item.review_id.clone().unwrap_or_else(|| "-".to_string()),
                    truncate(item.reason.as_deref().unwrap_or("-"), 48),
                ]
            })
            .collect(),
    ));
    let review_ids = result
        .items
        .iter()
        .filter_map(|item| item.review_id.clone())
        .collect::<BTreeSet<_>>();
    if !review_ids.is_empty() {
        lines.push("Pending review commands:".to_string());
        for review_id in review_ids {
            lines.push(format!("  contextctl review approve {review_id}"));
        }
    }
    lines.join("\n")
}

pub(crate) fn format_bundle_import(bundle: &ContextExportBundle) -> String {
    format!(
        "Imported UCM bundle\nPacks: {}\nEntries: {}\nReviews: {}\nRuns: {}",
        bundle.packs.len(),
        bundle.entries.len(),
        bundle.reviews.len(),
        bundle.runs.len()
    )
}

pub(crate) fn format_source_preview(preview: &SourceImportPreview) -> String {
    let mut lines = vec![
        format!("Destination: {}", preview.destination.label()),
        format!("Pack: {}", preview.pack_name),
        format!("Review mode: {}", preview.review_mode),
    ];
    if preview.candidates.is_empty() {
        lines.push("No importable context was detected.".to_string());
        lines.push(
            "Check the file type or select --source-kind plain-markdown explicitly.".to_string(),
        );
    } else {
        lines.push(table(
            &["#", "SOURCE", "DETECTED", "RESULT", "KEY", "WARNINGS"],
            preview
                .candidates
                .iter()
                .map(|candidate| {
                    vec![
                        candidate.candidate_index.to_string(),
                        truncate(candidate.source_path.as_deref().unwrap_or("-"), 42),
                        candidate.detected_source_kind.as_str().to_string(),
                        serialized_name(&candidate.disposition),
                        candidate.entry.key.clone(),
                        if candidate.warnings.is_empty() {
                            "-".to_string()
                        } else {
                            truncate(&candidate.warnings.join("; "), 56)
                        },
                    ]
                })
                .collect(),
        ));
        lines.push(if preview.apply_allowed {
            "Preview only. Apply with the matching `contextctl source-import apply ...` command."
                .to_string()
        } else {
            "Apply is blocked; resolve the reported warnings or validation errors first."
                .to_string()
        });
    }
    for warning in &preview.warnings {
        lines.push(format!("Warning: {}", one_line(warning)));
    }
    lines.join("\n")
}

pub(crate) fn format_source_apply(result: &SourceImportApplyResult) -> String {
    let mut lines = vec![
        format!("Source import: {}", result.request_id),
        format!(
            "Candidates: {} | Imported: {} | Applied: {} | Pending: {} | Skipped: {} | Rejected: {}",
            result.candidate_count,
            result.imported_count,
            result.applied_count,
            result.pending_count,
            result.skipped_count,
            result.rejected_count
        ),
    ];
    if result.items.is_empty() {
        lines.push("No source candidates were applied.".to_string());
    } else {
        lines.push(table(
            &["#", "SOURCE", "KEY", "RESULT", "REVIEW ID", "REASON"],
            result
                .items
                .iter()
                .map(|item| {
                    vec![
                        item.candidate_index.to_string(),
                        truncate(item.source_path.as_deref().unwrap_or("-"), 42),
                        item.entry_key.clone(),
                        serialized_name(&item.disposition),
                        item.review_id.clone().unwrap_or_else(|| "-".to_string()),
                        truncate(item.reason.as_deref().unwrap_or("-"), 48),
                    ]
                })
                .collect(),
        ));
    }
    let review_ids = result
        .affected_review_ids
        .iter()
        .cloned()
        .chain(
            result
                .items
                .iter()
                .filter_map(|item| item.review_id.clone()),
        )
        .collect::<BTreeSet<_>>();
    if !review_ids.is_empty() {
        lines.push("Pending review commands:".to_string());
        for review_id in review_ids {
            lines.push(format!("  contextctl review approve {review_id}"));
        }
    }
    lines.join("\n")
}

pub(crate) fn format_policy(policy: &ReviewPolicy, action: &str) -> String {
    format!(
        "{action} review policy: {}\nRevision: {} | Updated by: {} | Updated: {}",
        policy.mode,
        policy.revision_no,
        policy.updated_by,
        policy.updated_at.to_rfc3339()
    )
}

pub(crate) fn format_spool(report: &SpoolRetryReport) -> String {
    let mut lines = vec![format!(
        "Spool retry: {} attempted, {} delivered, {} retained",
        report.attempted, report.delivered, report.retained
    )];
    for error in &report.errors {
        lines.push(format!("Warning: {}", one_line(error)));
    }
    if report.attempted == 0 {
        lines.push("No queued commit requests were found.".to_string());
    }
    lines.join("\n")
}

pub(crate) fn table(headers: &[&str], rows: Vec<Vec<String>>) -> String {
    if rows.is_empty() {
        return String::new();
    }
    let mut widths = headers
        .iter()
        .map(|header| header.len())
        .collect::<Vec<_>>();
    for row in &rows {
        for (index, cell) in row.iter().enumerate().take(widths.len()) {
            widths[index] = widths[index].max(cell.chars().count());
        }
    }

    let mut output = String::new();
    append_row(
        &mut output,
        &headers
            .iter()
            .map(|header| (*header).to_string())
            .collect::<Vec<_>>(),
        &widths,
    );
    output.push('\n');
    append_row(
        &mut output,
        &widths
            .iter()
            .map(|width| "-".repeat(*width))
            .collect::<Vec<_>>(),
        &widths,
    );
    for row in rows {
        output.push('\n');
        append_row(&mut output, &row, &widths);
    }
    output
}

fn append_row(output: &mut String, row: &[String], widths: &[usize]) {
    for (index, width) in widths.iter().enumerate() {
        if index > 0 {
            output.push_str("  ");
        }
        let cell = row.get(index).map(String::as_str).unwrap_or("");
        output.push_str(cell);
        let padding = width.saturating_sub(cell.chars().count());
        output.push_str(&" ".repeat(padding));
    }
}

fn serialized_name<T: Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(ToString::to_string))
        .unwrap_or_else(|| "unknown".to_string())
}

fn truncate(value: &str, max_chars: usize) -> String {
    let value = one_line(value);
    if value.chars().count() <= max_chars {
        return value;
    }
    let mut truncated = value
        .chars()
        .take(max_chars.saturating_sub(1))
        .collect::<String>();
    truncated.push('…');
    truncated
}

fn one_line(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn yes_no(value: bool) -> String {
    if value { "yes" } else { "no" }.to_string()
}

pub(crate) fn print_human(text: String) {
    println!("{text}");
}

pub(crate) fn print_json<T: Serialize>(value: &T) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use context_core::{
        CommitDisposition, CommitStatus, CommitWorkResult, ComposeResponse, SourceImportApplyItem,
        SourceImportApplyResult,
    };
    use serde_json::json;

    #[test]
    fn empty_compose_is_actionable() {
        let response: ComposeResponse = serde_json::from_value(json!({
            "generated_at": "2026-01-01T00:00:00Z",
            "sections": [],
            "rendered_markdown": "",
            "metrics": {
                "rendered_bytes": 0,
                "estimated_tokens": 0,
                "included_entries": 0,
                "excluded_entries": 0
            },
            "exclusions": [],
            "warnings": []
        }))
        .expect("compose");
        let output = format_compose(&response, "/repo", None);
        assert!(output.contains("No active context"));
        assert!(output.contains("entry put"));
        assert!(output.contains("source-import preview"));
        assert!(output.contains("Scope: project:/repo"));
    }

    #[test]
    fn list_empty_states_are_not_raw_json() {
        assert!(format_packs(&[]).contains("pack create"));
        assert!(format_entries(&[], None).contains("entry put"));
        assert!(format_reviews(&[]).contains("No review items"));
        assert!(format_runs(&[]).contains("run create"));
    }

    #[test]
    fn legacy_compose_with_rendered_markdown_uses_fallback_metrics() {
        let response: ComposeResponse = serde_json::from_value(json!({
            "generated_at": "2026-01-01T00:00:00Z",
            "sections": [],
            "rendered_markdown": "# Legacy context\n\nKeep this.",
            "exclusions": [],
            "warnings": []
        }))
        .expect("compose");
        let output = format_compose(&response, "/repo", None);
        assert!(output.contains("Included 1 entries"));
        assert!(output.contains("# Legacy context"));
        assert!(!output.contains("No active context"));
    }

    #[test]
    fn legacy_compose_with_sections_renders_returned_entries() {
        let response: ComposeResponse = serde_json::from_value(json!({
            "generated_at": "2026-01-01T00:00:00Z",
            "sections": [{
                "scope": { "kind": "project", "id": "/repo" },
                "pack_name": "main",
                "entries": [{
                    "id": "entry-1",
                    "scope": { "kind": "project", "id": "/repo" },
                    "pack_name": "main",
                    "key": "build",
                    "title": "Build",
                    "kind": "instruction",
                    "format": "markdown",
                    "body": "Run cargo test.",
                    "tags": [],
                    "metadata": {},
                    "provenance": { "actor": "tester", "source": "test" },
                    "locked": false,
                    "status": "active",
                    "created_at": "2026-01-01T00:00:00Z",
                    "updated_at": "2026-01-01T00:00:00Z",
                    "revision_no": 1
                }]
            }],
            "rendered_markdown": "",
            "warnings": []
        }))
        .expect("compose");
        let output = format_compose(&response, "/repo", None);
        assert!(output.contains("Included 1 entries"));
        assert!(output.contains("Run cargo test."));
        assert!(!output.contains("No active context"));
    }

    #[test]
    fn review_and_pending_results_show_full_review_ids() {
        let review_id = "review-12345678-1234-1234-1234-123456789abc";
        let review: ReviewItem = serde_json::from_value(json!({
            "id": review_id,
            "request_id": "request-1",
            "scope": { "kind": "project", "id": "/repo" },
            "pack_name": "main",
            "entry_key": "build",
            "state": "pending",
            "reason": "strict_policy",
            "proposed_entry": {
                "key": "build",
                "title": "Build",
                "kind": "instruction",
                "format": "markdown",
                "body": "Run tests.",
                "tags": [],
                "metadata": {},
                "locked": false
            },
            "created_at": "2026-01-01T00:00:00Z",
            "updated_at": "2026-01-01T00:00:00Z",
            "revision_no": 1
        }))
        .expect("review");
        assert!(format_reviews(&[review]).contains(review_id));

        let commit = CommitWorkResult {
            request_id: "request-1".to_string(),
            status: CommitStatus::Pending,
            run_id: None,
            items: vec![context_core::CommitItemResult {
                scope: ScopeRef::normalized(context_core::ScopeKind::Project, "/repo")
                    .expect("scope"),
                pack_name: "main".to_string(),
                entry_key: "build".to_string(),
                disposition: CommitDisposition::Pending,
                reason: Some("strict_policy".to_string()),
                entry_id: None,
                review_id: Some(review_id.to_string()),
            }],
            spooled: false,
            spool_path: None,
        };
        let commit_output = format_commit(&commit);
        assert!(commit_output.contains(review_id));
        assert!(commit_output.contains(&format!("contextctl review approve {review_id}")));

        let source = SourceImportApplyResult {
            request_id: "import-1".to_string(),
            candidate_count: 1,
            imported_count: 1,
            applied_count: 0,
            pending_count: 1,
            skipped_count: 0,
            rejected_count: 0,
            items: vec![SourceImportApplyItem {
                candidate_index: 0,
                document_index: 0,
                source_path: Some("AGENTS.md".to_string()),
                entry_key: "agents-instructions".to_string(),
                disposition: CommitDisposition::Pending,
                reason: Some("strict_policy".to_string()),
                entry_id: None,
                review_id: Some(review_id.to_string()),
            }],
            affected_entry_ids: Vec::new(),
            affected_review_ids: vec![review_id.to_string()],
            affected_entry_keys: vec!["agents-instructions".to_string()],
        };
        let output = format_source_apply(&source);
        assert!(output.contains(review_id));
        assert!(output.contains(&format!("contextctl review approve {review_id}")));
    }

    #[test]
    fn table_alignment_is_stable() {
        let rendered = table(
            &["KEY", "VALUE"],
            vec![
                vec!["a".to_string(), "one".to_string()],
                vec!["long".to_string(), "two".to_string()],
            ],
        );
        assert_eq!(
            rendered,
            "KEY   VALUE\n----  -----\na     one  \nlong  two  "
        );
    }
}
