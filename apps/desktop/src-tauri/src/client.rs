use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    fs,
    io::Write,
    path::{Path, PathBuf},
};

use chrono::Utc;
use context_client::{ClientConfig, ContextClient};
use context_core::{
    ComposeRequest, ContextPaths, CreatePackRequest, EntryInput, EntryRecord, EntrySelector,
    EntryValue, ExportRequest, HealthReport, ImportFormat, ImportRequest, PackRecord,
    PackStatus as CorePackStatus, Provenance, PutEntryRequest, RevertEntryRequest,
    ReviewDecisionRequest, ReviewEditRequest, ReviewItem as CoreReviewItem, ReviewReason,
    ReviewState, RunInput, RunRecord, ScopeKind as CoreScopeKind, ScopeRef, SearchRequest,
    StoreStats, UpdatePackRequest,
};
use rusqlite::{params, Connection, OpenFlags, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::models::{
    ActivityRun, AdapterHealth, AdapterKind, AdapterStatus, ContextPack, ContextPreview,
    DashboardSnapshot, DashboardStats, ImportExportSummary, PackStatus as ViewPackStatus,
    PreviewSection, PreviewSource, RestoreRevisionResult, ReviewDecision as ViewReviewDecision,
    ReviewDecisionInput, ReviewItem, ReviewMode, RevisionEntry, RiskLevel, RunStatus,
    SavePackInput, ScopeKind as ViewScopeKind, SearchKind, SearchResult, Settings, ThemeMode,
    WorkspaceNode,
};

const DEFAULT_ACTOR: &str = "desktop-operator";
const DEFAULT_ENTRY_KEY: &str = "body";
const DESKTOP_METADATA_KEY: &str = "desktop";
const SETTINGS_FILE_NAME: &str = "desktop-settings.json";
const ADAPTER_DAEMON: &str = "adapter-daemon";
const ADAPTER_CODEX: &str = "adapter-codex";
const ADAPTER_CLAUDE: &str = "adapter-claude-code";

#[derive(Clone)]
pub(crate) struct DesktopContextService<B: ContextBackend> {
    backend: B,
    settings_store: LocalSettingsStore,
}

pub(crate) type DesktopContextClient = DesktopContextService<LiveContextBackend>;

impl DesktopContextClient {
    pub fn new() -> Result<Self, String> {
        let base_paths = ContextPaths::discover().map_err(|error| error.to_string())?;
        Ok(Self::with_backend(
            LiveContextBackend,
            LocalSettingsStore::new(base_paths),
        ))
    }
}

impl<B: ContextBackend> DesktopContextService<B> {
    fn with_backend(backend: B, settings_store: LocalSettingsStore) -> Self {
        Self {
            backend,
            settings_store,
        }
    }

    pub fn load_dashboard(&self) -> Result<DashboardSnapshot, String> {
        let runtime = self.runtime_snapshot()?;
        let scope_catalog = build_scope_catalog(&runtime.bundles, &runtime.reviews, &runtime.runs);
        let workspace = scope_catalog.to_workspace_nodes();
        let selected_scope_id = pick_selected_scope_id(&scope_catalog, &runtime.settings);
        let packs = runtime
            .bundles
            .iter()
            .map(|bundle| bundle.to_view_pack())
            .collect::<Vec<_>>();
        let review_queue = map_review_items(&runtime.reviews, &runtime.pack_lookup);
        let activity = map_runs(&runtime.runs);
        let revisions = if let Some(bundle) = runtime
            .bundles
            .iter()
            .find(|bundle| bundle.scope_id == selected_scope_id)
        {
            self.load_revisions_for_bundle(bundle, &runtime.paths)?
        } else {
            Vec::new()
        };
        let adapters = derive_adapters(
            &runtime.paths,
            runtime.connected,
            runtime.health.as_ref(),
            runtime.stats.as_ref(),
            &runtime.settings,
        )?;
        let stats = DashboardStats {
            active_packs: packs
                .iter()
                .filter(|pack| matches!(pack.status, ViewPackStatus::Active))
                .count() as u32,
            pending_reviews: review_queue.len() as u32,
            healthy_adapters: adapters
                .iter()
                .filter(|adapter| matches!(adapter.health, AdapterHealth::Healthy))
                .count() as u32,
            running_agents: activity
                .iter()
                .filter(|run| matches!(run.status, RunStatus::Running))
                .count() as u32,
        };

        Ok(DashboardSnapshot {
            workspace,
            packs,
            review_queue,
            activity,
            revisions,
            adapters,
            settings: runtime.settings.to_public(),
            stats,
            selected_scope_id,
            connected: runtime.connected,
            last_sync_at: now_iso(),
            notices: runtime.notices,
        })
    }

    pub fn list_packs(&self, scope_id: Option<String>) -> Result<Vec<ContextPack>, String> {
        let runtime = self.runtime_snapshot()?;
        let mut packs = runtime
            .bundles
            .into_iter()
            .map(|bundle| bundle.to_view_pack())
            .collect::<Vec<_>>();
        if let Some(scope_id) = scope_id {
            packs.retain(|pack| pack.scope_id == scope_id);
        }
        packs.sort_by(|left, right| right.updated_at.cmp(&left.updated_at));
        Ok(packs)
    }

    pub fn save_pack(&self, input: SavePackInput) -> Result<ContextPack, String> {
        let mut settings = self.settings_store.load()?;
        let paths = self.settings_store.resolve_paths(&settings)?;
        let scope = decode_scope_id(&input.scope_id)?;
        let pack_catalog = self.backend.list_packs(&paths)?;
        let existing = input
            .id
            .as_ref()
            .and_then(|id| pack_catalog.iter().find(|pack| &pack.id == id).cloned());
        let existing_entries = if let Some(pack) = &existing {
            self.backend.list_entries(
                &paths,
                ExportRequest {
                    project_scope_id: None,
                    task_scope_id: None,
                    scope: Some(pack.scope.clone()),
                    pack_name: Some(pack.name.clone()),
                    include_deleted: false,
                    include_reviews: false,
                    include_runs: false,
                },
            )?
        } else {
            Vec::new()
        };
        let existing_primary_key = existing
            .as_ref()
            .and_then(|pack| desktop_pack_metadata(&pack.metadata).primary_entry_key)
            .or_else(|| {
                existing_entries
                    .iter()
                    .find(|entry| entry.key == DEFAULT_ENTRY_KEY)
                    .map(|entry| entry.key.clone())
            })
            .or_else(|| existing_entries.first().map(|entry| entry.key.clone()))
            .unwrap_or_else(|| DEFAULT_ENTRY_KEY.to_string());

        let name_input = trimmed_or(&input.name, "Untitled context pack");
        let pack_name = existing
            .as_ref()
            .map(|pack| pack.name.clone())
            .unwrap_or_else(|| {
                unique_pack_name(&scope, &slug_pack_name(&name_input), &pack_catalog)
            });
        let summary = trimmed_or(&input.summary, "No summary provided yet.");
        let parent_project_scope_id = if matches!(scope.kind, CoreScopeKind::Task) {
            self.find_parent_project_scope_id(&paths, &scope.id)?
        } else {
            None
        };

        let pack_metadata = update_desktop_pack_metadata(
            existing
                .as_ref()
                .map(|pack| pack.metadata.clone())
                .unwrap_or_else(|| json!({})),
            &name_input,
            &summary,
            &input.status,
            &existing_primary_key,
            parent_project_scope_id.as_deref(),
        );
        let core_status = match input.status {
            ViewPackStatus::Draft => CorePackStatus::Archived,
            ViewPackStatus::Active | ViewPackStatus::Review => CorePackStatus::Active,
        };

        let mut pack_record = match existing {
            Some(pack) => {
                let selector = context_core::PackSelector {
                    scope: pack.scope.clone(),
                    name: pack.name.clone(),
                };
                if matches!(pack.status, CorePackStatus::Archived) {
                    self.backend.update_pack(
                        &paths,
                        UpdatePackRequest {
                            selector: selector.clone(),
                            description: None,
                            metadata: None,
                            status: Some(CorePackStatus::Active),
                            locked: None,
                            lock_reason: None,
                            actor: DEFAULT_ACTOR.to_string(),
                        },
                    )?;
                }
                self.backend.update_pack(
                    &paths,
                    UpdatePackRequest {
                        selector,
                        description: Some(summary.clone()),
                        metadata: Some(pack_metadata.clone()),
                        status: Some(CorePackStatus::Active),
                        locked: Some(pack.locked),
                        lock_reason: pack.lock_reason.clone(),
                        actor: DEFAULT_ACTOR.to_string(),
                    },
                )?
            }
            None => self.backend.create_pack(
                &paths,
                CreatePackRequest {
                    scope: scope.clone(),
                    name: pack_name.clone(),
                    description: Some(summary.clone()),
                    metadata: pack_metadata.clone(),
                    locked: false,
                    lock_reason: None,
                    actor: DEFAULT_ACTOR.to_string(),
                },
            )?,
        };

        let entry_metadata = update_desktop_entry_metadata(
            existing_entries
                .iter()
                .find(|entry| entry.key == existing_primary_key)
                .map(|entry| entry.metadata.clone())
                .unwrap_or_else(|| json!({})),
        );
        let title = if summary.is_empty() {
            Some(name_input.clone())
        } else {
            Some(summary.clone())
        };

        self.backend.put_entry(
            &paths,
            PutEntryRequest {
                scope: scope.clone(),
                pack_name: pack_record.name.clone(),
                entry: EntryInput {
                    key: existing_primary_key.clone(),
                    title,
                    kind: "context_note".to_string(),
                    value: EntryValue::Markdown {
                        body: input.body.trim().to_string(),
                    },
                    tags: input.tags.clone(),
                    metadata: entry_metadata,
                    locked: false,
                    provenance: Some(Provenance::system(DEFAULT_ACTOR, "desktop_editor")),
                },
                actor: DEFAULT_ACTOR.to_string(),
            },
        )?;

        if pack_record.status != core_status {
            pack_record = self.backend.update_pack(
                &paths,
                UpdatePackRequest {
                    selector: context_core::PackSelector {
                        scope: pack_record.scope.clone(),
                        name: pack_record.name.clone(),
                    },
                    description: Some(summary.clone()),
                    metadata: Some(pack_metadata.clone()),
                    status: Some(core_status.clone()),
                    locked: Some(pack_record.locked),
                    lock_reason: pack_record.lock_reason.clone(),
                    actor: DEFAULT_ACTOR.to_string(),
                },
            )?;
        }

        let _ = self.backend.create_run(
            &paths,
            RunInput {
                id: None,
                project_scope_id: project_scope_id_for(&scope, parent_project_scope_id.as_deref()),
                task_scope_id: task_scope_id_for(&scope),
                source: "desktop.save_pack".to_string(),
                metadata: json!({
                    "summary": format!("Saved {}", display_name_for_pack(&pack_record)),
                    "status": "completed",
                    "step_count": 2,
                    "pack_ids": [pack_record.id.clone()]
                }),
            },
        );

        settings.socket_path = paths.socket_path.display().to_string();
        self.settings_store.save(&settings)?;

        let refreshed_entries = self.backend.list_entries(
            &paths,
            ExportRequest {
                project_scope_id: None,
                task_scope_id: None,
                scope: Some(scope.clone()),
                pack_name: Some(pack_record.name.clone()),
                include_deleted: false,
                include_reviews: false,
                include_runs: false,
            },
        )?;
        let bundle = PackBundle::from_parts(
            pack_record,
            refreshed_entries,
            &HashMap::new(),
            parent_project_scope_id,
        );
        Ok(bundle.to_view_pack())
    }

    pub fn compose_preview(&self, scope_id: String) -> Result<ContextPreview, String> {
        let runtime = self.runtime_snapshot()?;
        let scope_catalog = build_scope_catalog(&runtime.bundles, &runtime.reviews, &runtime.runs);
        let scope_entry = scope_catalog
            .entries
            .get(&scope_id)
            .ok_or_else(|| format!("Unknown scope: {scope_id}"))?;
        let compose_request = compose_request_for_scope(scope_entry);
        let sections = self
            .backend
            .compose_context(&runtime.paths, compose_request)?
            .sections;

        let preview_sections = sections
            .iter()
            .map(|section| {
                let body = render_section_body(section.entries.as_slice());
                PreviewSection {
                    id: format!(
                        "preview:{}:{}",
                        encode_scope_ref(&section.scope),
                        section.pack_name
                    ),
                    title: section_title_for_scope(&section.scope.kind),
                    pack_name: display_name_from_lookup(
                        &runtime.pack_lookup,
                        &pack_lookup_key(&encode_scope_ref(&section.scope), &section.pack_name),
                    )
                    .unwrap_or_else(|| section.pack_name.clone()),
                    scope_label: scope_label(&section.scope),
                    scope_kind: map_scope_kind(&section.scope.kind),
                    tokens: estimate_tokens(&body),
                    body,
                }
            })
            .collect::<Vec<_>>();
        let sources = preview_sections
            .iter()
            .map(|section| PreviewSource {
                pack_id: pack_id_from_lookup(
                    &runtime.pack_lookup,
                    &pack_lookup_key_for_section(section),
                )
                .unwrap_or_else(|| format!("{}:{}", section.scope_label, section.pack_name)),
                pack_name: section.pack_name.clone(),
                scope_label: section.scope_label.clone(),
                excerpt: summarize_excerpt(&section.body, 140),
                tokens: section.tokens,
            })
            .collect::<Vec<_>>();
        let total_tokens = preview_sections.iter().map(|section| section.tokens).sum();

        let relevant_scope_ids = relevant_scope_ids(scope_entry);
        let mut warnings = Vec::new();
        if runtime.bundles.iter().any(|bundle| {
            relevant_scope_ids.contains(&bundle.scope_id)
                && matches!(bundle.status, ViewPackStatus::Draft)
        }) {
            warnings.push(
                "Draft packs exist for this scope and are excluded from the composed preview."
                    .to_string(),
            );
        }
        if total_tokens > runtime.settings.max_preview_tokens {
            warnings.push(format!(
                "Preview exceeds the {} token budget; trim before export.",
                runtime.settings.max_preview_tokens
            ));
        }

        Ok(ContextPreview {
            scope_id: scope_id.clone(),
            headline: format!("{} composed preview", scope_entry.label),
            total_tokens,
            warnings,
            sections: preview_sections,
            sources,
        })
    }

    pub fn search_index(&self, query: String) -> Result<Vec<SearchResult>, String> {
        let runtime = self.runtime_snapshot()?;
        if query.trim().is_empty() {
            return Ok(Vec::new());
        }
        let mut results = Vec::new();
        if runtime.connected {
            let response = self.backend.search_context(
                &runtime.paths,
                SearchRequest {
                    query: query.clone(),
                    project_scope_id: None,
                    task_scope_id: None,
                    limit: 12,
                },
            )?;
            results.extend(response.hits.into_iter().map(|hit| {
                let scope_id = encode_scope_ref(&hit.entry.scope);
                let title = display_name_from_lookup(
                    &runtime.pack_lookup,
                    &pack_lookup_key(&scope_id, &hit.entry.pack_name),
                )
                .map(|name| {
                    format!(
                        "{} / {}",
                        name,
                        hit.entry.title.clone().unwrap_or(hit.entry.key.clone())
                    )
                })
                .unwrap_or_else(|| {
                    format!(
                        "{} / {}",
                        hit.entry.pack_name,
                        hit.entry.title.clone().unwrap_or(hit.entry.key.clone())
                    )
                });
                SearchResult {
                    id: format!("entry:{}:{}", hit.entry.id, hit.entry.revision_no),
                    kind: SearchKind::Pack,
                    title,
                    excerpt: summarize_excerpt(&hit.snippet.replace(['[', ']'], ""), 140),
                    scope_label: scope_label(&hit.entry.scope),
                    score: normalize_search_score(hit.score),
                    updated_at: hit.entry.updated_at.to_rfc3339(),
                    tags: hit.entry.tags,
                }
            }));
        }

        let needle = query.trim().to_lowercase();
        for bundle in &runtime.bundles {
            let haystack = format!(
                "{} {} {} {}",
                bundle.display_name,
                bundle.summary,
                bundle.body,
                bundle.tags.join(" ")
            )
            .to_lowercase();
            if haystack.contains(&needle) {
                results.push(SearchResult {
                    id: bundle.id.clone(),
                    kind: SearchKind::Pack,
                    title: format!(
                        "{} / {}",
                        bundle.display_name,
                        summarize_excerpt(&bundle.summary, 60)
                    ),
                    excerpt: summarize_excerpt(&bundle.body, 140),
                    scope_label: bundle.scope_label.clone(),
                    score: 89,
                    updated_at: bundle.updated_at.clone(),
                    tags: bundle.tags.clone(),
                });
            }
        }
        for review in map_review_items(&runtime.reviews, &runtime.pack_lookup) {
            let haystack = format!(
                "{} {} {}",
                review.title, review.summary, review.suggested_edit
            )
            .to_lowercase();
            if haystack.contains(&needle) {
                results.push(SearchResult {
                    id: review.id.clone(),
                    kind: SearchKind::Review,
                    title: review.title.clone(),
                    excerpt: summarize_excerpt(&review.summary, 140),
                    scope_label: review.scope_label.clone(),
                    score: 91,
                    updated_at: review.requested_at.clone(),
                    tags: vec![risk_tag(&review.risk), "review".to_string()],
                });
            }
        }
        for run in map_runs(&runtime.runs) {
            let haystack = format!("{} {}", run.actor, run.summary).to_lowercase();
            if haystack.contains(&needle) {
                results.push(SearchResult {
                    id: run.id.clone(),
                    kind: SearchKind::Run,
                    title: run.summary.clone(),
                    excerpt: summarize_excerpt(
                        &format!("{} · {}", run.actor, run_status_tag(&run.status)),
                        120,
                    ),
                    scope_label: "Activity".to_string(),
                    score: 84,
                    updated_at: run.started_at.clone(),
                    tags: vec![run_status_tag(&run.status)],
                });
            }
        }
        for adapter in derive_adapters(
            &runtime.paths,
            runtime.connected,
            runtime.health.as_ref(),
            runtime.stats.as_ref(),
            &runtime.settings,
        )? {
            let haystack = format!("{} {}", adapter.name, adapter.note).to_lowercase();
            if haystack.contains(&needle) {
                results.push(SearchResult {
                    id: adapter.id.clone(),
                    kind: SearchKind::Adapter,
                    title: adapter.name.clone(),
                    excerpt: summarize_excerpt(&adapter.note, 140),
                    scope_label: "Adapters".to_string(),
                    score: 74,
                    updated_at: adapter.last_checked_at.clone(),
                    tags: vec![
                        adapter_health_tag(&adapter.health),
                        adapter_kind_tag(&adapter.kind),
                    ],
                });
            }
        }

        results.sort_by(|left, right| {
            right
                .score
                .cmp(&left.score)
                .then_with(|| right.updated_at.cmp(&left.updated_at))
        });
        results.truncate(12);
        Ok(results)
    }

    pub fn list_revisions(&self, entity_id: Option<String>) -> Result<Vec<RevisionEntry>, String> {
        let runtime = self.runtime_snapshot()?;
        if let Some(entity_id) = entity_id {
            if let Some(bundle) = runtime.bundles.iter().find(|bundle| bundle.id == entity_id) {
                return self.load_revisions_for_bundle(bundle, &runtime.paths);
            }
        }
        let mut revisions = Vec::new();
        for bundle in &runtime.bundles {
            revisions.extend(self.load_revisions_for_bundle(bundle, &runtime.paths)?);
        }
        revisions.sort_by(|left, right| right.created_at.cmp(&left.created_at));
        revisions.truncate(30);
        Ok(revisions)
    }

    pub fn review_decision(&self, input: ReviewDecisionInput) -> Result<(), String> {
        let runtime = self.runtime_snapshot()?;
        let review = runtime
            .reviews
            .iter()
            .find(|item| item.id == input.item_id)
            .cloned()
            .ok_or_else(|| format!("Unknown review item: {}", input.item_id))?;
        match input.decision {
            ViewReviewDecision::Approve => {
                self.backend.review_approve(
                    &runtime.paths,
                    ReviewDecisionRequest {
                        review_id: review.id.clone(),
                        actor: DEFAULT_ACTOR.to_string(),
                        note: Some("Approved from desktop review queue.".to_string()),
                    },
                )?;
            }
            ViewReviewDecision::Reject => {
                self.backend.review_reject(
                    &runtime.paths,
                    ReviewDecisionRequest {
                        review_id: review.id.clone(),
                        actor: DEFAULT_ACTOR.to_string(),
                        note: Some("Rejected from desktop review queue.".to_string()),
                    },
                )?;
            }
            ViewReviewDecision::Edit => {
                let content = input
                    .edited_content
                    .clone()
                    .unwrap_or_else(|| suggested_edit_body(&review));
                let edited_value = edited_review_value(&review.proposed_entry.value, content)?;
                self.backend.review_edit(
                    &runtime.paths,
                    ReviewEditRequest {
                        review_id: review.id.clone(),
                        title: review.proposed_entry.title.clone(),
                        kind: Some(review.proposed_entry.kind.clone()),
                        value: Some(edited_value),
                        tags: Some(review.proposed_entry.tags.clone()),
                        metadata: Some(review.proposed_entry.metadata.clone()),
                        locked: Some(review.proposed_entry.locked),
                        actor: DEFAULT_ACTOR.to_string(),
                    },
                )?;
                self.backend.review_approve(
                    &runtime.paths,
                    ReviewDecisionRequest {
                        review_id: review.id.clone(),
                        actor: DEFAULT_ACTOR.to_string(),
                        note: Some("Edited and approved from desktop review queue.".to_string()),
                    },
                )?;
            }
        }
        let _ = self.backend.create_run(
            &runtime.paths,
            RunInput {
                id: None,
                project_scope_id: match review.scope.kind {
                    CoreScopeKind::Project => Some(review.scope.id.clone()),
                    CoreScopeKind::Task => self.find_parent_project_scope_id(&runtime.paths, &review.scope.id)?,
                    CoreScopeKind::Global => None,
                },
                task_scope_id: match review.scope.kind {
                    CoreScopeKind::Task => Some(review.scope.id.clone()),
                    _ => None,
                },
                source: "desktop.review".to_string(),
                metadata: json!({
                    "summary": format!("{} review {}", display_name_for_review(&review), review_decision_label(&input.decision)),
                    "status": "completed",
                    "step_count": 2
                }),
            },
        );
        Ok(())
    }

    pub fn restore_revision(&self, revision_id: String) -> Result<RestoreRevisionResult, String> {
        let settings = self.settings_store.load()?;
        let paths = self.settings_store.resolve_paths(&settings)?;
        let record = load_revision_record(&paths.db_path, &revision_id)?
            .ok_or_else(|| format!("Unknown revision: {revision_id}"))?;
        let entity_id = match record.entity_type.as_str() {
            "entry" => {
                let snapshot: EntryRecord = serde_json::from_str(&record.snapshot_json)
                    .map_err(|error| error.to_string())?;
                let pack = self
                    .backend
                    .list_packs(&paths)?
                    .into_iter()
                    .find(|pack| pack.scope == snapshot.scope && pack.name == snapshot.pack_name)
                    .ok_or_else(|| {
                        format!("Pack not found for revision: {}", snapshot.pack_name)
                    })?;
                let was_archived = matches!(pack.status, CorePackStatus::Archived);
                if was_archived {
                    self.set_pack_status_only(&paths, &pack, CorePackStatus::Active)?;
                }
                let revert_result = self.backend.revert_entry(
                    &paths,
                    RevertEntryRequest {
                        selector: EntrySelector {
                            scope: snapshot.scope.clone(),
                            pack_name: snapshot.pack_name.clone(),
                            entry_key: snapshot.key.clone(),
                        },
                        revision_no: Some(record.revision_no),
                        actor: DEFAULT_ACTOR.to_string(),
                    },
                );
                if was_archived {
                    // Restore the draft/archive state even if the entry revert
                    // failed after the temporary unarchive.
                    let rearchive_result =
                        self.set_pack_status_only(&paths, &pack, CorePackStatus::Archived);
                    if let Err(error) = rearchive_result {
                        return Err(format!("failed to re-archive pack after restore: {error}"));
                    }
                }
                revert_result?;
                pack.id
            }
            "pack" => {
                let snapshot: PackRecord = serde_json::from_str(&record.snapshot_json)
                    .map_err(|error| error.to_string())?;
                if let Some(current) = self
                    .backend
                    .list_packs(&paths)?
                    .into_iter()
                    .find(|pack| pack.id == snapshot.id)
                {
                    if matches!(current.status, CorePackStatus::Archived) {
                        self.set_pack_status_only(&paths, &current, CorePackStatus::Active)?;
                    }
                }
                self.backend.update_pack(
                    &paths,
                    UpdatePackRequest {
                        selector: context_core::PackSelector {
                            scope: snapshot.scope.clone(),
                            name: snapshot.name.clone(),
                        },
                        description: snapshot.description.clone(),
                        metadata: Some(snapshot.metadata.clone()),
                        status: Some(snapshot.status.clone()),
                        locked: Some(snapshot.locked),
                        lock_reason: snapshot.lock_reason.clone(),
                        actor: DEFAULT_ACTOR.to_string(),
                    },
                )?;
                snapshot.id
            }
            other => return Err(format!("Unsupported revision entity type: {other}")),
        };

        let _ = self.backend.create_run(
            &paths,
            RunInput {
                id: None,
                project_scope_id: None,
                task_scope_id: None,
                source: "desktop.restore_revision".to_string(),
                metadata: json!({
                    "summary": format!("Restored revision {}", revision_id),
                    "status": "completed",
                    "step_count": 1
                }),
            },
        );

        Ok(RestoreRevisionResult {
            revision_id,
            entity_id,
            restored_at: now_iso(),
        })
    }

    fn set_pack_status_only(
        &self,
        paths: &ContextPaths,
        pack: &PackRecord,
        status: CorePackStatus,
    ) -> Result<PackRecord, String> {
        self.backend.update_pack(
            paths,
            UpdatePackRequest {
                selector: context_core::PackSelector {
                    scope: pack.scope.clone(),
                    name: pack.name.clone(),
                },
                description: None,
                metadata: None,
                status: Some(status),
                locked: None,
                lock_reason: None,
                actor: DEFAULT_ACTOR.to_string(),
            },
        )
    }

    pub fn list_adapters(&self) -> Result<Vec<AdapterStatus>, String> {
        let runtime = self.runtime_snapshot()?;
        derive_adapters(
            &runtime.paths,
            runtime.connected,
            runtime.health.as_ref(),
            runtime.stats.as_ref(),
            &runtime.settings,
        )
    }

    pub fn toggle_adapter(
        &self,
        adapter_id: String,
        enabled: bool,
    ) -> Result<AdapterStatus, String> {
        let mut settings = self.settings_store.load()?;
        settings.adapter_enabled.insert(adapter_id.clone(), enabled);
        let settings = self.settings_store.save(&settings)?;
        let paths = self.settings_store.resolve_paths(&settings)?;
        derive_adapters(&paths, false, None, None, &settings)?
            .into_iter()
            .find(|adapter| adapter.id == adapter_id)
            .ok_or_else(|| format!("Unknown adapter: {adapter_id}"))
    }

    pub fn load_settings(&self) -> Result<Settings, String> {
        Ok(self.settings_store.load()?.to_public())
    }

    pub fn save_settings(&self, settings: Settings) -> Result<Settings, String> {
        let current = self.settings_store.load()?;
        let next = LocalSettings::from_public(settings, current.adapter_enabled);
        Ok(self.settings_store.save(&next)?.to_public())
    }

    pub fn export_archive(&self, path: String) -> Result<ImportExportSummary, String> {
        let settings = self.settings_store.load()?;
        let paths = self.settings_store.resolve_paths(&settings)?;
        let expanded_path = expand_user_path(&path);
        let exported_at = now_iso();
        let payload = self.backend.export_json(
            &paths,
            ExportRequest {
                project_scope_id: None,
                task_scope_id: None,
                scope: None,
                pack_name: None,
                include_deleted: false,
                include_reviews: true,
                include_runs: true,
            },
        )?;
        write_text_file(&expanded_path, &payload)?;
        let bundle = self.backend.export_bundle(
            &paths,
            ExportRequest {
                project_scope_id: None,
                task_scope_id: None,
                scope: None,
                pack_name: None,
                include_deleted: false,
                include_reviews: true,
                include_runs: true,
            },
        )?;
        let _ = self.backend.create_run(
            &paths,
            RunInput {
                id: None,
                project_scope_id: None,
                task_scope_id: None,
                source: "desktop.export".to_string(),
                metadata: json!({
                    "summary": format!("Exported {} packs", bundle.packs.len()),
                    "status": "completed",
                    "step_count": 1
                }),
            },
        );
        Ok(ImportExportSummary {
            path,
            packs_imported: bundle.packs.len(),
            adapters_touched: 0,
            revision_id: format!("export-{}", Utc::now().timestamp()),
            exported_at,
        })
    }

    pub fn import_archive(&self, path: String) -> Result<ImportExportSummary, String> {
        let settings = self.settings_store.load()?;
        let paths = self.settings_store.resolve_paths(&settings)?;
        let expanded_path = expand_user_path(&path);
        let payload = fs::read_to_string(&expanded_path).map_err(|error| error.to_string())?;
        let bundle = self.backend.import_data(
            &paths,
            ImportRequest {
                actor: DEFAULT_ACTOR.to_string(),
                format: detect_import_format(&expanded_path, &payload),
                payload,
            },
        )?;
        let _ = self.backend.create_run(
            &paths,
            RunInput {
                id: None,
                project_scope_id: None,
                task_scope_id: None,
                source: "desktop.import".to_string(),
                metadata: json!({
                    "summary": format!("Imported {} packs", bundle.packs.len()),
                    "status": "completed",
                    "step_count": 1
                }),
            },
        );
        Ok(ImportExportSummary {
            path,
            packs_imported: bundle.packs.len(),
            adapters_touched: 0,
            revision_id: format!("import-{}", Utc::now().timestamp()),
            exported_at: now_iso(),
        })
    }

    fn runtime_snapshot(&self) -> Result<RuntimeSnapshot, String> {
        let settings = self.settings_store.load()?;
        let paths = self.settings_store.resolve_paths(&settings)?;
        let mut notices = Vec::new();

        let health = match self.backend.ping(&paths) {
            Ok(report) => Some(report),
            Err(error) => {
                notices.push(format!("Context daemon unavailable: {error}"));
                None
            }
        };
        let connected = health.is_some();
        let stats = if connected {
            self.backend.stats(&paths).ok()
        } else {
            None
        };
        let packs = if connected {
            self.backend.list_packs(&paths).unwrap_or_else(|error| {
                notices.push(format!("Unable to load packs: {error}"));
                Vec::new()
            })
        } else {
            Vec::new()
        };
        let entries = if connected {
            self.backend
                .list_entries(
                    &paths,
                    ExportRequest {
                        project_scope_id: None,
                        task_scope_id: None,
                        scope: None,
                        pack_name: None,
                        include_deleted: false,
                        include_reviews: false,
                        include_runs: false,
                    },
                )
                .unwrap_or_else(|error| {
                    notices.push(format!("Unable to load entries: {error}"));
                    Vec::new()
                })
        } else {
            Vec::new()
        };
        let reviews = if connected {
            self.backend
                .review_list(&paths, Some(ReviewState::Pending))
                .unwrap_or_else(|error| {
                    notices.push(format!("Unable to load review queue: {error}"));
                    Vec::new()
                })
        } else {
            Vec::new()
        };
        let runs = if connected {
            self.backend.list_runs(&paths).unwrap_or_else(|error| {
                notices.push(format!("Unable to load runs: {error}"));
                Vec::new()
            })
        } else {
            Vec::new()
        };

        let bundles = build_pack_bundles(packs, entries, &reviews);
        let pack_lookup = bundles
            .iter()
            .map(|bundle| {
                (
                    pack_lookup_key(&bundle.scope_id, &bundle.core_name),
                    bundle.clone(),
                )
            })
            .collect::<HashMap<_, _>>();

        Ok(RuntimeSnapshot {
            settings,
            paths,
            connected,
            health,
            stats,
            bundles,
            reviews,
            runs,
            notices,
            pack_lookup,
        })
    }

    fn load_revisions_for_bundle(
        &self,
        bundle: &PackBundle,
        paths: &ContextPaths,
    ) -> Result<Vec<RevisionEntry>, String> {
        let entry_ids = bundle
            .entries
            .iter()
            .map(|entry| entry.id.clone())
            .collect::<Vec<_>>();
        load_pack_revisions(paths, bundle, &entry_ids)
    }

    fn find_parent_project_scope_id(
        &self,
        paths: &ContextPaths,
        task_scope_id: &str,
    ) -> Result<Option<String>, String> {
        let reviews = self
            .backend
            .review_list(paths, Some(ReviewState::Pending))
            .unwrap_or_default();
        let runs = self.backend.list_runs(paths).unwrap_or_default();
        let bundles = build_pack_bundles(
            self.backend.list_packs(paths).unwrap_or_default(),
            self.backend
                .list_entries(
                    paths,
                    ExportRequest {
                        project_scope_id: None,
                        task_scope_id: None,
                        scope: None,
                        pack_name: None,
                        include_deleted: false,
                        include_reviews: false,
                        include_runs: false,
                    },
                )
                .unwrap_or_default(),
            &reviews,
        );
        Ok(build_scope_catalog(&bundles, &reviews, &runs)
            .entries
            .get(&encode_scope_ref(
                &ScopeRef::normalized(CoreScopeKind::Task, task_scope_id)
                    .map_err(|error| error.to_string())?,
            ))
            .and_then(|entry| entry.parent_project_scope_id.clone()))
    }
}

struct RuntimeSnapshot {
    settings: LocalSettings,
    paths: ContextPaths,
    connected: bool,
    health: Option<HealthReport>,
    stats: Option<StoreStats>,
    bundles: Vec<PackBundle>,
    reviews: Vec<CoreReviewItem>,
    runs: Vec<RunRecord>,
    notices: Vec<String>,
    pack_lookup: HashMap<String, PackBundle>,
}

pub(crate) trait ContextBackend: Clone + Send + Sync + 'static {
    fn ping(&self, paths: &ContextPaths) -> Result<HealthReport, String>;
    fn stats(&self, paths: &ContextPaths) -> Result<StoreStats, String>;
    fn compose_context(
        &self,
        paths: &ContextPaths,
        request: ComposeRequest,
    ) -> Result<context_core::ComposeResponse, String>;
    fn search_context(
        &self,
        paths: &ContextPaths,
        request: SearchRequest,
    ) -> Result<context_core::SearchResponse, String>;
    fn create_pack(
        &self,
        paths: &ContextPaths,
        request: CreatePackRequest,
    ) -> Result<PackRecord, String>;
    fn update_pack(
        &self,
        paths: &ContextPaths,
        request: UpdatePackRequest,
    ) -> Result<PackRecord, String>;
    fn list_packs(&self, paths: &ContextPaths) -> Result<Vec<PackRecord>, String>;
    fn put_entry(
        &self,
        paths: &ContextPaths,
        request: PutEntryRequest,
    ) -> Result<EntryRecord, String>;
    fn list_entries(
        &self,
        paths: &ContextPaths,
        request: ExportRequest,
    ) -> Result<Vec<EntryRecord>, String>;
    fn revert_entry(
        &self,
        paths: &ContextPaths,
        request: RevertEntryRequest,
    ) -> Result<EntryRecord, String>;
    fn review_list(
        &self,
        paths: &ContextPaths,
        state: Option<ReviewState>,
    ) -> Result<Vec<CoreReviewItem>, String>;
    fn review_approve(
        &self,
        paths: &ContextPaths,
        request: ReviewDecisionRequest,
    ) -> Result<CoreReviewItem, String>;
    fn review_reject(
        &self,
        paths: &ContextPaths,
        request: ReviewDecisionRequest,
    ) -> Result<CoreReviewItem, String>;
    fn review_edit(
        &self,
        paths: &ContextPaths,
        request: ReviewEditRequest,
    ) -> Result<CoreReviewItem, String>;
    fn export_bundle(
        &self,
        paths: &ContextPaths,
        request: ExportRequest,
    ) -> Result<context_core::ContextExportBundle, String>;
    fn export_json(&self, paths: &ContextPaths, request: ExportRequest) -> Result<String, String>;
    fn import_data(
        &self,
        paths: &ContextPaths,
        request: ImportRequest,
    ) -> Result<context_core::ContextExportBundle, String>;
    fn create_run(&self, paths: &ContextPaths, request: RunInput) -> Result<RunRecord, String>;
    fn list_runs(&self, paths: &ContextPaths) -> Result<Vec<RunRecord>, String>;
}

