use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    fs,
    io::Write,
    path::{Path, PathBuf},
};

use chrono::Utc;
use context_client::{ClientConfig, ContextClient, SpoolRetryReport};
use context_core::{
    ComposeRequest, ContextExportBundle, ContextPaths, CreatePackRequest, DeleteEntryRequest,
    EntryInput, EntryRecord, EntrySelector, EntryStatus as CoreEntryStatus, EntryValue,
    ExportRequest, HealthReport, ImportFormat, ImportRequest, PackRecord,
    PackStatus as CorePackStatus, Provenance, PutEntryRequest, RevertEntryRequest,
    ReviewDecisionRequest, ReviewEditAndApproveRequest, ReviewItem as CoreReviewItem,
    ReviewPolicy as CoreReviewPolicy, ReviewReason, ReviewState, RunInput, RunRecord,
    ScopeKind as CoreScopeKind, ScopeRef, SearchRequest, SetReviewPolicyRequest,
    SourceImportApplyRequest as CoreSourceImportApplyRequest,
    SourceImportApplyResult as CoreSourceImportApplyResult, SourceImportDocument,
    SourceImportKind as CoreSourceImportKind, SourceImportPreview as CoreSourceImportPreview,
    SourceImportPreviewRequest as CoreSourceImportPreviewRequest, StoreStats, UpdatePackRequest,
};
use rusqlite::{params, Connection, OpenFlags, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use crate::diagnostics::{
    adapters_from_diagnostics, collect_diagnostics, discover_binary, ADAPTER_CLAUDE, ADAPTER_CODEX,
    ADAPTER_DAEMON,
};
use crate::grants::PathGrantStore;
use crate::models::{
    ActivityRun, AdapterHealth, AdapterKind, AdapterStatus, BulkReviewDecisionInput,
    BulkReviewDecisionResult, BundleFormat, BundleImportApplyInput, BundleImportPreview,
    ComposeContextInput, ContextEntry, ContextExclusion, ContextMetrics, ContextPack,
    ContextPreview, DaemonControlResult, DashboardSnapshot, DashboardStats, DesktopError,
    DesktopErrorCode, DiagnosticsReport, DiscoveredInstructionSource, EntryFormat, EntryProvenance,
    EntryStatus, ForgetScopeFailure, ForgetScopeInput, ForgetScopeResult, ImportExportSummary,
    IncludedContextEntry, OnboardingState, PackStatus as ViewPackStatus, PathGrantPurpose,
    PathGrantSelection, PreviewSection, PreviewSource, PrivacyDataCounts, PrivacySummary,
    ProjectRegistration, RestoreRevisionResult, RevertEntryInput,
    ReviewDecision as ViewReviewDecision, ReviewDecisionInput, ReviewDecisionResult, ReviewDiff,
    ReviewItem, ReviewMode, ReviewPolicy, RevisionEntry, RiskLevel, RunStatus, SaveEntryInput,
    SavePackInput, ScopeKind as ViewScopeKind, SearchKind, SearchResult, SearchTarget,
    SetReviewPolicyInput, Settings, SourceImportApplyInput, SourceImportApplyItem,
    SourceImportApplyResult, SourceImportCandidate, SourceImportPreviewInput,
    SourceImportPreviewResult, SpoolRetryResult, ThemeMode, WorkspaceNode,
};

const DEFAULT_ACTOR: &str = "desktop-operator";
const DEFAULT_ENTRY_KEY: &str = "body";
const DESKTOP_METADATA_KEY: &str = "desktop";
const SETTINGS_FILE_NAME: &str = "desktop-settings.json";
const MAX_IMPORT_FILE_BYTES: u64 = 8 * 1024 * 1024;

#[derive(Clone)]
pub(crate) struct DesktopContextService<B: ContextBackend> {
    backend: B,
    settings_store: LocalSettingsStore,
    path_grants: PathGrantStore,
    settings_mutation_lock: std::sync::Arc<std::sync::Mutex<()>>,
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
            path_grants: PathGrantStore::default(),
            settings_mutation_lock: std::sync::Arc::new(std::sync::Mutex::new(())),
        }
    }

    pub(crate) fn issue_path_grant(
        &self,
        purpose: PathGrantPurpose,
        paths: Vec<PathBuf>,
    ) -> Result<PathGrantSelection, String> {
        self.path_grants.issue(purpose, paths)
    }

    fn consume_path_grant(
        &self,
        purpose: PathGrantPurpose,
        token: Option<&str>,
        paths: &[String],
    ) -> Result<Vec<PathBuf>, String> {
        self.path_grants.consume(purpose, token, paths)
    }

    fn update_local_settings<F>(&self, update: F) -> Result<LocalSettings, String>
    where
        F: FnOnce(&mut LocalSettings) -> Result<(), String>,
    {
        let _guard = self
            .settings_mutation_lock
            .lock()
            .map_err(|_| "settings mutation lock is unavailable".to_string())?;
        let mut settings = self.settings_store.load()?;
        update(&mut settings)?;
        self.settings_store.save(&settings)
    }

    pub fn load_dashboard(&self) -> Result<DashboardSnapshot, String> {
        let runtime = self.runtime_snapshot()?;
        let mut scope_catalog =
            build_scope_catalog(&runtime.bundles, &runtime.reviews, &runtime.runs);
        add_persisted_scope(&mut scope_catalog, &runtime.settings);
        let workspace = scope_catalog.to_workspace_nodes();
        let selected_scope_id = pick_selected_scope_id(&scope_catalog, &runtime.settings);
        let packs = runtime
            .bundles
            .iter()
            .map(|bundle| bundle.to_view_pack())
            .collect::<Vec<_>>();
        let entries = map_context_entries(&runtime.all_entries, &runtime.pack_records)?;
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
        let diagnostics = collect_diagnostics(
            &runtime.paths,
            &self.settings_store.file_path,
            runtime.connected,
            runtime.health.as_ref(),
            runtime.stats.as_ref(),
            &runtime.settings.adapter_enabled,
            &now_iso(),
        );
        let adapters = adapters_from_diagnostics(
            &diagnostics,
            &runtime.paths,
            runtime.stats.as_ref(),
            &runtime.settings.adapter_enabled,
        );
        let (durable_context, inferred_ready) = self.runtime_onboarding_readiness(&runtime);
        let onboarding = onboarding_state(&runtime.settings, durable_context, inferred_ready);
        let review_policy = runtime.review_policy.as_ref().map(map_review_policy);
        let settings = runtime
            .settings
            .to_public(review_policy.clone(), onboarding.clone());
        let privacy = build_privacy_summary(
            &runtime.paths,
            &self.settings_store.file_path,
            runtime.stats.as_ref(),
            runtime.health.as_ref(),
            diagnostics.spool_backlog,
        );
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
            entries,
            review_queue,
            activity,
            revisions,
            adapters,
            settings,
            review_policy,
            onboarding,
            diagnostics,
            privacy,
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

    pub fn list_entries(
        &self,
        scope_id: Option<String>,
        pack_id: Option<String>,
    ) -> Result<Vec<ContextEntry>, String> {
        let runtime = self.runtime_snapshot()?;
        let mut entries = map_context_entries(&runtime.all_entries, &runtime.pack_records)?;
        if let Some(scope_id) = scope_id {
            let scope_id = encode_scope_ref(&decode_scope_id(&scope_id)?);
            entries.retain(|entry| entry.scope_id == scope_id);
        }
        if let Some(pack_id) = pack_id {
            entries.retain(|entry| entry.pack_id == pack_id);
        }
        entries.sort_by(|left, right| {
            right
                .updated_at
                .cmp(&left.updated_at)
                .then_with(|| left.key.cmp(&right.key))
        });
        Ok(entries)
    }

    pub fn save_entry(&self, input: SaveEntryInput) -> Result<ContextEntry, String> {
        let settings = self.settings_store.load()?;
        let paths = self.settings_store.resolve_paths(&settings)?;
        let packs = self.backend.list_packs(&paths)?;
        let entries = self.backend.list_entries(
            &paths,
            ExportRequest {
                project_scope_id: None,
                task_scope_id: None,
                scope: None,
                pack_name: None,
                include_deleted: true,
                include_reviews: false,
                include_runs: false,
            },
        )?;
        let actor = effective_actor(input.actor.as_deref());
        let requested_scope = decode_scope_id(&input.scope_id)?;
        let requested_scope_id = encode_scope_ref(&requested_scope);
        let key = input.key.trim();
        if key.is_empty() {
            return Err("entry key must not be empty".to_string());
        }
        let kind = input.kind.trim();
        if kind.is_empty() {
            return Err("entry kind must not be empty".to_string());
        }
        let mut entry_input = EntryInput {
            key: key.to_string(),
            title: input
                .title
                .as_deref()
                .map(str::trim)
                .filter(|title| !title.is_empty())
                .map(ToString::to_string),
            kind: kind.to_string(),
            value: entry_value_from_input(&input.format, &input.body)?,
            tags: input.tags.clone(),
            metadata: json!({}),
            locked: input.locked,
            provenance: Some(Provenance {
                actor: actor.clone(),
                source: "desktop_editor".to_string(),
                source_ref: None,
                run_id: None,
                request_id: None,
                note: input.note.clone(),
            }),
        };
        context_core::secret::reject_entry_for_storage(&entry_input)
            .map_err(|error| error.to_string())?;

        let (scope, pack, metadata) = if let Some(entry_id) = input.id.as_deref() {
            let existing = entries
                .iter()
                .find(|entry| entry.id == entry_id)
                .ok_or_else(|| format!("Unknown entry id: {entry_id}"))?;
            if encode_scope_ref(&existing.scope) != requested_scope_id {
                return Err(
                    "conflict: an existing entry cannot be moved to another scope".to_string(),
                );
            }
            if existing.key != key {
                return Err(
                    "conflict: an existing entry key cannot be changed; create a new entry instead"
                        .to_string(),
                );
            }
            let pack = packs
                .iter()
                .find(|pack| pack.scope == existing.scope && pack.name == existing.pack_name)
                .cloned()
                .ok_or_else(|| format!("Pack not found for entry: {entry_id}"))?;
            if input
                .pack_id
                .as_deref()
                .is_some_and(|pack_id| pack_id != pack.id)
            {
                return Err(
                    "conflict: an existing entry cannot be moved to another pack".to_string(),
                );
            }
            if input.pack_name.as_deref().is_some_and(|pack_name| {
                pack_name != pack.name && pack_name != display_name_for_pack(&pack)
            }) {
                return Err(
                    "conflict: an existing entry cannot be moved to another pack".to_string(),
                );
            }
            (existing.scope.clone(), pack, existing.metadata.clone())
        } else {
            let scope = requested_scope;
            let requested_pack_name = input
                .pack_name
                .as_deref()
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .unwrap_or(context_core::DEFAULT_PACK_NAME);
            let pack = match resolve_pack_for_entry(
                &packs,
                &scope,
                input.pack_id.as_deref(),
                Some(requested_pack_name),
            )? {
                Some(pack) => pack,
                None => {
                    let core_name =
                        unique_pack_name(&scope, &slug_pack_name(requested_pack_name), &packs);
                    let parent_project_scope_id = if scope.kind == CoreScopeKind::Task {
                        self.find_parent_project_scope_id(&paths, &scope.id)?
                    } else {
                        None
                    };
                    self.backend.create_pack(
                        &paths,
                        CreatePackRequest {
                            scope: scope.clone(),
                            name: core_name,
                            description: Some("Created by the desktop entry editor.".to_string()),
                            metadata: update_desktop_pack_metadata(
                                json!({}),
                                requested_pack_name,
                                "Created by the desktop entry editor.",
                                &ViewPackStatus::Active,
                                key,
                                parent_project_scope_id.as_deref(),
                            ),
                            locked: false,
                            lock_reason: None,
                            actor: actor.clone(),
                        },
                    )?
                }
            };
            if entries.iter().any(|entry| {
                entry.scope == scope && entry.pack_name == pack.name && entry.key == key
            }) {
                return Err(format!(
                    "conflict: entry {key} already exists or is archived in pack {}; save or restore it by id",
                    display_name_for_pack(&pack)
                ));
            }
            (scope, pack, json!({}))
        };

        entry_input.metadata = update_desktop_entry_metadata(metadata);
        let record = self.backend.put_entry(
            &paths,
            PutEntryRequest {
                scope: scope.clone(),
                pack_name: pack.name.clone(),
                entry: entry_input,
                actor,
            },
        )?;
        self.update_local_settings(|settings| {
            remember_scope(settings, &requested_scope_id);
            Ok(())
        })?;
        Ok(map_context_entry(&record, &pack))
    }

    pub fn archive_entry(&self, entry_id: String) -> Result<ContextEntry, String> {
        let settings = self.settings_store.load()?;
        let paths = self.settings_store.resolve_paths(&settings)?;
        let (entry, pack) = self.resolve_entry_id(&paths, &entry_id, true)?;
        if entry.status == CoreEntryStatus::Deleted {
            return Ok(map_context_entry(&entry, &pack));
        }
        let archived = self.backend.delete_entry(
            &paths,
            DeleteEntryRequest {
                selector: selector_for_entry(&entry),
                actor: DEFAULT_ACTOR.to_string(),
            },
        )?;
        Ok(map_context_entry(&archived, &pack))
    }

    pub fn restore_entry(&self, entry_id: String) -> Result<ContextEntry, String> {
        let settings = self.settings_store.load()?;
        let paths = self.settings_store.resolve_paths(&settings)?;
        let (entry, pack) = self.resolve_entry_id(&paths, &entry_id, true)?;
        if entry.status != CoreEntryStatus::Deleted {
            return Err(
                "conflict: restore_entry requires an entry whose current status is deleted"
                    .to_string(),
            );
        }
        let prior_active_revision =
            latest_active_entry_revision(&paths.db_path, &entry.id, entry.revision_no)?;
        let restored = if let Some(revision_no) = prior_active_revision {
            self.backend.revert_entry(
                &paths,
                RevertEntryRequest {
                    selector: selector_for_entry(&entry),
                    revision_no: Some(revision_no),
                    actor: DEFAULT_ACTOR.to_string(),
                },
            )?
        } else {
            let previous_provenance = entry.provenance.clone();
            self.backend.put_entry(
                &paths,
                PutEntryRequest {
                    scope: entry.scope.clone(),
                    pack_name: entry.pack_name.clone(),
                    entry: EntryInput {
                        key: entry.key.clone(),
                        title: entry.title.clone(),
                        kind: entry.kind.clone(),
                        value: entry.value.clone(),
                        tags: entry.tags.clone(),
                        metadata: entry.metadata.clone(),
                        locked: entry.locked,
                        provenance: Some(Provenance {
                            actor: DEFAULT_ACTOR.to_string(),
                            source: "restore".to_string(),
                            source_ref: previous_provenance.source_ref.or_else(|| {
                                Some(format!("entry:{}:revision:{}", entry.id, entry.revision_no))
                            }),
                            run_id: previous_provenance.run_id,
                            request_id: previous_provenance.request_id,
                            note: Some(match previous_provenance.note {
                                Some(note) => format!(
                                    "Restored deleted snapshot from {}. {}",
                                    previous_provenance.source, note
                                ),
                                None => format!(
                                    "Restored deleted snapshot from {}.",
                                    previous_provenance.source
                                ),
                            }),
                        }),
                    },
                    actor: DEFAULT_ACTOR.to_string(),
                },
            )?
        };
        Ok(map_context_entry(&restored, &pack))
    }

    pub fn revert_entry_revision(&self, input: RevertEntryInput) -> Result<ContextEntry, String> {
        let settings = self.settings_store.load()?;
        let paths = self.settings_store.resolve_paths(&settings)?;
        let (entry, pack) = self.resolve_entry_id(&paths, &input.entry_id, true)?;
        let restored = self.backend.revert_entry(
            &paths,
            RevertEntryRequest {
                selector: selector_for_entry(&entry),
                revision_no: input.revision,
                actor: effective_actor(input.actor.as_deref()),
            },
        )?;
        Ok(map_context_entry(&restored, &pack))
    }

    pub fn save_pack(&self, input: SavePackInput) -> Result<ContextPack, String> {
        let settings = self.settings_store.load()?;
        let paths = self.settings_store.resolve_paths(&settings)?;
        let scope = decode_scope_id(&input.scope_id)?;
        let canonical_scope_id = encode_scope_ref(&scope);
        let pack_catalog = self.backend.list_packs(&paths)?;
        let existing = input
            .id
            .as_ref()
            .and_then(|id| pack_catalog.iter().find(|pack| &pack.id == id).cloned());
        if input.id.is_some() && existing.is_none() {
            return Err(format!(
                "Unknown pack id: {}",
                input.id.as_deref().unwrap_or_default()
            ));
        }
        if existing.as_ref().is_some_and(|pack| pack.scope != scope) {
            return Err("conflict: an existing pack cannot be moved to another scope".to_string());
        }
        let existing_entries = if let Some(pack) = &existing {
            self.backend.list_entries(
                &paths,
                ExportRequest {
                    project_scope_id: None,
                    task_scope_id: None,
                    scope: Some(pack.scope.clone()),
                    pack_name: Some(pack.name.clone()),
                    include_deleted: true,
                    include_reviews: false,
                    include_runs: false,
                },
            )?
        } else {
            Vec::new()
        };
        let existing_desktop_metadata = existing
            .as_ref()
            .map(|pack| desktop_pack_metadata(&pack.metadata))
            .unwrap_or_default();
        let existing_primary_key = existing_desktop_metadata
            .primary_entry_key
            .clone()
            .filter(|key| {
                existing_entries
                    .iter()
                    .any(|entry| entry.key == *key && entry.status == CoreEntryStatus::Active)
            })
            .or_else(|| {
                existing_entries
                    .iter()
                    .find(|entry| {
                        entry.key == DEFAULT_ENTRY_KEY
                            && entry.status == CoreEntryStatus::Active
                            && (existing_desktop_metadata.managed
                                || desktop_entry_managed(&entry.metadata))
                    })
                    .map(|entry| entry.key.clone())
            })
            .unwrap_or_else(|| {
                let preferred = if existing.is_some() && !existing_entries.is_empty() {
                    "desktop-body"
                } else {
                    DEFAULT_ENTRY_KEY
                };
                unique_entry_key(preferred, &existing_entries)
            });

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

        self.update_local_settings(|settings| {
            remember_scope(settings, &canonical_scope_id);
            Ok(())
        })?;

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
        self.compose_effective_context(ComposeContextInput {
            scope_id,
            destination_adapter: Some("generic".to_string()),
        })
    }

    pub fn compose_effective_context(
        &self,
        input: ComposeContextInput,
    ) -> Result<ContextPreview, String> {
        let runtime = self.runtime_snapshot()?;
        let scope_id = encode_scope_ref(&decode_scope_id(&input.scope_id)?);
        let mut scope_catalog =
            build_scope_catalog(&runtime.bundles, &runtime.reviews, &runtime.runs);
        add_persisted_scope(&mut scope_catalog, &runtime.settings);
        let scope_entry = scope_catalog
            .entries
            .get(&scope_id)
            .ok_or_else(|| format!("Unknown scope: {scope_id}"))?;
        let compose_request = compose_request_for_scope(scope_entry);
        let response = self
            .backend
            .compose_context(&runtime.paths, compose_request)?;

        let preview_sections = response
            .sections
            .iter()
            .enumerate()
            .map(|(index, section)| {
                let body = render_section_body(section.entries.as_slice());
                let encoded_scope = encode_scope_ref(&section.scope);
                let lookup_key = pack_lookup_key(&encoded_scope, &section.pack_name);
                PreviewSection {
                    id: format!("preview:{}:{}", encoded_scope, section.pack_name),
                    order: index as u32,
                    layer: section.scope.kind.as_str().to_string(),
                    title: section_title_for_scope(&section.scope.kind),
                    pack_name: display_name_from_lookup(&runtime.pack_lookup, &lookup_key)
                        .unwrap_or_else(|| section.pack_name.clone()),
                    scope_id: encoded_scope,
                    scope_label: scope_label(&section.scope),
                    scope_kind: map_scope_kind(&section.scope.kind),
                    tokens: estimate_tokens(&body),
                    body,
                    entry_ids: section
                        .entries
                        .iter()
                        .map(|entry| entry.id.clone())
                        .collect(),
                }
            })
            .collect::<Vec<_>>();
        let sources = response
            .sections
            .iter()
            .zip(preview_sections.iter())
            .map(|(core_section, section)| PreviewSource {
                pack_id: pack_id_from_lookup(
                    &runtime.pack_lookup,
                    &pack_lookup_key(&section.scope_id, &core_section.pack_name),
                )
                .unwrap_or_else(|| format!("{}:{}", section.scope_label, section.pack_name)),
                pack_name: section.pack_name.clone(),
                scope_label: section.scope_label.clone(),
                excerpt: summarize_excerpt(&section.body, 140),
                tokens: section.tokens,
            })
            .collect::<Vec<_>>();
        let included_entries = response
            .sections
            .iter()
            .flat_map(|section| section.entries.iter())
            .enumerate()
            .map(|(index, entry)| IncludedContextEntry {
                order: index as u32,
                entry_id: entry.id.clone(),
                pack_name: display_name_from_lookup(
                    &runtime.pack_lookup,
                    &pack_lookup_key(&encode_scope_ref(&entry.scope), &entry.pack_name),
                )
                .unwrap_or_else(|| entry.pack_name.clone()),
                scope_id: encode_scope_ref(&entry.scope),
                scope_kind: map_scope_kind(&entry.scope.kind),
                scope_label: scope_label(&entry.scope),
                key: entry.key.clone(),
                title: entry.title.clone(),
                kind: entry.kind.clone(),
                format: map_entry_format(&entry.value),
                provenance: map_provenance(&entry.provenance),
                revision: entry.revision_no,
                token_estimate: estimate_tokens(&entry.value.render_markdown()),
            })
            .collect::<Vec<_>>();
        let exclusions = response
            .exclusions
            .iter()
            .map(|exclusion| ContextExclusion {
                entry_id: exclusion.entry_id.clone(),
                scope_id: encode_scope_ref(&exclusion.scope),
                scope_kind: map_scope_kind(&exclusion.scope.kind),
                scope_label: scope_label(&exclusion.scope),
                pack_name: exclusion.pack_name.clone(),
                entry_key: exclusion.entry_key.clone(),
                revision: exclusion.revision_no,
                reason: exclusion.reason.clone(),
            })
            .collect::<Vec<_>>();
        let total_tokens = if response.metrics.estimated_tokens == 0 {
            preview_sections.iter().map(|section| section.tokens).sum()
        } else {
            u32::try_from(response.metrics.estimated_tokens).unwrap_or(u32::MAX)
        };

        let relevant_scope_ids = relevant_scope_ids(scope_entry);
        let mut warnings = response.warnings.clone();
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
            scope_id,
            headline: format!("{} composed preview", scope_entry.label),
            total_tokens,
            warnings,
            sections: preview_sections,
            sources,
            destination_adapter: input
                .destination_adapter
                .map(|adapter| adapter.trim().to_string())
                .filter(|adapter| !adapter.is_empty())
                .unwrap_or_else(|| "generic".to_string()),
            generated_at: response.generated_at.to_rfc3339(),
            rendered_markdown: response.rendered_markdown,
            metrics: ContextMetrics {
                rendered_bytes: response.metrics.rendered_bytes,
                estimated_tokens: response.metrics.estimated_tokens,
                included_entries: response.metrics.included_entries,
                excluded_entries: response.metrics.excluded_entries,
            },
            exclusions,
            included_entries,
        })
    }

    pub fn search_index(&self, query: String) -> Result<Vec<SearchResult>, String> {
        let runtime = self.runtime_snapshot()?;
        if query.trim().is_empty() {
            return Ok(Vec::new());
        }
        let mut results = Vec::new();
        if runtime.connected {
            let scope_catalog =
                build_scope_catalog(&runtime.bundles, &runtime.reviews, &runtime.runs);
            let mut scope_ids = BTreeSet::from([encode_scope_ref(&ScopeRef::global())]);
            scope_ids.extend(runtime.bundles.iter().map(|bundle| bundle.scope_id.clone()));
            let mut seen_entry_ids = BTreeSet::new();
            let mut entry_hits = Vec::new();
            for scope_id in scope_ids {
                let Some(scope) = scope_catalog.entries.get(&scope_id) else {
                    continue;
                };
                let compose_request = compose_request_for_scope(scope);
                let response = self.backend.search_context(
                    &runtime.paths,
                    SearchRequest {
                        query: query.clone(),
                        project_scope_id: compose_request.project_scope_id,
                        task_scope_id: compose_request.task_scope_id,
                        limit: 12,
                    },
                )?;
                entry_hits.extend(
                    response
                        .hits
                        .into_iter()
                        .filter(|hit| seen_entry_ids.insert(hit.entry.id.clone())),
                );
            }
            results.extend(entry_hits.into_iter().map(|hit| {
                let scope_id = encode_scope_ref(&hit.entry.scope);
                let pack_id = pack_id_from_lookup(
                    &runtime.pack_lookup,
                    &pack_lookup_key(&scope_id, &hit.entry.pack_name),
                );
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
                    id: hit.entry.id.clone(),
                    kind: SearchKind::Entry,
                    title,
                    excerpt: summarize_excerpt(&hit.snippet.replace(['[', ']'], ""), 140),
                    scope_label: scope_label(&hit.entry.scope),
                    score: normalize_search_score(hit.score),
                    updated_at: hit.entry.updated_at.to_rfc3339(),
                    tags: hit.entry.tags,
                    target: SearchTarget {
                        scope_id: Some(scope_id),
                        pack_id,
                        entry_id: Some(hit.entry.id),
                        ..SearchTarget::default()
                    },
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
                    target: SearchTarget {
                        scope_id: Some(bundle.scope_id.clone()),
                        pack_id: Some(bundle.id.clone()),
                        ..SearchTarget::default()
                    },
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
                let pack_id = runtime
                    .reviews
                    .iter()
                    .find(|record| record.id == review.id)
                    .and_then(|record| {
                        pack_id_from_lookup(
                            &runtime.pack_lookup,
                            &pack_lookup_key(&encode_scope_ref(&record.scope), &record.pack_name),
                        )
                    });
                results.push(SearchResult {
                    id: review.id.clone(),
                    kind: SearchKind::Review,
                    title: review.title.clone(),
                    excerpt: summarize_excerpt(&review.summary, 140),
                    scope_label: review.scope_label.clone(),
                    score: 91,
                    updated_at: review.requested_at.clone(),
                    tags: vec![risk_tag(&review.risk), "review".to_string()],
                    target: SearchTarget {
                        scope_id: Some(review.scope_id.clone()),
                        pack_id,
                        review_id: Some(review.id.clone()),
                        ..SearchTarget::default()
                    },
                });
            }
        }
        for run in map_runs(&runtime.runs) {
            let haystack = format!("{} {}", run.actor, run.summary).to_lowercase();
            if haystack.contains(&needle) {
                let scope_id = runtime
                    .runs
                    .iter()
                    .find(|record| record.id == run.id)
                    .and_then(run_scope_id);
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
                    target: SearchTarget {
                        scope_id,
                        ..SearchTarget::default()
                    },
                });
            }
        }
        for bundle in &runtime.bundles {
            let Ok(revisions) = self.load_revisions_for_bundle(bundle, &runtime.paths) else {
                continue;
            };
            for revision in revisions {
                let haystack = format!(
                    "{} {} {}",
                    revision.entity_label, revision.note, revision.change_summary
                )
                .to_lowercase();
                if haystack.contains(&needle) {
                    results.push(SearchResult {
                        id: revision.id.clone(),
                        kind: SearchKind::Revision,
                        title: revision.entity_label,
                        excerpt: summarize_excerpt(&revision.change_summary, 140),
                        scope_label: bundle.scope_label.clone(),
                        score: 80,
                        updated_at: revision.created_at,
                        tags: vec!["revision".to_string()],
                        target: SearchTarget {
                            scope_id: Some(bundle.scope_id.clone()),
                            pack_id: Some(bundle.id.clone()),
                            revision_id: Some(revision.id),
                            ..SearchTarget::default()
                        },
                    });
                }
            }
        }
        let diagnostics = collect_diagnostics(
            &runtime.paths,
            &self.settings_store.file_path,
            runtime.connected,
            runtime.health.as_ref(),
            runtime.stats.as_ref(),
            &runtime.settings.adapter_enabled,
            &now_iso(),
        );
        for adapter in adapters_from_diagnostics(
            &diagnostics,
            &runtime.paths,
            runtime.stats.as_ref(),
            &runtime.settings.adapter_enabled,
        ) {
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
                    target: SearchTarget {
                        adapter_id: Some(adapter.id),
                        ..SearchTarget::default()
                    },
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
        let result = self.bulk_review_decision(BulkReviewDecisionInput {
            item_ids: vec![input.item_id],
            decision: input.decision,
            confirmation: false,
            edited_content: input.edited_content,
            actor: None,
            note: None,
        })?;
        if let Some(failure) = result.results.into_iter().find(|item| !item.success) {
            return Err(failure
                .error
                .map(|error| error.message)
                .unwrap_or_else(|| "review decision failed".to_string()));
        }
        Ok(())
    }

    pub fn bulk_review_decision(
        &self,
        input: BulkReviewDecisionInput,
    ) -> Result<BulkReviewDecisionResult, String> {
        if input.item_ids.len() > 1 && !input.confirmation {
            return Err(
                "confirmation is required before applying a bulk review decision".to_string(),
            );
        }
        let mut item_ids = input.item_ids.clone();
        item_ids.sort();
        item_ids.dedup();
        if item_ids.is_empty() {
            return Err("review decision requires at least one item".to_string());
        }
        if input.decision == ViewReviewDecision::Edit && item_ids.len() != 1 {
            return Err("edit review decisions are limited to one item".to_string());
        }
        let actor = effective_actor(input.actor.as_deref());
        let runtime = self.runtime_snapshot()?;
        let mut results = Vec::with_capacity(item_ids.len());
        let mut stopped = false;
        for item_id in item_ids {
            let Some(review) = runtime
                .reviews
                .iter()
                .find(|item| item.id == item_id)
                .cloned()
            else {
                results.push(ReviewDecisionResult {
                    item_id: item_id.clone(),
                    success: false,
                    requires_follow_up: false,
                    state: None,
                    error: Some(desktop_error(format!("Unknown review item: {item_id}"))),
                });
                stopped = true;
                break;
            };
            let note = input.note.clone().or_else(|| {
                Some(format!(
                    "{} from desktop review queue.",
                    review_decision_label(&input.decision)
                ))
            });
            let decision_result = match &input.decision {
                ViewReviewDecision::Approve => self
                    .backend
                    .review_approve(
                        &runtime.paths,
                        ReviewDecisionRequest {
                            review_id: review.id.clone(),
                            actor: actor.clone(),
                            note,
                        },
                    )
                    .map(|review| review.state),
                ViewReviewDecision::Reject => self
                    .backend
                    .review_reject(
                        &runtime.paths,
                        ReviewDecisionRequest {
                            review_id: review.id.clone(),
                            actor: actor.clone(),
                            note,
                        },
                    )
                    .map(|review| review.state),
                ViewReviewDecision::Edit => {
                    let content = input
                        .edited_content
                        .clone()
                        .unwrap_or_else(|| suggested_edit_body(&review));
                    edited_review_value(&review.proposed_entry.value, content).and_then(
                        |edited_value| {
                            self.backend
                                .review_edit_and_approve(
                                    &runtime.paths,
                                    ReviewEditAndApproveRequest {
                                        review_id: review.id.clone(),
                                        title: review.proposed_entry.title.clone(),
                                        kind: Some(review.proposed_entry.kind.clone()),
                                        value: Some(edited_value),
                                        tags: Some(review.proposed_entry.tags.clone()),
                                        metadata: Some(review.proposed_entry.metadata.clone()),
                                        locked: Some(review.proposed_entry.locked),
                                        actor: actor.clone(),
                                        note,
                                    },
                                )
                                .map(|review| review.state)
                        },
                    )
                }
            };
            match decision_result {
                Ok(state) => {
                    let requires_follow_up = false;
                    results.push(ReviewDecisionResult {
                        item_id: review.id.clone(),
                        success: true,
                        requires_follow_up,
                        state: Some(state),
                        error: None,
                    });
                    let _ = self.backend.create_run(
                        &runtime.paths,
                        RunInput {
                            id: None,
                            project_scope_id: match review.scope.kind {
                                CoreScopeKind::Project => Some(review.scope.id.clone()),
                                CoreScopeKind::Task => self
                                    .find_parent_project_scope_id(&runtime.paths, &review.scope.id)
                                    .unwrap_or(None)
                                    .and_then(|scope_id| raw_project_scope_id(&scope_id)),
                                CoreScopeKind::Global => None,
                            },
                            task_scope_id: match review.scope.kind {
                                CoreScopeKind::Task => Some(review.scope.id.clone()),
                                _ => None,
                            },
                            source: "desktop.review".to_string(),
                            metadata: json!({
                                "summary": format!(
                                    "{} review {}",
                                    display_name_for_review(&review),
                                    review_decision_label(&input.decision)
                                ),
                                "status": "completed",
                                "step_count": if requires_follow_up { 1 } else { 2 },
                                "requires_follow_up": requires_follow_up
                            }),
                        },
                    );
                }
                Err(error) => {
                    results.push(ReviewDecisionResult {
                        item_id: review.id,
                        success: false,
                        requires_follow_up: false,
                        state: None,
                        error: Some(desktop_error(error)),
                    });
                    stopped = true;
                    break;
                }
            }
        }
        let completed = results.iter().filter(|result| result.success).count();
        Ok(BulkReviewDecisionResult {
            decision: input.decision,
            attempted: results.len(),
            completed,
            stopped,
            results,
        })
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
        let diagnostics = collect_diagnostics(
            &runtime.paths,
            &self.settings_store.file_path,
            runtime.connected,
            runtime.health.as_ref(),
            runtime.stats.as_ref(),
            &runtime.settings.adapter_enabled,
            &now_iso(),
        );
        Ok(adapters_from_diagnostics(
            &diagnostics,
            &runtime.paths,
            runtime.stats.as_ref(),
            &runtime.settings.adapter_enabled,
        ))
    }

    pub fn toggle_adapter(
        &self,
        adapter_id: String,
        enabled: bool,
    ) -> Result<AdapterStatus, String> {
        if ![ADAPTER_DAEMON, ADAPTER_CODEX, ADAPTER_CLAUDE].contains(&adapter_id.as_str()) {
            return Err(format!("Unknown adapter: {adapter_id}"));
        }
        self.update_local_settings(|settings| {
            settings.adapter_enabled.insert(adapter_id.clone(), enabled);
            Ok(())
        })?;
        self.list_adapters()?
            .into_iter()
            .find(|adapter| adapter.id == adapter_id)
            .ok_or_else(|| format!("Unknown adapter: {adapter_id}"))
    }

    pub fn load_settings(&self) -> Result<Settings, String> {
        let runtime = self.runtime_snapshot()?;
        let (durable_context, inferred_ready) = self.runtime_onboarding_readiness(&runtime);
        Ok(runtime.settings.to_public(
            runtime.review_policy.as_ref().map(map_review_policy),
            onboarding_state(&runtime.settings, durable_context, inferred_ready),
        ))
    }

    pub fn save_settings(&self, settings: Settings) -> Result<Settings, String> {
        let _guard = self
            .settings_mutation_lock
            .lock()
            .map_err(|_| "settings mutation lock is unavailable".to_string())?;
        let original = self.settings_store.load()?;
        let mut current = original.clone();
        let requested_mode = settings.review_mode;
        current.apply_public(settings);
        let paths = self.settings_store.resolve_paths(&current)?;
        self.settings_store.save(&current)?;
        let policy = match self.backend.set_review_policy(
            &paths,
            SetReviewPolicyRequest {
                mode: requested_mode,
                metadata: json!({"source": "desktop.settings"}),
                actor: DEFAULT_ACTOR.to_string(),
            },
        ) {
            Ok(policy) => policy,
            Err(error) => {
                if let Err(rollback_error) = self.settings_store.save(&original) {
                    return Err(format!(
                        "{error}; local settings rollback failed: {rollback_error}"
                    ));
                }
                return Err(error);
            }
        };
        current.review_mode = policy.mode;
        let (durable_context, inferred_ready) = self
            .onboarding_readiness(&current, &paths)
            .unwrap_or((false, false));
        Ok(current.to_public(
            Some(map_review_policy(&policy)),
            onboarding_state(&current, durable_context, inferred_ready),
        ))
    }

    pub fn set_review_policy(&self, input: SetReviewPolicyInput) -> Result<ReviewPolicy, String> {
        let _guard = self
            .settings_mutation_lock
            .lock()
            .map_err(|_| "settings mutation lock is unavailable".to_string())?;
        let original = self.settings_store.load()?;
        let mut settings = original.clone();
        let paths = self.settings_store.resolve_paths(&settings)?;
        let mode = input.mode;
        settings.review_mode = mode;
        self.settings_store.save(&settings)?;
        let policy = match self.backend.set_review_policy(
            &paths,
            SetReviewPolicyRequest {
                mode,
                metadata: json!({
                    "source": "desktop.governance",
                    "note": input.note,
                    "requestId": input.request_id,
                }),
                actor: input.actor,
            },
        ) {
            Ok(policy) => policy,
            Err(error) => {
                if let Err(rollback_error) = self.settings_store.save(&original) {
                    return Err(format!(
                        "{error}; local settings rollback failed: {rollback_error}"
                    ));
                }
                return Err(error);
            }
        };
        Ok(map_review_policy(&policy))
    }

    pub fn complete_onboarding(&self) -> Result<OnboardingState, String> {
        let _guard = self
            .settings_mutation_lock
            .lock()
            .map_err(|_| "settings mutation lock is unavailable".to_string())?;
        let mut settings = self.settings_store.load()?;
        let selected_scope = persisted_selected_scope(&settings).ok_or_else(|| {
            "onboarding requires a persisted selected project or scope".to_string()
        })?;
        let paths = self.settings_store.resolve_paths(&settings)?;
        if self.active_composable_entry_count(&paths)? == 0 {
            return Err(
                "onboarding requires at least one active durable context entry".to_string(),
            );
        }
        let composed = self.compose_persisted_scope(&paths, selected_scope)?;
        let included_entries = composed
            .sections
            .iter()
            .map(|section| section.entries.len())
            .sum::<usize>();
        if included_entries == 0 {
            return Err(
                "onboarding requires the selected scope to compose at least one active entry"
                    .to_string(),
            );
        }
        settings.onboarding_complete = Some(true);
        settings.onboarding_completed_at = Some(now_iso());
        let settings = self.settings_store.save(&settings)?;
        Ok(onboarding_state(&settings, true, true))
    }

    pub fn reset_onboarding(&self) -> Result<OnboardingState, String> {
        let settings = self.update_local_settings(|settings| {
            settings.onboarding_complete = Some(false);
            settings.onboarding_completed_at = None;
            Ok(())
        })?;
        let paths = self.settings_store.resolve_paths(&settings)?;
        let (durable_context, inferred_ready) = self
            .onboarding_readiness(&settings, &paths)
            .unwrap_or((false, false));
        Ok(onboarding_state(&settings, durable_context, inferred_ready))
    }

    pub fn set_selected_scope(
        &self,
        scope_id: String,
        project_path: Option<String>,
    ) -> Result<Settings, String> {
        let scope = decode_scope_id(&scope_id)?;
        let scope_id = encode_scope_ref(&scope);
        let normalized_project_path = if let Some(project_path) = project_path {
            let normalized = normalize_project_path(&project_path)?;
            if scope.kind == CoreScopeKind::Project && scope.id != normalized {
                return Err(
                    "conflict: selected project scope does not match the project path".to_string(),
                );
            }
            Some(normalized)
        } else {
            None
        };
        self.update_local_settings(|settings| {
            settings.last_selected_scope_id = Some(scope_id);
            if let Some(project_path) = normalized_project_path {
                settings.last_project_path = Some(project_path);
            }
            Ok(())
        })?;
        self.load_settings()
    }

    pub fn register_project(
        &self,
        path: String,
        grant_token: Option<String>,
    ) -> Result<ProjectRegistration, String> {
        let granted = self.consume_path_grant(
            PathGrantPurpose::ProjectRegistration,
            grant_token.as_deref(),
            std::slice::from_ref(&path),
        )?;
        let normalized_path = granted[0].display().to_string();
        let scope =
            ScopeRef::normalized(CoreScopeKind::Project, &normalized_path).map_err(|error| {
                format!("unable to create project scope from selected path: {error}")
            })?;
        let scope_id = encode_scope_ref(&scope);
        let instruction_sources = discover_instruction_sources(Path::new(&normalized_path))?;
        let settings = self.update_local_settings(|settings| {
            settings.last_project_path = Some(normalized_path.clone());
            settings.last_selected_scope_id = Some(scope_id.clone());
            Ok(())
        })?;
        let paths = self.settings_store.resolve_paths(&settings)?;
        let durable = self
            .backend
            .list_entries(
                &paths,
                ExportRequest {
                    project_scope_id: None,
                    task_scope_id: None,
                    scope: Some(scope),
                    pack_name: None,
                    include_deleted: true,
                    include_reviews: false,
                    include_runs: false,
                },
            )
            .map(|entries| !entries.is_empty())
            .unwrap_or(false);
        Ok(ProjectRegistration {
            input_path: path,
            normalized_path: normalized_path.clone(),
            scope_id,
            scope_kind: ViewScopeKind::Project,
            label: project_display_name(&normalized_path),
            instruction_sources,
            durable,
            selected: true,
        })
    }

    pub fn load_diagnostics(&self) -> Result<DiagnosticsReport, String> {
        let runtime = self.runtime_snapshot()?;
        Ok(collect_diagnostics(
            &runtime.paths,
            &self.settings_store.file_path,
            runtime.connected,
            runtime.health.as_ref(),
            runtime.stats.as_ref(),
            &runtime.settings.adapter_enabled,
            &now_iso(),
        ))
    }

    pub fn start_daemon(&self) -> Result<DaemonControlResult, String> {
        let settings = self.settings_store.load()?;
        let paths = self.settings_store.resolve_paths(&settings)?;
        let already_running = self.backend.ping(&paths).is_ok();
        self.backend.ensure_daemon(&paths)?;
        Ok(DaemonControlResult {
            action: "start".to_string(),
            performed: !already_running,
            message: if already_running {
                "The daemon was already running; its health was refreshed.".to_string()
            } else {
                "The daemon was started through the local context client.".to_string()
            },
            diagnostics: self.load_diagnostics()?,
        })
    }

    pub fn restart_daemon(&self) -> Result<DaemonControlResult, String> {
        let settings = self.settings_store.load()?;
        let paths = self.settings_store.resolve_paths(&settings)?;
        if self.backend.ping(&paths).is_ok() {
            let diagnostics = self.load_diagnostics()?;
            let state = diagnostics
                .checks
                .iter()
                .find(|check| check.id == "daemon-health")
                .map(|check| check.state.clone())
                .unwrap_or(crate::models::DiagnosticState::Degraded);
            let message = match state {
                crate::models::DiagnosticState::Healthy => {
                    "The reachable daemon is compatible and was rechecked. No restart was performed because the desktop does not own a shutdown protocol."
                }
                crate::models::DiagnosticState::Incompatible => {
                    "The reachable daemon is incompatible. No restart was performed because the desktop does not own a shutdown protocol."
                }
                crate::models::DiagnosticState::MigrationRequired => {
                    "The reachable daemon requires migration. No restart was performed because the desktop does not own a shutdown protocol."
                }
                _ => {
                    "The reachable daemon was rechecked without claiming a restart. The desktop does not own a shutdown protocol."
                }
            };
            return Ok(DaemonControlResult {
                action: "restart".to_string(),
                performed: false,
                message: message.to_string(),
                diagnostics,
            });
        }
        self.backend.ensure_daemon(&paths)?;
        Ok(DaemonControlResult {
            action: "restart".to_string(),
            performed: true,
            message: "The unavailable daemon was started through the local context client."
                .to_string(),
            diagnostics: self.load_diagnostics()?,
        })
    }

    pub fn retry_spool(&self) -> Result<SpoolRetryResult, String> {
        let settings = self.settings_store.load()?;
        let paths = self.settings_store.resolve_paths(&settings)?;
        let report = self.backend.retry_spool(&paths)?;
        Ok(SpoolRetryResult {
            attempted: report.attempted,
            delivered: report.delivered,
            retained: report.retained,
            errors: report
                .errors
                .into_iter()
                .map(|_| "A queued write could not be delivered and was retained.".to_string())
                .collect(),
            diagnostics: self.load_diagnostics()?,
        })
    }

    pub fn load_privacy_summary(&self) -> Result<PrivacySummary, String> {
        let runtime = self.runtime_snapshot()?;
        let diagnostics = collect_diagnostics(
            &runtime.paths,
            &self.settings_store.file_path,
            runtime.connected,
            runtime.health.as_ref(),
            runtime.stats.as_ref(),
            &runtime.settings.adapter_enabled,
            &now_iso(),
        );
        Ok(build_privacy_summary(
            &runtime.paths,
            &self.settings_store.file_path,
            runtime.stats.as_ref(),
            runtime.health.as_ref(),
            diagnostics.spool_backlog,
        ))
    }

    pub fn preview_source_import(
        &self,
        input: SourceImportPreviewInput,
    ) -> Result<SourceImportPreviewResult, String> {
        reject_bundle_source_kind(input.source_kind)?;
        let settings = self.settings_store.load()?;
        let paths = self.settings_store.resolve_paths(&settings)?;
        let destination = decode_scope_id(&input.destination_scope_id)?;
        let destination_scope_id = encode_scope_ref(&destination);
        let actor = effective_actor(input.actor.as_deref());
        let granted_paths = self.consume_path_grant(
            PathGrantPurpose::SourceImportPreview,
            input.grant_token.as_deref(),
            &input.paths,
        )?;
        let granted_path_strings = granted_paths
            .iter()
            .map(|path| path.display().to_string())
            .collect::<Vec<_>>();
        let documents = read_source_documents(&granted_path_strings, input.source_kind)?;
        let preview = self.backend.preview_source_import(
            &paths,
            CoreSourceImportPreviewRequest {
                source_kind: input.source_kind,
                documents: documents.clone(),
                destination,
                pack_name: input.pack_name.clone(),
                actor: actor.clone(),
            },
        )?;
        ensure_instruction_preview(&preview)?;
        let preview_fingerprint = preview
            .preview_fingerprint
            .clone()
            .filter(|fingerprint| !fingerprint.trim().is_empty())
            .ok_or_else(|| {
                "source import preview did not include an authoritative core fingerprint"
                    .to_string()
            })?;
        let preview_id = source_import_file_checksum(&documents, input.source_kind)?;
        let apply_grant = self
            .path_grants
            .issue_canonical(PathGrantPurpose::SourceImportApply, granted_paths)?;
        Ok(map_source_import_preview(
            preview_id,
            preview_fingerprint,
            apply_grant.grant_token,
            &destination_scope_id,
            preview,
        ))
    }

    pub fn apply_source_import(
        &self,
        input: SourceImportApplyInput,
    ) -> Result<SourceImportApplyResult, String> {
        if !input.confirmation {
            return Err("confirmation is required before applying a source import".to_string());
        }
        reject_bundle_source_kind(input.source_kind)?;
        let settings = self.settings_store.load()?;
        let paths = self.settings_store.resolve_paths(&settings)?;
        let destination = decode_scope_id(&input.destination_scope_id)?;
        let destination_scope_id = encode_scope_ref(&destination);
        let actor = effective_actor(input.actor.as_deref());
        let expected_preview_fingerprint = input
            .expected_preview_fingerprint
            .filter(|fingerprint| !fingerprint.trim().is_empty())
            .ok_or_else(|| {
                "source import apply requires expectedPreviewFingerprint from preview".to_string()
            })?;
        let granted_paths = self.consume_path_grant(
            PathGrantPurpose::SourceImportApply,
            input.grant_token.as_deref(),
            &input.paths,
        )?;
        let granted_path_strings = granted_paths
            .iter()
            .map(|path| path.display().to_string())
            .collect::<Vec<_>>();
        let documents = read_source_documents(&granted_path_strings, input.source_kind)?;
        let current_file_checksum = source_import_file_checksum(&documents, input.source_kind)?;
        if current_file_checksum != input.preview_id {
            return Err("conflict: source files changed after preview; preview again".to_string());
        }
        let preview = self.backend.preview_source_import(
            &paths,
            CoreSourceImportPreviewRequest {
                source_kind: input.source_kind,
                documents: documents.clone(),
                destination: destination.clone(),
                pack_name: input.pack_name.clone(),
                actor: actor.clone(),
            },
        )?;
        ensure_instruction_preview(&preview)?;
        self.update_local_settings(|settings| {
            remember_scope(settings, &destination_scope_id);
            Ok(())
        })?;
        let result = self.backend.apply_source_import(
            &paths,
            CoreSourceImportApplyRequest {
                source_kind: input.source_kind,
                documents,
                destination,
                pack_name: input.pack_name,
                actor,
                expected_preview_fingerprint: Some(expected_preview_fingerprint),
            },
        )?;
        Ok(map_source_import_apply(
            destination_scope_id,
            preview.pack_name,
            result,
        ))
    }

    pub fn preview_bundle_import(
        &self,
        path: String,
        grant_token: Option<String>,
    ) -> Result<BundleImportPreview, String> {
        let granted = self.consume_path_grant(
            PathGrantPurpose::BundleImportPreview,
            grant_token.as_deref(),
            std::slice::from_ref(&path),
        )?;
        let mut verified = load_verified_bundle(granted[0].display().to_string())?;
        verified.preview.path = path;
        verified.preview.apply_grant_token = self
            .path_grants
            .issue_canonical(PathGrantPurpose::BundleImportApply, granted)?
            .grant_token;
        Ok(verified.preview)
    }

    pub fn apply_bundle_import(
        &self,
        input: BundleImportApplyInput,
    ) -> Result<ImportExportSummary, String> {
        if !input.confirmation {
            return Err("confirmation is required before importing a UCM bundle".to_string());
        }
        let granted = self.consume_path_grant(
            PathGrantPurpose::BundleImportApply,
            input.grant_token.as_deref(),
            std::slice::from_ref(&input.path),
        )?;
        let mut verified = load_verified_bundle(granted[0].display().to_string())?;
        verified.preview.path = input.path;
        if verified.preview.checksum_sha256 != input.checksum_sha256 {
            return Err(
                "conflict: the UCM bundle changed after validation; preview it again".to_string(),
            );
        }
        let settings = self.settings_store.load()?;
        let paths = self.settings_store.resolve_paths(&settings)?;
        self.import_verified_bundle(&paths, verified)
    }

    pub fn forget_scope(&self, input: ForgetScopeInput) -> Result<ForgetScopeResult, String> {
        if !input.confirmation {
            return Err("confirmation is required before archiving scoped context".to_string());
        }
        let target = decode_scope_id(&input.scope_id)?;
        let target_scope_id = encode_scope_ref(&target);
        let actor = effective_actor(input.actor.as_deref());
        let settings = self.settings_store.load()?;
        let paths = self.settings_store.resolve_paths(&settings)?;
        let pack_records = self.backend.list_packs(&paths)?;
        let active_entries = self.backend.list_entries(
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
        )?;
        let reviews = self
            .backend
            .review_list(&paths, Some(ReviewState::Pending))?;
        let runs = self.backend.list_runs(&paths)?;
        let bundles = build_pack_bundles(pack_records.clone(), active_entries.clone(), &reviews);
        let catalog = build_scope_catalog(&bundles, &reviews, &runs);
        let mut scope_ids = BTreeSet::from([target_scope_id.clone()]);
        if target.kind == CoreScopeKind::Project {
            scope_ids.extend(
                catalog
                    .entries
                    .values()
                    .filter(|entry| {
                        entry.parent_project_scope_id.as_deref() == Some(target_scope_id.as_str())
                    })
                    .map(|entry| entry.encoded_id.clone()),
            );
        }
        let mut packs = pack_records
            .iter()
            .filter(|pack| scope_ids.contains(&encode_scope_ref(&pack.scope)))
            .cloned()
            .collect::<Vec<_>>();
        packs.sort_by(|left, right| left.id.cmp(&right.id));
        let mut entries_affected = 0;
        let mut packs_archived = 0;
        let mut packs_already_archived = 0;
        let mut failures = Vec::new();
        for pack in packs {
            if pack.status == CorePackStatus::Archived {
                packs_already_archived += 1;
                continue;
            }
            if let Err(error) = self.backend.update_pack(
                &paths,
                UpdatePackRequest {
                    selector: context_core::PackSelector {
                        scope: pack.scope.clone(),
                        name: pack.name.clone(),
                    },
                    description: None,
                    metadata: None,
                    status: Some(CorePackStatus::Archived),
                    locked: None,
                    lock_reason: None,
                    actor: actor.clone(),
                },
            ) {
                failures.push(ForgetScopeFailure {
                    pack_name: display_name_for_pack(&pack),
                    pack_id: pack.id,
                    error: desktop_error(error),
                });
                break;
            }
            packs_archived += 1;
            entries_affected += active_entries
                .iter()
                .filter(|entry| entry.scope == pack.scope && entry.pack_name == pack.name)
                .count();
        }
        Ok(ForgetScopeResult {
            scope_id: target_scope_id,
            scopes_matched: scope_ids.len(),
            packs_archived,
            packs_already_archived,
            entries_affected,
            reversible: true,
            stopped: !failures.is_empty(),
            failures,
        })
    }

    pub fn export_archive(
        &self,
        path: String,
        grant_token: Option<String>,
    ) -> Result<ImportExportSummary, String> {
        let settings = self.settings_store.load()?;
        let paths = self.settings_store.resolve_paths(&settings)?;
        let granted = self.consume_path_grant(
            PathGrantPurpose::ExportArchive,
            grant_token.as_deref(),
            std::slice::from_ref(&path),
        )?;
        let expanded_path = granted[0].clone();
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

    fn import_verified_bundle(
        &self,
        paths: &ContextPaths,
        verified: VerifiedBundleFile,
    ) -> Result<ImportExportSummary, String> {
        let path = verified.preview.path.clone();
        let bundle = self.backend.import_data(
            paths,
            ImportRequest {
                actor: DEFAULT_ACTOR.to_string(),
                format: verified.import_format,
                payload: verified.payload,
            },
        )?;
        let _ = self.backend.create_run(
            paths,
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

    fn resolve_entry_id(
        &self,
        paths: &ContextPaths,
        entry_id: &str,
        include_deleted: bool,
    ) -> Result<(EntryRecord, PackRecord), String> {
        if entry_id.trim().is_empty() {
            return Err("entry id must not be empty".to_string());
        }
        let entry = self
            .backend
            .list_entries(
                paths,
                ExportRequest {
                    project_scope_id: None,
                    task_scope_id: None,
                    scope: None,
                    pack_name: None,
                    include_deleted,
                    include_reviews: false,
                    include_runs: false,
                },
            )?
            .into_iter()
            .find(|entry| entry.id == entry_id)
            .ok_or_else(|| format!("Unknown entry id: {entry_id}"))?;
        let pack = self
            .backend
            .list_packs(paths)?
            .into_iter()
            .find(|pack| pack.scope == entry.scope && pack.name == entry.pack_name)
            .ok_or_else(|| format!("Pack not found for entry: {entry_id}"))?;
        Ok((entry, pack))
    }

    fn active_composable_entry_count(&self, paths: &ContextPaths) -> Result<usize, String> {
        let active_packs = self
            .backend
            .list_packs(paths)?
            .into_iter()
            .filter(|pack| pack.status == CorePackStatus::Active)
            .map(|pack| pack_lookup_key(&encode_scope_ref(&pack.scope), &pack.name))
            .collect::<BTreeSet<_>>();
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
            .map(|entries| {
                entries
                    .into_iter()
                    .filter(|entry| {
                        active_packs.contains(&pack_lookup_key(
                            &encode_scope_ref(&entry.scope),
                            &entry.pack_name,
                        ))
                    })
                    .count()
            })
    }

    fn compose_persisted_scope(
        &self,
        paths: &ContextPaths,
        selected_scope: ScopeRef,
    ) -> Result<context_core::ComposeResponse, String> {
        let compose_request = match selected_scope.kind {
            CoreScopeKind::Global => ComposeRequest {
                project_scope_id: None,
                task_scope_id: None,
                include_archived: false,
            },
            CoreScopeKind::Project => ComposeRequest {
                project_scope_id: Some(selected_scope.id),
                task_scope_id: None,
                include_archived: false,
            },
            CoreScopeKind::Task => ComposeRequest {
                project_scope_id: self
                    .find_parent_project_scope_id(paths, &selected_scope.id)?
                    .and_then(|scope_id| raw_project_scope_id(&scope_id)),
                task_scope_id: Some(selected_scope.id),
                include_archived: false,
            },
        };
        self.backend.compose_context(paths, compose_request)
    }

    fn onboarding_readiness(
        &self,
        settings: &LocalSettings,
        paths: &ContextPaths,
    ) -> Result<(bool, bool), String> {
        let durable_context = self.active_composable_entry_count(paths)? > 0;
        if !durable_context {
            return Ok((false, false));
        }
        let Some(selected_scope) = persisted_selected_scope(settings) else {
            return Ok((true, false));
        };
        let composed = self.compose_persisted_scope(paths, selected_scope)?;
        Ok((
            true,
            composed
                .sections
                .iter()
                .any(|section| !section.entries.is_empty()),
        ))
    }

    fn runtime_onboarding_readiness(&self, runtime: &RuntimeSnapshot) -> (bool, bool) {
        let durable_context = runtime_has_active_entries(runtime);
        if !durable_context || !runtime.connected {
            return (durable_context, false);
        }
        let Some(selected_scope) = persisted_selected_scope(&runtime.settings) else {
            return (true, false);
        };
        let inferred_ready = self
            .compose_persisted_scope(&runtime.paths, selected_scope)
            .map(|composed| {
                composed
                    .sections
                    .iter()
                    .any(|section| !section.entries.is_empty())
            })
            .unwrap_or(false);
        (true, inferred_ready)
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
            load_store_stats_read_only(&paths.db_path).ok()
        };
        let review_policy = if connected {
            self.backend.get_review_policy(&paths).map_or_else(
                |error| {
                    notices.push(format!("Unable to load review policy: {error}"));
                    None
                },
                Some,
            )
        } else {
            load_review_policy_read_only(&paths.db_path).ok().flatten()
        };
        let packs = if connected {
            self.backend.list_packs(&paths).unwrap_or_else(|error| {
                notices.push(format!("Unable to load packs: {error}"));
                Vec::new()
            })
        } else {
            Vec::new()
        };
        let active_entries = if connected {
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
        let all_entries = if connected {
            self.backend
                .list_entries(
                    &paths,
                    ExportRequest {
                        project_scope_id: None,
                        task_scope_id: None,
                        scope: None,
                        pack_name: None,
                        include_deleted: true,
                        include_reviews: false,
                        include_runs: false,
                    },
                )
                .unwrap_or_else(|error| {
                    notices.push(format!(
                        "Unable to load archived entries for recovery: {error}"
                    ));
                    active_entries.clone()
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

        let bundles = build_pack_bundles(packs.clone(), active_entries, &reviews);
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
            review_policy,
            pack_records: packs,
            all_entries,
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
    review_policy: Option<CoreReviewPolicy>,
    pack_records: Vec<PackRecord>,
    all_entries: Vec<EntryRecord>,
    bundles: Vec<PackBundle>,
    reviews: Vec<CoreReviewItem>,
    runs: Vec<RunRecord>,
    notices: Vec<String>,
    pack_lookup: HashMap<PackLookupKey, PackBundle>,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct PackLookupKey {
    scope_id: String,
    pack_name: String,
}

pub(crate) trait ContextBackend: Clone + Send + Sync + 'static {
    fn ping(&self, paths: &ContextPaths) -> Result<HealthReport, String>;
    fn stats(&self, paths: &ContextPaths) -> Result<StoreStats, String>;
    fn get_review_policy(&self, paths: &ContextPaths) -> Result<CoreReviewPolicy, String>;
    fn set_review_policy(
        &self,
        paths: &ContextPaths,
        request: SetReviewPolicyRequest,
    ) -> Result<CoreReviewPolicy, String>;
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
    fn delete_entry(
        &self,
        paths: &ContextPaths,
        request: DeleteEntryRequest,
    ) -> Result<EntryRecord, String>;
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
    fn review_edit_and_approve(
        &self,
        paths: &ContextPaths,
        request: ReviewEditAndApproveRequest,
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
    fn preview_source_import(
        &self,
        paths: &ContextPaths,
        request: CoreSourceImportPreviewRequest,
    ) -> Result<CoreSourceImportPreview, String>;
    fn apply_source_import(
        &self,
        paths: &ContextPaths,
        request: CoreSourceImportApplyRequest,
    ) -> Result<CoreSourceImportApplyResult, String>;
    fn create_run(&self, paths: &ContextPaths, request: RunInput) -> Result<RunRecord, String>;
    fn list_runs(&self, paths: &ContextPaths) -> Result<Vec<RunRecord>, String>;
    fn ensure_daemon(&self, paths: &ContextPaths) -> Result<(), String>;
    fn retry_spool(&self, paths: &ContextPaths) -> Result<SpoolRetryReport, String>;
}

#[derive(Clone, Default)]
pub struct LiveContextBackend;

impl LiveContextBackend {
    fn client(&self, paths: &ContextPaths) -> ContextClient {
        ContextClient::new(self.client_config(paths))
    }

    fn probe_client(&self, paths: &ContextPaths) -> ContextClient {
        let mut config = self.client_config(paths);
        config.autostart = false;
        ContextClient::new(config)
    }

    fn client_config(&self, paths: &ContextPaths) -> ClientConfig {
        let mut config = ClientConfig::with_paths(paths.clone());
        config.contextd_bin = discover_binary("contextd", "CONTEXTD_BIN");
        config
    }
}

impl ContextBackend for LiveContextBackend {
    fn ping(&self, paths: &ContextPaths) -> Result<HealthReport, String> {
        self.probe_client(paths)
            .ping()
            .map_err(|error| error.to_string())
    }

    fn stats(&self, paths: &ContextPaths) -> Result<StoreStats, String> {
        self.client(paths)
            .stats()
            .map_err(|error| error.to_string())
    }

    fn get_review_policy(&self, paths: &ContextPaths) -> Result<CoreReviewPolicy, String> {
        self.client(paths)
            .get_review_policy()
            .map_err(|error| error.to_string())
    }

    fn set_review_policy(
        &self,
        paths: &ContextPaths,
        request: SetReviewPolicyRequest,
    ) -> Result<CoreReviewPolicy, String> {
        self.client(paths)
            .set_review_policy(request)
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

    fn delete_entry(
        &self,
        paths: &ContextPaths,
        request: DeleteEntryRequest,
    ) -> Result<EntryRecord, String> {
        self.client(paths)
            .delete_entry(request)
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

    fn review_edit_and_approve(
        &self,
        paths: &ContextPaths,
        request: ReviewEditAndApproveRequest,
    ) -> Result<CoreReviewItem, String> {
        self.client(paths)
            .review_edit_and_approve(request)
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

    fn preview_source_import(
        &self,
        paths: &ContextPaths,
        request: CoreSourceImportPreviewRequest,
    ) -> Result<CoreSourceImportPreview, String> {
        self.client(paths)
            .preview_source_import(request)
            .map_err(|error| error.to_string())
    }

    fn apply_source_import(
        &self,
        paths: &ContextPaths,
        request: CoreSourceImportApplyRequest,
    ) -> Result<CoreSourceImportApplyResult, String> {
        self.client(paths)
            .apply_source_import(request)
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

    fn ensure_daemon(&self, paths: &ContextPaths) -> Result<(), String> {
        self.client(paths)
            .ensure_daemon()
            .map_err(|error| error.to_string())
    }

    fn retry_spool(&self, paths: &ContextPaths) -> Result<SpoolRetryReport, String> {
        self.client(paths)
            .retry_spool()
            .map_err(|error| error.to_string())
    }
}

#[derive(Clone)]
struct LocalSettingsStore {
    base_paths: ContextPaths,
    file_path: PathBuf,
    #[cfg(test)]
    fail_saves: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl LocalSettingsStore {
    fn new(base_paths: ContextPaths) -> Self {
        let file_path = base_paths.data_dir.join(SETTINGS_FILE_NAME);
        Self {
            base_paths,
            file_path,
            #[cfg(test)]
            fail_saves: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
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
        #[cfg(test)]
        if self.fail_saves.load(std::sync::atomic::Ordering::SeqCst) {
            return Err("simulated settings persistence failure".to_string());
        }
        let data = serde_json::to_string_pretty(settings).map_err(|error| error.to_string())?;
        write_private_atomic_file(&self.file_path, &data)?;
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
    #[serde(default)]
    onboarding_complete: Option<bool>,
    #[serde(default)]
    onboarding_completed_at: Option<String>,
    #[serde(default)]
    last_selected_scope_id: Option<String>,
    #[serde(default)]
    last_project_path: Option<String>,
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
            onboarding_complete: None,
            onboarding_completed_at: None,
            last_selected_scope_id: None,
            last_project_path: None,
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
        self.telemetry = false;
        if let Some(scope_id) = self.last_selected_scope_id.clone() {
            self.last_selected_scope_id = decode_scope_id(&scope_id)
                .ok()
                .map(|scope| encode_scope_ref(&scope));
        }
    }

    fn to_public(
        &self,
        review_policy: Option<ReviewPolicy>,
        onboarding: OnboardingState,
    ) -> Settings {
        Settings {
            theme: self.theme.clone(),
            auto_compose: self.auto_compose,
            review_mode: review_policy
                .as_ref()
                .map(|policy| policy.mode)
                .unwrap_or(self.review_mode),
            socket_path: self.socket_path.clone(),
            launch_on_login: self.launch_on_login,
            telemetry: false,
            max_preview_tokens: self.max_preview_tokens,
            review_policy,
            onboarding,
            last_selected_scope_id: self.last_selected_scope_id.clone(),
            last_project_path: self.last_project_path.clone(),
        }
    }

    fn apply_public(&mut self, settings: Settings) {
        self.theme = settings.theme;
        self.auto_compose = settings.auto_compose;
        self.review_mode = settings.review_mode;
        self.socket_path = settings.socket_path;
        self.launch_on_login = settings.launch_on_login;
        self.telemetry = false;
        self.max_preview_tokens = settings.max_preview_tokens.max(1);
        if settings.last_selected_scope_id.is_some() {
            self.last_selected_scope_id = settings.last_selected_scope_id;
        }
        if settings.last_project_path.is_some() {
            self.last_project_path = settings.last_project_path;
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
        pending_review_counts: &HashMap<PackLookupKey, usize>,
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

fn map_context_entries(
    records: &[EntryRecord],
    packs: &[PackRecord],
) -> Result<Vec<ContextEntry>, String> {
    let mut entries = records
        .iter()
        .map(|entry| {
            let pack = packs
                .iter()
                .find(|pack| pack.scope == entry.scope && pack.name == entry.pack_name)
                .ok_or_else(|| format!("Pack not found for entry: {}", entry.id))?;
            Ok(map_context_entry(entry, pack))
        })
        .collect::<Result<Vec<_>, String>>()?;
    entries.sort_by(|left, right| {
        left.scope_id
            .cmp(&right.scope_id)
            .then_with(|| left.pack_name.cmp(&right.pack_name))
            .then_with(|| left.key.cmp(&right.key))
    });
    Ok(entries)
}

fn map_context_entry(entry: &EntryRecord, pack: &PackRecord) -> ContextEntry {
    let (format, body, json_value) = match &entry.value {
        EntryValue::Markdown { body } => (EntryFormat::Markdown, body.clone(), None),
        EntryValue::Json { value } => (
            EntryFormat::Json,
            serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string()),
            Some(value.clone()),
        ),
    };
    ContextEntry {
        id: entry.id.clone(),
        pack_id: pack.id.clone(),
        pack_name: display_name_for_pack(pack),
        pack_key: pack.name.clone(),
        scope_id: encode_scope_ref(&entry.scope),
        scope_kind: map_scope_kind(&entry.scope.kind),
        scope_label: scope_label(&entry.scope),
        key: entry.key.clone(),
        title: entry.title.clone(),
        kind: entry.kind.clone(),
        format,
        body,
        rendered_body: entry.value.render_markdown(),
        json_value,
        tags: entry.tags.clone(),
        locked: entry.locked,
        status: match entry.status {
            CoreEntryStatus::Active => EntryStatus::Active,
            CoreEntryStatus::Deleted => EntryStatus::Deleted,
        },
        provenance: map_provenance(&entry.provenance),
        revision: entry.revision_no,
        created_at: entry.created_at.to_rfc3339(),
        updated_at: entry.updated_at.to_rfc3339(),
    }
}

fn map_provenance(provenance: &Provenance) -> EntryProvenance {
    EntryProvenance {
        actor: provenance.actor.clone(),
        source: provenance.source.clone(),
        source_ref: provenance.source_ref.clone(),
        run_id: provenance.run_id.clone(),
        request_id: provenance.request_id.clone(),
        note: provenance.note.clone(),
    }
}

fn map_entry_format(value: &EntryValue) -> EntryFormat {
    match value {
        EntryValue::Markdown { .. } => EntryFormat::Markdown,
        EntryValue::Json { .. } => EntryFormat::Json,
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
    managed: bool,
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
    let mut entries_by_pack: HashMap<PackLookupKey, Vec<EntryRecord>> = HashMap::new();
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
            let project_scope = raw_project_scope_id(project_scope_id)
                .and_then(|scope_id| ScopeRef::normalized(CoreScopeKind::Project, scope_id).ok())
                .map(|scope| encode_scope_ref(&scope))
                .unwrap_or_else(|| encode_scope_ref(&ScopeRef::global()));
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
            if let Some(scope) = raw_project_scope_id(project_scope_id)
                .and_then(|scope_id| ScopeRef::normalized(CoreScopeKind::Project, scope_id).ok())
            {
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

fn add_persisted_scope(catalog: &mut ScopeCatalog, settings: &LocalSettings) {
    let candidate = settings
        .last_selected_scope_id
        .as_deref()
        .and_then(|scope_id| decode_scope_id(scope_id).ok())
        .or_else(|| {
            settings
                .last_project_path
                .as_deref()
                .and_then(|path| ScopeRef::normalized(CoreScopeKind::Project, path).ok())
        });
    let Some(scope) = candidate else {
        return;
    };
    let encoded_id = encode_scope_ref(&scope);
    catalog
        .entries
        .entry(encoded_id.clone())
        .or_insert_with(|| ScopeCatalogEntry {
            scope: scope.clone(),
            encoded_id,
            label: scope_label(&scope),
            description: scope_description((0, 0, 0), &scope),
            status: "Empty".to_string(),
            parent_project_scope_id: None,
            kind: map_scope_kind(&scope.kind),
        });
}

fn pick_selected_scope_id(catalog: &ScopeCatalog, settings: &LocalSettings) -> String {
    if let Some(scope_id) = settings.last_selected_scope_id.as_deref() {
        if catalog.entries.contains_key(scope_id) {
            return scope_id.to_string();
        }
    }
    if let Some(project_path) = settings.last_project_path.as_deref() {
        if let Ok(scope) = ScopeRef::normalized(CoreScopeKind::Project, project_path) {
            let scope_id = encode_scope_ref(&scope);
            if catalog.entries.contains_key(&scope_id) {
                return scope_id;
            }
        }
    }
    encode_scope_ref(&ScopeRef::global())
}

fn compose_request_for_scope(scope: &ScopeCatalogEntry) -> ComposeRequest {
    ComposeRequest {
        project_scope_id: scope
            .parent_project_scope_id
            .as_deref()
            .and_then(raw_project_scope_id)
            .or_else(|| {
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
    pack_lookup: &HashMap<PackLookupKey, PackBundle>,
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
            let existing_content = review
                .existing_entry
                .as_ref()
                .map(|entry| entry.value.render_markdown());
            let proposed_content = review.proposed_entry.value.render_markdown();
            let provenance = review
                .proposed_entry
                .provenance
                .as_ref()
                .map(map_provenance);
            ReviewItem {
                id: review.id.clone(),
                request_id: review.request_id.clone(),
                pack_id: pack_lookup
                    .get(&lookup_key)
                    .map(|bundle| bundle.id.clone())
                    .unwrap_or_else(|| format!("pending-pack:{}", review.id)),
                pack_name: display_pack_name,
                scope_id,
                scope_kind: map_scope_kind(&review.scope.kind),
                scope_label: scope_label(&review.scope),
                entry_key: review.entry_key.clone(),
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
                age_seconds: Utc::now()
                    .signed_duration_since(review.created_at)
                    .num_seconds()
                    .max(0) as u64,
                risk: review_risk(&review.reason),
                reason: Some(review.reason.clone()),
                diff: review_diff(review),
                diff_sides: ReviewDiff {
                    before: existing_content.clone(),
                    after: proposed_content.clone(),
                    format: map_entry_format(&review.proposed_entry.value),
                    changed: existing_content.as_deref() != Some(proposed_content.as_str()),
                },
                existing_content,
                proposed_content,
                source: provenance
                    .as_ref()
                    .map(|provenance| provenance.source.clone())
                    .unwrap_or_else(|| "unknown".to_string()),
                provenance,
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

fn run_scope_id(run: &RunRecord) -> Option<String> {
    if let Some(task_scope_id) = run.task_scope_id.as_deref() {
        ScopeRef::normalized(CoreScopeKind::Task, task_scope_id)
            .ok()
            .map(|scope| encode_scope_ref(&scope))
    } else {
        run.project_scope_id
            .as_deref()
            .and_then(|project_scope_id| {
                raw_project_scope_id(project_scope_id)
                    .and_then(|scope_id| {
                        ScopeRef::normalized(CoreScopeKind::Project, scope_id).ok()
                    })
                    .map(|scope| encode_scope_ref(&scope))
            })
    }
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

fn latest_active_entry_revision(
    db_path: &Path,
    entry_id: &str,
    before_revision: i64,
) -> Result<Option<i64>, String> {
    let connection = open_read_only(db_path)?;
    let mut statement = connection
        .prepare(
            "SELECT revision_no, snapshot_json FROM revisions WHERE entity_type = 'entry' AND entity_id = ? AND revision_no < ? ORDER BY revision_no DESC",
        )
        .map_err(|error| error.to_string())?;
    let rows = statement
        .query_map(params![entry_id, before_revision], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(|error| error.to_string())?;
    for row in rows {
        let (revision_no, snapshot_json) = row.map_err(|error| error.to_string())?;
        let snapshot: EntryRecord =
            serde_json::from_str(&snapshot_json).map_err(|error| error.to_string())?;
        if snapshot.status == CoreEntryStatus::Active {
            return Ok(Some(revision_no));
        }
    }
    Ok(None)
}

fn open_read_only(path: &Path) -> Result<Connection, String> {
    Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|error| error.to_string())
}

fn load_store_stats_read_only(path: &Path) -> Result<StoreStats, String> {
    let connection = open_read_only(path)?;
    let schema_version = connection
        .query_row(
            "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
            [],
            |row| row.get(0),
        )
        .map_err(|error| error.to_string())?;
    let count = |table: &str| -> Result<usize, String> {
        connection
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get::<_, i64>(0)
            })
            .map(|value| value.max(0) as usize)
            .map_err(|error| error.to_string())
    };
    Ok(StoreStats {
        schema_version,
        packs: count("packs")?,
        entries: count("entries")?,
        reviews: count("review_items")?,
        runs: count("runs")?,
    })
}

fn count_active_entries_read_only(path: &Path) -> Result<usize, String> {
    let connection = open_read_only(path)?;
    connection
        .query_row(
            "SELECT COUNT(*) FROM entries e JOIN packs p ON p.id = e.pack_id WHERE e.status = 'active' AND p.status = 'active'",
            [],
            |row| row.get::<_, i64>(0),
        )
        .map(|count| count.max(0) as usize)
        .map_err(|error| error.to_string())
}

fn load_review_policy_read_only(path: &Path) -> Result<Option<CoreReviewPolicy>, String> {
    let connection = open_read_only(path)?;
    connection
        .query_row(
            "SELECT review_mode, metadata_json, updated_at, updated_by, current_revision_no FROM review_policy WHERE policy_key = 'review'",
            [],
            |row| {
                let mode = match row.get::<_, String>(0)?.as_str() {
                    "strict" => ReviewMode::Strict,
                    "balanced" => ReviewMode::Balanced,
                    "fast" => ReviewMode::Fast,
                    other => {
                        return Err(rusqlite::Error::FromSqlConversionFailure(
                            0,
                            rusqlite::types::Type::Text,
                            Box::new(std::io::Error::new(
                                std::io::ErrorKind::InvalidData,
                                format!("unknown review mode: {other}"),
                            )),
                        ));
                    }
                };
                let metadata = serde_json::from_str(&row.get::<_, String>(1)?).map_err(|error| {
                    rusqlite::Error::FromSqlConversionFailure(
                        1,
                        rusqlite::types::Type::Text,
                        Box::new(error),
                    )
                })?;
                let updated_at_text = row.get::<_, String>(2)?;
                let updated_at = chrono::DateTime::parse_from_rfc3339(&updated_at_text)
                    .map(|value| value.with_timezone(&Utc))
                    .map_err(|error| {
                        rusqlite::Error::FromSqlConversionFailure(
                            2,
                            rusqlite::types::Type::Text,
                            Box::new(error),
                        )
                    })?;
                Ok(CoreReviewPolicy {
                    mode,
                    metadata,
                    updated_at,
                    updated_by: row.get(3)?,
                    revision_no: row.get(4)?,
                })
            },
        )
        .optional()
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
        ReviewReason::StrictPolicy => {
            "Strict review policy requires explicit approval before this change is applied."
                .to_string()
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
        ReviewReason::StrictPolicy => RiskLevel::Low,
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
    pack_lookup: &HashMap<PackLookupKey, PackBundle>,
    key: &PackLookupKey,
) -> Option<String> {
    pack_lookup
        .get(key)
        .map(|bundle| bundle.display_name.clone())
}

fn pack_id_from_lookup(
    pack_lookup: &HashMap<PackLookupKey, PackBundle>,
    key: &PackLookupKey,
) -> Option<String> {
    pack_lookup.get(key).map(|bundle| bundle.id.clone())
}

fn pack_lookup_key(scope_id: &str, pack_name: &str) -> PackLookupKey {
    PackLookupKey {
        scope_id: scope_id.to_string(),
        pack_name: pack_name.to_string(),
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
    if kind == CoreScopeKind::Global && id.trim() != context_core::GLOBAL_SCOPE_ID {
        return Err(format!(
            "Invalid global scope id: expected global:{}",
            context_core::GLOBAL_SCOPE_ID
        ));
    }
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
            managed: object
                .get("managed")
                .and_then(Value::as_bool)
                .unwrap_or(false),
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

fn desktop_entry_managed(metadata: &Value) -> bool {
    metadata
        .get(DESKTOP_METADATA_KEY)
        .and_then(Value::as_object)
        .and_then(|desktop| desktop.get("managed"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

fn unique_entry_key(base: &str, entries: &[EntryRecord]) -> String {
    if entries.iter().all(|entry| entry.key != base) {
        return base.to_string();
    }
    for suffix in 2_u64.. {
        let candidate = format!("{base}-{suffix}");
        if entries.iter().all(|entry| entry.key != candidate) {
            return candidate;
        }
    }
    unreachable!("an unbounded numeric suffix always has an available value")
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

fn raw_project_scope_id(value: &str) -> Option<String> {
    match decode_scope_id(value) {
        Ok(scope) if scope.kind == CoreScopeKind::Project => Some(scope.id),
        Ok(_) => None,
        Err(_) => ScopeRef::normalized(CoreScopeKind::Project, value)
            .ok()
            .map(|scope| scope.id),
    }
}

fn task_scope_id_for(scope: &ScopeRef) -> Option<String> {
    match scope.kind {
        CoreScopeKind::Task => Some(scope.id.clone()),
        _ => None,
    }
}

fn resolve_pack_for_entry(
    packs: &[PackRecord],
    scope: &ScopeRef,
    pack_id: Option<&str>,
    pack_name: Option<&str>,
) -> Result<Option<PackRecord>, String> {
    if let Some(pack_id) = pack_id {
        return packs
            .iter()
            .find(|pack| pack.id == pack_id && &pack.scope == scope)
            .cloned()
            .map(Some)
            .ok_or_else(|| format!("Unknown pack id for selected scope: {pack_id}"));
    }
    let pack_name = pack_name
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .ok_or_else(|| "pack id or pack name is required".to_string())?;
    let matches = packs
        .iter()
        .filter(|pack| {
            &pack.scope == scope
                && (pack.name == pack_name || display_name_for_pack(pack) == pack_name)
        })
        .cloned()
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [pack] => Ok(Some(pack.clone())),
        [] => Ok(None),
        _ => Err(format!(
            "conflict: pack name {pack_name} is ambiguous; select it by id"
        )),
    }
}

fn selector_for_entry(entry: &EntryRecord) -> EntrySelector {
    EntrySelector {
        scope: entry.scope.clone(),
        pack_name: entry.pack_name.clone(),
        entry_key: entry.key.clone(),
    }
}

fn entry_value_from_input(format: &EntryFormat, body: &str) -> Result<EntryValue, String> {
    match format {
        EntryFormat::Markdown => Ok(EntryValue::Markdown {
            body: body.to_string(),
        }),
        EntryFormat::Json => serde_json::from_str(body)
            .map(|value| EntryValue::Json { value })
            .map_err(|error| format!("invalid JSON entry body: {error}")),
    }
}

fn effective_actor(actor: Option<&str>) -> String {
    actor
        .map(str::trim)
        .filter(|actor| !actor.is_empty())
        .unwrap_or(DEFAULT_ACTOR)
        .to_string()
}

fn remember_scope(settings: &mut LocalSettings, scope_id: &str) {
    settings.last_selected_scope_id = Some(scope_id.to_string());
    if let Ok(scope) = decode_scope_id(scope_id) {
        if scope.kind == CoreScopeKind::Project && Path::new(&scope.id).is_absolute() {
            settings.last_project_path = Some(scope.id);
        }
    }
}

fn persisted_selected_scope(settings: &LocalSettings) -> Option<ScopeRef> {
    settings
        .last_selected_scope_id
        .as_deref()
        .and_then(|scope_id| decode_scope_id(scope_id).ok())
        .or_else(|| {
            settings
                .last_project_path
                .as_deref()
                .and_then(|path| ScopeRef::normalized(CoreScopeKind::Project, path).ok())
        })
}

fn runtime_has_active_entries(runtime: &RuntimeSnapshot) -> bool {
    if runtime
        .bundles
        .iter()
        .any(|bundle| !matches!(bundle.status, ViewPackStatus::Draft) && !bundle.entries.is_empty())
    {
        return true;
    }
    !runtime.connected
        && count_active_entries_read_only(&runtime.paths.db_path)
            .map(|count| count > 0)
            .unwrap_or(false)
}

fn onboarding_state(
    settings: &LocalSettings,
    durable_context: bool,
    inferred_ready: bool,
) -> OnboardingState {
    let complete = durable_context && settings.onboarding_complete.unwrap_or(inferred_ready);
    OnboardingState {
        complete,
        inferred: settings.onboarding_complete.is_none(),
        durable_context,
        completed_at: complete
            .then(|| settings.onboarding_completed_at.clone())
            .flatten(),
        last_project_path: settings.last_project_path.clone(),
    }
}

fn map_review_policy(policy: &CoreReviewPolicy) -> ReviewPolicy {
    ReviewPolicy {
        mode: policy.mode,
        metadata: policy.metadata.clone(),
        updated_at: policy.updated_at.to_rfc3339(),
        updated_by: policy.updated_by.clone(),
        revision: policy.revision_no,
    }
}

fn build_privacy_summary(
    paths: &ContextPaths,
    settings_path: &Path,
    stats: Option<&StoreStats>,
    health: Option<&HealthReport>,
    spool_backlog: usize,
) -> PrivacySummary {
    let (counts, counts_available, counts_source) = if let Some(stats) = stats {
        (
            PrivacyDataCounts {
                packs: stats.packs,
                entries: stats.entries,
                reviews: stats.reviews,
                runs: stats.runs,
                spool_backlog,
            },
            true,
            Some("store_stats".to_string()),
        )
    } else if let Some(health) = health {
        (
            PrivacyDataCounts {
                packs: health.packs,
                entries: health.entries,
                reviews: health.reviews,
                runs: health.runs,
                spool_backlog,
            },
            true,
            Some("daemon_health".to_string()),
        )
    } else if let Ok(stats) = load_store_stats_read_only(&paths.db_path) {
        (
            PrivacyDataCounts {
                packs: stats.packs,
                entries: stats.entries,
                reviews: stats.reviews,
                runs: stats.runs,
                spool_backlog,
            },
            true,
            Some("read_only_database".to_string()),
        )
    } else {
        (
            PrivacyDataCounts {
                spool_backlog,
                ..PrivacyDataCounts::default()
            },
            false,
            None,
        )
    };
    PrivacySummary {
        data_path: paths.data_dir.display().to_string(),
        database_path: paths.db_path.display().to_string(),
        socket_path: paths.socket_path.display().to_string(),
        spool_path: paths.spool_dir.display().to_string(),
        settings_path: settings_path.display().to_string(),
        local_only_statement:
            "UCM stores durable context, reviews, revisions, and queued writes on this device."
                .to_string(),
        downstream_adapter_disclosure: "Composed context is disclosed only to locally configured downstream adapter processes; those tools may apply their own network policies.".to_string(),
        secret_scanning_statement: "Core secret scanning rejects recognized credential patterns before durable writes and imports.".to_string(),
        application_encryption_boundary: "Application-level encryption at rest is not enabled; UCM relies on operating-system account and filesystem protections.".to_string(),
        counts,
        counts_available,
        counts_source,
        telemetry_enabled: false,
        network_egress_enabled: false,
    }
}

fn normalize_project_path(path: &str) -> Result<String, String> {
    if path.trim().is_empty() {
        return Err("project path must not be empty".to_string());
    }
    let expanded = expand_user_path(path.trim());
    let canonical = fs::canonicalize(&expanded).map_err(|error| {
        if error.kind() == std::io::ErrorKind::PermissionDenied {
            "permission denied while resolving the project path".to_string()
        } else {
            format!("invalid project path: {error}")
        }
    })?;
    if !canonical.is_dir() {
        return Err("project path must identify a directory".to_string());
    }
    Ok(canonical.display().to_string())
}

fn discover_instruction_sources(root: &Path) -> Result<Vec<DiscoveredInstructionSource>, String> {
    let mut candidates = vec![
        root.join("AGENTS.md"),
        root.join("CLAUDE.md"),
        root.join("CLAUDE.local.md"),
        root.join(".github/copilot-instructions.md"),
        root.join(".cursorrules"),
    ];
    for (directory, suffix) in [
        (root.join(".github/instructions"), ".instructions.md"),
        (root.join(".cursor/rules"), ".mdc"),
        (root.join(".continue/rules"), ".md"),
    ] {
        collect_matching_files(&directory, suffix, 4, &mut candidates);
    }
    candidates.sort();
    candidates.dedup();
    let mut sources = Vec::new();
    for path in candidates {
        let Ok(metadata) = fs::metadata(&path) else {
            continue;
        };
        if !metadata.is_file() {
            continue;
        }
        let relative_path = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .display()
            .to_string();
        sources.push(DiscoveredInstructionSource {
            source_kind: source_kind_for_path(&path),
            readable: fs::File::open(&path).is_ok(),
            path: path.display().to_string(),
            relative_path,
        });
    }
    Ok(sources)
}

fn collect_matching_files(
    directory: &Path,
    suffix: &str,
    remaining_depth: usize,
    output: &mut Vec<PathBuf>,
) {
    if remaining_depth == 0 {
        return;
    }
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_symlink() {
            continue;
        }
        if file_type.is_dir() {
            collect_matching_files(&path, suffix, remaining_depth - 1, output);
        } else if path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.ends_with(suffix))
        {
            output.push(path);
        }
    }
}

fn source_kind_for_path(path: &Path) -> CoreSourceImportKind {
    let normalized = path
        .to_string_lossy()
        .replace('\\', "/")
        .to_ascii_lowercase();
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if file_name == "agents.md" {
        CoreSourceImportKind::AgentsMd
    } else if file_name == "claude.md" || file_name == "claude.local.md" {
        CoreSourceImportKind::ClaudeMd
    } else if file_name == "copilot-instructions.md" || file_name.ends_with(".instructions.md") {
        CoreSourceImportKind::CopilotInstructions
    } else if file_name == ".cursorrules" || normalized.contains("/.cursor/rules/") {
        CoreSourceImportKind::CursorRule
    } else if normalized.contains("/.continue/rules/") {
        CoreSourceImportKind::ContinueRule
    } else {
        CoreSourceImportKind::PlainMarkdown
    }
}

fn reject_bundle_source_kind(source_kind: CoreSourceImportKind) -> Result<(), String> {
    if matches!(
        source_kind,
        CoreSourceImportKind::UcmJson | CoreSourceImportKind::UcmMarkdown
    ) {
        Err("UCM bundles must use the separate bundle preview and apply commands".to_string())
    } else {
        Ok(())
    }
}

fn read_source_documents(
    paths: &[String],
    source_kind: CoreSourceImportKind,
) -> Result<Vec<SourceImportDocument>, String> {
    if paths.is_empty() {
        return Err("source import requires at least one path".to_string());
    }
    if paths.len() > 32 {
        return Err("source import supports at most 32 files at once".to_string());
    }
    let mut documents = Vec::with_capacity(paths.len());
    for path in paths {
        let expanded = expand_user_path(path);
        let canonical = canonical_regular_file(&expanded)?;
        if source_kind == CoreSourceImportKind::Auto && !auto_source_path_supported(&canonical) {
            if canonical
                .extension()
                .and_then(|extension| extension.to_str())
                == Some("json")
            {
                return Err(
                    "UCM JSON bundles must use the separate bundle import workflow".to_string(),
                );
            }
            return Err(format!(
                "unsupported instruction source: {}; choose an explicit source type for generic Markdown",
                canonical.display()
            ));
        }
        let metadata = fs::metadata(&canonical).map_err(|error| error.to_string())?;
        if metadata.len() == 0 {
            return Err(format!(
                "source import file is empty: {}",
                canonical.display()
            ));
        }
        if metadata.len() > MAX_IMPORT_FILE_BYTES {
            return Err(format!(
                "source import file exceeds the {} byte limit: {}",
                MAX_IMPORT_FILE_BYTES,
                canonical.display()
            ));
        }
        let payload = fs::read_to_string(&canonical).map_err(|error| {
            if error.kind() == std::io::ErrorKind::InvalidData {
                format!(
                    "unsupported non-text instruction source: {}",
                    canonical.display()
                )
            } else {
                error.to_string()
            }
        })?;
        if payload.trim().is_empty() {
            return Err(format!(
                "source import file is empty: {}",
                canonical.display()
            ));
        }
        documents.push(SourceImportDocument {
            path: Some(canonical.display().to_string()),
            payload,
        });
    }
    Ok(documents)
}

fn canonical_regular_file(path: &Path) -> Result<PathBuf, String> {
    let source_metadata = fs::symlink_metadata(path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::PermissionDenied {
            "permission denied while inspecting the selected file".to_string()
        } else {
            format!("invalid import path: {error}")
        }
    })?;
    if source_metadata.file_type().is_symlink() {
        return Err("import paths must not be symbolic links".to_string());
    }
    let canonical = fs::canonicalize(path).map_err(|error| error.to_string())?;
    let metadata = fs::metadata(&canonical).map_err(|error| error.to_string())?;
    if !metadata.is_file() {
        return Err(format!(
            "import path is not a regular file: {}",
            canonical.display()
        ));
    }
    Ok(canonical)
}

fn auto_source_path_supported(path: &Path) -> bool {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    file_name == ".cursorrules" || file_name.ends_with(".md") || file_name.ends_with(".mdc")
}

#[derive(Serialize)]
struct SourceImportFileChecksum<'a> {
    version: u8,
    requested_source_kind: CoreSourceImportKind,
    documents: &'a [SourceImportDocument],
}

fn source_import_file_checksum(
    documents: &[SourceImportDocument],
    source_kind: CoreSourceImportKind,
) -> Result<String, String> {
    let token = SourceImportFileChecksum {
        version: 1,
        requested_source_kind: source_kind,
        documents,
    };
    let serialized = serde_json::to_vec(&token).map_err(|error| error.to_string())?;
    let mut hasher = Sha256::new();
    hasher.update(b"ucm.desktop.source-import-files");
    hasher.update((serialized.len() as u64).to_be_bytes());
    hasher.update(serialized);
    Ok(format!("{:x}", hasher.finalize()))
}

fn ensure_instruction_preview(preview: &CoreSourceImportPreview) -> Result<(), String> {
    if preview.candidates.iter().any(|candidate| {
        matches!(
            candidate.detected_source_kind,
            CoreSourceImportKind::UcmJson | CoreSourceImportKind::UcmMarkdown
        )
    }) {
        Err("a UCM bundle was detected; use the separate bundle import workflow".to_string())
    } else {
        Ok(())
    }
}

fn map_source_import_preview(
    preview_id: String,
    preview_fingerprint: String,
    apply_grant_token: String,
    destination_scope_id: &str,
    preview: CoreSourceImportPreview,
) -> SourceImportPreviewResult {
    let conflicts = preview
        .candidates
        .iter()
        .filter(|candidate| {
            candidate.disposition == context_core::SourceImportDisposition::Conflict
        })
        .count();
    let duplicates = preview
        .candidates
        .iter()
        .filter(|candidate| {
            candidate.disposition == context_core::SourceImportDisposition::Duplicate
        })
        .count();
    SourceImportPreviewResult {
        preview_id,
        preview_fingerprint,
        apply_grant_token,
        destination_scope_id: destination_scope_id.to_string(),
        pack_name: preview.pack_name,
        review_mode: preview.review_mode,
        candidates: preview
            .candidates
            .into_iter()
            .map(|candidate| SourceImportCandidate {
                candidate_index: candidate.candidate_index,
                document_index: candidate.document_index,
                source_path: candidate.source_path,
                detected_source_kind: candidate.detected_source_kind,
                key: candidate.entry.key,
                title: candidate.entry.title,
                kind: candidate.entry.kind,
                format: map_entry_format(&candidate.entry.value),
                rendered_body: candidate.entry.value.render_markdown(),
                tags: candidate.entry.tags,
                locked: candidate.entry.locked,
                provenance: candidate.entry.provenance.as_ref().map(map_provenance),
                disposition: candidate.disposition,
                existing_entry_id: candidate.existing_entry_id,
                existing_revision: candidate.existing_revision_no,
                warnings: candidate.warnings,
            })
            .collect(),
        conflicts,
        duplicates,
        warnings: preview.warnings,
        apply_allowed: preview.apply_allowed,
    }
}

fn map_source_import_apply(
    destination_scope_id: String,
    pack_name: String,
    result: CoreSourceImportApplyResult,
) -> SourceImportApplyResult {
    SourceImportApplyResult {
        request_id: result.request_id,
        destination_scope_id: destination_scope_id.clone(),
        pack_name,
        navigation_scope_id: destination_scope_id,
        candidate_count: result.candidate_count,
        imported_count: result.imported_count,
        applied_count: result.applied_count,
        pending_count: result.pending_count,
        skipped_count: result.skipped_count,
        rejected_count: result.rejected_count,
        items: result
            .items
            .into_iter()
            .map(|item| SourceImportApplyItem {
                candidate_index: item.candidate_index,
                document_index: item.document_index,
                source_path: item.source_path,
                entry_key: item.entry_key,
                disposition: item.disposition,
                reason: item.reason,
                entry_id: item.entry_id,
                review_id: item.review_id,
            })
            .collect(),
        affected_entry_ids: result.affected_entry_ids,
        affected_review_ids: result.affected_review_ids,
        affected_entry_keys: result.affected_entry_keys,
    }
}

struct VerifiedBundleFile {
    preview: BundleImportPreview,
    payload: String,
    import_format: ImportFormat,
}

fn load_verified_bundle(path: String) -> Result<VerifiedBundleFile, String> {
    let expanded = canonical_regular_file(&expand_user_path(&path))?;
    let metadata = fs::metadata(&expanded).map_err(|error| error.to_string())?;
    if metadata.len() == 0 {
        return Err("source import file is empty".to_string());
    }
    if metadata.len() > MAX_IMPORT_FILE_BYTES {
        return Err(format!(
            "source import file exceeds the {} byte limit",
            MAX_IMPORT_FILE_BYTES
        ));
    }
    let payload = fs::read_to_string(&expanded).map_err(|error| error.to_string())?;
    context_core::secret::reject_if_secret(&payload).map_err(|error| error.to_string())?;
    let (format, bundle) = parse_bundle_payload(&expanded, &payload)?;
    let mut scope_ids = bundle
        .packs
        .iter()
        .map(|pack| encode_scope_ref(&pack.scope))
        .chain(
            bundle
                .entries
                .iter()
                .map(|entry| encode_scope_ref(&entry.scope)),
        )
        .collect::<Vec<_>>();
    scope_ids.sort();
    scope_ids.dedup();
    let import_format = match &format {
        BundleFormat::UcmJson => ImportFormat::Json,
        BundleFormat::UcmMarkdown => ImportFormat::Markdown,
    };
    Ok(VerifiedBundleFile {
        preview: BundleImportPreview {
            path,
            apply_grant_token: String::new(),
            format,
            valid: true,
            file_size_bytes: metadata.len(),
            checksum_sha256: sha256_hex(payload.as_bytes()),
            exported_at: bundle.exported_at.to_rfc3339(),
            pack_count: bundle.packs.len(),
            entry_count: bundle.entries.len(),
            review_count: bundle.reviews.len(),
            run_count: bundle.runs.len(),
            scope_ids,
            warnings: if bundle.entries.is_empty() {
                vec!["The bundle contains no context entries.".to_string()]
            } else {
                Vec::new()
            },
            requires_confirmation: true,
        },
        payload,
        import_format,
    })
}

fn parse_bundle_payload(
    path: &Path,
    payload: &str,
) -> Result<(BundleFormat, ContextExportBundle), String> {
    if payload.trim().is_empty() {
        return Err("UCM bundle is empty".to_string());
    }
    if path.extension().and_then(|extension| extension.to_str()) == Some("json")
        || payload.trim_start().starts_with('{')
    {
        return serde_json::from_str(payload)
            .map(|bundle| (BundleFormat::UcmJson, bundle))
            .map_err(|_| "invalid UCM JSON bundle".to_string());
    }
    if !payload.contains("<!-- UCM_ENTRY") {
        return Err("unsupported import: the selected file is not a UCM bundle".to_string());
    }
    context_core::markdown::import_markdown(payload)
        .map(|bundle| (BundleFormat::UcmMarkdown, bundle))
        .map_err(|_| "invalid UCM Markdown bundle".to_string())
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

pub(crate) fn desktop_error(message: String) -> DesktopError {
    let normalized = message.to_ascii_lowercase();
    let (code, friendly_message, retryable) = if normalized.contains("path grant is required") {
        (DesktopErrorCode::PathGrantRequired, message, false)
    } else if normalized.contains("path grant has expired") {
        (DesktopErrorCode::PathGrantExpired, message, false)
    } else if normalized.contains("path grant") {
        (DesktopErrorCode::PathGrantInvalid, message, false)
    } else if normalized.contains("secret rejected") || normalized.contains("secret detected") {
        (
            DesktopErrorCode::SecretDetected,
            "Potential secret detected. Remove credentials or tokens before saving.".to_string(),
            false,
        )
    } else if normalized.contains("permission denied") {
        (
            DesktopErrorCode::PermissionDenied,
            "The selected local path is not accessible with current permissions.".to_string(),
            false,
        )
    } else if normalized.contains("confirmation is required") {
        (DesktopErrorCode::ConfirmationRequired, message, false)
    } else if normalized.contains("conflict") || normalized.contains("changed after") {
        (DesktopErrorCode::Conflict, message, false)
    } else if normalized.contains("not found") || normalized.contains("unknown ") {
        (DesktopErrorCode::NotFound, message, false)
    } else if normalized.contains("import")
        || normalized.contains("bundle")
        || normalized.contains("unsupported")
    {
        (DesktopErrorCode::InvalidImport, message, false)
    } else if normalized.contains("unavailable")
        || normalized.contains("transport")
        || normalized.contains("socket")
        || normalized.contains("timeout")
    {
        (DesktopErrorCode::Unavailable, message, true)
    } else if normalized.contains("incompatible") || normalized.contains("newer") {
        (DesktopErrorCode::Incompatible, message, false)
    } else if normalized.contains("invalid")
        || normalized.contains("must ")
        || normalized.contains("requires ")
    {
        (DesktopErrorCode::InvalidInput, message, false)
    } else {
        (DesktopErrorCode::Internal, message, false)
    };
    DesktopError {
        code,
        message: friendly_message,
        retryable,
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

fn write_private_atomic_file(path: &Path, contents: &str) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| "settings path requires a parent directory".to_string())?;
    fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| "settings path requires a valid file name".to_string())?;
    let temporary_path = parent.join(format!(".{file_name}.{}.tmp", context_core::new_id()));
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(&temporary_path)
        .map_err(|error| error.to_string())?;
    let write_result = (|| -> std::io::Result<()> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(fs::Permissions::from_mode(0o600))?;
        }
        file.write_all(contents.as_bytes())?;
        file.flush()?;
        file.sync_all()
    })();
    drop(file);
    if let Err(error) = write_result {
        let _ = fs::remove_file(&temporary_path);
        return Err(error.to_string());
    }
    if let Err(error) = fs::rename(&temporary_path, path) {
        let _ = fs::remove_file(&temporary_path);
        return Err(error.to_string());
    }
    sync_parent_directory(parent)
}

#[cfg(unix)]
fn sync_parent_directory(parent: &Path) -> Result<(), String> {
    let directory = fs::File::open(parent).map_err(|error| {
        format!(
            "settings file was atomically replaced with a complete value, but opening the parent directory for durability sync failed: {error}; the new value may already be visible"
        )
    })?;
    directory.sync_all().map_err(|error| {
        format!(
            "settings file was atomically replaced with a complete value, but parent-directory durability sync failed: {error}; the new value may already be visible"
        )
    })
}

#[cfg(not(unix))]
fn sync_parent_directory(_parent: &Path) -> Result<(), String> {
    // Rust does not expose a portable parent-directory fsync operation.
    Ok(())
}

fn expand_user_path(value: &str) -> PathBuf {
    if let Some(stripped) = value.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(stripped);
        }
    }
    PathBuf::from(value)
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
    use std::sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Barrier, Condvar, Mutex,
    };
    use std::time::Duration;

    use context_core::ContextStore;
    use tempfile::tempdir;

    #[derive(Default)]
    struct PolicyGateState {
        failure_entered: bool,
        success_entered: bool,
    }

    #[derive(Clone)]
    struct PolicyConcurrencyGate {
        failing_mode: ReviewMode,
        state: Arc<(Mutex<PolicyGateState>, Condvar)>,
    }

    impl PolicyConcurrencyGate {
        fn new(failing_mode: ReviewMode) -> Self {
            Self {
                failing_mode,
                state: Arc::new((Mutex::new(PolicyGateState::default()), Condvar::new())),
            }
        }

        fn wait_for_failure(&self) {
            let (lock, condition) = &*self.state;
            let mut state = lock.lock().expect("policy gate");
            while !state.failure_entered {
                state = condition.wait(state).expect("policy gate wait");
            }
        }

        fn before_policy_call(&self, mode: ReviewMode) -> Result<(), String> {
            let (lock, condition) = &*self.state;
            let mut state = lock
                .lock()
                .map_err(|_| "policy gate poisoned".to_string())?;
            if mode == self.failing_mode {
                state.failure_entered = true;
                condition.notify_all();
                let (_state, _) = condition
                    .wait_timeout_while(state, Duration::from_secs(1), |state| {
                        !state.success_entered
                    })
                    .map_err(|_| "policy gate poisoned".to_string())?;
                Err("simulated concurrent policy failure".to_string())
            } else {
                state.success_entered = true;
                condition.notify_all();
                Ok(())
            }
        }
    }

    #[derive(Clone)]
    struct PutEntryGate {
        entered: Arc<Barrier>,
        release: Arc<Barrier>,
    }

    impl PutEntryGate {
        fn new() -> Self {
            Self {
                entered: Arc::new(Barrier::new(2)),
                release: Arc::new(Barrier::new(2)),
            }
        }

        fn block(&self) {
            self.entered.wait();
            self.release.wait();
        }

        fn wait_until_blocked(&self) {
            self.entered.wait();
        }

        fn release(&self) {
            self.release.wait();
        }
    }

    #[derive(Clone)]
    struct StoreBackend {
        store: Arc<ContextStore>,
        list_packs_error: Option<String>,
        stats_error: Option<String>,
        set_review_policy_error: Option<String>,
        update_pack_errors: BTreeSet<String>,
        health_override: Option<HealthReport>,
        ensure_daemon_calls: Arc<AtomicUsize>,
        policy_concurrency_gate: Option<PolicyConcurrencyGate>,
        put_entry_gate: Option<PutEntryGate>,
    }

    impl StoreBackend {
        fn new(path: &Path) -> Self {
            Self {
                store: Arc::new(ContextStore::open(path).expect("open store")),
                list_packs_error: None,
                stats_error: None,
                set_review_policy_error: None,
                update_pack_errors: BTreeSet::new(),
                health_override: None,
                ensure_daemon_calls: Arc::new(AtomicUsize::new(0)),
                policy_concurrency_gate: None,
                put_entry_gate: None,
            }
        }

        fn with_list_packs_error(mut self, error: &str) -> Self {
            self.list_packs_error = Some(error.to_string());
            self
        }

        fn with_stats_error(mut self, error: &str) -> Self {
            self.stats_error = Some(error.to_string());
            self
        }

        fn with_set_review_policy_error(mut self, error: &str) -> Self {
            self.set_review_policy_error = Some(error.to_string());
            self
        }

        fn with_update_pack_error(mut self, pack_name: String) -> Self {
            self.update_pack_errors.insert(pack_name);
            self
        }

        fn with_health_override(mut self, health: HealthReport) -> Self {
            self.health_override = Some(health);
            self
        }

        fn with_policy_concurrency_gate(mut self, gate: PolicyConcurrencyGate) -> Self {
            self.policy_concurrency_gate = Some(gate);
            self
        }

        fn with_put_entry_gate(mut self, gate: PutEntryGate) -> Self {
            self.put_entry_gate = Some(gate);
            self
        }
    }

    impl ContextBackend for StoreBackend {
        fn ping(&self, _paths: &ContextPaths) -> Result<HealthReport, String> {
            if let Some(health) = &self.health_override {
                return Ok(health.clone());
            }
            self.store.health().map_err(|error| error.to_string())
        }

        fn stats(&self, _paths: &ContextPaths) -> Result<StoreStats, String> {
            if let Some(error) = &self.stats_error {
                return Err(error.clone());
            }
            self.store.stats().map_err(|error| error.to_string())
        }

        fn get_review_policy(&self, _paths: &ContextPaths) -> Result<CoreReviewPolicy, String> {
            self.store
                .get_review_policy()
                .map_err(|error| error.to_string())
        }

        fn set_review_policy(
            &self,
            _paths: &ContextPaths,
            request: SetReviewPolicyRequest,
        ) -> Result<CoreReviewPolicy, String> {
            if let Some(gate) = &self.policy_concurrency_gate {
                gate.before_policy_call(request.mode)?;
            }
            if let Some(error) = &self.set_review_policy_error {
                return Err(error.clone());
            }
            self.store
                .set_review_policy(request)
                .map_err(|error| error.to_string())
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
            if self.update_pack_errors.contains(&request.selector.name) {
                return Err(format!(
                    "simulated update failure for {}",
                    request.selector.name
                ));
            }
            self.store
                .update_pack(request)
                .map_err(|error| error.to_string())
        }

        fn list_packs(&self, _paths: &ContextPaths) -> Result<Vec<PackRecord>, String> {
            if let Some(error) = &self.list_packs_error {
                return Err(error.clone());
            }
            self.store.list_packs().map_err(|error| error.to_string())
        }

        fn put_entry(
            &self,
            _paths: &ContextPaths,
            request: PutEntryRequest,
        ) -> Result<EntryRecord, String> {
            if let Some(gate) = &self.put_entry_gate {
                gate.block();
            }
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

        fn delete_entry(
            &self,
            _paths: &ContextPaths,
            request: DeleteEntryRequest,
        ) -> Result<EntryRecord, String> {
            self.store
                .delete_entry(request)
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

        fn review_edit_and_approve(
            &self,
            _paths: &ContextPaths,
            request: ReviewEditAndApproveRequest,
        ) -> Result<CoreReviewItem, String> {
            self.store
                .review_edit_and_approve(request)
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

        fn preview_source_import(
            &self,
            _paths: &ContextPaths,
            request: CoreSourceImportPreviewRequest,
        ) -> Result<CoreSourceImportPreview, String> {
            self.store
                .preview_source_import(request)
                .map_err(|error| error.to_string())
        }

        fn apply_source_import(
            &self,
            _paths: &ContextPaths,
            request: CoreSourceImportApplyRequest,
        ) -> Result<CoreSourceImportApplyResult, String> {
            self.store
                .apply_source_import(request)
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

        fn ensure_daemon(&self, _paths: &ContextPaths) -> Result<(), String> {
            self.ensure_daemon_calls.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        fn retry_spool(&self, _paths: &ContextPaths) -> Result<SpoolRetryReport, String> {
            Ok(SpoolRetryReport::default())
        }
    }

    fn test_paths(root: &Path, name: &str) -> ContextPaths {
        let data_dir = root.join(name);
        ContextPaths {
            db_path: data_dir.join("context.db"),
            socket_path: data_dir.join("contextd.sock"),
            spool_dir: data_dir.join("spool"),
            data_dir,
        }
    }

    fn grant<B: ContextBackend>(
        service: &DesktopContextService<B>,
        purpose: PathGrantPurpose,
        paths: Vec<PathBuf>,
    ) -> String {
        service
            .issue_path_grant(purpose, paths)
            .expect("issue path grant")
            .grant_token
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
                review_policy: None,
                onboarding: OnboardingState::default(),
                last_selected_scope_id: None,
                last_project_path: None,
            })
            .expect("save settings");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&settings_store.file_path)
                    .expect("settings metadata")
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }

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
        let entry = restarted
            .list_entries(Some("project:atlas".to_string()), Some(saved.id.clone()))
            .expect("list searchable entry")
            .into_iter()
            .find(|entry| entry.body.contains("daemon persistence"))
            .expect("searchable entry");
        let hit = results
            .iter()
            .find(|result| result.kind == SearchKind::Entry)
            .expect("typed entry search hit");
        assert_eq!(hit.id, entry.id);
        assert_eq!(hit.target.scope_id.as_deref(), Some("project:atlas"));
        assert_eq!(hit.target.pack_id.as_deref(), Some(saved.id.as_str()));
        assert_eq!(hit.target.entry_id.as_deref(), Some(entry.id.as_str()));
    }

    #[test]
    fn policy_mutations_never_commit_before_local_settings_persist() {
        let temp = tempdir().expect("tempdir");
        let paths = test_paths(temp.path(), "policy-order-home");
        let backend = StoreBackend::new(&paths.db_path);
        let settings_store = LocalSettingsStore::new(paths.clone());
        settings_store
            .fail_saves
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let service = DesktopContextService::with_backend(backend.clone(), settings_store.clone());
        assert!(service
            .set_review_policy(SetReviewPolicyInput {
                mode: ReviewMode::Strict,
                actor: "policy-test".to_string(),
                note: None,
                request_id: None,
            })
            .is_err());
        assert_eq!(
            backend.store.get_review_policy().expect("policy").mode,
            ReviewMode::Balanced
        );

        assert!(service
            .save_settings(Settings {
                theme: ThemeMode::Dark,
                auto_compose: true,
                review_mode: ReviewMode::Fast,
                socket_path: paths.socket_path.display().to_string(),
                launch_on_login: false,
                telemetry: false,
                max_preview_tokens: 1400,
                review_policy: None,
                onboarding: OnboardingState::default(),
                last_selected_scope_id: None,
                last_project_path: None,
            })
            .is_err());
        assert_eq!(
            backend.store.get_review_policy().expect("policy").mode,
            ReviewMode::Balanced
        );

        settings_store
            .fail_saves
            .store(false, std::sync::atomic::Ordering::SeqCst);
        let failing_backend = backend.with_set_review_policy_error("core policy unavailable");
        let rollback_service =
            DesktopContextService::with_backend(failing_backend, settings_store.clone());
        assert!(rollback_service
            .set_review_policy(SetReviewPolicyInput {
                mode: ReviewMode::Strict,
                actor: "policy-test".to_string(),
                note: None,
                request_id: None,
            })
            .is_err());
        assert_eq!(
            settings_store
                .load()
                .expect("rolled back settings")
                .review_mode,
            ReviewMode::Balanced
        );
    }

    #[test]
    fn atomic_settings_save_never_exposes_partial_json_to_concurrent_readers() {
        let temp = tempdir().expect("tempdir");
        let paths = test_paths(temp.path(), "atomic-settings-home");
        let store = LocalSettingsStore::new(paths.clone());
        let socket_a = paths.data_dir.join("socket-a.sock").display().to_string();
        let socket_b = paths.data_dir.join("socket-b.sock").display().to_string();
        let mut initial = LocalSettings::default_for(&paths);
        initial.theme = ThemeMode::Light;
        initial.socket_path = socket_a.clone();
        initial.max_preview_tokens = 111;
        store.save(&initial).expect("initial settings");

        let done = Arc::new(AtomicBool::new(false));
        let start = Arc::new(Barrier::new(5));
        let writer_store = store.clone();
        let writer_done = done.clone();
        let writer_start = start.clone();
        let writer_paths = paths.clone();
        let writer_socket_a = socket_a.clone();
        let writer_socket_b = socket_b.clone();
        let writer = std::thread::spawn(move || -> Result<(), String> {
            writer_start.wait();
            for index in 0..100 {
                let mut settings = LocalSettings::default_for(&writer_paths);
                if index % 2 == 0 {
                    settings.theme = ThemeMode::Light;
                    settings.socket_path = writer_socket_a.clone();
                    settings.max_preview_tokens = 111;
                } else {
                    settings.theme = ThemeMode::Dark;
                    settings.socket_path = writer_socket_b.clone();
                    settings.max_preview_tokens = 222;
                }
                writer_store.save(&settings)?;
            }
            writer_done.store(true, Ordering::SeqCst);
            Ok(())
        });

        let readers = (0..4)
            .map(|_| {
                let reader_store = store.clone();
                let reader_done = done.clone();
                let reader_start = start.clone();
                let reader_socket_a = socket_a.clone();
                let reader_socket_b = socket_b.clone();
                std::thread::spawn(move || -> Result<(), String> {
                    reader_start.wait();
                    for iteration in 0..5_000 {
                        let settings = reader_store.load()?;
                        let valid_a = settings.socket_path == reader_socket_a
                            && settings.max_preview_tokens == 111
                            && matches!(settings.theme, ThemeMode::Light);
                        let valid_b = settings.socket_path == reader_socket_b
                            && settings.max_preview_tokens == 222
                            && matches!(settings.theme, ThemeMode::Dark);
                        if !valid_a && !valid_b {
                            return Err(format!(
                                "observed partial settings value: {} / {}",
                                settings.socket_path, settings.max_preview_tokens
                            ));
                        }
                        if iteration > 100 && reader_done.load(Ordering::SeqCst) {
                            break;
                        }
                    }
                    Ok(())
                })
            })
            .collect::<Vec<_>>();
        writer
            .join()
            .expect("writer thread")
            .expect("writer result");
        for reader in readers {
            reader
                .join()
                .expect("reader thread")
                .expect("reader result");
        }
        let temporary_files = fs::read_dir(&paths.data_dir)
            .expect("settings directory")
            .filter_map(Result::ok)
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".desktop-settings.json.")
            })
            .count();
        assert_eq!(temporary_files, 0);
    }

    #[test]
    fn atomic_settings_save_removes_temp_file_when_rename_fails() {
        let temp = tempdir().expect("tempdir");
        let paths = test_paths(temp.path(), "atomic-settings-failure-home");
        fs::create_dir_all(&paths.data_dir).expect("settings parent");
        let store = LocalSettingsStore::new(paths.clone());
        fs::create_dir(&store.file_path).expect("directory blocks settings rename");
        let settings = LocalSettings::default_for(&paths);
        assert!(store.save(&settings).is_err());
        let temporary_files = fs::read_dir(&paths.data_dir)
            .expect("settings directory")
            .filter_map(Result::ok)
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".desktop-settings.json.")
            })
            .count();
        assert_eq!(temporary_files, 0);
    }

    #[cfg(unix)]
    #[test]
    fn parent_directory_sync_success_and_failure_are_explicit() {
        let temp = tempdir().expect("tempdir");
        sync_parent_directory(temp.path()).expect("directory sync");
        let error = sync_parent_directory(&temp.path().join("missing-directory"))
            .expect_err("missing parent sync must fail");
        assert!(error.contains("atomically replaced with a complete value"));
        assert!(error.contains("new value may already be visible"));
    }

    #[test]
    fn concurrent_failed_policy_update_cannot_restore_stale_local_settings() {
        let temp = tempdir().expect("tempdir");
        let paths = test_paths(temp.path(), "concurrent-policy-home");
        let backend = StoreBackend::new(&paths.db_path);
        let gate = PolicyConcurrencyGate::new(ReviewMode::Strict);
        let settings_store = LocalSettingsStore::new(paths);
        let service = DesktopContextService::with_backend(
            backend.clone().with_policy_concurrency_gate(gate.clone()),
            settings_store.clone(),
        );

        let failing_service = service.clone();
        let failing = std::thread::spawn(move || {
            failing_service.set_review_policy(SetReviewPolicyInput {
                mode: ReviewMode::Strict,
                actor: "failing-update".to_string(),
                note: None,
                request_id: None,
            })
        });
        gate.wait_for_failure();

        let started = Arc::new(Barrier::new(2));
        let succeeding_service = service.clone();
        let succeeding_started = started.clone();
        let succeeding = std::thread::spawn(move || {
            succeeding_started.wait();
            succeeding_service.set_review_policy(SetReviewPolicyInput {
                mode: ReviewMode::Fast,
                actor: "successful-update".to_string(),
                note: None,
                request_id: None,
            })
        });
        started.wait();

        assert!(failing.join().expect("failing thread").is_err());
        let successful_policy = succeeding
            .join()
            .expect("successful thread")
            .expect("successful policy update");
        assert_eq!(successful_policy.mode, ReviewMode::Fast);
        assert_eq!(
            settings_store
                .load()
                .expect("final local settings")
                .review_mode,
            ReviewMode::Fast
        );
        assert_eq!(
            backend
                .store
                .get_review_policy()
                .expect("final core policy")
                .mode,
            ReviewMode::Fast
        );
    }

    #[test]
    fn failed_policy_rollback_cannot_overwrite_concurrent_adapter_toggle() {
        let temp = tempdir().expect("tempdir");
        let paths = test_paths(temp.path(), "concurrent-adapter-home");
        let backend = StoreBackend::new(&paths.db_path);
        let gate = PolicyConcurrencyGate::new(ReviewMode::Strict);
        let settings_store = LocalSettingsStore::new(paths);
        let service = DesktopContextService::with_backend(
            backend.with_policy_concurrency_gate(gate.clone()),
            settings_store.clone(),
        );
        let failing_service = service.clone();
        let failing = std::thread::spawn(move || {
            failing_service.set_review_policy(SetReviewPolicyInput {
                mode: ReviewMode::Strict,
                actor: "failing-update".to_string(),
                note: None,
                request_id: None,
            })
        });
        gate.wait_for_failure();

        let started = Arc::new(Barrier::new(2));
        let toggle_service = service.clone();
        let toggle_started = started.clone();
        let toggle = std::thread::spawn(move || {
            toggle_started.wait();
            toggle_service.toggle_adapter(ADAPTER_CODEX.to_string(), false)
        });
        started.wait();
        assert!(failing.join().expect("failing thread").is_err());
        toggle
            .join()
            .expect("toggle thread")
            .expect("adapter toggle");
        assert_eq!(
            settings_store
                .load()
                .expect("final settings")
                .adapter_enabled
                .get(ADAPTER_CODEX),
            Some(&false)
        );
    }

    #[test]
    fn failed_policy_rollback_cannot_overwrite_scope_or_onboarding_changes() {
        let temp = tempdir().expect("tempdir");
        let paths = test_paths(temp.path(), "concurrent-scope-home");
        let backend = StoreBackend::new(&paths.db_path);
        let gate = PolicyConcurrencyGate::new(ReviewMode::Strict);
        let settings_store = LocalSettingsStore::new(paths);
        let service = DesktopContextService::with_backend(
            backend.with_policy_concurrency_gate(gate.clone()),
            settings_store.clone(),
        );
        let failing_service = service.clone();
        let failing = std::thread::spawn(move || {
            failing_service.set_review_policy(SetReviewPolicyInput {
                mode: ReviewMode::Strict,
                actor: "failing-update".to_string(),
                note: None,
                request_id: None,
            })
        });
        gate.wait_for_failure();

        let started = Arc::new(Barrier::new(2));
        let update_service = service.clone();
        let update_started = started.clone();
        let update = std::thread::spawn(move || {
            update_started.wait();
            update_service.set_selected_scope("project:concurrent".to_string(), None)?;
            update_service.reset_onboarding()
        });
        started.wait();
        assert!(failing.join().expect("failing thread").is_err());
        update
            .join()
            .expect("settings update thread")
            .expect("scope and onboarding update");
        let final_settings = settings_store.load().expect("final settings");
        assert_eq!(
            final_settings.last_selected_scope_id.as_deref(),
            Some("project:concurrent")
        );
        assert_eq!(final_settings.onboarding_complete, Some(false));
    }

    #[test]
    fn save_pack_scope_update_preserves_concurrent_socket_path_change() {
        let temp = tempdir().expect("tempdir");
        let paths = test_paths(temp.path(), "socket-race-home");
        let gate = PutEntryGate::new();
        let settings_store = LocalSettingsStore::new(paths.clone());
        let service = DesktopContextService::with_backend(
            StoreBackend::new(&paths.db_path).with_put_entry_gate(gate.clone()),
            settings_store.clone(),
        );
        let save_service = service.clone();
        let save = std::thread::spawn(move || {
            save_service.save_pack(SavePackInput {
                id: None,
                scope_id: "project:socket-race".to_string(),
                name: "Socket race".to_string(),
                status: ViewPackStatus::Active,
                summary: "Concurrent socket update".to_string(),
                tags: Vec::new(),
                body: "Save pack while settings change.".to_string(),
            })
        });
        gate.wait_until_blocked();

        let updated_socket = temp.path().join("new-socket/contextd.sock");
        service
            .save_settings(Settings {
                theme: ThemeMode::System,
                auto_compose: true,
                review_mode: ReviewMode::Balanced,
                socket_path: updated_socket.display().to_string(),
                launch_on_login: false,
                telemetry: false,
                max_preview_tokens: 1400,
                review_policy: None,
                onboarding: OnboardingState::default(),
                last_selected_scope_id: None,
                last_project_path: None,
            })
            .expect("save concurrent socket path");
        gate.release();
        save.join().expect("save pack thread").expect("save pack");

        let final_settings = settings_store.load().expect("final settings");
        assert_eq!(
            final_settings.socket_path,
            updated_socket.display().to_string()
        );
        assert_eq!(
            final_settings.last_selected_scope_id.as_deref(),
            Some("project:socket-race")
        );
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
        let revision_hit = service
            .search_index("Initial draft body".to_string())
            .expect("search revisions")
            .into_iter()
            .find(|result| result.kind == SearchKind::Revision)
            .expect("revision search target");
        assert_eq!(
            revision_hit.target.pack_id.as_deref(),
            Some(draft.id.as_str())
        );
        assert_eq!(
            revision_hit.target.revision_id.as_deref(),
            Some(revision_hit.id.as_str())
        );

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
    fn structured_pack_lookup_keys_do_not_collide_on_legal_delimiters() {
        assert_eq!(
            format!("{}::{}", "project:a", "b::c"),
            format!("{}::{}", "project:a::b", "c")
        );
        let left = pack_lookup_key("project:a", "b::c");
        let right = pack_lookup_key("project:a::b", "c");
        assert_ne!(left, right);
        let mut lookup = HashMap::new();
        lookup.insert(left, 1);
        lookup.insert(right, 2);
        assert_eq!(lookup.len(), 2);
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
    fn entry_crud_is_id_safe_and_soft_delete_is_reversible() {
        let temp = tempdir().expect("tempdir");
        let paths = test_paths(temp.path(), "entry-home");
        let service = DesktopContextService::with_backend(
            StoreBackend::new(&paths.db_path),
            LocalSettingsStore::new(paths),
        );
        let pack = service
            .save_pack(SavePackInput {
                id: None,
                scope_id: "project:atlas".to_string(),
                name: "Atlas".to_string(),
                status: ViewPackStatus::Active,
                summary: "Primary context".to_string(),
                tags: Vec::new(),
                body: "Keep the primary entry.".to_string(),
            })
            .expect("save pack");
        let saved = service
            .save_entry(SaveEntryInput {
                id: None,
                scope_id: pack.scope_id.clone(),
                pack_id: Some(pack.id.clone()),
                pack_name: None,
                key: "handoff".to_string(),
                title: Some("Handoff".to_string()),
                kind: "handoff_note".to_string(),
                format: EntryFormat::Json,
                body: "{\"status\":\"ready\"}".to_string(),
                tags: vec!["handoff".to_string()],
                locked: false,
                actor: Some("desktop-test".to_string()),
                note: Some("Created in test".to_string()),
            })
            .expect("save entry");
        assert_eq!(saved.pack_id, pack.id);
        assert_eq!(saved.format, EntryFormat::Json);
        assert!(saved.rendered_body.contains("\"status\": \"ready\""));
        assert!(service
            .save_entry(SaveEntryInput {
                id: Some(saved.id.clone()),
                scope_id: "project:other".to_string(),
                pack_id: Some(pack.id.clone()),
                pack_name: None,
                key: saved.key.clone(),
                title: saved.title.clone(),
                kind: saved.kind.clone(),
                format: saved.format.clone(),
                body: saved.body.clone(),
                tags: saved.tags.clone(),
                locked: saved.locked,
                actor: None,
                note: None,
            })
            .is_err());

        let archived = service
            .archive_entry(saved.id.clone())
            .expect("archive entry");
        assert_eq!(archived.status, EntryStatus::Deleted);
        assert!(service
            .list_entries(Some(pack.scope_id.clone()), Some(pack.id.clone()))
            .expect("list entries including archived")
            .iter()
            .any(|entry| entry.id == saved.id && entry.status == EntryStatus::Deleted));

        let restored = service
            .revert_entry_revision(RevertEntryInput {
                entry_id: saved.id.clone(),
                revision: None,
                actor: Some("desktop-test".to_string()),
            })
            .expect("restore entry");
        assert_eq!(restored.id, saved.id);
        assert_eq!(restored.status, EntryStatus::Active);
        assert!(service
            .list_entries(Some(pack.scope_id), Some(pack.id))
            .expect("list restored entries")
            .iter()
            .any(|entry| entry.id == saved.id));
        let dashboard = service.load_dashboard().expect("dashboard entries");
        assert!(dashboard.entries.iter().any(|entry| entry.id == saved.id));
    }

    #[test]
    fn fresh_project_save_entry_creates_default_pack_and_completes_onboarding() {
        let temp = tempdir().expect("tempdir");
        let paths = test_paths(temp.path(), "fresh-entry-home");
        let service = DesktopContextService::with_backend(
            StoreBackend::new(&paths.db_path),
            LocalSettingsStore::new(paths),
        );
        let saved = service
            .save_entry(SaveEntryInput {
                id: None,
                scope_id: "project:fresh".to_string(),
                pack_id: None,
                pack_name: None,
                key: "first-note".to_string(),
                title: Some("First note".to_string()),
                kind: "context_note".to_string(),
                format: EntryFormat::Markdown,
                body: "The first durable project entry.".to_string(),
                tags: vec!["manual".to_string()],
                locked: false,
                actor: Some("desktop-test".to_string()),
                note: None,
            })
            .expect("save first project entry");
        assert_eq!(saved.pack_key, context_core::DEFAULT_PACK_NAME);
        assert_eq!(saved.scope_id, "project:fresh");

        let packs = service
            .list_packs(Some("project:fresh".to_string()))
            .expect("list fresh project packs");
        assert_eq!(packs.len(), 1);
        assert_eq!(packs[0].id, saved.pack_id);
        let entries = service
            .list_entries(
                Some("project:fresh".to_string()),
                Some(saved.pack_id.clone()),
            )
            .expect("list fresh project entries");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].id, saved.id);
        assert!(service.complete_onboarding().expect("complete").complete);
    }

    #[test]
    fn archived_entry_remains_discoverable_after_refresh_and_can_be_restored() {
        let temp = tempdir().expect("tempdir");
        let paths = test_paths(temp.path(), "archive-refresh-home");
        let settings_store = LocalSettingsStore::new(paths.clone());
        let service = DesktopContextService::with_backend(
            StoreBackend::new(&paths.db_path),
            settings_store.clone(),
        );
        let saved = service
            .save_entry(SaveEntryInput {
                id: None,
                scope_id: "project:recovery".to_string(),
                pack_id: None,
                pack_name: Some("Recovery Notes".to_string()),
                key: "recoverable".to_string(),
                title: Some("Recoverable note".to_string()),
                kind: "context_note".to_string(),
                format: EntryFormat::Markdown,
                body: "This archived entry must remain discoverable.".to_string(),
                tags: vec!["recovery".to_string()],
                locked: false,
                actor: None,
                note: None,
            })
            .expect("save recoverable entry");
        service
            .archive_entry(saved.id.clone())
            .expect("archive recoverable entry");

        let refreshed =
            DesktopContextService::with_backend(StoreBackend::new(&paths.db_path), settings_store);
        let dashboard = refreshed.load_dashboard().expect("refreshed dashboard");
        let archived = dashboard
            .entries
            .iter()
            .find(|entry| entry.id == saved.id)
            .expect("archived entry remains visible");
        assert_eq!(archived.status, EntryStatus::Deleted);
        assert_eq!(archived.pack_id, saved.pack_id);
        assert_eq!(archived.pack_name, "Recovery Notes");
        assert!(refreshed
            .list_entries(
                Some("project:recovery".to_string()),
                Some(saved.pack_id.clone()),
            )
            .expect("list archived entry")
            .iter()
            .any(|entry| entry.id == saved.id && entry.status == EntryStatus::Deleted));

        let preview = refreshed
            .compose_effective_context(ComposeContextInput {
                scope_id: "project:recovery".to_string(),
                destination_adapter: None,
            })
            .expect("compose without archived entry");
        assert!(preview.included_entries.is_empty());
        assert!(!preview.rendered_markdown.contains(&saved.body));

        refreshed
            .restore_entry(saved.id.clone())
            .expect("restore archived entry");
        let repeated_restore = refreshed
            .restore_entry(saved.id.clone())
            .expect_err("active entry cannot be restored again");
        assert_eq!(
            repeated_restore,
            "conflict: restore_entry requires an entry whose current status is deleted"
        );
        let restored_dashboard = refreshed.load_dashboard().expect("restored dashboard");
        assert!(restored_dashboard
            .entries
            .iter()
            .any(|entry| entry.id == saved.id && entry.status == EntryStatus::Active));
    }

    #[test]
    fn imported_deleted_first_revision_can_be_safely_undeleted() {
        let temp = tempdir().expect("tempdir");
        let paths = test_paths(temp.path(), "imported-deleted-home");
        let backend = StoreBackend::new(&paths.db_path);
        let scope =
            ScopeRef::normalized(CoreScopeKind::Project, "imported-deleted").expect("scope");
        let now = Utc::now();
        backend
            .store
            .import_bundle(
                ContextExportBundle {
                    exported_at: now,
                    packs: vec![PackRecord {
                        id: "imported-pack".to_string(),
                        scope: scope.clone(),
                        name: "main".to_string(),
                        description: Some("Imported pack".to_string()),
                        metadata: json!({"desktop": {"displayName": "Imported pack"}}),
                        status: CorePackStatus::Active,
                        locked: false,
                        lock_reason: None,
                        created_at: now,
                        updated_at: now,
                        revision_no: 1,
                    }],
                    entries: vec![EntryRecord {
                        id: "imported-deleted-entry".to_string(),
                        scope,
                        pack_name: "main".to_string(),
                        key: "deleted-first".to_string(),
                        title: Some("Deleted first revision".to_string()),
                        kind: "context_note".to_string(),
                        value: EntryValue::Markdown {
                            body: "Restore this imported snapshot.".to_string(),
                        },
                        tags: vec!["imported".to_string()],
                        metadata: json!({"preserved": true}),
                        provenance: Provenance {
                            actor: "importer".to_string(),
                            source: "bundle_import".to_string(),
                            source_ref: Some("bundle:one".to_string()),
                            run_id: Some("run-import".to_string()),
                            request_id: Some("request-import".to_string()),
                            note: Some("Original import note".to_string()),
                        },
                        locked: true,
                        status: CoreEntryStatus::Deleted,
                        created_at: now,
                        updated_at: now,
                        revision_no: 1,
                    }],
                    reviews: Vec::new(),
                    runs: Vec::new(),
                },
                "import-test",
            )
            .expect("import deleted revision one");
        let service = DesktopContextService::with_backend(backend, LocalSettingsStore::new(paths));
        let imported_entry_id = service
            .list_entries(Some("project:imported-deleted".to_string()), None)
            .expect("list imported deleted entry")
            .into_iter()
            .find(|entry| entry.key == "deleted-first")
            .expect("imported deleted entry")
            .id;
        let restored = service
            .restore_entry(imported_entry_id.clone())
            .expect("undelete imported entry");
        assert_eq!(restored.status, EntryStatus::Active);
        assert_eq!(restored.body, "Restore this imported snapshot.");
        assert_eq!(restored.tags, vec!["imported"]);
        assert!(restored.locked);
        assert_eq!(restored.provenance.source, "restore");
        assert_eq!(
            restored.provenance.source_ref.as_deref(),
            Some("bundle:one")
        );
        assert_eq!(restored.provenance.run_id.as_deref(), Some("run-import"));
        assert_eq!(
            restored.provenance.request_id.as_deref(),
            Some("request-import")
        );
        assert!(restored
            .provenance
            .note
            .as_deref()
            .is_some_and(|note| note.contains("Original import note")));
        assert!(service.restore_entry(imported_entry_id).is_err());
    }

    #[test]
    fn pack_editor_does_not_overwrite_agent_created_entries() {
        let temp = tempdir().expect("tempdir");
        let paths = test_paths(temp.path(), "agent-pack-home");
        let backend = StoreBackend::new(&paths.db_path);
        let scope = ScopeRef::normalized(CoreScopeKind::Project, "atlas").expect("project scope");
        let pack = backend
            .store
            .create_pack(CreatePackRequest {
                scope: scope.clone(),
                name: "agent-pack".to_string(),
                description: Some("Agent managed".to_string()),
                metadata: json!({}),
                locked: false,
                lock_reason: None,
                actor: "agent".to_string(),
            })
            .expect("create agent pack");
        let original = backend
            .store
            .put_entry(PutEntryRequest {
                scope,
                pack_name: pack.name.clone(),
                entry: EntryInput {
                    key: "body".to_string(),
                    title: Some("Agent instructions".to_string()),
                    kind: "instruction".to_string(),
                    value: EntryValue::Markdown {
                        body: "Do not replace this agent entry.".to_string(),
                    },
                    tags: vec!["agent".to_string()],
                    metadata: json!({}),
                    locked: false,
                    provenance: Some(Provenance::system("agent", "agent.commit")),
                },
                actor: "agent".to_string(),
            })
            .expect("create agent entry");
        let service =
            DesktopContextService::with_backend(backend, LocalSettingsStore::new(paths.clone()));
        assert!(service
            .save_pack(SavePackInput {
                id: Some("missing-pack".to_string()),
                scope_id: "project:atlas".to_string(),
                name: "Missing".to_string(),
                status: ViewPackStatus::Active,
                summary: String::new(),
                tags: Vec::new(),
                body: String::new(),
            })
            .is_err());
        service
            .save_pack(SavePackInput {
                id: Some(pack.id),
                scope_id: "project:atlas".to_string(),
                name: "Agent pack edited".to_string(),
                status: ViewPackStatus::Active,
                summary: "Desktop summary".to_string(),
                tags: vec!["desktop".to_string()],
                body: "Desktop-authored body.".to_string(),
            })
            .expect("edit agent pack");

        let entries = service
            .backend
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
            .expect("list entries");
        let preserved = entries
            .iter()
            .find(|entry| entry.id == original.id)
            .expect("original entry preserved");
        assert_eq!(
            preserved.value.render_markdown(),
            "Do not replace this agent entry."
        );
        assert!(entries.iter().any(|entry| {
            entry.key.starts_with("desktop-body")
                && entry.value.render_markdown() == "Desktop-authored body."
        }));
    }

    #[test]
    fn legacy_multi_entry_pack_gets_dedicated_body_without_reusing_managed_sibling() {
        let temp = tempdir().expect("tempdir");
        let paths = test_paths(temp.path(), "legacy-multi-entry-home");
        let backend = StoreBackend::new(&paths.db_path);
        let scope = ScopeRef::normalized(CoreScopeKind::Project, "legacy").expect("scope");
        let pack = backend
            .store
            .create_pack(CreatePackRequest {
                scope: scope.clone(),
                name: "legacy-pack".to_string(),
                description: Some("Legacy desktop pack".to_string()),
                metadata: json!({
                    "desktop": {
                        "managed": true,
                        "displayName": "Legacy pack"
                    }
                }),
                locked: false,
                lock_reason: None,
                actor: "desktop".to_string(),
            })
            .expect("create legacy pack");
        for (key, body) in [("alpha", "Alpha sibling"), ("beta", "Beta sibling")] {
            backend
                .store
                .put_entry(PutEntryRequest {
                    scope: scope.clone(),
                    pack_name: pack.name.clone(),
                    entry: EntryInput {
                        key: key.to_string(),
                        title: None,
                        kind: "context_note".to_string(),
                        value: EntryValue::Markdown {
                            body: body.to_string(),
                        },
                        tags: Vec::new(),
                        metadata: update_desktop_entry_metadata(json!({})),
                        locked: false,
                        provenance: Some(Provenance::system("desktop", "legacy")),
                    },
                    actor: "desktop".to_string(),
                })
                .expect("create sibling entry");
        }
        let service = DesktopContextService::with_backend(
            backend.clone(),
            LocalSettingsStore::new(paths.clone()),
        );
        service
            .save_pack(SavePackInput {
                id: Some(pack.id.clone()),
                scope_id: "project:legacy".to_string(),
                name: "Legacy pack".to_string(),
                status: ViewPackStatus::Active,
                summary: "Updated safely".to_string(),
                tags: Vec::new(),
                body: "Dedicated primary body".to_string(),
            })
            .expect("save legacy pack");

        let entries = backend
            .store
            .list_entries(ExportRequest {
                project_scope_id: None,
                task_scope_id: None,
                scope: Some(scope),
                pack_name: Some(pack.name.clone()),
                include_deleted: false,
                include_reviews: false,
                include_runs: false,
            })
            .expect("list legacy entries");
        assert_eq!(entries.len(), 3);
        assert_eq!(
            entries
                .iter()
                .find(|entry| entry.key == "alpha")
                .expect("alpha")
                .value
                .render_markdown(),
            "Alpha sibling"
        );
        assert_eq!(
            entries
                .iter()
                .find(|entry| entry.key == "beta")
                .expect("beta")
                .value
                .render_markdown(),
            "Beta sibling"
        );
        assert!(entries.iter().any(|entry| {
            entry.key == "desktop-body" && entry.value.render_markdown() == "Dedicated primary body"
        }));
        let updated_pack = backend
            .store
            .list_packs()
            .expect("list packs")
            .into_iter()
            .find(|candidate| candidate.id == pack.id)
            .expect("updated pack");
        assert_eq!(
            desktop_pack_metadata(&updated_pack.metadata)
                .primary_entry_key
                .as_deref(),
            Some("desktop-body")
        );
    }

    #[test]
    fn deleted_explicit_primary_is_not_resurrected_by_save_pack() {
        let temp = tempdir().expect("tempdir");
        let paths = test_paths(temp.path(), "deleted-primary-home");
        let service = DesktopContextService::with_backend(
            StoreBackend::new(&paths.db_path),
            LocalSettingsStore::new(paths),
        );
        let pack = service
            .save_pack(SavePackInput {
                id: None,
                scope_id: "project:deleted-primary".to_string(),
                name: "Deleted primary".to_string(),
                status: ViewPackStatus::Active,
                summary: "Initial".to_string(),
                tags: Vec::new(),
                body: "Former primary body".to_string(),
            })
            .expect("create pack");
        let primary = service
            .list_entries(
                Some("project:deleted-primary".to_string()),
                Some(pack.id.clone()),
            )
            .expect("list primary")
            .into_iter()
            .find(|entry| entry.key == DEFAULT_ENTRY_KEY)
            .expect("body entry");
        service
            .archive_entry(primary.id.clone())
            .expect("archive primary");
        let updated = service
            .save_pack(SavePackInput {
                id: Some(pack.id.clone()),
                scope_id: "project:deleted-primary".to_string(),
                name: "Deleted primary".to_string(),
                status: ViewPackStatus::Active,
                summary: "Updated".to_string(),
                tags: Vec::new(),
                body: "New dedicated body".to_string(),
            })
            .expect("save after primary deletion");
        assert_eq!(updated.body, "New dedicated body");
        let entries = service
            .list_entries(Some("project:deleted-primary".to_string()), Some(pack.id))
            .expect("list updated entries");
        assert!(entries
            .iter()
            .any(|entry| { entry.id == primary.id && entry.status == EntryStatus::Deleted }));
        assert!(entries.iter().any(|entry| {
            entry.key == "desktop-body"
                && entry.status == EntryStatus::Active
                && entry.body == "New dedicated body"
        }));
    }

    #[test]
    fn effective_context_exposes_exact_core_rendering_and_entry_metadata() {
        let temp = tempdir().expect("tempdir");
        let paths = test_paths(temp.path(), "compose-home");
        let backend = StoreBackend::new(&paths.db_path);
        let service =
            DesktopContextService::with_backend(backend.clone(), LocalSettingsStore::new(paths));
        service
            .save_pack(SavePackInput {
                id: None,
                scope_id: "project:atlas".to_string(),
                name: "Compose".to_string(),
                status: ViewPackStatus::Active,
                summary: "Compose test".to_string(),
                tags: vec!["compose".to_string()],
                body: "Use the exact rendered Markdown.".to_string(),
            })
            .expect("save compose pack");
        let core = backend
            .store
            .compose_context(ComposeRequest {
                project_scope_id: Some("atlas".to_string()),
                task_scope_id: None,
                include_archived: false,
            })
            .expect("core compose");
        let preview = service
            .compose_effective_context(ComposeContextInput {
                scope_id: "project:atlas".to_string(),
                destination_adapter: Some("codex".to_string()),
            })
            .expect("desktop compose");
        assert_eq!(preview.destination_adapter, "codex");
        assert_eq!(preview.rendered_markdown, core.rendered_markdown);
        assert_eq!(
            preview.metrics.estimated_tokens,
            core.metrics.estimated_tokens
        );
        assert_eq!(preview.total_tokens as usize, core.metrics.estimated_tokens);
        assert_eq!(
            preview.included_entries.len(),
            core.metrics.included_entries
        );
        assert!(preview
            .included_entries
            .iter()
            .all(|entry| entry.revision > 0 && !entry.provenance.source.is_empty()));
    }

    #[test]
    fn task_composition_and_review_runs_use_raw_parent_project_scope_ids() {
        let temp = tempdir().expect("tempdir");
        let paths = test_paths(temp.path(), "task-inheritance-home");
        let backend = StoreBackend::new(&paths.db_path);
        let service =
            DesktopContextService::with_backend(backend.clone(), LocalSettingsStore::new(paths));
        service
            .save_pack(SavePackInput {
                id: None,
                scope_id: "project:atlas".to_string(),
                name: "Project context".to_string(),
                status: ViewPackStatus::Active,
                summary: "Project inheritance".to_string(),
                tags: Vec::new(),
                body: "Inherited project guidance.".to_string(),
            })
            .expect("save project context");
        let task_scope = ScopeRef::normalized(CoreScopeKind::Task, "issue-1").expect("task scope");
        let task_pack = backend
            .store
            .create_pack(CreatePackRequest {
                scope: task_scope.clone(),
                name: "task-context".to_string(),
                description: Some("Task context".to_string()),
                metadata: update_desktop_pack_metadata(
                    json!({}),
                    "Task context",
                    "Task context",
                    &ViewPackStatus::Active,
                    DEFAULT_ENTRY_KEY,
                    Some("project:atlas"),
                ),
                locked: false,
                lock_reason: None,
                actor: "desktop".to_string(),
            })
            .expect("create task pack");
        backend
            .store
            .put_entry(PutEntryRequest {
                scope: task_scope.clone(),
                pack_name: task_pack.name,
                entry: EntryInput {
                    key: DEFAULT_ENTRY_KEY.to_string(),
                    title: None,
                    kind: "context_note".to_string(),
                    value: EntryValue::Markdown {
                        body: "Task-specific guidance.".to_string(),
                    },
                    tags: Vec::new(),
                    metadata: update_desktop_entry_metadata(json!({})),
                    locked: false,
                    provenance: Some(Provenance::system("desktop", "test")),
                },
                actor: "desktop".to_string(),
            })
            .expect("create task entry");

        let preview = service
            .compose_effective_context(ComposeContextInput {
                scope_id: "task:issue-1".to_string(),
                destination_adapter: None,
            })
            .expect("compose task context");
        assert!(preview
            .included_entries
            .iter()
            .any(|entry| entry.scope_id == "project:atlas"));
        assert!(preview
            .included_entries
            .iter()
            .any(|entry| entry.scope_id == "task:issue-1"));
        let runtime = service.runtime_snapshot().expect("runtime");
        let catalog = build_scope_catalog(&runtime.bundles, &runtime.reviews, &runtime.runs);
        let task = catalog.entries.get("task:issue-1").expect("task catalog");
        assert_eq!(
            compose_request_for_scope(task).project_scope_id.as_deref(),
            Some("atlas")
        );

        service
            .set_review_policy(SetReviewPolicyInput {
                mode: ReviewMode::Strict,
                actor: "reviewer".to_string(),
                note: None,
                request_id: None,
            })
            .expect("strict policy");
        let commit = backend
            .store
            .commit_work(context_core::CommitWorkRequest {
                request_id: "task-review-request".to_string(),
                actor: "agent".to_string(),
                run: None,
                proposals: vec![context_core::CommitProposal {
                    scope: task_scope,
                    pack_name: "task-context".to_string(),
                    entry: EntryInput {
                        key: "reviewed-task-entry".to_string(),
                        title: None,
                        kind: "context_note".to_string(),
                        value: EntryValue::Markdown {
                            body: "Reviewed task update.".to_string(),
                        },
                        tags: Vec::new(),
                        metadata: json!({}),
                        locked: false,
                        provenance: Some(Provenance::system("agent", "test")),
                    },
                }],
            })
            .expect("create task review");
        service
            .bulk_review_decision(BulkReviewDecisionInput {
                item_ids: vec![commit.items[0].review_id.clone().expect("review id")],
                decision: ViewReviewDecision::Approve,
                confirmation: false,
                edited_content: None,
                actor: Some("reviewer".to_string()),
                note: None,
            })
            .expect("approve task review");
        let review_run = backend
            .store
            .list_runs()
            .expect("list runs")
            .into_iter()
            .find(|run| run.source == "desktop.review")
            .expect("desktop review run");
        assert_eq!(review_run.project_scope_id.as_deref(), Some("atlas"));
        assert_ne!(
            review_run.project_scope_id.as_deref(),
            Some("project:atlas")
        );
    }

    #[test]
    fn staged_instruction_import_previews_without_writing_then_applies() {
        let temp = tempdir().expect("tempdir");
        let paths = test_paths(temp.path(), "import-home");
        let project = temp.path().join("project");
        fs::create_dir_all(&project).expect("project directory");
        let source = project.join("AGENTS.md");
        write_text_file(&source, "# AGENTS.md\n\nRun focused Rust tests.")
            .expect("write instruction source");
        let service = DesktopContextService::with_backend(
            StoreBackend::new(&paths.db_path),
            LocalSettingsStore::new(paths),
        );
        let scope_id = format!(
            "project:{}",
            fs::canonicalize(&project)
                .expect("canonical project")
                .display()
        );
        let preview_grant = grant(
            &service,
            PathGrantPurpose::SourceImportPreview,
            vec![source.clone()],
        );
        let preview = service
            .preview_source_import(SourceImportPreviewInput {
                paths: vec![source.display().to_string()],
                grant_token: Some(preview_grant),
                destination_scope_id: scope_id.clone(),
                pack_name: Some("instructions".to_string()),
                source_kind: CoreSourceImportKind::Auto,
                actor: Some("import-test".to_string()),
            })
            .expect("preview source import");
        assert_eq!(preview.candidates.len(), 1);
        assert_eq!(
            preview.candidates[0].detected_source_kind,
            CoreSourceImportKind::AgentsMd
        );
        assert!(!preview.preview_fingerprint.is_empty());
        assert!(service
            .list_entries(Some(scope_id.clone()), None)
            .expect("entries before apply")
            .is_empty());
        let missing_fingerprint = service
            .apply_source_import(SourceImportApplyInput {
                paths: vec![source.display().to_string()],
                grant_token: Some(preview.apply_grant_token.clone()),
                destination_scope_id: scope_id.clone(),
                pack_name: Some("instructions".to_string()),
                source_kind: CoreSourceImportKind::Auto,
                actor: Some("import-test".to_string()),
                preview_id: preview.preview_id.clone(),
                expected_preview_fingerprint: None,
                confirmation: true,
            })
            .expect_err("authoritative fingerprint is required");
        assert_eq!(
            missing_fingerprint,
            "source import apply requires expectedPreviewFingerprint from preview"
        );

        let applied = service
            .apply_source_import(SourceImportApplyInput {
                paths: vec![source.display().to_string()],
                grant_token: Some(preview.apply_grant_token),
                destination_scope_id: scope_id.clone(),
                pack_name: Some("instructions".to_string()),
                source_kind: CoreSourceImportKind::Auto,
                actor: Some("import-test".to_string()),
                preview_id: preview.preview_id,
                expected_preview_fingerprint: Some(preview.preview_fingerprint),
                confirmation: true,
            })
            .expect("apply source import");
        assert_eq!(applied.navigation_scope_id, scope_id);
        assert_eq!(applied.applied_count, 1);
        assert_eq!(
            service
                .list_entries(Some(applied.navigation_scope_id), None)
                .expect("entries after apply")
                .len(),
            1
        );
    }

    #[test]
    fn source_import_settings_failure_occurs_before_core_mutation() {
        let temp = tempdir().expect("tempdir");
        let paths = test_paths(temp.path(), "settings-failure-import-home");
        let source = temp.path().join("AGENTS.md");
        write_text_file(&source, "# AGENTS.md\n\nDo not duplicate this import.")
            .expect("write source");
        let backend = StoreBackend::new(&paths.db_path);
        let settings_store = LocalSettingsStore::new(paths);
        let service = DesktopContextService::with_backend(backend.clone(), settings_store.clone());
        let preview_grant = grant(
            &service,
            PathGrantPurpose::SourceImportPreview,
            vec![source.clone()],
        );
        let preview = service
            .preview_source_import(SourceImportPreviewInput {
                paths: vec![source.display().to_string()],
                grant_token: Some(preview_grant),
                destination_scope_id: "project:settings-failure".to_string(),
                pack_name: Some("instructions".to_string()),
                source_kind: CoreSourceImportKind::Auto,
                actor: Some("import-test".to_string()),
            })
            .expect("preview import");
        settings_store
            .fail_saves
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let error = service
            .apply_source_import(SourceImportApplyInput {
                paths: vec![source.display().to_string()],
                grant_token: Some(preview.apply_grant_token),
                destination_scope_id: "project:settings-failure".to_string(),
                pack_name: Some("instructions".to_string()),
                source_kind: CoreSourceImportKind::Auto,
                actor: Some("import-test".to_string()),
                preview_id: preview.preview_id,
                expected_preview_fingerprint: Some(preview.preview_fingerprint),
                confirmation: true,
            })
            .expect_err("settings failure precedes commit");
        assert_eq!(error, "simulated settings persistence failure");
        assert_eq!(backend.store.stats().expect("stats").entries, 0);
        assert!(backend
            .store
            .review_list(Some(ReviewState::Pending))
            .expect("reviews")
            .is_empty());
    }

    #[test]
    fn source_import_preview_is_invalidated_by_review_policy_changes() {
        let temp = tempdir().expect("tempdir");
        let paths = test_paths(temp.path(), "policy-import-home");
        let source = temp.path().join("AGENTS.md");
        write_text_file(&source, "# AGENTS.md\n\nKeep imports reviewable.")
            .expect("write instruction source");
        let service = DesktopContextService::with_backend(
            StoreBackend::new(&paths.db_path),
            LocalSettingsStore::new(paths),
        );
        let preview_grant = grant(
            &service,
            PathGrantPurpose::SourceImportPreview,
            vec![source.clone()],
        );
        let preview = service
            .preview_source_import(SourceImportPreviewInput {
                paths: vec![source.display().to_string()],
                grant_token: Some(preview_grant),
                destination_scope_id: "project:policy-import".to_string(),
                pack_name: Some("instructions".to_string()),
                source_kind: CoreSourceImportKind::Auto,
                actor: Some("import-test".to_string()),
            })
            .expect("preview source import");
        assert_eq!(preview.review_mode, ReviewMode::Balanced);
        service
            .set_review_policy(SetReviewPolicyInput {
                mode: ReviewMode::Strict,
                actor: "policy-test".to_string(),
                note: Some("Change import semantics".to_string()),
                request_id: None,
            })
            .expect("change review policy");

        let error = service
            .apply_source_import(SourceImportApplyInput {
                paths: vec![source.display().to_string()],
                grant_token: Some(preview.apply_grant_token),
                destination_scope_id: "project:policy-import".to_string(),
                pack_name: Some("instructions".to_string()),
                source_kind: CoreSourceImportKind::Auto,
                actor: Some("import-test".to_string()),
                preview_id: preview.preview_id,
                expected_preview_fingerprint: Some(preview.preview_fingerprint),
                confirmation: true,
            })
            .expect_err("policy change invalidates preview");
        assert_eq!(
            error,
            "conflict: source import preview fingerprint no longer matches authoritative state; preview again"
        );
        assert!(service
            .list_entries(Some("project:policy-import".to_string()), None)
            .expect("no import applied")
            .is_empty());
    }

    #[test]
    fn source_import_file_checksums_use_unambiguous_serialized_boundaries() {
        let left = vec![SourceImportDocument {
            path: Some("a".to_string()),
            payload: "bc".to_string(),
        }];
        let right = vec![SourceImportDocument {
            path: Some("ab".to_string()),
            payload: "c".to_string(),
        }];
        assert_eq!(
            format!(
                "{}{}",
                left[0].path.as_deref().unwrap_or_default(),
                left[0].payload
            ),
            format!(
                "{}{}",
                right[0].path.as_deref().unwrap_or_default(),
                right[0].payload
            )
        );
        let left_id =
            source_import_file_checksum(&left, CoreSourceImportKind::Auto).expect("left checksum");
        let right_id = source_import_file_checksum(&right, CoreSourceImportKind::Auto)
            .expect("right checksum");
        assert_ne!(left_id, right_id);
        assert_ne!(
            left_id,
            source_import_file_checksum(&left, CoreSourceImportKind::PlainMarkdown)
                .expect("source-kind-bound checksum")
        );
    }

    #[test]
    fn source_import_preview_is_invalidated_by_destination_state_changes() {
        let temp = tempdir().expect("tempdir");
        let paths = test_paths(temp.path(), "state-import-home");
        let source = temp.path().join("AGENTS.md");
        write_text_file(&source, "# AGENTS.md\n\nReviewed import content.")
            .expect("write instruction source");
        let backend = StoreBackend::new(&paths.db_path);
        let service =
            DesktopContextService::with_backend(backend.clone(), LocalSettingsStore::new(paths));
        let preview_grant = grant(
            &service,
            PathGrantPurpose::SourceImportPreview,
            vec![source.clone()],
        );
        let preview = service
            .preview_source_import(SourceImportPreviewInput {
                paths: vec![source.display().to_string()],
                grant_token: Some(preview_grant),
                destination_scope_id: "project:state-import".to_string(),
                pack_name: Some("instructions".to_string()),
                source_kind: CoreSourceImportKind::Auto,
                actor: Some("import-test".to_string()),
            })
            .expect("preview source import");
        assert_eq!(
            preview.candidates[0].disposition,
            context_core::SourceImportDisposition::New
        );
        let candidate_key = preview.candidates[0].key.clone();
        backend
            .store
            .put_entry(PutEntryRequest {
                scope: ScopeRef::normalized(CoreScopeKind::Project, "state-import")
                    .expect("destination scope"),
                pack_name: "instructions".to_string(),
                entry: EntryInput {
                    key: candidate_key.clone(),
                    title: None,
                    kind: "instruction".to_string(),
                    value: EntryValue::Markdown {
                        body: "Concurrent destination content.".to_string(),
                    },
                    tags: Vec::new(),
                    metadata: json!({}),
                    locked: false,
                    provenance: Some(Provenance::system("other-actor", "concurrent-write")),
                },
                actor: "other-actor".to_string(),
            })
            .expect("change destination state");

        let error = service
            .apply_source_import(SourceImportApplyInput {
                paths: vec![source.display().to_string()],
                grant_token: Some(preview.apply_grant_token),
                destination_scope_id: "project:state-import".to_string(),
                pack_name: Some("instructions".to_string()),
                source_kind: CoreSourceImportKind::Auto,
                actor: Some("import-test".to_string()),
                preview_id: preview.preview_id,
                expected_preview_fingerprint: Some(preview.preview_fingerprint),
                confirmation: true,
            })
            .expect_err("destination state invalidates preview");
        assert_eq!(
            error,
            "conflict: source import preview fingerprint no longer matches authoritative state; preview again"
        );
        let current = backend
            .store
            .get_entry(&EntrySelector {
                scope: ScopeRef::normalized(CoreScopeKind::Project, "state-import")
                    .expect("destination scope"),
                pack_name: "instructions".to_string(),
                entry_key: candidate_key,
            })
            .expect("current destination entry");
        assert_eq!(
            current.value.render_markdown(),
            "Concurrent destination content."
        );
        assert!(backend
            .store
            .review_list(Some(ReviewState::Pending))
            .expect("pending reviews")
            .is_empty());
    }

    #[test]
    fn project_selection_onboarding_and_instruction_discovery_persist() {
        let temp = tempdir().expect("tempdir");
        let paths = test_paths(temp.path(), "onboarding-home");
        let project = temp.path().join("selected-project");
        fs::create_dir_all(project.join(".github")).expect("project directories");
        write_text_file(
            &project.join(".github/copilot-instructions.md"),
            "# Copilot instructions\n",
        )
        .expect("write project instructions");
        let store = LocalSettingsStore::new(paths.clone());
        let backend = StoreBackend::new(&paths.db_path);
        let service = DesktopContextService::with_backend(backend.clone(), store.clone());
        let initial = service.load_settings().expect("initial settings");
        assert!(!initial.onboarding.complete);
        assert!(initial.onboarding.inferred);

        let project_grant = grant(
            &service,
            PathGrantPurpose::ProjectRegistration,
            vec![project.clone()],
        );
        let registration = service
            .register_project(project.display().to_string(), Some(project_grant))
            .expect("register project");
        assert_eq!(registration.instruction_sources.len(), 1);
        assert_eq!(
            registration.instruction_sources[0].source_kind,
            CoreSourceImportKind::CopilotInstructions
        );
        let error = service
            .complete_onboarding()
            .expect_err("empty project cannot complete onboarding");
        assert_eq!(
            error,
            "onboarding requires at least one active durable context entry"
        );
        assert!(
            !service
                .load_settings()
                .expect("still incomplete")
                .onboarding
                .complete
        );
        backend
            .store
            .put_entry(PutEntryRequest {
                scope: ScopeRef::normalized(CoreScopeKind::Project, "other")
                    .expect("other project"),
                pack_name: "main".to_string(),
                entry: EntryInput {
                    key: "other".to_string(),
                    title: None,
                    kind: "instruction".to_string(),
                    value: EntryValue::Markdown {
                        body: "Context for another project.".to_string(),
                    },
                    tags: Vec::new(),
                    metadata: json!({}),
                    locked: false,
                    provenance: Some(Provenance::system("test", "test")),
                },
                actor: "test".to_string(),
            })
            .expect("create context in another project");
        let error = service
            .complete_onboarding()
            .expect_err("selected project must compose context");
        assert_eq!(
            error,
            "onboarding requires the selected scope to compose at least one active entry"
        );
        assert_eq!(
            store
                .load()
                .expect("settings after failed completion")
                .onboarding_complete,
            None
        );
        service
            .save_pack(SavePackInput {
                id: None,
                scope_id: registration.scope_id.clone(),
                name: "Project instructions".to_string(),
                status: ViewPackStatus::Active,
                summary: "Durable onboarding context".to_string(),
                tags: vec!["onboarding".to_string()],
                body: "Use the registered project instructions.".to_string(),
            })
            .expect("create durable project context");
        assert!(service.complete_onboarding().expect("complete").complete);

        let restarted =
            DesktopContextService::with_backend(StoreBackend::new(&paths.db_path), store);
        let dashboard = restarted.load_dashboard().expect("dashboard");
        assert_eq!(dashboard.selected_scope_id, registration.scope_id);
        assert!(dashboard.onboarding.complete);
        assert!(!restarted.reset_onboarding().expect("reset").complete);
    }

    #[test]
    fn onboarding_state_never_reports_completion_without_active_context() {
        let temp = tempdir().expect("tempdir");
        let paths = test_paths(temp.path(), "onboarding-state-home");
        let mut settings = LocalSettings::default_for(&paths);
        settings.onboarding_complete = Some(true);
        settings.onboarding_completed_at = Some(now_iso());
        let state = onboarding_state(&settings, false, false);
        assert!(!state.complete);
        assert!(!state.durable_context);
        assert!(state.completed_at.is_none());
    }

    #[test]
    fn inferred_onboarding_ignores_entries_in_archived_packs() {
        let temp = tempdir().expect("tempdir");
        let paths = test_paths(temp.path(), "archived-onboarding-home");
        let service = DesktopContextService::with_backend(
            StoreBackend::new(&paths.db_path),
            LocalSettingsStore::new(paths),
        );
        service
            .save_pack(SavePackInput {
                id: None,
                scope_id: "project:archived-onboarding".to_string(),
                name: "Archived onboarding".to_string(),
                status: ViewPackStatus::Active,
                summary: "Initially active".to_string(),
                tags: Vec::new(),
                body: "Initially composable context.".to_string(),
            })
            .expect("save onboarding pack");
        let active = service.load_settings().expect("active onboarding state");
        assert!(active.onboarding.durable_context);
        assert!(active.onboarding.complete);
        assert!(active.onboarding.inferred);

        service
            .forget_scope(ForgetScopeInput {
                scope_id: "project:archived-onboarding".to_string(),
                confirmation: true,
                actor: None,
            })
            .expect("archive onboarding pack");
        let archived = service.load_settings().expect("archived onboarding state");
        assert!(!archived.onboarding.durable_context);
        assert!(!archived.onboarding.complete);
    }

    #[test]
    fn strict_review_policy_supports_deterministic_bulk_approval() {
        let temp = tempdir().expect("tempdir");
        let paths = test_paths(temp.path(), "review-home");
        let backend = StoreBackend::new(&paths.db_path);
        let service =
            DesktopContextService::with_backend(backend.clone(), LocalSettingsStore::new(paths));
        let policy = service
            .set_review_policy(SetReviewPolicyInput {
                mode: ReviewMode::Strict,
                actor: "review-test".to_string(),
                note: Some("Strict review test".to_string()),
                request_id: Some("policy-test".to_string()),
            })
            .expect("set review policy");
        assert_eq!(policy.mode, ReviewMode::Strict);
        let scope = ScopeRef::normalized(CoreScopeKind::Project, "atlas").expect("project scope");
        for key in ["alpha", "beta", "gamma"] {
            backend
                .store
                .commit_work(context_core::CommitWorkRequest {
                    request_id: format!("request-{key}"),
                    actor: "agent".to_string(),
                    run: None,
                    proposals: vec![context_core::CommitProposal {
                        scope: scope.clone(),
                        pack_name: "review-pack".to_string(),
                        entry: EntryInput {
                            key: key.to_string(),
                            title: Some(key.to_string()),
                            kind: "instruction".to_string(),
                            value: EntryValue::Markdown {
                                body: format!("Proposed {key} content"),
                            },
                            tags: Vec::new(),
                            metadata: json!({}),
                            locked: false,
                            provenance: Some(Provenance::system("agent", "agent.commit")),
                        },
                    }],
                })
                .expect("create strict review");
        }
        let runtime = service.runtime_snapshot().expect("runtime");
        let reviews = map_review_items(&runtime.reviews, &runtime.pack_lookup);
        assert_eq!(reviews.len(), 3);
        assert!(reviews.iter().all(|review| {
            review.reason == Some(ReviewReason::StrictPolicy)
                && matches!(review.risk, RiskLevel::Low)
                && review.existing_content.is_none()
                && !review.proposed_content.is_empty()
        }));
        let review_hit = service
            .search_index("Proposed alpha content".to_string())
            .expect("search review")
            .into_iter()
            .find(|result| result.kind == SearchKind::Review)
            .expect("typed review search target");
        assert_eq!(review_hit.target.scope_id.as_deref(), Some("project:atlas"));
        assert_eq!(
            review_hit.target.review_id.as_deref(),
            Some(review_hit.id.as_str())
        );
        assert!(service
            .bulk_review_decision(BulkReviewDecisionInput {
                item_ids: reviews.iter().map(|review| review.id.clone()).collect(),
                decision: ViewReviewDecision::Edit,
                confirmation: true,
                edited_content: Some("No bulk edits".to_string()),
                actor: None,
                note: None,
            })
            .is_err());

        let edit_result = service
            .bulk_review_decision(BulkReviewDecisionInput {
                item_ids: vec![reviews[0].id.clone()],
                decision: ViewReviewDecision::Edit,
                confirmation: false,
                edited_content: Some("Edited content awaiting approval".to_string()),
                actor: Some("review-test".to_string()),
                note: None,
            })
            .expect("edit pending review");
        assert_eq!(edit_result.completed, 1);
        assert_eq!(edit_result.results[0].state, Some(ReviewState::Approved));
        assert!(!edit_result.results[0].requires_follow_up);
        let pending = backend
            .store
            .review_list(Some(ReviewState::Pending))
            .expect("pending reviews after atomic edit");
        assert_eq!(pending.len(), 2);
        assert!(pending.iter().all(|review| review.id != reviews[0].id));
        let approved_entry = backend
            .store
            .get_entry(&EntrySelector {
                scope: scope.clone(),
                pack_name: "review-pack".to_string(),
                entry_key: reviews[0].entry_key.clone(),
            })
            .expect("atomically approved entry");
        assert_eq!(
            approved_entry.value.render_markdown(),
            "Edited content awaiting approval"
        );

        let mut ids = reviews
            .iter()
            .filter(|review| review.id != reviews[0].id)
            .map(|review| review.id.clone())
            .collect::<Vec<_>>();
        ids.reverse();
        let confirmation_error = service
            .bulk_review_decision(BulkReviewDecisionInput {
                item_ids: ids.clone(),
                decision: ViewReviewDecision::Approve,
                confirmation: false,
                edited_content: None,
                actor: Some("review-test".to_string()),
                note: None,
            })
            .expect_err("bulk approval requires confirmation");
        assert_eq!(
            confirmation_error,
            "confirmation is required before applying a bulk review decision"
        );
        assert_eq!(
            desktop_error(confirmation_error).code,
            DesktopErrorCode::ConfirmationRequired
        );
        let result = service
            .bulk_review_decision(BulkReviewDecisionInput {
                item_ids: ids,
                decision: ViewReviewDecision::Approve,
                confirmation: true,
                edited_content: None,
                actor: Some("review-test".to_string()),
                note: Some("Approved in bulk".to_string()),
            })
            .expect("bulk approve");
        assert_eq!(result.completed, 2);
        assert!(!result.stopped);
        assert!(result
            .results
            .windows(2)
            .all(|pair| pair[0].item_id <= pair[1].item_id));
    }

    #[test]
    fn verified_bundle_import_uses_loaded_payload_without_reopening_path() {
        let temp = tempdir().expect("tempdir");
        let source_paths = test_paths(temp.path(), "verified-bundle-source");
        let source = DesktopContextService::with_backend(
            StoreBackend::new(&source_paths.db_path),
            LocalSettingsStore::new(source_paths),
        );
        source
            .save_pack(SavePackInput {
                id: None,
                scope_id: "project:verified".to_string(),
                name: "Verified bundle".to_string(),
                status: ViewPackStatus::Active,
                summary: "Verified payload".to_string(),
                tags: Vec::new(),
                body: "Import these exact verified bytes.".to_string(),
            })
            .expect("save verified bundle source");
        let bundle_path = temp.path().join("verified-bundle.json");
        let export_grant = grant(
            &source,
            PathGrantPurpose::ExportArchive,
            vec![bundle_path.clone()],
        );
        source
            .export_archive(bundle_path.display().to_string(), Some(export_grant))
            .expect("export verified bundle");
        let verified =
            load_verified_bundle(bundle_path.display().to_string()).expect("load verified bytes");
        write_text_file(&bundle_path, "{\"changed\":true}")
            .expect("replace bundle after verification");

        let destination_paths = test_paths(temp.path(), "verified-bundle-destination");
        let destination = DesktopContextService::with_backend(
            StoreBackend::new(&destination_paths.db_path),
            LocalSettingsStore::new(destination_paths.clone()),
        );
        destination
            .import_verified_bundle(&destination_paths, verified)
            .expect("import in-memory verified payload");
        let entries = destination
            .list_entries(Some("project:verified".to_string()), None)
            .expect("list imported verified entries");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].body, "Import these exact verified bytes.");
    }

    #[test]
    fn forget_scope_fails_when_authoritative_pack_load_is_unavailable() {
        let temp = tempdir().expect("tempdir");
        let paths = test_paths(temp.path(), "forget-offline-home");
        let backend =
            StoreBackend::new(&paths.db_path).with_list_packs_error("transport unavailable");
        let service = DesktopContextService::with_backend(backend, LocalSettingsStore::new(paths));
        let error = service
            .forget_scope(ForgetScopeInput {
                scope_id: "project:offline".to_string(),
                confirmation: true,
                actor: None,
            })
            .expect_err("offline forget must not report success");
        assert_eq!(error, "transport unavailable");
    }

    #[test]
    fn forget_scope_counts_only_successfully_archived_pack_entries() {
        let temp = tempdir().expect("tempdir");
        let paths = test_paths(temp.path(), "forget-partial-home");
        let backend = StoreBackend::new(&paths.db_path);
        let seed = DesktopContextService::with_backend(
            backend.clone(),
            LocalSettingsStore::new(paths.clone()),
        );
        for name in ["First pack", "Second pack"] {
            seed.save_pack(SavePackInput {
                id: None,
                scope_id: "project:partial".to_string(),
                name: name.to_string(),
                status: ViewPackStatus::Active,
                summary: format!("{name} summary"),
                tags: Vec::new(),
                body: format!("{name} entry"),
            })
            .expect("seed pack");
        }
        let mut packs = backend.store.list_packs().expect("seeded packs");
        packs.sort_by(|left, right| left.id.cmp(&right.id));
        let successful_pack = packs[0].clone();
        let failed_pack = packs[1].clone();
        let failing_backend = backend
            .clone()
            .with_update_pack_error(failed_pack.name.clone());
        let service =
            DesktopContextService::with_backend(failing_backend, LocalSettingsStore::new(paths));
        let result = service
            .forget_scope(ForgetScopeInput {
                scope_id: "project:partial".to_string(),
                confirmation: true,
                actor: Some("forget-test".to_string()),
            })
            .expect("partial forget result");
        assert_eq!(result.packs_archived, 1);
        assert_eq!(result.entries_affected, 1);
        assert!(result.stopped);
        assert_eq!(result.failures.len(), 1);
        assert_eq!(result.failures[0].pack_id, failed_pack.id);

        let current = backend.store.list_packs().expect("current packs");
        assert_eq!(
            current
                .iter()
                .find(|pack| pack.id == successful_pack.id)
                .expect("successful pack")
                .status,
            CorePackStatus::Archived
        );
        assert_eq!(
            current
                .iter()
                .find(|pack| pack.id == failed_pack.id)
                .expect("failed pack")
                .status,
            CorePackStatus::Active
        );
    }

    #[test]
    fn privacy_counts_fall_back_to_health_when_stats_are_unavailable() {
        let temp = tempdir().expect("tempdir");
        let paths = test_paths(temp.path(), "privacy-stats-home");
        let backend = StoreBackend::new(&paths.db_path);
        let seed = DesktopContextService::with_backend(
            backend.clone(),
            LocalSettingsStore::new(paths.clone()),
        );
        seed.save_pack(SavePackInput {
            id: None,
            scope_id: "project:privacy".to_string(),
            name: "Privacy".to_string(),
            status: ViewPackStatus::Active,
            summary: "Privacy counts".to_string(),
            tags: Vec::new(),
            body: "Count this entry.".to_string(),
        })
        .expect("seed privacy data");
        let service = DesktopContextService::with_backend(
            backend.with_stats_error("stats unavailable"),
            LocalSettingsStore::new(paths),
        );
        let summary = service.load_privacy_summary().expect("privacy summary");
        assert!(summary.counts_available);
        assert_eq!(summary.counts_source.as_deref(), Some("daemon_health"));
        assert_eq!(summary.counts.packs, 1);
        assert_eq!(summary.counts.entries, 1);
    }

    #[test]
    fn bundle_preview_confirmation_and_scoped_forget_are_safe() {
        let temp = tempdir().expect("tempdir");
        let source_paths = test_paths(temp.path(), "bundle-source");
        let source = DesktopContextService::with_backend(
            StoreBackend::new(&source_paths.db_path),
            LocalSettingsStore::new(source_paths.clone()),
        );
        source
            .save_pack(SavePackInput {
                id: None,
                scope_id: "project:atlas".to_string(),
                name: "Bundle".to_string(),
                status: ViewPackStatus::Active,
                summary: "Bundle source".to_string(),
                tags: Vec::new(),
                body: "Portable local context.".to_string(),
            })
            .expect("save bundle source");
        let bundle_path = temp.path().join("ucm-bundle.json");
        let export_grant = grant(
            &source,
            PathGrantPurpose::ExportArchive,
            vec![bundle_path.clone()],
        );
        source
            .export_archive(bundle_path.display().to_string(), Some(export_grant))
            .expect("export bundle");

        let destination_paths = test_paths(temp.path(), "bundle-destination");
        let destination = DesktopContextService::with_backend(
            StoreBackend::new(&destination_paths.db_path),
            LocalSettingsStore::new(destination_paths),
        );
        let preview_grant = grant(
            &destination,
            PathGrantPurpose::BundleImportPreview,
            vec![bundle_path.clone()],
        );
        let preview = destination
            .preview_bundle_import(bundle_path.display().to_string(), Some(preview_grant))
            .expect("preview bundle");
        assert!(preview.valid);
        assert_eq!(preview.entry_count, 1);
        assert!(destination
            .apply_bundle_import(BundleImportApplyInput {
                path: bundle_path.display().to_string(),
                grant_token: Some(preview.apply_grant_token.clone()),
                checksum_sha256: preview.checksum_sha256.clone(),
                confirmation: false,
            })
            .is_err());
        destination
            .apply_bundle_import(BundleImportApplyInput {
                path: bundle_path.display().to_string(),
                grant_token: Some(preview.apply_grant_token),
                checksum_sha256: preview.checksum_sha256,
                confirmation: true,
            })
            .expect("apply bundle");
        let privacy = destination.load_privacy_summary().expect("privacy summary");
        assert_eq!(privacy.counts.entries, 1);
        assert!(!privacy.telemetry_enabled);
        assert!(!privacy.network_egress_enabled);

        assert!(destination
            .forget_scope(ForgetScopeInput {
                scope_id: "project:atlas".to_string(),
                confirmation: false,
                actor: None,
            })
            .is_err());
        let forgotten = destination
            .forget_scope(ForgetScopeInput {
                scope_id: "project:atlas".to_string(),
                confirmation: true,
                actor: Some("privacy-test".to_string()),
            })
            .expect("archive project");
        assert_eq!(forgotten.packs_archived, 1);
        assert_eq!(forgotten.entries_affected, 1);
        assert!(forgotten.reversible);
    }

    #[test]
    fn typed_errors_hide_secret_details() {
        let error = desktop_error(
            "secret rejected: credential assignment token = hidden-value".to_string(),
        );
        assert_eq!(error.code, DesktopErrorCode::SecretDetected);
        assert!(!error.message.contains("hidden-value"));
    }

    #[test]
    fn restart_daemon_rechecks_reachable_incompatible_or_migration_daemons() {
        let temp = tempdir().expect("tempdir");
        let paths = test_paths(temp.path(), "restart-diagnostics-home");
        let base = StoreBackend::new(&paths.db_path);
        let incompatible_backend = base.clone().with_health_override(HealthReport {
            component_version: Some(env!("CARGO_PKG_VERSION").to_string()),
            api_version: Some(context_core::CONTEXT_API_VERSION + 1),
            schema_version: context_core::LATEST_SCHEMA_VERSION,
            packs: 0,
            entries: 0,
            reviews: 0,
            runs: 0,
        });
        let incompatible_calls = incompatible_backend.ensure_daemon_calls.clone();
        let incompatible_service = DesktopContextService::with_backend(
            incompatible_backend,
            LocalSettingsStore::new(paths.clone()),
        );
        let incompatible = incompatible_service
            .restart_daemon()
            .expect("recheck incompatible daemon");
        assert!(!incompatible.performed);
        assert!(incompatible.message.contains("incompatible"));
        assert!(!incompatible.message.contains("healthy"));
        assert_eq!(incompatible_calls.load(Ordering::SeqCst), 0);

        let migration_backend = base.with_health_override(HealthReport {
            component_version: Some(env!("CARGO_PKG_VERSION").to_string()),
            api_version: Some(context_core::CONTEXT_API_VERSION),
            schema_version: context_core::LATEST_SCHEMA_VERSION - 1,
            packs: 0,
            entries: 0,
            reviews: 0,
            runs: 0,
        });
        let migration_calls = migration_backend.ensure_daemon_calls.clone();
        let migration_service =
            DesktopContextService::with_backend(migration_backend, LocalSettingsStore::new(paths));
        let migration = migration_service
            .restart_daemon()
            .expect("recheck migration daemon");
        assert!(!migration.performed);
        assert!(migration.message.contains("requires migration"));
        assert!(!migration.message.contains("healthy"));
        assert_eq!(migration_calls.load(Ordering::SeqCst), 0);
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