#[derive(Clone, Default)]
pub struct LiveContextBackend;

impl LiveContextBackend {
    fn client(&self, paths: &ContextPaths) -> ContextClient {
        ContextClient::new(ClientConfig::with_paths(paths.clone()))
    }
}

impl ContextBackend for LiveContextBackend {
    fn ping(&self, paths: &ContextPaths) -> Result<HealthReport, String> {
        self.client(paths).ping().map_err(|error| error.to_string())
    }

    fn stats(&self, paths: &ContextPaths) -> Result<StoreStats, String> {
        self.client(paths)
            .stats()
            .map_err(|error| error.to_string())
    }

    fn compose_context(
        &self,
        paths: &ContextPaths,
        request: ComposeRequest,
    ) -> Result<context_core::ComposeResponse, String> {
        self.client(paths)
            .compose_context(request)
            .map_err(|error| error.to_string())
    }

    fn search_context(
        &self,
        paths: &ContextPaths,
        request: SearchRequest,
    ) -> Result<context_core::SearchResponse, String> {
        self.client(paths)
            .search_context(request)
            .map_err(|error| error.to_string())
    }

    fn create_pack(
        &self,
        paths: &ContextPaths,
        request: CreatePackRequest,
    ) -> Result<PackRecord, String> {
        self.client(paths)
            .create_pack(request)
            .map_err(|error| error.to_string())
    }

    fn update_pack(
        &self,
        paths: &ContextPaths,
        request: UpdatePackRequest,
    ) -> Result<PackRecord, String> {
        self.client(paths)
            .update_pack(request)
            .map_err(|error| error.to_string())
    }

    fn list_packs(&self, paths: &ContextPaths) -> Result<Vec<PackRecord>, String> {
        self.client(paths)
            .list_packs()
            .map_err(|error| error.to_string())
    }

    fn put_entry(
        &self,
        paths: &ContextPaths,
        request: PutEntryRequest,
    ) -> Result<EntryRecord, String> {
        self.client(paths)
            .put_entry(request)
            .map_err(|error| error.to_string())
    }

    fn list_entries(
        &self,
        paths: &ContextPaths,
        request: ExportRequest,
    ) -> Result<Vec<EntryRecord>, String> {
        self.client(paths)
            .list_entries(request)
            .map_err(|error| error.to_string())
    }

    fn revert_entry(
        &self,
        paths: &ContextPaths,
        request: RevertEntryRequest,
    ) -> Result<EntryRecord, String> {
        self.client(paths)
            .revert_entry(request)
            .map_err(|error| error.to_string())
    }

    fn review_list(
        &self,
        paths: &ContextPaths,
        state: Option<ReviewState>,
    ) -> Result<Vec<CoreReviewItem>, String> {
        self.client(paths)
            .review_list(state)
            .map_err(|error| error.to_string())
    }

    fn review_approve(
        &self,
        paths: &ContextPaths,
        request: ReviewDecisionRequest,
    ) -> Result<CoreReviewItem, String> {
        self.client(paths)
            .review_approve(request)
            .map_err(|error| error.to_string())
    }

    fn review_reject(
        &self,
        paths: &ContextPaths,
        request: ReviewDecisionRequest,
    ) -> Result<CoreReviewItem, String> {
        self.client(paths)
            .review_reject(request)
            .map_err(|error| error.to_string())
    }

    fn review_edit(
        &self,
        paths: &ContextPaths,
        request: ReviewEditRequest,
    ) -> Result<CoreReviewItem, String> {
        self.client(paths)
            .review_edit(request)
            .map_err(|error| error.to_string())
    }

    fn export_bundle(
        &self,
        paths: &ContextPaths,
        request: ExportRequest,
    ) -> Result<context_core::ContextExportBundle, String> {
        self.client(paths)
            .export_bundle(request)
            .map_err(|error| error.to_string())
    }

    fn export_json(&self, paths: &ContextPaths, request: ExportRequest) -> Result<String, String> {
        self.client(paths)
            .export_json(request)
            .map_err(|error| error.to_string())
    }

    fn import_data(
        &self,
        paths: &ContextPaths,
        request: ImportRequest,
    ) -> Result<context_core::ContextExportBundle, String> {
        self.client(paths)
            .import_data(request)
            .map_err(|error| error.to_string())
    }

    fn create_run(&self, paths: &ContextPaths, request: RunInput) -> Result<RunRecord, String> {
        self.client(paths)
            .create_run(request)
            .map_err(|error| error.to_string())
    }

    fn list_runs(&self, paths: &ContextPaths) -> Result<Vec<RunRecord>, String> {
        self.client(paths)
            .list_runs()
            .map_err(|error| error.to_string())
    }
}

#[derive(Clone)]
struct LocalSettingsStore {
    base_paths: ContextPaths,
    file_path: PathBuf,
}

impl LocalSettingsStore {
    fn new(base_paths: ContextPaths) -> Self {
        let file_path = base_paths.data_dir.join(SETTINGS_FILE_NAME);
        Self {
            base_paths,
            file_path,
        }
    }

    fn load(&self) -> Result<LocalSettings, String> {
        if !self.file_path.exists() {
            return Ok(LocalSettings::default_for(&self.base_paths));
        }
        let contents = fs::read_to_string(&self.file_path).map_err(|error| error.to_string())?;
        let mut settings: LocalSettings =
            serde_json::from_str(&contents).map_err(|error| error.to_string())?;
        settings.apply_defaults(&self.base_paths);
        Ok(settings)
    }

    fn save(&self, settings: &LocalSettings) -> Result<LocalSettings, String> {
        if let Some(parent) = self.file_path.parent() {
            fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        let data = serde_json::to_vec_pretty(settings).map_err(|error| error.to_string())?;
        fs::write(&self.file_path, data).map_err(|error| error.to_string())?;
        Ok(settings.clone())
    }

    fn resolve_paths(&self, settings: &LocalSettings) -> Result<ContextPaths, String> {
        let mut paths = self.base_paths.clone();
        if !settings.socket_path.trim().is_empty() {
            paths.socket_path = expand_user_path(&settings.socket_path);
        }
        paths
            .ensure_parent_dirs()
            .map_err(|error| error.to_string())?;
        Ok(paths)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LocalSettings {
    theme: ThemeMode,
    auto_compose: bool,
    review_mode: ReviewMode,
    socket_path: String,
    launch_on_login: bool,
    telemetry: bool,
    max_preview_tokens: u32,
    #[serde(default)]
    adapter_enabled: BTreeMap<String, bool>,
}

impl LocalSettings {
    fn default_for(paths: &ContextPaths) -> Self {
        Self {
            theme: ThemeMode::System,
            auto_compose: true,
            review_mode: ReviewMode::Balanced,
            socket_path: paths.socket_path.display().to_string(),
            launch_on_login: false,
            telemetry: false,
            max_preview_tokens: 1400,
            adapter_enabled: BTreeMap::from([
                (ADAPTER_DAEMON.to_string(), true),
                (ADAPTER_CODEX.to_string(), true),
                (ADAPTER_CLAUDE.to_string(), true),
            ]),
        }
    }

    fn apply_defaults(&mut self, paths: &ContextPaths) {
        if self.socket_path.trim().is_empty() {
            self.socket_path = paths.socket_path.display().to_string();
        }
        for key in [ADAPTER_DAEMON, ADAPTER_CODEX, ADAPTER_CLAUDE] {
            self.adapter_enabled.entry(key.to_string()).or_insert(true);
        }
        if self.max_preview_tokens == 0 {
            self.max_preview_tokens = 1400;
        }
    }

    fn to_public(&self) -> Settings {
        Settings {
            theme: self.theme.clone(),
            auto_compose: self.auto_compose,
            review_mode: self.review_mode.clone(),
            socket_path: self.socket_path.clone(),
            launch_on_login: self.launch_on_login,
            telemetry: self.telemetry,
            max_preview_tokens: self.max_preview_tokens,
        }
    }

    fn from_public(settings: Settings, adapter_enabled: BTreeMap<String, bool>) -> Self {
        Self {
            theme: settings.theme,
            auto_compose: settings.auto_compose,
            review_mode: settings.review_mode,
            socket_path: settings.socket_path,
            launch_on_login: settings.launch_on_login,
            telemetry: settings.telemetry,
            max_preview_tokens: settings.max_preview_tokens,
            adapter_enabled,
        }
    }
}

#[derive(Clone)]
struct PackBundle {
    id: String,
    scope: ScopeRef,
    scope_id: String,
    scope_label: String,
    core_name: String,
    display_name: String,
    summary: String,
    status: ViewPackStatus,
    token_estimate: u32,
    updated_at: String,
    tags: Vec<String>,
    body: String,
    provenance: Vec<String>,
    revision: u32,
    parent_project_scope_id: Option<String>,
    entries: Vec<EntryRecord>,
}

impl PackBundle {
    fn from_parts(
        pack: PackRecord,
        entries: Vec<EntryRecord>,
        pending_review_counts: &HashMap<String, usize>,
        parent_project_scope_id: Option<String>,
    ) -> Self {
        let scope_id = encode_scope_ref(&pack.scope);
        let metadata = desktop_pack_metadata(&pack.metadata);
        let primary_entry_key = metadata
            .primary_entry_key
            .clone()
            .or_else(|| {
                entries
                    .iter()
                    .find(|entry| entry.key == DEFAULT_ENTRY_KEY)
                    .map(|entry| entry.key.clone())
            })
            .or_else(|| entries.first().map(|entry| entry.key.clone()))
            .unwrap_or_else(|| DEFAULT_ENTRY_KEY.to_string());
        let primary_entry = entries
            .iter()
            .find(|entry| entry.key == primary_entry_key)
            .cloned()
            .or_else(|| entries.first().cloned());
        let display_name = metadata
            .display_name
            .clone()
            .filter(|name| !name.trim().is_empty())
            .unwrap_or_else(|| pack.name.clone());
        let body = primary_entry
            .as_ref()
            .map(|entry| entry.value.render_markdown())
            .unwrap_or_default();
        let summary = metadata
            .summary
            .clone()
            .or_else(|| pack.description.clone())
            .unwrap_or_else(|| summarize_excerpt(&body, 140));
        let pending_reviews = *pending_review_counts
            .get(&pack_lookup_key(&scope_id, &pack.name))
            .unwrap_or(&0);
        let status = pack_status_to_view(&pack.status, metadata.status.as_deref(), pending_reviews);
        let token_estimate = estimate_tokens(
            &entries
                .iter()
                .map(|entry| entry.value.render_markdown())
                .collect::<Vec<_>>()
                .join("\n\n"),
        );
        let updated_at = entries
            .iter()
            .map(|entry| entry.updated_at.to_rfc3339())
            .max()
            .unwrap_or_else(|| pack.updated_at.to_rfc3339());
        let tags = {
            let mut set = BTreeSet::new();
            for entry in &entries {
                for tag in &entry.tags {
                    set.insert(tag.clone());
                }
            }
            if set.is_empty() {
                for tag in metadata.tags {
                    set.insert(tag);
                }
            }
            set.into_iter().collect::<Vec<_>>()
        };
        let provenance = {
            let mut set = BTreeSet::new();
            for entry in &entries {
                if let Some(source_ref) = &entry.provenance.source_ref {
                    set.insert(source_ref.clone());
                } else {
                    set.insert(entry.provenance.source.clone());
                }
            }
            if set.is_empty() {
                set.insert("desktop/manual-edit".to_string());
            }
            set.into_iter().collect::<Vec<_>>()
        };
        let revision = entries
            .iter()
            .map(|entry| entry.revision_no.max(0) as u32)
            .max()
            .unwrap_or(0)
            .max(pack.revision_no.max(0) as u32);

        Self {
            id: pack.id.clone(),
            scope: pack.scope.clone(),
            scope_id,
            scope_label: scope_label(&pack.scope),
            core_name: pack.name.clone(),
            display_name,
            summary,
            status,
            token_estimate,
            updated_at,
            tags,
            body,
            provenance,
            revision,
            parent_project_scope_id: parent_project_scope_id
                .or(metadata.parent_project_scope_id)
                .filter(|value| !value.trim().is_empty()),
            entries,
        }
    }

    fn to_view_pack(&self) -> ContextPack {
        ContextPack {
            id: self.id.clone(),
            scope_id: self.scope_id.clone(),
            scope_kind: map_scope_kind(&self.scope.kind),
            scope_label: self.scope_label.clone(),
            name: self.display_name.clone(),
            status: self.status.clone(),
            token_estimate: self.token_estimate,
            updated_at: self.updated_at.clone(),
            summary: self.summary.clone(),
            tags: self.tags.clone(),
            body: self.body.clone(),
            provenance: self.provenance.clone(),
            revision: self.revision,
        }
    }
}

#[derive(Default)]
struct ScopeCatalog {
    entries: BTreeMap<String, ScopeCatalogEntry>,
}

#[derive(Clone)]
struct ScopeCatalogEntry {
    scope: ScopeRef,
    encoded_id: String,
    label: String,
    description: String,
    status: String,
    parent_project_scope_id: Option<String>,
    kind: ViewScopeKind,
}

impl ScopeCatalog {
    fn to_workspace_nodes(&self) -> Vec<WorkspaceNode> {
        let mut globals = Vec::new();
        let mut projects = BTreeMap::<String, WorkspaceNode>::new();
        let mut root_tasks = Vec::new();

        for entry in self.entries.values() {
            let node = WorkspaceNode {
                id: entry.encoded_id.clone(),
                label: entry.label.clone(),
                kind: entry.kind.clone(),
                description: entry.description.clone(),
                status: entry.status.clone(),
                children: Vec::new(),
            };
            match entry.scope.kind {
                CoreScopeKind::Global => globals.push(node),
                CoreScopeKind::Project => {
                    projects.insert(entry.encoded_id.clone(), node);
                }
                CoreScopeKind::Task => {
                    if let Some(parent) = &entry.parent_project_scope_id {
                        if let Some(project) = projects.get_mut(parent) {
                            project.children.push(node);
                        } else {
                            root_tasks.push(node);
                        }
                    } else {
                        root_tasks.push(node);
                    }
                }
            }
        }

        let mut nodes = globals;
        nodes.extend(projects.into_values());
        nodes.extend(root_tasks);
        nodes
    }
}

#[derive(Default, Clone)]
struct DesktopPackMetadataView {
    display_name: Option<String>,
    summary: Option<String>,
    status: Option<String>,
    primary_entry_key: Option<String>,
    parent_project_scope_id: Option<String>,
    tags: Vec<String>,
}

fn build_pack_bundles(
    packs: Vec<PackRecord>,
    entries: Vec<EntryRecord>,
    reviews: &[CoreReviewItem],
) -> Vec<PackBundle> {
    let mut entries_by_pack: HashMap<String, Vec<EntryRecord>> = HashMap::new();
    for entry in entries {
        entries_by_pack
            .entry(pack_lookup_key(
                &encode_scope_ref(&entry.scope),
                &entry.pack_name,
            ))
            .or_default()
            .push(entry);
    }
    let mut review_counts = HashMap::new();
    for review in reviews {
        *review_counts
            .entry(pack_lookup_key(
                &encode_scope_ref(&review.scope),
                &review.pack_name,
            ))
            .or_insert(0usize) += 1;
    }

    let mut bundles = packs
        .into_iter()
        .map(|pack| {
            let key = pack_lookup_key(&encode_scope_ref(&pack.scope), &pack.name);
            let parent = desktop_pack_metadata(&pack.metadata).parent_project_scope_id;
            PackBundle::from_parts(
                pack,
                entries_by_pack.remove(&key).unwrap_or_default(),
                &review_counts,
                parent,
            )
        })
        .collect::<Vec<_>>();
    bundles.sort_by(|left, right| right.updated_at.cmp(&left.updated_at));
    bundles
}

fn build_scope_catalog(
    bundles: &[PackBundle],
    reviews: &[CoreReviewItem],
    runs: &[RunRecord],
) -> ScopeCatalog {
    let mut catalog = ScopeCatalog::default();
    catalog.entries.insert(
        encode_scope_ref(&ScopeRef::global()),
        ScopeCatalogEntry {
            scope: ScopeRef::global(),
            encoded_id: encode_scope_ref(&ScopeRef::global()),
            label: "Global context".to_string(),
            description: "Repository-wide policies and reusable operator context.".to_string(),
            status: "Synced".to_string(),
            parent_project_scope_id: None,
            kind: ViewScopeKind::Global,
        },
    );

    let mut task_to_project = HashMap::<String, String>::new();
    for bundle in bundles {
        if matches!(bundle.scope.kind, CoreScopeKind::Task) {
            if let Some(parent) = &bundle.parent_project_scope_id {
                task_to_project.insert(bundle.scope_id.clone(), parent.clone());
            }
        }
    }
    for run in runs {
        if let (Some(project_scope_id), Some(task_scope_id)) =
            (run.project_scope_id.as_ref(), run.task_scope_id.as_ref())
        {
            let task_scope = encode_scope_ref(
                &ScopeRef::normalized(CoreScopeKind::Task, task_scope_id)
                    .unwrap_or_else(|_| ScopeRef::global()),
            );
            let project_scope = encode_scope_ref(
                &ScopeRef::normalized(CoreScopeKind::Project, project_scope_id)
                    .unwrap_or_else(|_| ScopeRef::global()),
            );
            if task_scope.starts_with("task:") && project_scope.starts_with("project:") {
                task_to_project.insert(task_scope, project_scope);
            }
        }
    }

    let mut scope_stats = HashMap::<String, (usize, usize, usize)>::new();
    for bundle in bundles {
        let entry = scope_stats
            .entry(bundle.scope_id.clone())
            .or_insert((0, 0, 0));
        entry.0 += 1;
        if matches!(bundle.status, ViewPackStatus::Review) {
            entry.1 += 1;
        }
    }
    for review in reviews {
        let scope_id = encode_scope_ref(&review.scope);
        let entry = scope_stats.entry(scope_id).or_insert((0, 0, 0));
        entry.1 += 1;
    }
    for run in runs {
        if let Some(task_scope_id) = &run.task_scope_id {
            let scope_id = encode_scope_ref(
                &ScopeRef::normalized(CoreScopeKind::Task, task_scope_id)
                    .unwrap_or_else(|_| ScopeRef::global()),
            );
            let entry = scope_stats.entry(scope_id).or_insert((0, 0, 0));
            if run_status_from_metadata(run).0 == RunStatus::Running {
                entry.2 += 1;
            }
        } else if let Some(project_scope_id) = &run.project_scope_id {
            let scope_id = encode_scope_ref(
                &ScopeRef::normalized(CoreScopeKind::Project, project_scope_id)
                    .unwrap_or_else(|_| ScopeRef::global()),
            );
            let entry = scope_stats.entry(scope_id).or_insert((0, 0, 0));
            if run_status_from_metadata(run).0 == RunStatus::Running {
                entry.2 += 1;
            }
        }
    }

    for bundle in bundles {
        let status = scope_status(
            scope_stats
                .get(&bundle.scope_id)
                .copied()
                .unwrap_or_default(),
        );
        let description = scope_description(
            scope_stats
                .get(&bundle.scope_id)
                .copied()
                .unwrap_or_default(),
            &bundle.scope,
        );
        catalog
            .entries
            .entry(bundle.scope_id.clone())
            .or_insert_with(|| ScopeCatalogEntry {
                scope: bundle.scope.clone(),
                encoded_id: bundle.scope_id.clone(),
                label: scope_label(&bundle.scope),
                description,
                status,
                parent_project_scope_id: task_to_project.get(&bundle.scope_id).cloned(),
                kind: map_scope_kind(&bundle.scope.kind),
            });
    }

    for review in reviews {
        let scope_id = encode_scope_ref(&review.scope);
        let stats = scope_stats.get(&scope_id).copied().unwrap_or_default();
        catalog
            .entries
            .entry(scope_id.clone())
            .or_insert_with(|| ScopeCatalogEntry {
                scope: review.scope.clone(),
                encoded_id: scope_id.clone(),
                label: scope_label(&review.scope),
                description: scope_description(stats, &review.scope),
                status: scope_status(stats),
                parent_project_scope_id: task_to_project.get(&scope_id).cloned(),
                kind: map_scope_kind(&review.scope.kind),
            });
    }

    for run in runs {
        if let Some(project_scope_id) = &run.project_scope_id {
            if let Ok(scope) = ScopeRef::normalized(CoreScopeKind::Project, project_scope_id) {
                let encoded = encode_scope_ref(&scope);
                let stats = scope_stats.get(&encoded).copied().unwrap_or_default();
                catalog
                    .entries
                    .entry(encoded.clone())
                    .or_insert_with(|| ScopeCatalogEntry {
                        scope: scope.clone(),
                        encoded_id: encoded,
                        label: scope_label(&scope),
                        description: scope_description(stats, &scope),
                        status: scope_status(stats),
                        parent_project_scope_id: None,
                        kind: ViewScopeKind::Project,
                    });
            }
        }
        if let Some(task_scope_id) = &run.task_scope_id {
            if let Ok(scope) = ScopeRef::normalized(CoreScopeKind::Task, task_scope_id) {
                let encoded = encode_scope_ref(&scope);
                let stats = scope_stats.get(&encoded).copied().unwrap_or_default();
                catalog
                    .entries
                    .entry(encoded.clone())
                    .or_insert_with(|| ScopeCatalogEntry {
                        scope: scope.clone(),
                        encoded_id: encoded.clone(),
                        label: scope_label(&scope),
                        description: scope_description(stats, &scope),
                        status: scope_status(stats),
                        parent_project_scope_id: task_to_project.get(&encoded).cloned(),
                        kind: ViewScopeKind::Task,
                    });
            }
        }
    }

    catalog
}

fn pick_selected_scope_id(catalog: &ScopeCatalog, _settings: &LocalSettings) -> String {
    catalog
        .entries
        .values()
        .find(|entry| matches!(entry.scope.kind, CoreScopeKind::Task))
        .or_else(|| {
            catalog
                .entries
                .values()
                .find(|entry| matches!(entry.scope.kind, CoreScopeKind::Project))
        })
        .map(|entry| entry.encoded_id.clone())
        .unwrap_or_else(|| encode_scope_ref(&ScopeRef::global()))
}

fn compose_request_for_scope(scope: &ScopeCatalogEntry) -> ComposeRequest {
    ComposeRequest {
        project_scope_id: scope.parent_project_scope_id.clone().or_else(|| {
            if matches!(scope.scope.kind, CoreScopeKind::Project) {
                Some(scope.scope.id.clone())
            } else {
                None
            }
        }),
        task_scope_id: if matches!(scope.scope.kind, CoreScopeKind::Task) {
            Some(scope.scope.id.clone())
        } else {
            None
        },
        include_archived: false,
    }
}

fn map_review_items(
    reviews: &[CoreReviewItem],
    pack_lookup: &HashMap<String, PackBundle>,
) -> Vec<ReviewItem> {
    let mut items = reviews
        .iter()
        .filter(|review| matches!(review.state, ReviewState::Pending))
        .map(|review| {
            let scope_id = encode_scope_ref(&review.scope);
            let lookup_key = pack_lookup_key(&scope_id, &review.pack_name);
            let display_pack_name = pack_lookup
                .get(&lookup_key)
                .map(|bundle| bundle.display_name.clone())
                .unwrap_or_else(|| review.pack_name.clone());
            ReviewItem {
                id: review.id.clone(),
                pack_id: pack_lookup
                    .get(&lookup_key)
                    .map(|bundle| bundle.id.clone())
                    .unwrap_or_else(|| lookup_key.clone()),
                pack_name: display_pack_name,
                scope_id,
                scope_label: scope_label(&review.scope),
                title: review
                    .proposed_entry
                    .title
                    .clone()
                    .unwrap_or_else(|| titleize_identifier(&review.entry_key)),
                summary: review_summary(review),
                requested_by: review
                    .proposed_entry
                    .provenance
                    .clone()
                    .map(|provenance| provenance.actor)
                    .unwrap_or_else(|| review.request_id.clone()),
                requested_at: review.created_at.to_rfc3339(),
                risk: review_risk(&review.reason),
                diff: review_diff(review),
                suggested_edit: suggested_edit_body(review),
            }
        })
        .collect::<Vec<_>>();
    items.sort_by(|left, right| right.requested_at.cmp(&left.requested_at));
    items
}

fn map_runs(runs: &[RunRecord]) -> Vec<ActivityRun> {
    let mut mapped = runs
        .iter()
        .map(|run| {
            let (status, duration_ms, step_count, summary, context_pack_ids) =
                run_status_from_metadata(run);
            ActivityRun {
                id: run.id.clone(),
                actor: run.source.clone(),
                summary,
                status,
                started_at: run.started_at.to_rfc3339(),
                duration_ms,
                step_count,
                context_pack_ids,
            }
        })
        .collect::<Vec<_>>();
    mapped.sort_by(|left, right| right.started_at.cmp(&left.started_at));
    mapped
}

fn run_status_from_metadata(run: &RunRecord) -> (RunStatus, u64, u32, String, Vec<String>) {
    let status = match run
        .metadata
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("completed")
    {
        "running" => RunStatus::Running,
        "blocked" => RunStatus::Blocked,
        "failed" => RunStatus::Failed,
        _ => RunStatus::Completed,
    };
    let duration_ms = run
        .metadata
        .get("duration_ms")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let step_count = run
        .metadata
        .get("step_count")
        .and_then(Value::as_u64)
        .unwrap_or(1) as u32;
    let summary = run
        .metadata
        .get("summary")
        .and_then(Value::as_str)
        .map(ToString::to_string)
        .unwrap_or_else(|| format!("Run from {}", run.source));
    let context_pack_ids = run
        .metadata
        .get("pack_ids")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(ToString::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    (status, duration_ms, step_count, summary, context_pack_ids)
}

fn derive_adapters(
    paths: &ContextPaths,
    connected: bool,
    health: Option<&HealthReport>,
    stats: Option<&StoreStats>,
    settings: &LocalSettings,
) -> Result<Vec<AdapterStatus>, String> {
    let codex_enabled = *settings.adapter_enabled.get(ADAPTER_CODEX).unwrap_or(&true);
    let claude_enabled = *settings
        .adapter_enabled
        .get(ADAPTER_CLAUDE)
        .unwrap_or(&true);
    let daemon_enabled = *settings
        .adapter_enabled
        .get(ADAPTER_DAEMON)
        .unwrap_or(&true);
    let codex_path = user_home().join(".codex");
    let claude_path = user_home().join(".claude");

    let codex_health = if !codex_enabled {
        AdapterHealth::Offline
    } else if codex_path.exists() {
        AdapterHealth::Healthy
    } else {
        AdapterHealth::Degraded
    };
    let claude_health = if !claude_enabled {
        AdapterHealth::Offline
    } else if claude_path.exists() {
        AdapterHealth::Healthy
    } else {
        AdapterHealth::Degraded
    };
    let daemon_health = if !daemon_enabled {
        AdapterHealth::Offline
    } else if connected {
        AdapterHealth::Healthy
    } else {
        AdapterHealth::Offline
    };

    Ok(vec![
        AdapterStatus {
            id: ADAPTER_CODEX.to_string(),
            name: "Codex harness".to_string(),
            kind: AdapterKind::Terminal,
            enabled: codex_enabled,
            health: codex_health,
            last_checked_at: now_iso(),
            queue_depth: 0,
            path: codex_path.display().to_string(),
            note: if codex_path.exists() {
                "Codex local configuration directory is available for workspace integrations."
                    .to_string()
            } else {
                "Codex configuration directory was not found on this machine.".to_string()
            },
        },
        AdapterStatus {
            id: ADAPTER_CLAUDE.to_string(),
            name: "Claude Code harness".to_string(),
            kind: AdapterKind::Terminal,
            enabled: claude_enabled,
            health: claude_health,
            last_checked_at: now_iso(),
            queue_depth: 0,
            path: claude_path.display().to_string(),
            note: if claude_path.exists() {
                "Claude Code local configuration directory is available for shared context workflows.".to_string()
            } else {
                "Claude Code configuration directory was not found on this machine.".to_string()
            },
        },
        AdapterStatus {
            id: ADAPTER_DAEMON.to_string(),
            name: "Context daemon".to_string(),
            kind: AdapterKind::Api,
            enabled: daemon_enabled,
            health: daemon_health,
            last_checked_at: now_iso(),
            queue_depth: stats.map(|report| report.reviews as u32).unwrap_or(0),
            path: paths.socket_path.display().to_string(),
            note: if connected {
                format!(
                    "Daemon is reachable with {} packs and {} entries.",
                    health.map(|report| report.packs).unwrap_or(0),
                    health.map(|report| report.entries).unwrap_or(0)
                )
            } else {
                "The desktop app could not reach contextd at the configured socket path."
                    .to_string()
            },
        },
    ])
}

fn load_pack_revisions(
    paths: &ContextPaths,
    bundle: &PackBundle,
    entry_ids: &[String],
) -> Result<Vec<RevisionEntry>, String> {
    let connection = open_read_only(&paths.db_path)?;
    let mut revisions = Vec::new();

    let mut pack_stmt = connection
        .prepare(
            "SELECT entity_type, entity_id, revision_no, action, snapshot_json, provenance_json, created_at FROM revisions WHERE entity_type = 'pack' AND entity_id = ? ORDER BY revision_no DESC",
        )
        .map_err(|error| error.to_string())?;
    let pack_rows = pack_stmt
        .query_map(params![bundle.id], |row| {
            Ok(DbRevisionRow {
                entity_type: row.get(0)?,
                entity_id: row.get(1)?,
                revision_no: row.get(2)?,
                action: row.get(3)?,
                snapshot_json: row.get(4)?,
                provenance_json: row.get(5)?,
                created_at: row.get(6)?,
            })
        })
        .map_err(|error| error.to_string())?;
    for row in pack_rows {
        let row = row.map_err(|error| error.to_string())?;
        let snapshot: PackRecord =
            serde_json::from_str(&row.snapshot_json).map_err(|error| error.to_string())?;
        let provenance: Provenance =
            serde_json::from_str(&row.provenance_json).map_err(|error| error.to_string())?;
        revisions.push(RevisionEntry {
            id: encode_revision_id("pack", &row.entity_id, row.revision_no),
            entity_id: bundle.id.clone(),
            entity_label: display_name_for_pack(&snapshot),
            author: provenance.actor,
            created_at: row.created_at,
            note: format!("Pack {}", row.action),
            change_summary: snapshot
                .description
                .clone()
                .unwrap_or_else(|| summarize_excerpt(&snapshot.name, 120)),
            restorable: true,
        });
    }

    let mut entry_stmt = connection
        .prepare(
            "SELECT entity_type, entity_id, revision_no, action, snapshot_json, provenance_json, created_at FROM revisions WHERE entity_type = 'entry' AND entity_id = ? ORDER BY revision_no DESC",
        )
        .map_err(|error| error.to_string())?;
    for entry_id in entry_ids {
        let rows = entry_stmt
            .query_map(params![entry_id], |row| {
                Ok(DbRevisionRow {
                    entity_type: row.get(0)?,
                    entity_id: row.get(1)?,
                    revision_no: row.get(2)?,
                    action: row.get(3)?,
                    snapshot_json: row.get(4)?,
                    provenance_json: row.get(5)?,
                    created_at: row.get(6)?,
                })
            })
            .map_err(|error| error.to_string())?;
        for row in rows {
            let row = row.map_err(|error| error.to_string())?;
            let snapshot: EntryRecord =
                serde_json::from_str(&row.snapshot_json).map_err(|error| error.to_string())?;
            let provenance: Provenance =
                serde_json::from_str(&row.provenance_json).map_err(|error| error.to_string())?;
            revisions.push(RevisionEntry {
                id: encode_revision_id("entry", &row.entity_id, row.revision_no),
                entity_id: bundle.id.clone(),
                entity_label: format!("{} / {}", bundle.display_name, snapshot.key),
                author: provenance.actor,
                created_at: row.created_at,
                note: format!("Entry {}", row.action),
                change_summary: summarize_excerpt(&snapshot.value.render_markdown(), 140),
                restorable: true,
            });
        }
    }

    revisions.sort_by(|left, right| right.created_at.cmp(&left.created_at));
    Ok(revisions)
}

#[derive(Clone)]
struct DbRevisionRow {
    entity_type: String,
    entity_id: String,
    revision_no: i64,
    action: String,
    snapshot_json: String,
    provenance_json: String,
    created_at: String,
}

fn load_revision_record(
    db_path: &Path,
    revision_id: &str,
) -> Result<Option<DbRevisionRow>, String> {
    let (entity_type, entity_id, revision_no) = decode_revision_id(revision_id)?;
    let connection = open_read_only(db_path)?;
    connection
        .query_row(
            "SELECT entity_type, entity_id, revision_no, action, snapshot_json, provenance_json, created_at FROM revisions WHERE entity_type = ? AND entity_id = ? AND revision_no = ?",
            params![entity_type, entity_id, revision_no],
            |row| {
                Ok(DbRevisionRow {
                    entity_type: row.get(0)?,
                    entity_id: row.get(1)?,
                    revision_no: row.get(2)?,
                    action: row.get(3)?,
                    snapshot_json: row.get(4)?,
                    provenance_json: row.get(5)?,
                    created_at: row.get(6)?,
                })
            },
        )
        .optional()
        .map_err(|error| error.to_string())
}

fn open_read_only(path: &Path) -> Result<Connection, String> {
    Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|error| error.to_string())
}

fn display_name_for_pack(pack: &PackRecord) -> String {
    desktop_pack_metadata(&pack.metadata)
        .display_name
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| pack.name.clone())
}

fn suggested_edit_body(review: &CoreReviewItem) -> String {
    review.proposed_entry.value.render_markdown()
}

fn edited_review_value(original: &EntryValue, content: String) -> Result<EntryValue, String> {
    match original {
        EntryValue::Markdown { .. } => Ok(EntryValue::Markdown { body: content }),
        EntryValue::Json { .. } => {
            let trimmed = content.trim();
            let json_text = trimmed
                .strip_prefix("```json")
                .and_then(|value| value.strip_suffix("```"))
                .map(str::trim)
                .unwrap_or(trimmed);
            let value = serde_json::from_str(json_text)
                .map_err(|error| format!("Edited JSON context is invalid: {error}"))?;
            Ok(EntryValue::Json { value })
        }
    }
}

fn review_summary(review: &CoreReviewItem) -> String {
    match review.reason {
        ReviewReason::GlobalScope => {
            "This global change requires explicit human approval.".to_string()
        }
        ReviewReason::Conflict => {
            "A conflicting active entry exists and needs manual reconciliation.".to_string()
        }
        ReviewReason::Locked => {
            "The target pack or entry is locked and needs review before changing.".to_string()
        }
    }
}

fn review_diff(review: &CoreReviewItem) -> String {
    let before = review
        .existing_entry
        .as_ref()
        .map(|entry| summarize_excerpt(&entry.value.render_markdown(), 120))
        .unwrap_or_else(|| "(new entry)".to_string());
    let after = summarize_excerpt(&review.proposed_entry.value.render_markdown(), 120);
    format!("- {}\n+ {}", before, after)
}

fn display_name_for_review(review: &CoreReviewItem) -> String {
    review
        .proposed_entry
        .title
        .clone()
        .unwrap_or_else(|| review.entry_key.clone())
}

fn review_risk(reason: &ReviewReason) -> RiskLevel {
    match reason {
        ReviewReason::GlobalScope => RiskLevel::High,
        ReviewReason::Conflict => RiskLevel::High,
        ReviewReason::Locked => RiskLevel::Medium,
    }
}

fn review_decision_label(decision: &ViewReviewDecision) -> &'static str {
    match decision {
        ViewReviewDecision::Approve => "approved",
        ViewReviewDecision::Reject => "rejected",
        ViewReviewDecision::Edit => "edited",
    }
}

fn display_name_from_lookup(
    pack_lookup: &HashMap<String, PackBundle>,
    key: &str,
) -> Option<String> {
    pack_lookup
        .get(key)
        .map(|bundle| bundle.display_name.clone())
}

fn pack_id_from_lookup(pack_lookup: &HashMap<String, PackBundle>, key: &str) -> Option<String> {
    pack_lookup.get(key).map(|bundle| bundle.id.clone())
}

fn pack_lookup_key(scope_id: &str, pack_name: &str) -> String {
    format!("{}::{}", scope_id, pack_name)
}

fn pack_lookup_key_for_section(section: &PreviewSection) -> String {
    pack_lookup_key(
        &encode_scope_ref(
            &ScopeRef::normalized(
                map_scope_kind_back(&section.scope_kind),
                section.scope_label_id(),
            )
            .unwrap_or_else(|_| ScopeRef::global()),
        ),
        &section.pack_name,
    )
}

trait PreviewScopeId {
    fn scope_label_id(&self) -> String;
}

impl PreviewScopeId for PreviewSection {
    fn scope_label_id(&self) -> String {
        match self.scope_kind {
            ViewScopeKind::Global => "global".to_string(),
            _ => self.scope_label.clone(),
        }
    }
}

fn render_section_body(entries: &[EntryRecord]) -> String {
    entries
        .iter()
        .map(|entry| {
            let mut body = String::new();
            body.push_str(&format!("## {} ({})\n\n", entry.key, entry.kind));
            if let Some(title) = &entry.title {
                body.push_str(&format!("**{}**\n\n", title));
            }
            body.push_str(&entry.value.render_markdown());
            body
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn relevant_scope_ids(scope: &ScopeCatalogEntry) -> Vec<String> {
    let mut scopes = vec![encode_scope_ref(&ScopeRef::global())];
    if let Some(project) = &scope.parent_project_scope_id {
        scopes.push(project.clone());
    }
    match scope.scope.kind {
        CoreScopeKind::Project => scopes.push(scope.encoded_id.clone()),
        CoreScopeKind::Task => scopes.push(scope.encoded_id.clone()),
        CoreScopeKind::Global => {}
    }
    scopes
}

fn map_scope_kind(kind: &CoreScopeKind) -> ViewScopeKind {
    match kind {
        CoreScopeKind::Global => ViewScopeKind::Global,
        CoreScopeKind::Project => ViewScopeKind::Project,
        CoreScopeKind::Task => ViewScopeKind::Task,
    }
}

fn map_scope_kind_back(kind: &ViewScopeKind) -> CoreScopeKind {
    match kind {
        ViewScopeKind::Global => CoreScopeKind::Global,
        ViewScopeKind::Project => CoreScopeKind::Project,
        ViewScopeKind::Task => CoreScopeKind::Task,
    }
}

fn encode_scope_ref(scope: &ScopeRef) -> String {
    format!("{}:{}", scope.kind.as_str(), scope.id)
}

fn decode_scope_id(scope_id: &str) -> Result<ScopeRef, String> {
    let (kind, id) = scope_id
        .split_once(':')
        .ok_or_else(|| format!("Invalid scope id: {scope_id}"))?;
    let kind = match kind {
        "global" => CoreScopeKind::Global,
        "project" => CoreScopeKind::Project,
        "task" => CoreScopeKind::Task,
        other => return Err(format!("Unknown scope kind: {other}")),
    };
    ScopeRef::normalized(kind, id).map_err(|error| error.to_string())
}

fn section_title_for_scope(kind: &CoreScopeKind) -> String {
    match kind {
        CoreScopeKind::Global => "Global context".to_string(),
        CoreScopeKind::Project => "Project context".to_string(),
        CoreScopeKind::Task => "Task-specific context".to_string(),
    }
}

fn scope_label(scope: &ScopeRef) -> String {
    match scope.kind {
        CoreScopeKind::Global => "Global context".to_string(),
        CoreScopeKind::Project => project_display_name(&scope.id),
        CoreScopeKind::Task => titleize_identifier(&scope.id),
    }
}

fn scope_status(stats: (usize, usize, usize)) -> String {
    if stats.1 > 0 {
        "Needs review".to_string()
    } else if stats.2 > 0 {
        "In progress".to_string()
    } else if stats.0 == 0 {
        "Empty".to_string()
    } else {
        "Synced".to_string()
    }
}

fn scope_description(stats: (usize, usize, usize), scope: &ScopeRef) -> String {
    if stats.0 == 0 {
        match scope.kind {
            CoreScopeKind::Global => {
                "Global policies and reusable guidance will appear here.".to_string()
            }
            CoreScopeKind::Project => format!("No project packs yet · {}", scope.id),
            CoreScopeKind::Task => "No task-scoped packs have been created yet.".to_string(),
        }
    } else {
        let mut parts = vec![format!(
            "{} pack{}",
            stats.0,
            if stats.0 == 1 { "" } else { "s" }
        )];
        if stats.1 > 0 {
            parts.push(format!("{} pending review", stats.1));
        }
        if stats.2 > 0 {
            parts.push(format!(
                "{} running run{}",
                stats.2,
                if stats.2 == 1 { "" } else { "s" }
            ));
        }
        if matches!(scope.kind, CoreScopeKind::Project) {
            parts.push(scope.id.clone());
        }
        parts.join(" · ")
    }
}

fn estimate_tokens(body: &str) -> u32 {
    let word_count = body
        .split_whitespace()
        .filter(|word| !word.is_empty())
        .count() as u32;
    (word_count.saturating_mul(135) / 100).max(72)
}

fn summarize_excerpt(value: &str, max_len: usize) -> String {
    if value.chars().count() <= max_len {
        return value.to_string();
    }
    let excerpt: String = value.chars().take(max_len.saturating_sub(1)).collect();
    format!("{}…", excerpt.trim_end())
}

fn titleize_identifier(value: &str) -> String {
    value
        .split(|ch: char| !ch.is_ascii_alphanumeric())
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(first) => format!("{}{}", first.to_ascii_uppercase(), chars.as_str()),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn project_display_name(value: &str) -> String {
    let trimmed = value.trim_end_matches(std::path::MAIN_SEPARATOR);
    let leaf = Path::new(trimmed)
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or(trimmed);
    titleize_identifier(leaf)
}

fn slug_pack_name(value: &str) -> String {
    let slug = value
        .to_lowercase()
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    if slug.is_empty() {
        "context-pack".to_string()
    } else {
        slug
    }
}

fn unique_pack_name(scope: &ScopeRef, base: &str, packs: &[PackRecord]) -> String {
    let is_available = |candidate: &str| {
        !packs
            .iter()
            .any(|pack| &pack.scope == scope && pack.name == candidate)
    };
    if is_available(base) {
        return base.to_string();
    }
    for suffix in 2_u64.. {
        let candidate = format!("{base}-{suffix}");
        if is_available(&candidate) {
            return candidate;
        }
    }
    unreachable!("an unbounded numeric suffix always has an available value")
}

fn trimmed_or(value: &str, fallback: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        fallback.to_string()
    } else {
        trimmed.to_string()
    }
}

fn pack_status_to_view(
    status: &CorePackStatus,
    _explicit: Option<&str>,
    pending_reviews: usize,
) -> ViewPackStatus {
    match status {
        CorePackStatus::Archived => ViewPackStatus::Draft,
        CorePackStatus::Active if pending_reviews > 0 => ViewPackStatus::Review,
        CorePackStatus::Active => ViewPackStatus::Active,
    }
}

fn desktop_pack_metadata(metadata: &Value) -> DesktopPackMetadataView {
    metadata
        .get(DESKTOP_METADATA_KEY)
        .and_then(Value::as_object)
        .map(|object| DesktopPackMetadataView {
            display_name: object
                .get("displayName")
                .and_then(Value::as_str)
                .map(ToString::to_string),
            summary: object
                .get("summary")
                .and_then(Value::as_str)
                .map(ToString::to_string),
            status: object
                .get("status")
                .and_then(Value::as_str)
                .map(ToString::to_string),
            primary_entry_key: object
                .get("primaryEntryKey")
                .and_then(Value::as_str)
                .map(ToString::to_string),
            parent_project_scope_id: object
                .get("parentProjectScopeId")
                .and_then(Value::as_str)
                .map(|value| {
                    encode_scope_ref(
                        &ScopeRef::normalized(CoreScopeKind::Project, value)
                            .unwrap_or_else(|_| ScopeRef::global()),
                    )
                }),
            tags: object
                .get("tags")
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(Value::as_str)
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default(),
        })
        .unwrap_or_default()
}

fn update_desktop_pack_metadata(
    metadata: Value,
    display_name: &str,
    summary: &str,
    status: &ViewPackStatus,
    primary_entry_key: &str,
    parent_project_scope_id: Option<&str>,
) -> Value {
    let mut root = ensure_object(metadata);
    let desktop = root
        .entry(DESKTOP_METADATA_KEY.to_string())
        .or_insert_with(|| Value::Object(Map::new()));
    let desktop_object = ensure_object_ref(desktop);
    desktop_object.insert("managed".to_string(), Value::Bool(true));
    desktop_object.insert(
        "displayName".to_string(),
        Value::String(display_name.to_string()),
    );
    desktop_object.insert("summary".to_string(), Value::String(summary.to_string()));
    desktop_object.insert(
        "status".to_string(),
        Value::String(
            match status {
                ViewPackStatus::Active => "active",
                ViewPackStatus::Draft => "draft",
                ViewPackStatus::Review => "review",
            }
            .to_string(),
        ),
    );
    desktop_object.insert(
        "primaryEntryKey".to_string(),
        Value::String(primary_entry_key.to_string()),
    );
    if let Some(parent_scope_id) = parent_project_scope_id {
        if let Ok(parent_scope) = decode_scope_id(parent_scope_id) {
            desktop_object.insert(
                "parentProjectScopeId".to_string(),
                Value::String(parent_scope.id),
            );
        }
    }
    Value::Object(root)
}

fn update_desktop_entry_metadata(metadata: Value) -> Value {
    let mut root = ensure_object(metadata);
    let desktop = root
        .entry(DESKTOP_METADATA_KEY.to_string())
        .or_insert_with(|| Value::Object(Map::new()));
    let desktop_object = ensure_object_ref(desktop);
    desktop_object.insert("managed".to_string(), Value::Bool(true));
    Value::Object(root)
}

fn ensure_object(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(object) => object,
        _ => Map::new(),
    }
}

fn ensure_object_ref(value: &mut Value) -> &mut Map<String, Value> {
    if !value.is_object() {
        *value = Value::Object(Map::new());
    }
    value.as_object_mut().expect("object")
}

fn normalize_search_score(score: f64) -> u32 {
    let bounded = (100.0 - score.abs() * 10.0).round();
    bounded.clamp(1.0, 99.0) as u32
}

fn adapter_health_tag(health: &AdapterHealth) -> String {
    match health {
        AdapterHealth::Healthy => "healthy".to_string(),
        AdapterHealth::Degraded => "degraded".to_string(),
        AdapterHealth::Offline => "offline".to_string(),
    }
}

fn adapter_kind_tag(kind: &AdapterKind) -> String {
    match kind {
        AdapterKind::Filesystem => "filesystem".to_string(),
        AdapterKind::Git => "git".to_string(),
        AdapterKind::Terminal => "terminal".to_string(),
        AdapterKind::Api => "api".to_string(),
    }
}

fn run_status_tag(status: &RunStatus) -> String {
    match status {
        RunStatus::Running => "running".to_string(),
        RunStatus::Completed => "completed".to_string(),
        RunStatus::Blocked => "blocked".to_string(),
        RunStatus::Failed => "failed".to_string(),
    }
}

fn risk_tag(risk: &RiskLevel) -> String {
    match risk {
        RiskLevel::Low => "low".to_string(),
        RiskLevel::Medium => "medium".to_string(),
        RiskLevel::High => "high".to_string(),
    }
}

fn project_scope_id_for(scope: &ScopeRef, parent: Option<&str>) -> Option<String> {
    match scope.kind {
        CoreScopeKind::Project => Some(scope.id.clone()),
        CoreScopeKind::Task => parent
            .and_then(|value| decode_scope_id(value).ok())
            .map(|scope| scope.id),
        CoreScopeKind::Global => None,
    }
}

fn task_scope_id_for(scope: &ScopeRef) -> Option<String> {
    match scope.kind {
        CoreScopeKind::Task => Some(scope.id.clone()),
        _ => None,
    }
}

fn write_text_file(path: &Path, contents: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path).map_err(|error| error.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(fs::Permissions::from_mode(0o600))
            .map_err(|error| error.to_string())?;
    }
    file.write_all(contents.as_bytes())
        .and_then(|_| file.flush())
        .map_err(|error| error.to_string())
}

fn user_home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("~"))
}

fn expand_user_path(value: &str) -> PathBuf {
    if let Some(stripped) = value.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(stripped);
        }
    }
    PathBuf::from(value)
}

fn detect_import_format(path: &Path, payload: &str) -> ImportFormat {
    if path.extension().and_then(|ext| ext.to_str()) == Some("md") {
        ImportFormat::Markdown
    } else if payload.trim_start().starts_with('{') {
        ImportFormat::Json
    } else {
        ImportFormat::Markdown
    }
}

fn encode_revision_id(entity_type: &str, entity_id: &str, revision_no: i64) -> String {
    format!("{}:{}:{}", entity_type, entity_id, revision_no)
}

fn decode_revision_id(value: &str) -> Result<(String, String, i64), String> {
    let mut parts = value.splitn(3, ':');
    let entity_type = parts
        .next()
        .ok_or_else(|| format!("Invalid revision id: {value}"))?;
    let entity_id = parts
        .next()
        .ok_or_else(|| format!("Invalid revision id: {value}"))?;
    let revision_no = parts
        .next()
        .ok_or_else(|| format!("Invalid revision id: {value}"))?
        .parse::<i64>()
        .map_err(|error| error.to_string())?;
    Ok((entity_type.to_string(), entity_id.to_string(), revision_no))
}

fn now_iso() -> String {
    Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use context_core::ContextStore;
    use tempfile::tempdir;

    #[derive(Clone)]
    struct StoreBackend {
        store: Arc<ContextStore>,
    }

    impl StoreBackend {
        fn new(path: &Path) -> Self {
            Self {
                store: Arc::new(ContextStore::open(path).expect("open store")),
            }
        }
    }

    impl ContextBackend for StoreBackend {
        fn ping(&self, _paths: &ContextPaths) -> Result<HealthReport, String> {
            self.store.health().map_err(|error| error.to_string())
        }

        fn stats(&self, _paths: &ContextPaths) -> Result<StoreStats, String> {
            self.store.stats().map_err(|error| error.to_string())
        }

        fn compose_context(
            &self,
            _paths: &ContextPaths,
            request: ComposeRequest,
        ) -> Result<context_core::ComposeResponse, String> {
            self.store
                .compose_context(request)
                .map_err(|error| error.to_string())
        }

        fn search_context(
            &self,
            _paths: &ContextPaths,
            request: SearchRequest,
        ) -> Result<context_core::SearchResponse, String> {
            self.store
                .search_context(request)
                .map_err(|error| error.to_string())
        }

        fn create_pack(
            &self,
            _paths: &ContextPaths,
            request: CreatePackRequest,
        ) -> Result<PackRecord, String> {
            self.store
                .create_pack(request)
                .map_err(|error| error.to_string())
        }

        fn update_pack(
            &self,
            _paths: &ContextPaths,
            request: UpdatePackRequest,
        ) -> Result<PackRecord, String> {
            self.store
                .update_pack(request)
                .map_err(|error| error.to_string())
        }

        fn list_packs(&self, _paths: &ContextPaths) -> Result<Vec<PackRecord>, String> {
            self.store.list_packs().map_err(|error| error.to_string())
        }

        fn put_entry(
            &self,
            _paths: &ContextPaths,
            request: PutEntryRequest,
        ) -> Result<EntryRecord, String> {
            self.store
                .put_entry(request)
                .map_err(|error| error.to_string())
        }

        fn list_entries(
            &self,
            _paths: &ContextPaths,
            request: ExportRequest,
        ) -> Result<Vec<EntryRecord>, String> {
            self.store
                .list_entries(request)
                .map_err(|error| error.to_string())
        }

        fn revert_entry(
            &self,
            _paths: &ContextPaths,
            request: RevertEntryRequest,
        ) -> Result<EntryRecord, String> {
            self.store
                .revert_entry(request)
                .map_err(|error| error.to_string())
        }

        fn review_list(
            &self,
            _paths: &ContextPaths,
            state: Option<ReviewState>,
        ) -> Result<Vec<CoreReviewItem>, String> {
            self.store
                .review_list(state)
                .map_err(|error| error.to_string())
        }

        fn review_approve(
            &self,
            _paths: &ContextPaths,
            request: ReviewDecisionRequest,
        ) -> Result<CoreReviewItem, String> {
            self.store
                .review_approve(request)
                .map_err(|error| error.to_string())
        }

        fn review_reject(
            &self,
            _paths: &ContextPaths,
            request: ReviewDecisionRequest,
        ) -> Result<CoreReviewItem, String> {
            self.store
                .review_reject(request)
                .map_err(|error| error.to_string())
        }

        fn review_edit(
            &self,
            _paths: &ContextPaths,
            request: ReviewEditRequest,
        ) -> Result<CoreReviewItem, String> {
            self.store
                .review_edit(request)
                .map_err(|error| error.to_string())
        }

        fn export_bundle(
            &self,
            _paths: &ContextPaths,
            request: ExportRequest,
        ) -> Result<context_core::ContextExportBundle, String> {
            self.store
                .export_bundle(request)
                .map_err(|error| error.to_string())
        }

        fn export_json(
            &self,
            _paths: &ContextPaths,
            request: ExportRequest,
        ) -> Result<String, String> {
            self.store
                .export_json(request)
                .map_err(|error| error.to_string())
        }

        fn import_data(
            &self,
            _paths: &ContextPaths,
            request: ImportRequest,
        ) -> Result<context_core::ContextExportBundle, String> {
            self.store
                .import_data(request)
                .map_err(|error| error.to_string())
        }

        fn create_run(
            &self,
            _paths: &ContextPaths,
            request: RunInput,
        ) -> Result<RunRecord, String> {
            self.store
                .create_run(request)
                .map_err(|error| error.to_string())
        }

        fn list_runs(&self, _paths: &ContextPaths) -> Result<Vec<RunRecord>, String> {
            self.store.list_runs().map_err(|error| error.to_string())
        }
    }

    #[test]
    fn saves_packs_and_settings_persist_across_service_restarts() {
        let temp = tempdir().expect("tempdir");
        let base_paths = ContextPaths {
            data_dir: temp.path().join("manager-home"),
            db_path: temp.path().join("manager-home/context.db"),
            socket_path: temp.path().join("manager-home/contextd.sock"),
            spool_dir: temp.path().join("manager-home/spool"),
        };
        let settings_store = LocalSettingsStore::new(base_paths.clone());
        let service = DesktopContextService::with_backend(
            StoreBackend::new(&base_paths.db_path),
            settings_store.clone(),
        );

        let saved = service
            .save_pack(SavePackInput {
                id: None,
                scope_id: "project:atlas".to_string(),
                name: "Atlas Release Notes".to_string(),
                status: ViewPackStatus::Active,
                summary: "Release summary".to_string(),
                tags: vec!["release".to_string(), "atlas".to_string()],
                body: "Ship the desktop control plane with live daemon persistence.".to_string(),
            })
            .expect("save pack");
        assert_eq!(saved.name, "Atlas Release Notes");

        service
            .save_settings(Settings {
                theme: ThemeMode::Dark,
                auto_compose: true,
                review_mode: ReviewMode::Strict,
                socket_path: base_paths.socket_path.display().to_string(),
                launch_on_login: true,
                telemetry: false,
                max_preview_tokens: 2048,
            })
            .expect("save settings");

        let restarted = DesktopContextService::with_backend(
            StoreBackend::new(&base_paths.db_path),
            settings_store,
        );
        let packs = restarted
            .list_packs(Some("project:atlas".to_string()))
            .expect("list packs");
        assert!(packs.iter().any(|pack| pack.name == "Atlas Release Notes"));

        let settings = restarted.load_settings().expect("load settings");
        assert!(matches!(settings.theme, ThemeMode::Dark));
        assert_eq!(settings.max_preview_tokens, 2048);

        let results = restarted
            .search_index("daemon persistence".to_string())
            .expect("search");
        assert!(results
            .iter()
            .any(|result| result.title.contains("Atlas Release Notes")));
    }

    #[test]
    fn creates_and_updates_draft_packs() {
        let temp = tempdir().expect("tempdir");
        let base_paths = ContextPaths {
            data_dir: temp.path().join("manager-home"),
            db_path: temp.path().join("manager-home/context.db"),
            socket_path: temp.path().join("manager-home/contextd.sock"),
            spool_dir: temp.path().join("manager-home/spool"),
        };
        let service = DesktopContextService::with_backend(
            StoreBackend::new(&base_paths.db_path),
            LocalSettingsStore::new(base_paths.clone()),
        );

        let draft = service
            .save_pack(SavePackInput {
                id: None,
                scope_id: "project:atlas".to_string(),
                name: "Atlas Draft".to_string(),
                status: ViewPackStatus::Draft,
                summary: "Draft summary".to_string(),
                tags: vec!["draft".to_string()],
                body: "Initial draft body".to_string(),
            })
            .expect("create draft");
        assert!(matches!(draft.status, ViewPackStatus::Draft));
        let preview = service
            .compose_preview("project:atlas".to_string())
            .expect("compose draft scope");
        assert!(preview.sections.is_empty());
        assert!(preview
            .warnings
            .iter()
            .any(|warning| warning.contains("excluded")));

        let updated = service
            .save_pack(SavePackInput {
                id: Some(draft.id.clone()),
                scope_id: draft.scope_id.clone(),
                name: "Atlas Draft".to_string(),
                status: ViewPackStatus::Draft,
                summary: "Draft summary updated".to_string(),
                tags: vec!["draft".to_string(), "updated".to_string()],
                body: "Updated draft body".to_string(),
            })
            .expect("update draft");
        assert!(matches!(updated.status, ViewPackStatus::Draft));
        assert!(updated.body.contains("Updated draft body"));

        let initial_revision = service
            .list_revisions(Some(draft.id.clone()))
            .expect("list draft revisions")
            .into_iter()
            .find(|revision| revision.change_summary.contains("Initial draft body"))
            .expect("initial entry revision");
        service
            .restore_revision(initial_revision.id)
            .expect("restore archived draft entry");

        let reopened = DesktopContextService::with_backend(
            StoreBackend::new(&base_paths.db_path),
            LocalSettingsStore::new(base_paths),
        );
        let packs = reopened
            .list_packs(Some("project:atlas".to_string()))
            .expect("list packs");
        let persisted = packs
            .into_iter()
            .find(|pack| pack.id == draft.id)
            .expect("persisted draft");
        assert!(matches!(persisted.status, ViewPackStatus::Draft));
        assert!(persisted.body.contains("Initial draft body"));
    }

    #[test]
    fn project_paths_use_a_readable_leaf_label() {
        assert_eq!(
            project_display_name("/Users/example/src/universal-context-manager"),
            "Universal Context Manager"
        );
        assert_eq!(project_display_name("atlas"), "Atlas");
    }

    #[test]
    fn pending_review_cannot_be_hidden_by_desktop_metadata() {
        assert!(matches!(
            pack_status_to_view(&CorePackStatus::Active, Some("active"), 1),
            ViewPackStatus::Review
        ));
        assert!(matches!(
            pack_status_to_view(&CorePackStatus::Archived, Some("review"), 0),
            ViewPackStatus::Draft
        ));
    }

    #[test]
    fn pack_slugs_are_non_empty_and_new_packs_do_not_alias_existing_packs() {
        assert_eq!(slug_pack_name("A---B"), "a-b");
        assert_eq!(slug_pack_name("💭"), "context-pack");

        let scope = ScopeRef::normalized(CoreScopeKind::Project, "atlas").expect("scope");
        let existing = PackRecord {
            id: "pack-1".to_string(),
            scope: scope.clone(),
            name: "release-notes".to_string(),
            description: None,
            metadata: json!({}),
            status: CorePackStatus::Active,
            locked: false,
            lock_reason: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            revision_no: 1,
        };
        assert_eq!(
            unique_pack_name(&scope, "release-notes", &[existing]),
            "release-notes-2"
        );
    }

    #[test]
    fn review_edits_preserve_json_entries() {
        let edited = edited_review_value(
            &EntryValue::Json {
                value: json!({"old": true}),
            },
            "```json\n{\"new\": true}\n```".to_string(),
        )
        .expect("valid JSON edit");
        assert_eq!(
            edited,
            EntryValue::Json {
                value: json!({"new": true})
            }
        );
        assert!(edited_review_value(
            &EntryValue::Json { value: json!({}) },
            "not json".to_string()
        )
        .is_err());
    }

    #[test]
    fn exported_text_files_are_private() {
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("context-export.json");
        write_text_file(&path, "{\"safe\":true}").expect("write export");
        assert_eq!(
            fs::read_to_string(&path).expect("read export"),
            "{\"safe\":true}"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&path).expect("metadata").permissions().mode() & 0o777,
                0o600
            );
        }
    }
}
