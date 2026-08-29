use crate::error::{ContextError, ContextResult};
use crate::markdown::{
    export_markdown as bundle_to_markdown, import_markdown as bundle_from_markdown,
};
use crate::model::*;
use crate::protocol::CONTEXT_API_VERSION;
use crate::secret::{
    reject_commit_metadata_for_storage, reject_entry_write_for_storage,
    reject_pack_write_for_storage, reject_review_for_storage,
    reject_review_policy_write_for_storage, reject_review_transition_for_storage,
    reject_revision_metadata_for_storage, reject_run_for_storage,
};
use crate::source_import::{ParsedSourceCandidate, parse_source_import};
use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension, Row, Transaction, TransactionBehavior, params};
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tracing::debug;

pub const LATEST_SCHEMA_VERSION: i64 = 5;

pub struct ContextStore {
    db_path: PathBuf,
    conn: Mutex<Connection>,
}

#[derive(Clone)]
struct PackRow {
    id: String,
    record: PackRecord,
}

#[derive(Clone)]
struct EntryRow {
    _pack_id: String,
    content_hash: String,
    record: EntryRecord,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DuplicateCheck {
    ReviewModeDefault,
    ExactCandidate,
}

struct PreparedSourceImport {
    destination: ScopeRef,
    pack_name: String,
    candidates: Vec<ParsedSourceCandidate>,
    warnings: Vec<String>,
}

#[derive(Clone)]
struct ReviewRow {
    record: ReviewItem,
}

impl ContextStore {
    pub fn open(path: impl AsRef<Path>) -> ContextResult<Self> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            ensure_private_dir_chain(parent)?;
        }
        let mut conn = Connection::open(&path)?;
        set_private_file_permissions(&path)?;
        Self::configure_connection(&mut conn)?;
        Self::migrate(&mut conn)?;
        Ok(Self {
            db_path: path,
            conn: Mutex::new(conn),
        })
    }

    pub fn open_in_memory() -> ContextResult<Self> {
        let mut conn = Connection::open_in_memory()?;
        Self::configure_connection(&mut conn)?;
        Self::migrate(&mut conn)?;
        Ok(Self {
            db_path: PathBuf::from(":memory:"),
            conn: Mutex::new(conn),
        })
    }

    pub fn db_path(&self) -> &Path {
        &self.db_path
    }

    pub fn get_review_policy(&self) -> ContextResult<ReviewPolicy> {
        let conn = self.lock_conn()?;
        get_review_policy_tx(&conn)
    }

    pub fn set_review_policy(
        &self,
        request: SetReviewPolicyRequest,
    ) -> ContextResult<ReviewPolicy> {
        validate_actor(&request.actor)?;
        reject_review_policy_write_for_storage(request.mode, &request.metadata, &request.actor)?;
        let mut conn = self.lock_conn()?;
        let tx = conn.transaction()?;
        let mut policy = get_review_policy_tx(&tx)?;
        if policy.mode == request.mode && policy.metadata == request.metadata {
            tx.commit()?;
            return Ok(policy);
        }

        policy.mode = request.mode;
        policy.metadata = request.metadata;
        policy.updated_at = now_utc();
        policy.updated_by = request.actor.clone();
        policy.revision_no = record_revision(
            &tx,
            "policy",
            "review",
            "update",
            &policy,
            &Provenance::system(request.actor.clone(), "review_policy_update"),
            None,
            None,
        )?;
        tx.execute(
            "UPDATE review_policy SET review_mode = ?, metadata_json = ?, updated_at = ?, updated_by = ?, current_revision_no = ? WHERE policy_key = 'review'",
            params![
                policy.mode.as_str(),
                to_json_text(&policy.metadata)?,
                policy.updated_at.to_rfc3339(),
                policy.updated_by,
                policy.revision_no,
            ],
        )?;
        tx.commit()?;
        Ok(policy)
    }

    pub fn create_pack(&self, request: CreatePackRequest) -> ContextResult<PackRecord> {
        let mut conn = self.lock_conn()?;
        let tx = conn.transaction()?;
        let scope = request.scope.normalize()?;
        validate_actor(&request.actor)?;
        validate_pack_name(&request.name)?;
        reject_pack_write_for_storage(
            &scope,
            &request.name,
            &request.actor,
            request.description.as_deref(),
            &request.metadata,
            request.lock_reason.as_deref(),
        )?;
        if get_pack_tx(&tx, &scope, &request.name)?.is_some() {
            return Err(ContextError::conflict(format!(
                "pack {} already exists in {}",
                request.name,
                scope.label()
            )));
        }
        let record = ensure_pack_tx(
            &tx,
            &scope,
            &request.name,
            &request.actor,
            request.description.clone(),
            request.metadata.clone(),
            request.locked,
            request.lock_reason.clone(),
        )?;
        tx.commit()?;
        Ok(record)
    }

    pub fn update_pack(&self, request: UpdatePackRequest) -> ContextResult<PackRecord> {
        let mut conn = self.lock_conn()?;
        let tx = conn.transaction()?;
        let scope = request.selector.scope.normalize()?;
        validate_actor(&request.actor)?;
        validate_pack_name(&request.selector.name)?;
        let mut row = get_pack_tx(&tx, &scope, &request.selector.name)?.ok_or_else(|| {
            ContextError::not_found(format!(
                "pack {} in {}",
                request.selector.name,
                scope.label()
            ))
        })?;
        reject_archived_pack_update(&row.record, &request)?;
        let now = now_utc();
        let status = request.status.unwrap_or(row.record.status.clone());
        let locked = request.locked.unwrap_or(row.record.locked);
        let lock_reason = if locked {
            request.lock_reason.or(row.record.lock_reason.clone())
        } else {
            None
        };
        let description = request.description.or(row.record.description.clone());
        let metadata = request
            .metadata
            .unwrap_or_else(|| row.record.metadata.clone());
        reject_pack_write_for_storage(
            &scope,
            &row.record.name,
            &request.actor,
            description.as_deref(),
            &metadata,
            lock_reason.as_deref(),
        )?;
        tx.execute(
            "UPDATE packs SET description = ?, metadata_json = ?, status = ?, locked = ?, lock_reason = ?, updated_at = ?, updated_by = ? WHERE id = ?",
            params![
                description,
                to_json_text(&metadata)?,
                status.as_str(),
                bool_to_int(locked),
                lock_reason,
                now.to_rfc3339(),
                request.actor,
                row.id,
            ],
        )?;
        row.record.description = description;
        row.record.metadata = metadata;
        row.record.status = status;
        row.record.locked = locked;
        row.record.lock_reason = lock_reason;
        row.record.updated_at = now;
        row.record.revision_no = record_revision(
            &tx,
            "pack",
            &row.id,
            "update",
            &row.record,
            &Provenance::system(request.actor, "pack_update"),
            None,
            None,
        )?;
        tx.execute(
            "UPDATE packs SET current_revision_no = ? WHERE id = ?",
            params![row.record.revision_no, row.id],
        )?;
        tx.commit()?;
        Ok(row.record)
    }

    pub fn list_packs(&self) -> ContextResult<Vec<PackRecord>> {
        let conn = self.lock_conn()?;
        let mut stmt = conn.prepare(
            "SELECT id, scope_kind, scope_id, name, description, metadata_json, status, locked, lock_reason, created_at, updated_at, current_revision_no FROM packs ORDER BY scope_kind, scope_id, name",
        )?;
        let rows = stmt.query_map([], map_pack_row)?;
        collect_rows(rows)
            .map(|rows: Vec<PackRow>| rows.into_iter().map(|row| row.record).collect())
    }

    pub fn put_entry(&self, request: PutEntryRequest) -> ContextResult<EntryRecord> {
        let mut conn = self.lock_conn()?;
        let tx = conn.transaction()?;
        let scope = request.scope.normalize()?;
        validate_actor(&request.actor)?;
        validate_pack_name(&request.pack_name)?;
        let provenance = request
            .entry
            .provenance
            .clone()
            .unwrap_or_else(|| Provenance {
                actor: request.actor.clone(),
                source: "manual_put".to_string(),
                source_ref: None,
                run_id: None,
                request_id: None,
                note: None,
            });
        let record = apply_entry_tx(
            &tx,
            &scope,
            &request.pack_name,
            &request.entry,
            &request.actor,
            &provenance,
            None,
            None,
        )?;
        tx.commit()?;
        Ok(record)
    }

    pub fn get_entry(&self, selector: &EntrySelector) -> ContextResult<EntryRecord> {
        let conn = self.lock_conn()?;
        let scope = selector.scope.normalize()?;
        validate_pack_name(&selector.pack_name)?;
        validate_entry_key(&selector.entry_key)?;
        get_entry_tx(&conn, &scope, &selector.pack_name, &selector.entry_key)?
            .map(|row| row.record)
            .ok_or_else(|| {
                ContextError::not_found(format!(
                    "entry {} in pack {} ({})",
                    selector.entry_key,
                    selector.pack_name,
                    scope.label()
                ))
            })
    }

    pub fn list_entries(&self, request: ExportRequest) -> ContextResult<Vec<EntryRecord>> {
        let conn = self.lock_conn()?;
        let mut sql = String::from(
            "SELECT e.id, p.scope_kind, p.scope_id, p.name, e.entry_key, e.title, e.kind, e.value_format, e.markdown_body, e.json_body, e.tags_json, e.metadata_json, e.provenance_json, e.locked, e.status, e.created_at, e.updated_at, e.current_revision_no, e.pack_id, e.content_hash FROM entries e JOIN packs p ON p.id = e.pack_id WHERE 1=1",
        );
        let filters = request_to_filters(&request)?;
        if !request.include_deleted {
            sql.push_str(" AND e.status = 'active'");
        }
        if !filters.is_empty() {
            sql.push_str(" AND (");
            sql.push_str(&filters.join(" OR "));
            sql.push(')');
        }
        if let Some(pack_name) = &request.pack_name {
            sql.push_str(" AND p.name = ");
            sql.push_str(&sql_quote(pack_name));
        }
        sql.push_str(" ORDER BY p.scope_kind, p.scope_id, p.name, e.entry_key");
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map([], map_entry_row)?;
        collect_rows(rows)
            .map(|rows: Vec<EntryRow>| rows.into_iter().map(|row| row.record).collect())
    }

    pub fn delete_entry(&self, request: DeleteEntryRequest) -> ContextResult<EntryRecord> {
        let mut conn = self.lock_conn()?;
        let tx = conn.transaction()?;
        let scope = request.selector.scope.normalize()?;
        validate_actor(&request.actor)?;
        validate_pack_name(&request.selector.pack_name)?;
        validate_entry_key(&request.selector.entry_key)?;
        let mut row = get_entry_tx(
            &tx,
            &scope,
            &request.selector.pack_name,
            &request.selector.entry_key,
        )?
        .ok_or_else(|| ContextError::not_found(format!("entry {}", request.selector.entry_key)))?;
        let pack = get_pack_tx(&tx, &scope, &request.selector.pack_name)?.ok_or_else(|| {
            ContextError::not_found(format!("pack {}", request.selector.pack_name))
        })?;
        reject_archived_pack_mutation(&pack.record, "delete entries from")?;
        let now = now_utc();
        tx.execute(
            "UPDATE entries SET status = 'deleted', updated_at = ?, updated_by = ? WHERE id = ?",
            params![now.to_rfc3339(), request.actor, row.record.id],
        )?;
        delete_fts_tx(&tx, &row.record.id)?;
        row.record.status = EntryStatus::Deleted;
        row.record.updated_at = now;
        row.record.provenance = Provenance {
            actor: request.actor.clone(),
            source: "delete".to_string(),
            source_ref: None,
            run_id: None,
            request_id: None,
            note: None,
        };
        row.record.revision_no = record_revision(
            &tx,
            "entry",
            &row.record.id,
            "delete",
            &row.record,
            &row.record.provenance,
            None,
            None,
        )?;
        tx.execute(
            "UPDATE entries SET current_revision_no = ?, provenance_json = ? WHERE id = ?",
            params![
                row.record.revision_no,
                to_json_text(&row.record.provenance)?,
                row.record.id
            ],
        )?;
        tx.commit()?;
        Ok(row.record)
    }

    pub fn revert_entry(&self, request: RevertEntryRequest) -> ContextResult<EntryRecord> {
        let mut conn = self.lock_conn()?;
        let tx = conn.transaction()?;
        let scope = request.selector.scope.normalize()?;
        validate_actor(&request.actor)?;
        validate_pack_name(&request.selector.pack_name)?;
        validate_entry_key(&request.selector.entry_key)?;
        let row = get_entry_tx(
            &tx,
            &scope,
            &request.selector.pack_name,
            &request.selector.entry_key,
        )?
        .ok_or_else(|| ContextError::not_found(format!("entry {}", request.selector.entry_key)))?;
        let revision_no = request
            .revision_no
            .unwrap_or_else(|| row.record.revision_no.saturating_sub(1));
        if revision_no <= 0 {
            return Err(ContextError::validation("no earlier revision exists"));
        }
        let snapshot_json: String = tx
            .query_row(
                "SELECT snapshot_json FROM revisions WHERE entity_type = 'entry' AND entity_id = ? AND revision_no = ?",
                params![row.record.id, revision_no],
                |row| row.get(0),
            )
            .optional()?
            .ok_or_else(|| ContextError::not_found(format!("revision {} for {}", revision_no, row.record.id)))?;
        let mut snapshot: EntryRecord = serde_json::from_str(&snapshot_json)?;
        snapshot.provenance = Provenance {
            actor: request.actor.clone(),
            source: "revert".to_string(),
            source_ref: Some(format!("revision:{revision_no}")),
            run_id: None,
            request_id: None,
            note: None,
        };
        let entry = EntryInput {
            key: snapshot.key.clone(),
            title: snapshot.title.clone(),
            kind: snapshot.kind.clone(),
            value: snapshot.value.clone(),
            tags: snapshot.tags.clone(),
            metadata: snapshot.metadata.clone(),
            locked: snapshot.locked,
            provenance: Some(snapshot.provenance.clone()),
        };
        let mut restored = apply_entry_tx(
            &tx,
            &scope,
            &request.selector.pack_name,
            &entry,
            &request.actor,
            &snapshot.provenance,
            None,
            None,
        )?;
        if snapshot.status == EntryStatus::Deleted {
            tx.execute(
                "UPDATE entries SET status = 'deleted' WHERE id = ?",
                params![restored.id],
            )?;
            delete_fts_tx(&tx, &restored.id)?;
            restored.status = EntryStatus::Deleted;
        }
        restored.updated_at = now_utc();
        tx.commit()?;
        Ok(restored)
    }

    pub fn compose_context(&self, request: ComposeRequest) -> ContextResult<ComposeResponse> {
        let conn = self.lock_conn()?;
        let filters = compose_filters(&request)?;
        let mut sql = String::from(
            "SELECT e.id, p.scope_kind, p.scope_id, p.name, e.entry_key, e.title, e.kind, e.value_format, e.markdown_body, e.json_body, e.tags_json, e.metadata_json, e.provenance_json, e.locked, e.status, e.created_at, e.updated_at, e.current_revision_no, e.pack_id, e.content_hash FROM entries e JOIN packs p ON p.id = e.pack_id WHERE e.status = 'active'",
        );
        if !request.include_archived {
            sql.push_str(" AND p.status = 'active'");
        }
        if !filters.is_empty() {
            sql.push_str(" AND (");
            sql.push_str(&filters.join(" OR "));
            sql.push(')');
        }
        sql.push_str(" ORDER BY CASE p.scope_kind WHEN 'global' THEN 0 WHEN 'project' THEN 1 ELSE 2 END, p.scope_id, p.name, e.entry_key");
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map([], map_entry_row)?;
        let rows = collect_rows(rows)?;
        let mut sections: Vec<ComposeSection> = Vec::new();
        for row in rows {
            if let Some(section) = sections.iter_mut().find(|section| {
                section.scope == row.record.scope && section.pack_name == row.record.pack_name
            }) {
                section.entries.push(row.record);
            } else {
                sections.push(ComposeSection {
                    scope: row.record.scope.clone(),
                    pack_name: row.record.pack_name.clone(),
                    entries: vec![row.record],
                });
            }
        }
        let rendered_markdown = render_compose_markdown(&sections);
        let exclusions = list_compose_exclusions(&conn, &request)?;
        let metrics = ComposeMetrics {
            rendered_bytes: rendered_markdown.len(),
            estimated_tokens: estimate_tokens(&rendered_markdown),
            included_entries: sections.iter().map(|section| section.entries.len()).sum(),
            excluded_entries: exclusions.len(),
        };
        Ok(ComposeResponse {
            generated_at: now_utc(),
            sections,
            rendered_markdown,
            metrics,
            exclusions,
            warnings: Vec::new(),
        })
    }

    pub fn search_context(&self, request: SearchRequest) -> ContextResult<SearchResponse> {
        if request.query.trim().is_empty() {
            return Err(ContextError::validation("search query must not be empty"));
        }
        let conn = self.lock_conn()?;
        let filters = compose_filters(&ComposeRequest {
            project_scope_id: request.project_scope_id.clone(),
            task_scope_id: request.task_scope_id.clone(),
            include_archived: false,
        })?;
        let mut sql = String::from(
            "SELECT e.id, p.scope_kind, p.scope_id, p.name, e.entry_key, e.title, e.kind, e.value_format, e.markdown_body, e.json_body, e.tags_json, e.metadata_json, e.provenance_json, e.locked, e.status, e.created_at, e.updated_at, e.current_revision_no, e.pack_id, e.content_hash, bm25(entry_fts) AS score, snippet(entry_fts, 6, '[', ']', '…', 12) AS snippet FROM entry_fts JOIN entries e ON e.id = entry_fts.entry_id JOIN packs p ON p.id = e.pack_id WHERE entry_fts MATCH ? AND e.status = 'active' AND p.status = 'active'",
        );
        if !filters.is_empty() {
            sql.push_str(" AND (");
            sql.push_str(&filters.join(" OR "));
            sql.push(')');
        }
        sql.push_str(" ORDER BY score LIMIT ?");
        let query = fts_query(&request.query);
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params![query, request.limit], |row| {
            let entry = map_entry_row(row)?;
            let score: f64 = row.get(20)?;
            let snippet: String = row.get(21)?;
            Ok(SearchHit {
                score,
                snippet,
                entry: entry.record,
            })
        })?;
        Ok(SearchResponse {
            query: request.query,
            hits: collect_rows(rows)?,
        })
    }

    pub fn create_run(&self, input: RunInput) -> ContextResult<RunRecord> {
        let mut conn = self.lock_conn()?;
        let tx = conn.transaction()?;
        let run = ensure_run_tx(&tx, input)?;
        tx.commit()?;
        Ok(run)
    }

    pub fn commit_work(&self, request: CommitWorkRequest) -> ContextResult<CommitWorkResult> {
        self.commit_work_with_duplicate_check(request, DuplicateCheck::ReviewModeDefault)
    }

    fn commit_work_with_duplicate_check(
        &self,
        request: CommitWorkRequest,
        duplicate_check: DuplicateCheck,
    ) -> ContextResult<CommitWorkResult> {
        let mut conn = self.lock_conn()?;
        let tx = conn.transaction()?;
        let result = commit_work_tx(&tx, request, duplicate_check)?;
        tx.commit()?;
        Ok(result)
    }

    pub fn review_list(&self, state: Option<ReviewState>) -> ContextResult<Vec<ReviewItem>> {
        let conn = self.lock_conn()?;
        let mut sql = String::from(
            "SELECT id, request_id, scope_kind, scope_id, pack_name, entry_key, state, reason, proposed_entry_json, existing_entry_json, resolution_note, created_at, updated_at, current_revision_no FROM review_items",
        );
        if let Some(state) = state {
            sql.push_str(" WHERE state = ");
            sql.push_str(&sql_quote(state.as_str()));
        }
        sql.push_str(" ORDER BY created_at, id");
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map([], map_review_row)?;
        collect_rows(rows)
            .map(|rows: Vec<ReviewRow>| rows.into_iter().map(|row| row.record).collect())
    }

    pub fn review_approve(&self, request: ReviewDecisionRequest) -> ContextResult<ReviewItem> {
        let mut conn = self.lock_conn()?;
        let tx = conn.transaction()?;
        validate_actor(&request.actor)?;
        validate_review_id(&request.review_id)?;
        let mut review = get_review_tx(&tx, &request.review_id)?
            .ok_or_else(|| ContextError::not_found(format!("review {}", request.review_id)))?;
        if review.record.state != ReviewState::Pending {
            return Err(ContextError::validation("review is not pending"));
        }
        let provenance = review
            .record
            .proposed_entry
            .provenance
            .clone()
            .unwrap_or_else(|| Provenance {
                actor: request.actor.clone(),
                source: "review_approve".to_string(),
                source_ref: Some(review.record.id.clone()),
                run_id: None,
                request_id: Some(review.record.request_id.clone()),
                note: request.note.clone(),
            });
        let _entry = apply_entry_tx(
            &tx,
            &review.record.scope,
            &review.record.pack_name,
            &review.record.proposed_entry,
            &request.actor,
            &provenance,
            Some(review.record.request_id.as_str()),
            provenance.run_id.as_deref(),
        )?;
        let now = now_utc();
        review.record.state = ReviewState::Approved;
        review.record.updated_at = now;
        review.record.resolution_note = request.note;
        reject_review_for_storage(&review.record)?;
        review.record.revision_no =
            update_review_tx(&tx, &review.record, &request.actor, "approve")?;
        tx.commit()?;
        Ok(review.record)
    }

    pub fn review_reject(&self, request: ReviewDecisionRequest) -> ContextResult<ReviewItem> {
        let mut conn = self.lock_conn()?;
        let tx = conn.transaction()?;
        validate_actor(&request.actor)?;
        validate_review_id(&request.review_id)?;
        let mut review = get_review_tx(&tx, &request.review_id)?
            .ok_or_else(|| ContextError::not_found(format!("review {}", request.review_id)))?;
        if review.record.state != ReviewState::Pending {
            return Err(ContextError::validation("review is not pending"));
        }
        review.record.state = ReviewState::Rejected;
        review.record.updated_at = now_utc();
        review.record.resolution_note = request.note;
        reject_review_for_storage(&review.record)?;
        review.record.revision_no =
            update_review_tx(&tx, &review.record, &request.actor, "reject")?;
        tx.commit()?;
        Ok(review.record)
    }

    pub fn review_edit(&self, request: ReviewEditRequest) -> ContextResult<ReviewItem> {
        let mut conn = self.lock_conn()?;
        let tx = conn.transaction()?;
        validate_actor(&request.actor)?;
        validate_review_id(&request.review_id)?;
        let mut review = get_review_tx(&tx, &request.review_id)?
            .ok_or_else(|| ContextError::not_found(format!("review {}", request.review_id)))?;
        if review.record.state != ReviewState::Pending {
            return Err(ContextError::validation("review is not pending"));
        }
        apply_review_edit_fields(&mut review.record, &request)?;
        review.record.updated_at = now_utc();
        review.record.revision_no = update_review_tx(&tx, &review.record, &request.actor, "edit")?;
        tx.commit()?;
        Ok(review.record)
    }

    pub fn review_edit_and_approve(
        &self,
        request: ReviewEditAndApproveRequest,
    ) -> ContextResult<ReviewItem> {
        let note = request.note.clone();
        let edit_request = ReviewEditRequest::from(request);
        let mut conn = self.lock_conn()?;
        let tx = conn.transaction()?;
        validate_actor(&edit_request.actor)?;
        validate_review_id(&edit_request.review_id)?;
        let mut review = get_review_tx(&tx, &edit_request.review_id)?
            .ok_or_else(|| ContextError::not_found(format!("review {}", edit_request.review_id)))?;
        if review.record.state != ReviewState::Pending {
            return Err(ContextError::validation("review is not pending"));
        }

        apply_review_edit_fields(&mut review.record, &edit_request)?;
        let provenance = review
            .record
            .proposed_entry
            .provenance
            .clone()
            .unwrap_or_else(|| Provenance {
                actor: edit_request.actor.clone(),
                source: "review_edit_and_approve".to_string(),
                source_ref: Some(review.record.id.clone()),
                run_id: None,
                request_id: Some(review.record.request_id.clone()),
                note: note.clone(),
            });
        let _entry = apply_entry_tx(
            &tx,
            &review.record.scope,
            &review.record.pack_name,
            &review.record.proposed_entry,
            &edit_request.actor,
            &provenance,
            Some(review.record.request_id.as_str()),
            provenance.run_id.as_deref(),
        )?;
        review.record.state = ReviewState::Approved;
        review.record.updated_at = now_utc();
        review.record.resolution_note = note;
        reject_review_for_storage(&review.record)?;
        review.record.revision_no =
            update_review_tx(&tx, &review.record, &edit_request.actor, "edit_and_approve")?;
        tx.commit()?;
        Ok(review.record)
    }

    pub fn export_bundle(&self, request: ExportRequest) -> ContextResult<ContextExportBundle> {
        let packs = self.list_export_packs(&request)?;
        let entries = self.list_entries(request.clone())?;
        let reviews = if request.include_reviews {
            self.list_export_reviews(&request)?
        } else {
            Vec::new()
        };
        let runs = if request.include_runs {
            self.list_export_runs(&request)?
        } else {
            Vec::new()
        };
        Ok(ContextExportBundle {
            exported_at: now_utc(),
            packs,
            entries,
            reviews,
            runs,
        })
    }

    pub fn export_json(&self, request: ExportRequest) -> ContextResult<String> {
        Ok(serde_json::to_string_pretty(&self.export_bundle(request)?)?)
    }

    pub fn export_markdown(&self, request: ExportRequest) -> ContextResult<String> {
        bundle_to_markdown(&self.export_bundle(request)?)
    }

    pub fn import_data(&self, request: ImportRequest) -> ContextResult<ContextExportBundle> {
        validate_actor(&request.actor)?;
        match request.format {
            ImportFormat::Json => {
                let bundle: ContextExportBundle = serde_json::from_str(&request.payload)?;
                self.import_bundle(bundle.clone(), &request.actor)?;
                Ok(bundle)
            }
            ImportFormat::Markdown => {
                let bundle = bundle_from_markdown(&request.payload)?;
                self.import_bundle(bundle.clone(), &request.actor)?;
                Ok(bundle)
            }
        }
    }

    pub fn preview_source_import(
        &self,
        request: SourceImportPreviewRequest,
    ) -> ContextResult<SourceImportPreview> {
        let prepared = prepare_source_import(&request)?;
        let conn = self.lock_conn()?;
        build_source_import_preview(&conn, &request.actor, prepared)
    }

    pub fn apply_source_import(
        &self,
        request: SourceImportApplyRequest,
    ) -> ContextResult<SourceImportApplyResult> {
        let preview_request = SourceImportPreviewRequest::from(&request);
        let prepared = prepare_source_import(&preview_request)?;
        let mut conn = self.lock_conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let preview = build_source_import_preview(&tx, &request.actor, prepared)?;
        if let Some(expected) = request.expected_preview_fingerprint.as_deref() {
            if preview.preview_fingerprint.as_deref() != Some(expected) {
                return Err(ContextError::conflict(
                    "source import preview fingerprint no longer matches authoritative state; preview again",
                ));
            }
        }
        if !preview.apply_allowed {
            return Err(ContextError::validation(format!(
                "source import preview does not allow apply: {}",
                preview.warnings.join("; ")
            )));
        }
        let request_id = source_import_request_id(&preview)?;
        let mut items = Vec::with_capacity(preview.candidates.len());
        for candidate in &preview.candidates {
            if candidate.disposition == SourceImportDisposition::Duplicate {
                items.push(SourceImportApplyItem {
                    candidate_index: candidate.candidate_index,
                    document_index: candidate.document_index,
                    source_path: candidate.source_path.clone(),
                    entry_key: candidate.entry.key.clone(),
                    disposition: CommitDisposition::Duplicate,
                    reason: None,
                    entry_id: candidate.existing_entry_id.clone(),
                    review_id: None,
                });
                continue;
            }

            let candidate_request_id =
                source_import_candidate_request_id(&request.actor, &preview, candidate)?;
            let mut entry = candidate.entry.clone();
            if let Some(provenance) = entry.provenance.as_mut() {
                provenance.request_id = Some(candidate_request_id.clone());
            }
            let result = commit_work_tx(
                &tx,
                CommitWorkRequest {
                    request_id: candidate_request_id,
                    actor: request.actor.clone(),
                    run: None,
                    proposals: vec![CommitProposal {
                        scope: preview.destination.clone(),
                        pack_name: preview.pack_name.clone(),
                        entry,
                    }],
                },
                DuplicateCheck::ExactCandidate,
            )?;
            let result_item =
                result.items.into_iter().next().ok_or_else(|| {
                    ContextError::validation("source import commit returned no item")
                })?;
            items.push(SourceImportApplyItem {
                candidate_index: candidate.candidate_index,
                document_index: candidate.document_index,
                source_path: candidate.source_path.clone(),
                entry_key: result_item.entry_key,
                disposition: result_item.disposition,
                reason: result_item.reason,
                entry_id: result_item.entry_id,
                review_id: result_item.review_id,
            });
        }
        items.sort_by_key(|item| item.candidate_index);

        let applied_count = items
            .iter()
            .filter(|item| item.disposition == CommitDisposition::Applied)
            .count();
        let pending_count = items
            .iter()
            .filter(|item| item.disposition == CommitDisposition::Pending)
            .count();
        let rejected_count = items
            .iter()
            .filter(|item| item.disposition == CommitDisposition::Rejected)
            .count();
        let skipped_count = items
            .iter()
            .filter(|item| {
                matches!(
                    item.disposition,
                    CommitDisposition::Duplicate | CommitDisposition::Rejected
                )
            })
            .count();
        let affected_entry_ids = items
            .iter()
            .filter(|item| item.disposition == CommitDisposition::Applied)
            .filter_map(|item| item.entry_id.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let affected_review_ids = items
            .iter()
            .filter_map(|item| item.review_id.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let affected_entry_keys = items
            .iter()
            .filter(|item| {
                matches!(
                    item.disposition,
                    CommitDisposition::Applied | CommitDisposition::Pending
                )
            })
            .map(|item| item.entry_key.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let result = SourceImportApplyResult {
            request_id,
            candidate_count: preview.candidates.len(),
            imported_count: applied_count + pending_count,
            applied_count,
            pending_count,
            skipped_count,
            rejected_count,
            items,
            affected_entry_ids,
            affected_review_ids,
            affected_entry_keys,
        };
        tx.commit()?;
        Ok(result)
    }

    pub fn import_bundle(&self, bundle: ContextExportBundle, actor: &str) -> ContextResult<()> {
        let mut conn = self.lock_conn()?;
        let tx = conn.transaction()?;
        for pack in &bundle.packs {
            upsert_imported_pack_tx(&tx, pack, actor)?;
        }
        for entry in &bundle.entries {
            let scope = entry.scope.normalize()?;
            let input = EntryInput {
                key: entry.key.clone(),
                title: entry.title.clone(),
                kind: entry.kind.clone(),
                value: entry.value.clone(),
                tags: entry.tags.clone(),
                metadata: entry.metadata.clone(),
                locked: entry.locked,
                provenance: Some(entry.provenance.clone()),
            };
            let _ = apply_entry_tx_inner(
                &tx,
                &scope,
                &entry.pack_name,
                &input,
                actor,
                input.provenance.as_ref().expect("provenance exists"),
                None,
                None,
                true,
            )?;
            if entry.status == EntryStatus::Deleted {
                tx.execute(
                    "UPDATE entries SET status = 'deleted' WHERE id = (SELECT e.id FROM entries e JOIN packs p ON p.id = e.pack_id WHERE p.scope_kind = ? AND p.scope_id = ? AND p.name = ? AND e.entry_key = ?)",
                    params![scope.kind.as_str(), scope.id, entry.pack_name, entry.key],
                )?;
                if let Some(current) = get_entry_tx(&tx, &scope, &entry.pack_name, &entry.key)? {
                    delete_fts_tx(&tx, &current.record.id)?;
                }
            }
        }
        for run in &bundle.runs {
            let _ = upsert_imported_run_tx(&tx, run)?;
        }
        for review in &bundle.reviews {
            reject_review_for_storage(review)?;
            upsert_imported_review_item_tx(&tx, review, actor)?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn list_runs(&self) -> ContextResult<Vec<RunRecord>> {
        let conn = self.lock_conn()?;
        let mut stmt = conn.prepare(
            "SELECT id, project_scope_id, task_scope_id, source, metadata_json, started_at FROM runs ORDER BY started_at, id",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(RunRecord {
                id: row.get(0)?,
                project_scope_id: row.get(1)?,
                task_scope_id: row.get(2)?,
                source: row.get(3)?,
                metadata: from_json_text(&row.get::<_, String>(4)?).map_err(to_sql_err)?,
                started_at: parse_ts(&row.get::<_, String>(5)?).map_err(to_sql_err)?,
            })
        })?;
        collect_rows(rows)
    }

    pub fn stats(&self) -> ContextResult<StoreStats> {
        let conn = self.lock_conn()?;
        Ok(StoreStats {
            schema_version: current_schema_version(&conn)?,
            packs: count_table(&conn, "packs")?,
            entries: count_table(&conn, "entries")?,
            reviews: count_table(&conn, "review_items")?,
            runs: count_table(&conn, "runs")?,
        })
    }

    pub fn health(&self) -> ContextResult<HealthReport> {
        self.health_with_component_version(env!("CARGO_PKG_VERSION"))
    }

    pub fn health_with_component_version(
        &self,
        component_version: &str,
    ) -> ContextResult<HealthReport> {
        let stats = self.stats()?;
        Ok(HealthReport {
            component_version: Some(component_version.to_string()),
            api_version: Some(CONTEXT_API_VERSION),
            schema_version: stats.schema_version,
            packs: stats.packs,
            entries: stats.entries,
            reviews: stats.reviews,
            runs: stats.runs,
        })
    }

    fn list_export_packs(&self, request: &ExportRequest) -> ContextResult<Vec<PackRecord>> {
        let conn = self.lock_conn()?;
        let mut sql = String::from(
            "SELECT id, scope_kind, scope_id, name, description, metadata_json, status, locked, lock_reason, created_at, updated_at, current_revision_no FROM packs WHERE 1=1",
        );
        let filters = request_to_filters(request)?;
        if !filters.is_empty() {
            sql.push_str(" AND (");
            sql.push_str(&filters.join(" OR "));
            sql.push(')');
        }
        if let Some(pack_name) = &request.pack_name {
            sql.push_str(" AND name = ");
            sql.push_str(&sql_quote(pack_name));
        }
        sql.push_str(" ORDER BY scope_kind, scope_id, name");
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map([], map_pack_row)?;
        collect_rows(rows)
            .map(|rows: Vec<PackRow>| rows.into_iter().map(|row| row.record).collect())
    }

    fn list_export_reviews(&self, request: &ExportRequest) -> ContextResult<Vec<ReviewItem>> {
        let mut reviews = Vec::new();
        for review in self.review_list(None)? {
            if export_review_matches(&review, request)? {
                reviews.push(review);
            }
        }
        Ok(reviews)
    }

    fn list_export_runs(&self, request: &ExportRequest) -> ContextResult<Vec<RunRecord>> {
        let mut runs = Vec::new();
        for run in self.list_runs()? {
            if export_run_matches(&run, request)? {
                runs.push(run);
            }
        }
        Ok(runs)
    }

    fn configure_connection(conn: &mut Connection) -> ContextResult<()> {
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.pragma_update(None, "busy_timeout", 5_000_i64)?;
        conn.pragma_update(None, "temp_store", "MEMORY")?;
        Ok(())
    }

    fn migrate(conn: &mut Connection) -> ContextResult<()> {
        conn.execute(
            "CREATE TABLE IF NOT EXISTS schema_migrations (version INTEGER PRIMARY KEY NOT NULL, applied_at TEXT NOT NULL) STRICT",
            [],
        )?;
        let current = current_schema_version(conn)?;
        for version in (current + 1)..=LATEST_SCHEMA_VERSION {
            debug!(version, "applying schema migration");
            let tx = conn.transaction()?;
            match version {
                1 => migration_v1(&tx)?,
                2 => migration_v2(&tx)?,
                3 => migration_v3(&tx)?,
                4 => migration_v4(&tx)?,
                5 => migration_v5(&tx)?,
                _ => {
                    return Err(ContextError::validation(format!(
                        "unknown migration {version}"
                    )));
                }
            }
            tx.execute(
                "INSERT INTO schema_migrations (version, applied_at) VALUES (?, ?)",
                params![version, now_utc().to_rfc3339()],
            )?;
            tx.commit()?;
        }
        Ok(())
    }

    fn lock_conn(&self) -> ContextResult<std::sync::MutexGuard<'_, Connection>> {
        self.conn
            .lock()
            .map_err(|_| ContextError::validation("database mutex poisoned"))
    }
}

fn export_scope_refs(request: &ExportRequest) -> ContextResult<Option<Vec<ScopeRef>>> {
    if let Some(scope) = &request.scope {
        return Ok(Some(vec![scope.normalize()?]));
    }
    if request.project_scope_id.is_none() && request.task_scope_id.is_none() {
        return Ok(None);
    }

    let mut scopes = vec![ScopeRef::global()];
    if let Some(project_scope_id) = &request.project_scope_id {
        scopes.push(ScopeRef::normalized(ScopeKind::Project, project_scope_id)?);
    }
    if let Some(task_scope_id) = &request.task_scope_id {
        scopes.push(ScopeRef::normalized(ScopeKind::Task, task_scope_id)?);
    }
    Ok(Some(scopes))
}

fn export_scope_matches(scope: &ScopeRef, request: &ExportRequest) -> ContextResult<bool> {
    let Some(scopes) = export_scope_refs(request)? else {
        return Ok(true);
    };
    let normalized = scope.normalize()?;
    Ok(scopes.contains(&normalized))
}

fn export_review_matches(review: &ReviewItem, request: &ExportRequest) -> ContextResult<bool> {
    if !export_scope_matches(&review.scope, request)? {
        return Ok(false);
    }
    if let Some(pack_name) = &request.pack_name {
        return Ok(review.pack_name == *pack_name);
    }
    Ok(true)
}

fn export_run_matches(run: &RunRecord, request: &ExportRequest) -> ContextResult<bool> {
    if request.pack_name.is_some() {
        return Ok(false);
    }
    if let Some(scope) = &request.scope {
        let scope = scope.normalize()?;
        return Ok(match scope.kind {
            ScopeKind::Global => false,
            ScopeKind::Project => run.project_scope_id.as_deref() == Some(scope.id.as_str()),
            ScopeKind::Task => run.task_scope_id.as_deref() == Some(scope.id.as_str()),
        });
    }

    let mut matched_any = false;
    let mut matched = false;
    if let Some(project_scope_id) = &request.project_scope_id {
        let project_scope_id = ScopeKind::Project.normalize_id(project_scope_id)?;
        matched_any = true;
        matched |= run.project_scope_id.as_deref() == Some(project_scope_id.as_str());
    }
    if let Some(task_scope_id) = &request.task_scope_id {
        let task_scope_id = ScopeKind::Task.normalize_id(task_scope_id)?;
        matched_any = true;
        matched |= run.task_scope_id.as_deref() == Some(task_scope_id.as_str());
    }
    Ok(if matched_any { matched } else { true })
}

fn upsert_imported_pack_tx(
    tx: &Transaction<'_>,
    pack: &PackRecord,
    actor: &str,
) -> ContextResult<PackRecord> {
    validate_actor(actor)?;
    let scope = pack.scope.normalize()?;
    validate_pack_name(&pack.name)?;
    let lock_reason = if pack.locked {
        pack.lock_reason.clone()
    } else {
        None
    };
    reject_pack_write_for_storage(
        &scope,
        &pack.name,
        actor,
        pack.description.as_deref(),
        &pack.metadata,
        lock_reason.as_deref(),
    )?;
    if let Some(mut existing) = get_pack_tx(tx, &scope, &pack.name)? {
        tx.execute(
            "UPDATE packs SET description = ?, metadata_json = ?, status = ?, locked = ?, lock_reason = ?, created_at = ?, updated_at = ?, updated_by = ? WHERE id = ?",
            params![
                pack.description,
                to_json_text(&pack.metadata)?,
                pack.status.as_str(),
                bool_to_int(pack.locked),
                lock_reason,
                pack.created_at.to_rfc3339(),
                pack.updated_at.to_rfc3339(),
                actor,
                existing.id,
            ],
        )?;
        existing.record.description = pack.description.clone();
        existing.record.metadata = pack.metadata.clone();
        existing.record.status = pack.status.clone();
        existing.record.locked = pack.locked;
        existing.record.lock_reason = if pack.locked {
            pack.lock_reason.clone()
        } else {
            None
        };
        existing.record.created_at = pack.created_at;
        existing.record.updated_at = pack.updated_at;
        existing.record.revision_no = record_revision(
            tx,
            "pack",
            &existing.id,
            "import_update",
            &existing.record,
            &Provenance::system(actor.to_string(), "pack_import"),
            None,
            None,
        )?;
        tx.execute(
            "UPDATE packs SET current_revision_no = ? WHERE id = ?",
            params![existing.record.revision_no, existing.id],
        )?;
        Ok(existing.record)
    } else {
        let id = new_id();
        tx.execute(
            "INSERT INTO packs (id, scope_kind, scope_id, name, description, metadata_json, status, locked, lock_reason, created_at, updated_at, created_by, updated_by, current_revision_no) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 0)",
            params![
                id,
                scope.kind.as_str(),
                scope.id,
                pack.name,
                pack.description,
                to_json_text(&pack.metadata)?,
                pack.status.as_str(),
                bool_to_int(pack.locked),
                lock_reason,
                pack.created_at.to_rfc3339(),
                pack.updated_at.to_rfc3339(),
                actor,
                actor,
            ],
        )?;
        let mut record = PackRecord {
            id: id.clone(),
            scope,
            name: pack.name.clone(),
            description: pack.description.clone(),
            metadata: pack.metadata.clone(),
            status: pack.status.clone(),
            locked: pack.locked,
            lock_reason: if pack.locked {
                pack.lock_reason.clone()
            } else {
                None
            },
            created_at: pack.created_at,
            updated_at: pack.updated_at,
            revision_no: 0,
        };
        record.revision_no = record_revision(
            tx,
            "pack",
            &id,
            "import_create",
            &record,
            &Provenance::system(actor.to_string(), "pack_import"),
            None,
            None,
        )?;
        tx.execute(
            "UPDATE packs SET current_revision_no = ? WHERE id = ?",
            params![record.revision_no, id],
        )?;
        Ok(record)
    }
}

fn upsert_imported_run_tx(tx: &Transaction<'_>, run: &RunRecord) -> ContextResult<RunRecord> {
    validate_run_id(&run.id)?;
    if run.source.trim().is_empty() {
        return Err(ContextError::validation("run source must not be empty"));
    }
    let input = RunInput {
        id: Some(run.id.clone()),
        project_scope_id: run.project_scope_id.clone(),
        task_scope_id: run.task_scope_id.clone(),
        source: run.source.clone(),
        metadata: run.metadata.clone(),
    };
    reject_run_for_storage(&input)?;
    tx.execute(
        "INSERT INTO runs (id, project_scope_id, task_scope_id, source, metadata_json, started_at)
         VALUES (?, ?, ?, ?, ?, ?)
         ON CONFLICT(id) DO UPDATE SET
             project_scope_id = excluded.project_scope_id,
             task_scope_id = excluded.task_scope_id,
             source = excluded.source,
             metadata_json = excluded.metadata_json,
             started_at = excluded.started_at",
        params![
            run.id,
            run.project_scope_id,
            run.task_scope_id,
            run.source,
            to_json_text(&run.metadata)?,
            run.started_at.to_rfc3339(),
        ],
    )?;
    Ok(run.clone())
}

fn migration_v1(tx: &Transaction<'_>) -> ContextResult<()> {
    tx.execute_batch(
        r#"
        CREATE TABLE packs (
            id TEXT PRIMARY KEY NOT NULL,
            scope_kind TEXT NOT NULL CHECK (scope_kind IN ('global', 'project', 'task')),
            scope_id TEXT NOT NULL,
            name TEXT NOT NULL,
            description TEXT,
            metadata_json TEXT NOT NULL CHECK (json_valid(metadata_json)),
            status TEXT NOT NULL CHECK (status IN ('active', 'archived')),
            locked INTEGER NOT NULL CHECK (locked IN (0, 1)),
            lock_reason TEXT,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            created_by TEXT NOT NULL,
            updated_by TEXT NOT NULL,
            current_revision_no INTEGER NOT NULL DEFAULT 0,
            UNIQUE (scope_kind, scope_id, name)
        ) STRICT;

        CREATE TABLE entries (
            id TEXT PRIMARY KEY NOT NULL,
            pack_id TEXT NOT NULL REFERENCES packs(id) ON DELETE RESTRICT,
            entry_key TEXT NOT NULL,
            title TEXT,
            kind TEXT NOT NULL,
            value_format TEXT NOT NULL CHECK (value_format IN ('markdown', 'json')),
            markdown_body TEXT,
            json_body TEXT,
            tags_json TEXT NOT NULL CHECK (json_valid(tags_json)),
            metadata_json TEXT NOT NULL CHECK (json_valid(metadata_json)),
            provenance_json TEXT NOT NULL CHECK (json_valid(provenance_json)),
            content_hash TEXT NOT NULL,
            status TEXT NOT NULL CHECK (status IN ('active', 'deleted')),
            locked INTEGER NOT NULL CHECK (locked IN (0, 1)),
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            created_by TEXT NOT NULL,
            updated_by TEXT NOT NULL,
            current_revision_no INTEGER NOT NULL DEFAULT 0,
            UNIQUE (pack_id, entry_key),
            CHECK ((value_format = 'markdown' AND markdown_body IS NOT NULL AND json_body IS NULL) OR (value_format = 'json' AND json_body IS NOT NULL AND markdown_body IS NULL))
        ) STRICT;

        CREATE INDEX idx_packs_scope ON packs(scope_kind, scope_id, name);
        CREATE INDEX idx_entries_pack ON entries(pack_id, entry_key);
        "#,
    )?;
    Ok(())
}

fn migration_v2(tx: &Transaction<'_>) -> ContextResult<()> {
    tx.execute_batch(
        r#"
        CREATE TABLE revisions (
            id TEXT PRIMARY KEY NOT NULL,
            entity_type TEXT NOT NULL CHECK (entity_type IN ('pack', 'entry', 'review')),
            entity_id TEXT NOT NULL,
            revision_no INTEGER NOT NULL,
            action TEXT NOT NULL,
            snapshot_json TEXT NOT NULL CHECK (json_valid(snapshot_json)),
            provenance_json TEXT NOT NULL CHECK (json_valid(provenance_json)),
            commit_request_id TEXT,
            run_id TEXT,
            created_at TEXT NOT NULL,
            UNIQUE (entity_type, entity_id, revision_no)
        ) STRICT;

        CREATE INDEX idx_revisions_entity ON revisions(entity_type, entity_id, revision_no DESC);

        CREATE TABLE runs (
            id TEXT PRIMARY KEY NOT NULL,
            project_scope_id TEXT,
            task_scope_id TEXT,
            source TEXT NOT NULL,
            metadata_json TEXT NOT NULL CHECK (json_valid(metadata_json)),
            started_at TEXT NOT NULL
        ) STRICT;

        CREATE TABLE commits (
            request_id TEXT PRIMARY KEY NOT NULL,
            run_id TEXT,
            request_json TEXT NOT NULL CHECK (json_valid(request_json)),
            result_json TEXT NOT NULL CHECK (json_valid(result_json)),
            status TEXT NOT NULL CHECK (status IN ('applied', 'pending', 'partial', 'rejected', 'spooled')),
            created_at TEXT NOT NULL
        ) STRICT;
        "#,
    )?;
    Ok(())
}

fn migration_v3(tx: &Transaction<'_>) -> ContextResult<()> {
    tx.execute_batch(
        r#"
        CREATE TABLE review_items (
            id TEXT PRIMARY KEY NOT NULL,
            request_id TEXT NOT NULL,
            scope_kind TEXT NOT NULL CHECK (scope_kind IN ('global', 'project', 'task')),
            scope_id TEXT NOT NULL,
            pack_name TEXT NOT NULL,
            entry_key TEXT NOT NULL,
            state TEXT NOT NULL CHECK (state IN ('pending', 'approved', 'rejected')),
            reason TEXT NOT NULL CHECK (reason IN ('global_scope', 'conflict', 'locked')),
            proposed_entry_json TEXT NOT NULL CHECK (json_valid(proposed_entry_json)),
            existing_entry_json TEXT,
            resolution_note TEXT,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            current_revision_no INTEGER NOT NULL DEFAULT 0
        ) STRICT;

        CREATE INDEX idx_review_state ON review_items(state, created_at);

        CREATE VIRTUAL TABLE entry_fts USING fts5(
            entry_id UNINDEXED,
            scope_kind UNINDEXED,
            scope_id UNINDEXED,
            pack_name UNINDEXED,
            entry_key UNINDEXED,
            title,
            body,
            tags
        );
        "#,
    )?;
    Ok(())
}

fn migration_v4(tx: &Transaction<'_>) -> ContextResult<()> {
    tx.execute_batch(
        r#"
        DROP TABLE IF EXISTS entry_fts;
        CREATE VIRTUAL TABLE entry_fts USING fts5(
            entry_id UNINDEXED,
            scope_kind UNINDEXED,
            scope_id UNINDEXED,
            pack_name UNINDEXED,
            entry_key UNINDEXED,
            title,
            body,
            tags
        );
        INSERT INTO entry_fts (entry_id, scope_kind, scope_id, pack_name, entry_key, title, body, tags)
        SELECT e.id,
               p.scope_kind,
               p.scope_id,
               p.name,
               e.entry_key,
               COALESCE(e.title, ''),
               CASE e.value_format WHEN 'markdown' THEN COALESCE(e.markdown_body, '') ELSE COALESCE(e.json_body, '') END,
               COALESCE((SELECT GROUP_CONCAT(value, ' ') FROM json_each(e.tags_json)), '')
        FROM entries e
        JOIN packs p ON p.id = e.pack_id
        WHERE e.status = 'active';
        "#,
    )?;
    Ok(())
}

fn migration_v5(tx: &Transaction<'_>) -> ContextResult<()> {
    tx.execute_batch(
        r#"
        CREATE TABLE revisions_v5 (
            id TEXT PRIMARY KEY NOT NULL,
            entity_type TEXT NOT NULL CHECK (entity_type IN ('pack', 'entry', 'review', 'policy')),
            entity_id TEXT NOT NULL,
            revision_no INTEGER NOT NULL,
            action TEXT NOT NULL,
            snapshot_json TEXT NOT NULL CHECK (json_valid(snapshot_json)),
            provenance_json TEXT NOT NULL CHECK (json_valid(provenance_json)),
            commit_request_id TEXT,
            run_id TEXT,
            created_at TEXT NOT NULL,
            UNIQUE (entity_type, entity_id, revision_no)
        ) STRICT;

        INSERT INTO revisions_v5
        SELECT id, entity_type, entity_id, revision_no, action, snapshot_json, provenance_json,
               commit_request_id, run_id, created_at
        FROM revisions;
        DROP TABLE revisions;
        ALTER TABLE revisions_v5 RENAME TO revisions;
        CREATE INDEX idx_revisions_entity
            ON revisions(entity_type, entity_id, revision_no DESC);

        CREATE TABLE review_items_v5 (
            id TEXT PRIMARY KEY NOT NULL,
            request_id TEXT NOT NULL,
            scope_kind TEXT NOT NULL CHECK (scope_kind IN ('global', 'project', 'task')),
            scope_id TEXT NOT NULL,
            pack_name TEXT NOT NULL,
            entry_key TEXT NOT NULL,
            state TEXT NOT NULL CHECK (state IN ('pending', 'approved', 'rejected')),
            reason TEXT NOT NULL CHECK (reason IN ('global_scope', 'conflict', 'locked', 'strict_policy')),
            proposed_entry_json TEXT NOT NULL CHECK (json_valid(proposed_entry_json)),
            existing_entry_json TEXT,
            resolution_note TEXT,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            current_revision_no INTEGER NOT NULL DEFAULT 0
        ) STRICT;

        INSERT INTO review_items_v5
        SELECT id, request_id, scope_kind, scope_id, pack_name, entry_key, state, reason,
               proposed_entry_json, existing_entry_json, resolution_note, created_at, updated_at,
               current_revision_no
        FROM review_items;
        DROP TABLE review_items;
        ALTER TABLE review_items_v5 RENAME TO review_items;
        CREATE INDEX idx_review_state ON review_items(state, created_at);

        CREATE TABLE review_policy (
            policy_key TEXT PRIMARY KEY NOT NULL CHECK (policy_key = 'review'),
            review_mode TEXT NOT NULL CHECK (review_mode IN ('strict', 'balanced', 'fast')),
            metadata_json TEXT NOT NULL CHECK (json_valid(metadata_json)),
            updated_at TEXT NOT NULL,
            updated_by TEXT NOT NULL,
            current_revision_no INTEGER NOT NULL DEFAULT 0
        ) STRICT;
        "#,
    )?;

    let now = now_utc();
    let mut policy = ReviewPolicy {
        mode: ReviewMode::Balanced,
        metadata: default_json_object(),
        updated_at: now,
        updated_by: "system".to_string(),
        revision_no: 0,
    };
    tx.execute(
        "INSERT INTO review_policy (policy_key, review_mode, metadata_json, updated_at, updated_by, current_revision_no) VALUES ('review', 'balanced', ?, ?, ?, 0)",
        params![
            to_json_text(&policy.metadata)?,
            policy.updated_at.to_rfc3339(),
            policy.updated_by,
        ],
    )?;
    policy.revision_no = record_revision(
        tx,
        "policy",
        "review",
        "create",
        &policy,
        &Provenance::system("system", "migration_v5"),
        None,
        None,
    )?;
    tx.execute(
        "UPDATE review_policy SET current_revision_no = ? WHERE policy_key = 'review'",
        params![policy.revision_no],
    )?;
    Ok(())
}

fn current_schema_version(conn: &Connection) -> ContextResult<i64> {
    Ok(conn.query_row(
        "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
        [],
        |row| row.get(0),
    )?)
}

fn count_table(conn: &Connection, table: &str) -> ContextResult<usize> {
    let sql = format!("SELECT COUNT(*) FROM {table}");
    Ok(conn
        .query_row(&sql, [], |row| row.get::<_, i64>(0))
        .map(|v| v as usize)?)
}

#[allow(clippy::too_many_arguments)]
fn ensure_pack_tx(
    tx: &Transaction<'_>,
    scope: &ScopeRef,
    name: &str,
    actor: &str,
    description: Option<String>,
    metadata: Value,
    locked: bool,
    lock_reason: Option<String>,
) -> ContextResult<PackRecord> {
    validate_pack_name(name)?;
    if let Some(existing) = get_pack_tx(tx, scope, name)? {
        return Ok(existing.record);
    }
    reject_pack_write_for_storage(
        scope,
        name,
        actor,
        description.as_deref(),
        &metadata,
        lock_reason.as_deref(),
    )?;
    let now = now_utc();
    let id = new_id();
    tx.execute(
        "INSERT INTO packs (id, scope_kind, scope_id, name, description, metadata_json, status, locked, lock_reason, created_at, updated_at, created_by, updated_by, current_revision_no) VALUES (?, ?, ?, ?, ?, ?, 'active', ?, ?, ?, ?, ?, ?, 0)",
        params![
            id,
            scope.kind.as_str(),
            scope.id,
            name,
            description,
            to_json_text(&metadata)?,
            bool_to_int(locked),
            lock_reason,
            now.to_rfc3339(),
            now.to_rfc3339(),
            actor,
            actor,
        ],
    )?;
    let mut record = PackRecord {
        id: id.clone(),
        scope: scope.clone(),
        name: name.to_string(),
        description,
        metadata,
        status: PackStatus::Active,
        locked,
        lock_reason,
        created_at: now,
        updated_at: now,
        revision_no: 0,
    };
    record.revision_no = record_revision(
        tx,
        "pack",
        &id,
        "create",
        &record,
        &Provenance::system(actor.to_string(), "pack_create"),
        None,
        None,
    )?;
    tx.execute(
        "UPDATE packs SET current_revision_no = ? WHERE id = ?",
        params![record.revision_no, id],
    )?;
    Ok(record)
}

#[allow(clippy::too_many_arguments)]
fn apply_entry_tx(
    tx: &Transaction<'_>,
    scope: &ScopeRef,
    pack_name: &str,
    entry: &EntryInput,
    actor: &str,
    provenance: &Provenance,
    request_id: Option<&str>,
    run_id: Option<&str>,
) -> ContextResult<EntryRecord> {
    apply_entry_tx_inner(
        tx, scope, pack_name, entry, actor, provenance, request_id, run_id, false,
    )
}

#[allow(clippy::too_many_arguments)]
fn apply_entry_tx_inner(
    tx: &Transaction<'_>,
    scope: &ScopeRef,
    pack_name: &str,
    entry: &EntryInput,
    actor: &str,
    provenance: &Provenance,
    request_id: Option<&str>,
    run_id: Option<&str>,
    allow_archived_pack: bool,
) -> ContextResult<EntryRecord> {
    validate_actor(actor)?;
    validate_pack_name(pack_name)?;
    entry.validate()?;
    reject_entry_write_for_storage(
        scope, pack_name, actor, entry, provenance, request_id, run_id,
    )?;
    let pack = ensure_pack_tx(
        tx,
        scope,
        pack_name,
        actor,
        None,
        default_json_object(),
        false,
        None,
    )?;
    if !allow_archived_pack {
        reject_archived_pack_mutation(&pack, "write entries to")?;
    }
    let existing = get_entry_tx(tx, scope, pack_name, &entry.key)?;
    let now = now_utc();
    let content_hash = entry.content_hash();
    let (record_id, revision_action, created_at) = if let Some(existing) = &existing {
        tx.execute(
            "UPDATE entries SET title = ?, kind = ?, value_format = ?, markdown_body = ?, json_body = ?, tags_json = ?, metadata_json = ?, provenance_json = ?, content_hash = ?, status = 'active', locked = ?, updated_at = ?, updated_by = ? WHERE id = ?",
            params![
                entry.title,
                entry.kind,
                entry.value.format_name(),
                markdown_body(&entry.value),
                json_body(&entry.value),
                to_json_text(&entry.tags)?,
                to_json_text(&entry.metadata)?,
                to_json_text(provenance)?,
                content_hash,
                bool_to_int(entry.locked),
                now.to_rfc3339(),
                actor,
                existing.record.id,
            ],
        )?;
        (
            existing.record.id.clone(),
            "update",
            existing.record.created_at,
        )
    } else {
        let id = new_id();
        tx.execute(
            "INSERT INTO entries (id, pack_id, entry_key, title, kind, value_format, markdown_body, json_body, tags_json, metadata_json, provenance_json, content_hash, status, locked, created_at, updated_at, created_by, updated_by, current_revision_no) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 'active', ?, ?, ?, ?, ?, 0)",
            params![
                id,
                pack.id,
                entry.key,
                entry.title,
                entry.kind,
                entry.value.format_name(),
                markdown_body(&entry.value),
                json_body(&entry.value),
                to_json_text(&entry.tags)?,
                to_json_text(&entry.metadata)?,
                to_json_text(provenance)?,
                content_hash,
                bool_to_int(entry.locked),
                now.to_rfc3339(),
                now.to_rfc3339(),
                actor,
                actor,
            ],
        )?;
        (id, "create", now)
    };
    let mut record = EntryRecord {
        id: record_id.clone(),
        scope: scope.clone(),
        pack_name: pack.name,
        key: entry.key.clone(),
        title: entry.title.clone(),
        kind: entry.kind.clone(),
        value: entry.value.clone(),
        tags: entry.tags.clone(),
        metadata: entry.metadata.clone(),
        provenance: provenance.clone(),
        locked: entry.locked,
        status: EntryStatus::Active,
        created_at,
        updated_at: now,
        revision_no: 0,
    };
    record.revision_no = record_revision(
        tx,
        "entry",
        &record_id,
        revision_action,
        &record,
        provenance,
        request_id,
        run_id,
    )?;
    tx.execute(
        "UPDATE entries SET current_revision_no = ? WHERE id = ?",
        params![record.revision_no, record_id],
    )?;
    upsert_fts_tx(tx, &record)?;
    Ok(record)
}

fn prepare_source_import(
    request: &SourceImportPreviewRequest,
) -> ContextResult<PreparedSourceImport> {
    validate_actor(&request.actor)?;
    let destination = request.destination.normalize()?;
    let pack_name = request.pack_name.clone().unwrap_or_else(default_pack_name);
    validate_pack_name(&pack_name)?;
    let (candidates, warnings) = parse_source_import(request)?;
    Ok(PreparedSourceImport {
        destination,
        pack_name,
        candidates,
        warnings,
    })
}

fn build_source_import_preview(
    conn: &Connection,
    actor: &str,
    prepared: PreparedSourceImport,
) -> ContextResult<SourceImportPreview> {
    let PreparedSourceImport {
        destination,
        pack_name,
        candidates: parsed_candidates,
        mut warnings,
    } = prepared;
    let review_mode = get_review_policy_tx(conn)?.mode;
    let destination_pack = match get_pack_tx(conn, &destination, &pack_name)? {
        Some(pack) => SourceImportPackGovernance {
            exists: true,
            status: Some(pack.record.status),
            locked: pack.record.locked,
            lock_reason: pack.record.lock_reason,
            revision_no: Some(pack.record.revision_no),
        },
        None => SourceImportPackGovernance::default(),
    };
    let mut candidates = Vec::with_capacity(parsed_candidates.len());
    for (candidate_index, parsed) in parsed_candidates.into_iter().enumerate() {
        parsed.entry.validate()?;
        let provenance = parsed
            .entry
            .provenance
            .as_ref()
            .ok_or_else(|| ContextError::validation("source import provenance is required"))?;
        reject_entry_write_for_storage(
            &destination,
            &pack_name,
            actor,
            &parsed.entry,
            provenance,
            None,
            None,
        )?;
        let existing = get_entry_tx(conn, &destination, &pack_name, &parsed.entry.key)?;
        let disposition = match existing.as_ref() {
            Some(existing)
                if existing.record.status == EntryStatus::Active
                    && entry_record_matches_input(&existing.record, &parsed.entry) =>
            {
                SourceImportDisposition::Duplicate
            }
            Some(existing) if existing.record.status == EntryStatus::Active => {
                SourceImportDisposition::Conflict
            }
            _ => SourceImportDisposition::New,
        };
        let mut candidate_warnings = parsed.warnings;
        match disposition {
            SourceImportDisposition::Duplicate => candidate_warnings
                .push("an identical active entry already exists and will be skipped".to_string()),
            SourceImportDisposition::Conflict => candidate_warnings.push(
                "an active entry with the same key has different content or governance state"
                    .to_string(),
            ),
            SourceImportDisposition::New => {}
        }
        if destination_pack.status == Some(PackStatus::Archived) {
            candidate_warnings
                .push("destination pack is archived and cannot accept source imports".to_string());
        } else if destination_pack.locked && disposition != SourceImportDisposition::Duplicate {
            candidate_warnings
                .push("destination pack is locked; this candidate will require review".to_string());
        }
        candidates.push(SourceImportCandidate {
            candidate_index,
            document_index: parsed.document_index,
            source_path: parsed.source_path,
            detected_source_kind: parsed.detected_source_kind,
            entry: parsed.entry,
            disposition,
            existing_entry_id: existing.as_ref().map(|row| row.record.id.clone()),
            existing_revision_no: existing.as_ref().map(|row| row.record.revision_no),
            warnings: candidate_warnings,
        });
    }
    let mut seen_keys = BTreeSet::new();
    let duplicate_keys = candidates
        .iter()
        .filter_map(|candidate| {
            if seen_keys.insert(candidate.entry.key.clone()) {
                None
            } else {
                Some(candidate.entry.key.clone())
            }
        })
        .collect::<BTreeSet<_>>();
    let mut apply_allowed = duplicate_keys.is_empty();
    if !apply_allowed {
        warnings.push(format!(
            "multiple candidates map to the same destination key: {}",
            duplicate_keys.into_iter().collect::<Vec<_>>().join(", ")
        ));
    }
    if destination_pack.status == Some(PackStatus::Archived) {
        apply_allowed = false;
        warnings.push(format!(
            "destination pack {pack_name} is archived; unarchive it before applying source import"
        ));
    } else if destination_pack.locked
        && candidates
            .iter()
            .any(|candidate| candidate.disposition != SourceImportDisposition::Duplicate)
    {
        warnings.push(format!(
            "destination pack {pack_name} is locked; non-duplicate candidates will require review"
        ));
    }

    let mut preview = SourceImportPreview {
        destination,
        pack_name,
        review_mode,
        destination_pack,
        preview_fingerprint: None,
        candidates,
        warnings,
        apply_allowed,
    };
    preview.preview_fingerprint = Some(source_import_preview_fingerprint(actor, &preview)?);
    Ok(preview)
}

fn source_import_preview_fingerprint(
    actor: &str,
    preview: &SourceImportPreview,
) -> ContextResult<String> {
    let canonical = json!({
        "contract": "source_import_preview_v1",
        "destination": &preview.destination,
        "pack_name": &preview.pack_name,
        "actor": actor,
        "review_mode": preview.review_mode,
        "destination_pack": &preview.destination_pack,
        "candidates": &preview.candidates,
        "warnings": &preview.warnings,
        "apply_allowed": preview.apply_allowed,
    });
    let mut hasher = Sha256::new();
    hasher.update(serde_json::to_vec(&canonicalize_json_value(canonical))?);
    Ok(format!("source-import-preview-v1:{:x}", hasher.finalize()))
}

fn canonicalize_json_value(value: Value) -> Value {
    match value {
        Value::Array(values) => {
            Value::Array(values.into_iter().map(canonicalize_json_value).collect())
        }
        Value::Object(values) => {
            let mut entries = values.into_iter().collect::<Vec<_>>();
            entries.sort_by(|left, right| left.0.cmp(&right.0));
            Value::Object(
                entries
                    .into_iter()
                    .map(|(key, value)| (key, canonicalize_json_value(value)))
                    .collect(),
            )
        }
        other => other,
    }
}

fn commit_work_tx(
    tx: &Transaction<'_>,
    request: CommitWorkRequest,
    duplicate_check: DuplicateCheck,
) -> ContextResult<CommitWorkResult> {
    validate_request_id(&request.request_id)?;
    validate_actor(&request.actor)?;
    if request.proposals.is_empty() {
        return Err(ContextError::validation("proposals must not be empty"));
    }
    reject_commit_metadata_for_storage(&request)?;
    if let Some(existing) = get_commit_result_tx(tx, &request.request_id)? {
        return Ok(existing);
    }
    let review_mode = get_review_policy_tx(tx)?.mode;
    let run_id = if let Some(run) = request.run.clone() {
        Some(ensure_run_tx(tx, run)?.id)
    } else {
        None
    };
    let mut items = Vec::new();
    for proposal in &request.proposals {
        let scope = proposal.scope.normalize()?;
        validate_pack_name(&proposal.pack_name)?;
        proposal.entry.validate()?;
        let provenance = proposal
            .entry
            .provenance
            .clone()
            .unwrap_or_else(|| Provenance {
                actor: request.actor.clone(),
                source: "commit_work".to_string(),
                source_ref: None,
                run_id: run_id.clone(),
                request_id: Some(request.request_id.clone()),
                note: None,
            });
        let item = commit_proposal_tx(
            tx,
            &request.request_id,
            run_id.as_deref(),
            &scope,
            &proposal.pack_name,
            &proposal.entry,
            &request.actor,
            &provenance,
            review_mode,
            duplicate_check,
        )?;
        items.push(item);
    }
    let status = summarize_commit_status(&items);
    let result = CommitWorkResult {
        request_id: request.request_id.clone(),
        status,
        run_id,
        items,
        spooled: false,
        spool_path: None,
    };
    tx.execute(
        "INSERT INTO commits (request_id, run_id, request_json, result_json, status, created_at) VALUES (?, ?, ?, ?, ?, ?)",
        params![
            result.request_id,
            result.run_id,
            to_json_text(&redacted_commit_request(&request))?,
            to_json_text(&result)?,
            commit_status_str(&result.status),
            now_utc().to_rfc3339(),
        ],
    )?;
    Ok(result)
}

#[allow(clippy::too_many_arguments)]
fn commit_proposal_tx(
    tx: &Transaction<'_>,
    request_id: &str,
    run_id: Option<&str>,
    scope: &ScopeRef,
    pack_name: &str,
    entry: &EntryInput,
    actor: &str,
    provenance: &Provenance,
    review_mode: ReviewMode,
    duplicate_check: DuplicateCheck,
) -> ContextResult<CommitItemResult> {
    let existing = get_entry_tx(tx, scope, pack_name, &entry.key)?;
    if let Err(err) = reject_entry_write_for_storage(
        scope,
        pack_name,
        actor,
        entry,
        provenance,
        Some(request_id),
        run_id,
    ) {
        return Ok(CommitItemResult {
            scope: scope.clone(),
            pack_name: pack_name.to_string(),
            entry_key: entry.key.clone(),
            disposition: CommitDisposition::Rejected,
            reason: Some(err.to_string()),
            entry_id: existing.as_ref().map(|row| row.record.id.clone()),
            review_id: None,
        });
    }
    let pack = if duplicate_check == DuplicateCheck::ExactCandidate {
        get_pack_tx(tx, scope, pack_name)?.map(|row| row.record)
    } else {
        Some(ensure_pack_tx(
            tx,
            scope,
            pack_name,
            actor,
            None,
            default_json_object(),
            false,
            None,
        )?)
    };
    if let Some(pack) = &pack {
        if let Err(err) = reject_archived_pack_mutation(pack, "write entries to") {
            return Ok(CommitItemResult {
                scope: scope.clone(),
                pack_name: pack_name.to_string(),
                entry_key: entry.key.clone(),
                disposition: CommitDisposition::Rejected,
                reason: Some(err.to_string()),
                entry_id: existing.as_ref().map(|row| row.record.id.clone()),
                review_id: None,
            });
        }
    }
    let content_hash = entry.content_hash();
    let is_duplicate = existing
        .as_ref()
        .map(|row| {
            row.record.status == EntryStatus::Active
                && if review_mode == ReviewMode::Strict
                    || duplicate_check == DuplicateCheck::ExactCandidate
                {
                    entry_record_matches_input(&row.record, entry)
                } else {
                    row.content_hash == content_hash
                }
        })
        .unwrap_or(false);
    if review_mode == ReviewMode::Strict && is_duplicate {
        return Ok(CommitItemResult {
            scope: scope.clone(),
            pack_name: pack_name.to_string(),
            entry_key: entry.key.clone(),
            disposition: CommitDisposition::Duplicate,
            reason: None,
            entry_id: existing.as_ref().map(|row| row.record.id.clone()),
            review_id: None,
        });
    }
    if scope.kind == ScopeKind::Global {
        let review = insert_or_reuse_review_item_tx(
            tx,
            request_id,
            scope,
            pack_name,
            entry,
            existing.map(|row| row.record),
            ReviewReason::GlobalScope,
            duplicate_check,
        )?;
        return Ok(CommitItemResult {
            scope: scope.clone(),
            pack_name: pack_name.to_string(),
            entry_key: entry.key.clone(),
            disposition: CommitDisposition::Pending,
            reason: Some(ReviewReason::GlobalScope.as_str().to_string()),
            entry_id: None,
            review_id: Some(review.id),
        });
    }
    if pack.as_ref().map(|pack| pack.locked).unwrap_or(false)
        || existing
            .as_ref()
            .map(|row| row.record.locked)
            .unwrap_or(false)
    {
        let review = insert_or_reuse_review_item_tx(
            tx,
            request_id,
            scope,
            pack_name,
            entry,
            existing.map(|row| row.record),
            ReviewReason::Locked,
            duplicate_check,
        )?;
        return Ok(CommitItemResult {
            scope: scope.clone(),
            pack_name: pack_name.to_string(),
            entry_key: entry.key.clone(),
            disposition: CommitDisposition::Pending,
            reason: Some(ReviewReason::Locked.as_str().to_string()),
            entry_id: None,
            review_id: Some(review.id),
        });
    }
    if review_mode == ReviewMode::Strict {
        let reason = if existing
            .as_ref()
            .map(|row| row.record.status == EntryStatus::Active)
            .unwrap_or(false)
        {
            ReviewReason::Conflict
        } else {
            ReviewReason::StrictPolicy
        };
        let review = insert_or_reuse_review_item_tx(
            tx,
            request_id,
            scope,
            pack_name,
            entry,
            existing.map(|row| row.record),
            reason.clone(),
            duplicate_check,
        )?;
        return Ok(CommitItemResult {
            scope: scope.clone(),
            pack_name: pack_name.to_string(),
            entry_key: entry.key.clone(),
            disposition: CommitDisposition::Pending,
            reason: Some(reason.as_str().to_string()),
            entry_id: None,
            review_id: Some(review.id),
        });
    }
    if let Some(existing) = existing {
        if is_duplicate {
            return Ok(CommitItemResult {
                scope: scope.clone(),
                pack_name: pack_name.to_string(),
                entry_key: entry.key.clone(),
                disposition: CommitDisposition::Duplicate,
                reason: None,
                entry_id: Some(existing.record.id),
                review_id: None,
            });
        }
        if review_mode == ReviewMode::Balanced && existing.record.status == EntryStatus::Active {
            let review = insert_or_reuse_review_item_tx(
                tx,
                request_id,
                scope,
                pack_name,
                entry,
                Some(existing.record),
                ReviewReason::Conflict,
                duplicate_check,
            )?;
            return Ok(CommitItemResult {
                scope: scope.clone(),
                pack_name: pack_name.to_string(),
                entry_key: entry.key.clone(),
                disposition: CommitDisposition::Pending,
                reason: Some(ReviewReason::Conflict.as_str().to_string()),
                entry_id: None,
                review_id: Some(review.id),
            });
        }
    }
    let applied = apply_entry_tx(
        tx,
        scope,
        pack_name,
        entry,
        actor,
        provenance,
        Some(request_id),
        run_id,
    )?;
    Ok(CommitItemResult {
        scope: scope.clone(),
        pack_name: pack_name.to_string(),
        entry_key: entry.key.clone(),
        disposition: CommitDisposition::Applied,
        reason: None,
        entry_id: Some(applied.id),
        review_id: None,
    })
}

fn apply_review_edit_fields(
    review: &mut ReviewItem,
    request: &ReviewEditRequest,
) -> ContextResult<()> {
    if let Some(title) = &request.title {
        review.proposed_entry.title = Some(title.clone());
    }
    if let Some(kind) = &request.kind {
        review.proposed_entry.kind = kind.clone();
    }
    if let Some(value) = &request.value {
        review.proposed_entry.value = value.clone();
    }
    if let Some(tags) = &request.tags {
        review.proposed_entry.tags = tags.clone();
    }
    if let Some(metadata) = &request.metadata {
        review.proposed_entry.metadata = metadata.clone();
    }
    if let Some(locked) = request.locked {
        review.proposed_entry.locked = locked;
    }
    review.proposed_entry.validate()?;
    reject_review_for_storage(review)
}

#[allow(clippy::too_many_arguments)]
fn insert_or_reuse_review_item_tx(
    tx: &Transaction<'_>,
    request_id: &str,
    scope: &ScopeRef,
    pack_name: &str,
    entry: &EntryInput,
    existing_entry: Option<EntryRecord>,
    reason: ReviewReason,
    duplicate_check: DuplicateCheck,
) -> ContextResult<ReviewItem> {
    if duplicate_check == DuplicateCheck::ExactCandidate {
        if let Some(review) = find_matching_pending_review_tx(
            tx,
            scope,
            pack_name,
            entry,
            existing_entry.as_ref(),
            &reason,
        )? {
            return Ok(review);
        }
    }
    insert_review_item_tx(
        tx,
        request_id,
        scope,
        pack_name,
        entry,
        existing_entry,
        reason,
    )
}

fn insert_review_item_tx(
    tx: &Transaction<'_>,
    request_id: &str,
    scope: &ScopeRef,
    pack_name: &str,
    entry: &EntryInput,
    existing_entry: Option<EntryRecord>,
    reason: ReviewReason,
) -> ContextResult<ReviewItem> {
    let id = new_id();
    let now = now_utc();
    let mut record = ReviewItem {
        id: id.clone(),
        request_id: request_id.to_string(),
        scope: scope.clone(),
        pack_name: pack_name.to_string(),
        entry_key: entry.key.clone(),
        state: ReviewState::Pending,
        reason: reason.clone(),
        proposed_entry: entry.clone(),
        existing_entry,
        resolution_note: None,
        created_at: now,
        updated_at: now,
        revision_no: 0,
    };
    reject_review_for_storage(&record)?;
    tx.execute(
        "INSERT INTO review_items (id, request_id, scope_kind, scope_id, pack_name, entry_key, state, reason, proposed_entry_json, existing_entry_json, resolution_note, created_at, updated_at, current_revision_no) VALUES (?, ?, ?, ?, ?, ?, 'pending', ?, ?, ?, NULL, ?, ?, 0)",
        params![
            id,
            request_id,
            scope.kind.as_str(),
            scope.id,
            pack_name,
            entry.key,
            record.reason.as_str(),
            to_json_text(entry)?,
            optional_json_text(record.existing_entry.as_ref())?,
            now.to_rfc3339(),
            now.to_rfc3339(),
        ],
    )?;
    record.revision_no = record_revision(
        tx,
        "review",
        &id,
        "create",
        &record,
        &Provenance::system("system", "review_create"),
        Some(request_id),
        None,
    )?;
    tx.execute(
        "UPDATE review_items SET current_revision_no = ? WHERE id = ?",
        params![record.revision_no, id],
    )?;
    Ok(record)
}

fn update_review_tx(
    tx: &Transaction<'_>,
    record: &ReviewItem,
    actor: &str,
    action: &str,
) -> ContextResult<i64> {
    validate_actor(actor)?;
    reject_review_transition_for_storage(record, actor)?;
    tx.execute(
        "UPDATE review_items SET state = ?, proposed_entry_json = ?, existing_entry_json = ?, resolution_note = ?, updated_at = ?, current_revision_no = current_revision_no WHERE id = ?",
        params![
            record.state.as_str(),
            to_json_text(&record.proposed_entry)?,
            optional_json_text(record.existing_entry.as_ref())?,
            record.resolution_note,
            record.updated_at.to_rfc3339(),
            record.id,
        ],
    )?;
    let revision_no = record_revision(
        tx,
        "review",
        &record.id,
        action,
        record,
        &Provenance::system(actor.to_string(), format!("review_{action}")),
        Some(&record.request_id),
        None,
    )?;
    tx.execute(
        "UPDATE review_items SET current_revision_no = ? WHERE id = ?",
        params![revision_no, record.id],
    )?;
    Ok(revision_no)
}

fn upsert_imported_review_item_tx(
    tx: &Transaction<'_>,
    review: &ReviewItem,
    actor: &str,
) -> ContextResult<()> {
    validate_actor(actor)?;
    validate_imported_review(review)?;
    reject_review_for_storage(review)?;
    let revision_no = review.revision_no.max(1);
    tx.execute(
        "INSERT INTO review_items (id, request_id, scope_kind, scope_id, pack_name, entry_key, state, reason, proposed_entry_json, existing_entry_json, resolution_note, created_at, updated_at, current_revision_no)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT(id) DO UPDATE SET
             request_id = excluded.request_id,
             scope_kind = excluded.scope_kind,
             scope_id = excluded.scope_id,
             pack_name = excluded.pack_name,
             entry_key = excluded.entry_key,
             state = excluded.state,
             reason = excluded.reason,
             proposed_entry_json = excluded.proposed_entry_json,
             existing_entry_json = excluded.existing_entry_json,
             resolution_note = excluded.resolution_note,
             created_at = excluded.created_at,
             updated_at = excluded.updated_at,
             current_revision_no = excluded.current_revision_no",
        params![
            review.id,
            review.request_id,
            review.scope.kind.as_str(),
            review.scope.id,
            review.pack_name,
            review.entry_key,
            review.state.as_str(),
            review.reason.as_str(),
            to_json_text(&review.proposed_entry)?,
            optional_json_text(review.existing_entry.as_ref())?,
            review.resolution_note,
            review.created_at.to_rfc3339(),
            review.updated_at.to_rfc3339(),
            revision_no,
        ],
    )?;
    tx.execute(
        "INSERT INTO revisions (id, entity_type, entity_id, revision_no, action, snapshot_json, provenance_json, commit_request_id, run_id, created_at)
         VALUES (?, 'review', ?, ?, ?, ?, ?, ?, NULL, ?)
         ON CONFLICT(entity_type, entity_id, revision_no) DO UPDATE SET
             action = excluded.action,
             snapshot_json = excluded.snapshot_json,
             provenance_json = excluded.provenance_json,
             commit_request_id = excluded.commit_request_id,
             created_at = excluded.created_at",
        params![
            new_id(),
            review.id,
            revision_no,
            imported_review_revision_action(review),
            to_json_text(review)?,
            to_json_text(&Provenance::system(actor.to_string(), "review_import"))?,
            review.request_id,
            review.updated_at.to_rfc3339(),
        ],
    )?;
    Ok(())
}

fn imported_review_revision_action(review: &ReviewItem) -> &'static str {
    match review.state {
        ReviewState::Pending => "import_pending",
        ReviewState::Approved => "import_approved",
        ReviewState::Rejected => "import_rejected",
    }
}

fn ensure_run_tx(tx: &Transaction<'_>, input: RunInput) -> ContextResult<RunRecord> {
    if input.source.trim().is_empty() {
        return Err(ContextError::validation("run source must not be empty"));
    }
    if let Some(id) = &input.id {
        validate_run_id(id)?;
    }
    let id = input.id.clone().unwrap_or_else(new_id);
    if let Some(existing) = tx
        .query_row(
            "SELECT id, project_scope_id, task_scope_id, source, metadata_json, started_at FROM runs WHERE id = ?",
            params![id.clone()],
            |row| {
                Ok(RunRecord {
                    id: row.get(0)?,
                    project_scope_id: row.get(1)?,
                    task_scope_id: row.get(2)?,
                    source: row.get(3)?,
                    metadata: from_json_text(&row.get::<_, String>(4)?).map_err(to_sql_err)?,
                    started_at: parse_ts(&row.get::<_, String>(5)?).map_err(to_sql_err)?,
                })
            },
        )
        .optional()?
    {
        return Ok(existing);
    }
    reject_run_for_storage(&input)?;
    let now = now_utc();
    tx.execute(
        "INSERT INTO runs (id, project_scope_id, task_scope_id, source, metadata_json, started_at) VALUES (?, ?, ?, ?, ?, ?)",
        params![
            id,
            input.project_scope_id,
            input.task_scope_id,
            input.source,
            to_json_text(&input.metadata)?,
            now.to_rfc3339(),
        ],
    )?;
    Ok(RunRecord {
        id,
        project_scope_id: input.project_scope_id,
        task_scope_id: input.task_scope_id,
        source: input.source,
        metadata: input.metadata,
        started_at: now,
    })
}

#[allow(clippy::too_many_arguments)]
fn record_revision<T: Serialize>(
    tx: &Transaction<'_>,
    entity_type: &str,
    entity_id: &str,
    action: &str,
    snapshot: &T,
    provenance: &Provenance,
    commit_request_id: Option<&str>,
    run_id: Option<&str>,
) -> ContextResult<i64> {
    reject_revision_metadata_for_storage(provenance, commit_request_id, run_id)?;
    let revision_no: i64 = tx.query_row(
        "SELECT COALESCE(MAX(revision_no), 0) + 1 FROM revisions WHERE entity_type = ? AND entity_id = ?",
        params![entity_type, entity_id],
        |row| row.get(0),
    )?;
    tx.execute(
        "INSERT INTO revisions (id, entity_type, entity_id, revision_no, action, snapshot_json, provenance_json, commit_request_id, run_id, created_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        params![
            new_id(),
            entity_type,
            entity_id,
            revision_no,
            action,
            to_json_text(snapshot)?,
            to_json_text(provenance)?,
            commit_request_id,
            run_id,
            now_utc().to_rfc3339(),
        ],
    )?;
    Ok(revision_no)
}

fn reject_archived_pack_mutation(pack: &PackRecord, action: &str) -> ContextResult<()> {
    if pack.status == PackStatus::Archived {
        return Err(ContextError::conflict(format!(
            "cannot {action} archived pack {} ({})",
            pack.name,
            pack.scope.label()
        )));
    }
    Ok(())
}

fn reject_archived_pack_update(
    pack: &PackRecord,
    request: &UpdatePackRequest,
) -> ContextResult<()> {
    if pack.status != PackStatus::Archived {
        return Ok(());
    }
    let only_unarchive = matches!(request.status, Some(PackStatus::Active))
        && request.description.is_none()
        && request.metadata.is_none()
        && request.locked.is_none()
        && request.lock_reason.is_none();
    if only_unarchive {
        return Ok(());
    }
    Err(ContextError::conflict(format!(
        "cannot mutate archived pack {} ({}); unarchive first",
        pack.name,
        pack.scope.label()
    )))
}

fn get_review_policy_tx(conn: &Connection) -> ContextResult<ReviewPolicy> {
    conn.query_row(
        "SELECT review_mode, metadata_json, updated_at, updated_by, current_revision_no FROM review_policy WHERE policy_key = 'review'",
        [],
        |row| {
            Ok(ReviewPolicy {
                mode: row
                    .get::<_, String>(0)?
                    .parse::<ReviewMode>()
                    .map_err(to_sql_err)?,
                metadata: from_json_text(&row.get::<_, String>(1)?).map_err(to_sql_err)?,
                updated_at: parse_ts(&row.get::<_, String>(2)?).map_err(to_sql_err)?,
                updated_by: row.get(3)?,
                revision_no: row.get(4)?,
            })
        },
    )
    .map_err(ContextError::from)
}

fn get_pack_tx(conn: &Connection, scope: &ScopeRef, name: &str) -> ContextResult<Option<PackRow>> {
    conn.query_row(
        "SELECT id, scope_kind, scope_id, name, description, metadata_json, status, locked, lock_reason, created_at, updated_at, current_revision_no FROM packs WHERE scope_kind = ? AND scope_id = ? AND name = ?",
        params![scope.kind.as_str(), scope.id, name],
        map_pack_row,
    )
    .optional()
    .map_err(ContextError::from)
}

fn get_entry_tx(
    conn: &Connection,
    scope: &ScopeRef,
    pack_name: &str,
    key: &str,
) -> ContextResult<Option<EntryRow>> {
    conn.query_row(
        "SELECT e.id, p.scope_kind, p.scope_id, p.name, e.entry_key, e.title, e.kind, e.value_format, e.markdown_body, e.json_body, e.tags_json, e.metadata_json, e.provenance_json, e.locked, e.status, e.created_at, e.updated_at, e.current_revision_no, e.pack_id, e.content_hash FROM entries e JOIN packs p ON p.id = e.pack_id WHERE p.scope_kind = ? AND p.scope_id = ? AND p.name = ? AND e.entry_key = ?",
        params![scope.kind.as_str(), scope.id, pack_name, key],
        map_entry_row,
    )
    .optional()
    .map_err(ContextError::from)
}

fn get_review_tx(conn: &Connection, review_id: &str) -> ContextResult<Option<ReviewRow>> {
    conn.query_row(
        "SELECT id, request_id, scope_kind, scope_id, pack_name, entry_key, state, reason, proposed_entry_json, existing_entry_json, resolution_note, created_at, updated_at, current_revision_no FROM review_items WHERE id = ?",
        params![review_id],
        map_review_row,
    )
    .optional()
    .map_err(ContextError::from)
}

fn find_matching_pending_review_tx(
    conn: &Connection,
    scope: &ScopeRef,
    pack_name: &str,
    entry: &EntryInput,
    existing_entry: Option<&EntryRecord>,
    reason: &ReviewReason,
) -> ContextResult<Option<ReviewItem>> {
    let mut stmt = conn.prepare(
        "SELECT id, request_id, scope_kind, scope_id, pack_name, entry_key, state, reason, proposed_entry_json, existing_entry_json, resolution_note, created_at, updated_at, current_revision_no
         FROM review_items
         WHERE state = 'pending' AND scope_kind = ? AND scope_id = ? AND pack_name = ? AND entry_key = ? AND reason = ?
         ORDER BY created_at, id",
    )?;
    let rows = stmt.query_map(
        params![
            scope.kind.as_str(),
            scope.id,
            pack_name,
            entry.key,
            reason.as_str(),
        ],
        map_review_row,
    )?;
    for row in rows {
        let review = row?.record;
        if entry_inputs_match(&review.proposed_entry, entry)
            && review.existing_entry.as_ref() == existing_entry
        {
            return Ok(Some(review));
        }
    }
    Ok(None)
}

fn get_commit_result_tx(
    conn: &Connection,
    request_id: &str,
) -> ContextResult<Option<CommitWorkResult>> {
    conn.query_row(
        "SELECT result_json FROM commits WHERE request_id = ?",
        params![request_id],
        |row| row.get::<_, String>(0),
    )
    .optional()?
    .map(|json| serde_json::from_str(&json).map_err(ContextError::from))
    .transpose()
}

fn map_pack_row(row: &Row<'_>) -> rusqlite::Result<PackRow> {
    let id: String = row.get(0)?;
    let scope = ScopeRef::normalized(
        row.get::<_, String>(1)?.parse().map_err(to_sql_err)?,
        row.get::<_, String>(2)?,
    )
    .map_err(to_sql_err)?;
    Ok(PackRow {
        id: id.clone(),
        record: PackRecord {
            id,
            scope,
            name: row.get(3)?,
            description: row.get(4)?,
            metadata: from_json_text(&row.get::<_, String>(5)?).map_err(to_sql_err)?,
            status: row.get::<_, String>(6)?.parse().map_err(to_sql_err)?,
            locked: int_to_bool(row.get::<_, i64>(7)?),
            lock_reason: row.get(8)?,
            created_at: parse_ts(&row.get::<_, String>(9)?).map_err(to_sql_err)?,
            updated_at: parse_ts(&row.get::<_, String>(10)?).map_err(to_sql_err)?,
            revision_no: row.get(11)?,
        },
    })
}

fn map_entry_row(row: &Row<'_>) -> rusqlite::Result<EntryRow> {
    let id: String = row.get(0)?;
    let scope = ScopeRef::normalized(
        row.get::<_, String>(1)?.parse().map_err(to_sql_err)?,
        row.get::<_, String>(2)?,
    )
    .map_err(to_sql_err)?;
    let value_format: String = row.get(7)?;
    let value = match value_format.as_str() {
        "markdown" => EntryValue::Markdown {
            body: row.get::<_, Option<String>>(8)?.unwrap_or_default(),
        },
        "json" => EntryValue::Json {
            value: from_json_text(
                &row.get::<_, Option<String>>(9)?
                    .unwrap_or_else(|| "null".to_string()),
            )
            .map_err(to_sql_err)?,
        },
        other => {
            return Err(to_sql_err(ContextError::validation(format!(
                "unknown value format: {other}"
            ))));
        }
    };
    Ok(EntryRow {
        _pack_id: row.get(18)?,
        content_hash: row.get(19)?,
        record: EntryRecord {
            id,
            scope,
            pack_name: row.get(3)?,
            key: row.get(4)?,
            title: row.get(5)?,
            kind: row.get(6)?,
            value,
            tags: from_json_text(&row.get::<_, String>(10)?).map_err(to_sql_err)?,
            metadata: from_json_text(&row.get::<_, String>(11)?).map_err(to_sql_err)?,
            provenance: from_json_text(&row.get::<_, String>(12)?).map_err(to_sql_err)?,
            locked: int_to_bool(row.get::<_, i64>(13)?),
            status: row.get::<_, String>(14)?.parse().map_err(to_sql_err)?,
            created_at: parse_ts(&row.get::<_, String>(15)?).map_err(to_sql_err)?,
            updated_at: parse_ts(&row.get::<_, String>(16)?).map_err(to_sql_err)?,
            revision_no: row.get(17)?,
        },
    })
}

fn map_review_row(row: &Row<'_>) -> rusqlite::Result<ReviewRow> {
    Ok(ReviewRow {
        record: ReviewItem {
            id: row.get(0)?,
            request_id: row.get(1)?,
            scope: ScopeRef::normalized(
                row.get::<_, String>(2)?.parse().map_err(to_sql_err)?,
                row.get::<_, String>(3)?,
            )
            .map_err(to_sql_err)?,
            pack_name: row.get(4)?,
            entry_key: row.get(5)?,
            state: row.get::<_, String>(6)?.parse().map_err(to_sql_err)?,
            reason: row.get::<_, String>(7)?.parse().map_err(to_sql_err)?,
            proposed_entry: from_json_text(&row.get::<_, String>(8)?).map_err(to_sql_err)?,
            existing_entry: row
                .get::<_, Option<String>>(9)?
                .map(|text| from_json_text(&text).map_err(to_sql_err))
                .transpose()?,
            resolution_note: row.get(10)?,
            created_at: parse_ts(&row.get::<_, String>(11)?).map_err(to_sql_err)?,
            updated_at: parse_ts(&row.get::<_, String>(12)?).map_err(to_sql_err)?,
            revision_no: row.get(13)?,
        },
    })
}

fn upsert_fts_tx(tx: &Transaction<'_>, record: &EntryRecord) -> ContextResult<()> {
    delete_fts_tx(tx, &record.id)?;
    if record.status == EntryStatus::Deleted {
        return Ok(());
    }
    tx.execute(
        "INSERT INTO entry_fts (entry_id, scope_kind, scope_id, pack_name, entry_key, title, body, tags) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        params![
            record.id,
            record.scope.kind.as_str(),
            record.scope.id,
            record.pack_name,
            record.key,
            record.title.clone().unwrap_or_default(),
            record.value.search_text(),
            record.tags.join(" "),
        ],
    )?;
    Ok(())
}

fn delete_fts_tx(tx: &Transaction<'_>, entry_id: &str) -> ContextResult<()> {
    tx.execute(
        "DELETE FROM entry_fts WHERE entry_id = ?",
        params![entry_id],
    )?;
    Ok(())
}

fn list_compose_exclusions(
    conn: &Connection,
    request: &ComposeRequest,
) -> ContextResult<Vec<ComposeExclusion>> {
    let filters = compose_filters(request)?;
    let mut sql = String::from(
        "SELECT e.id, p.scope_kind, p.scope_id, p.name, e.entry_key, e.current_revision_no, e.status, p.status FROM entries e JOIN packs p ON p.id = e.pack_id WHERE (e.status = 'deleted'",
    );
    if !request.include_archived {
        sql.push_str(" OR p.status = 'archived'");
    }
    sql.push(')');
    if !filters.is_empty() {
        sql.push_str(" AND (");
        sql.push_str(&filters.join(" OR "));
        sql.push(')');
    }
    sql.push_str(
        " ORDER BY CASE p.scope_kind WHEN 'global' THEN 0 WHEN 'project' THEN 1 ELSE 2 END, p.scope_id, p.name, e.entry_key",
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], |row| {
        let entry_status: String = row.get(6)?;
        Ok(ComposeExclusion {
            entry_id: row.get(0)?,
            scope: ScopeRef::normalized(
                row.get::<_, String>(1)?.parse().map_err(to_sql_err)?,
                row.get::<_, String>(2)?,
            )
            .map_err(to_sql_err)?,
            pack_name: row.get(3)?,
            entry_key: row.get(4)?,
            revision_no: row.get(5)?,
            reason: if entry_status == EntryStatus::Deleted.as_str() {
                ComposeExclusionReason::DeletedEntry
            } else {
                ComposeExclusionReason::ArchivedPack
            },
        })
    })?;
    collect_rows(rows)
}

fn render_compose_markdown(sections: &[ComposeSection]) -> String {
    let mut out = String::new();
    for section in sections {
        out.push_str(&format!(
            "# {} / {}\n\n",
            section.scope.label(),
            section.pack_name
        ));
        for entry in &section.entries {
            out.push_str(&format!("## {} ({})\n\n", entry.key, entry.kind));
            if let Some(title) = &entry.title {
                out.push_str(&format!("**{}**\n\n", title));
            }
            out.push_str(&entry.value.render_markdown());
            out.push_str("\n\n");
        }
    }
    out.trim().to_string()
}

fn estimate_tokens(rendered: &str) -> usize {
    rendered.chars().count().div_ceil(4)
}

fn entry_record_matches_input(record: &EntryRecord, input: &EntryInput) -> bool {
    // Source-import apply adds its stable request ID after preview. Provenance
    // is therefore intentionally excluded while all durable entry fields and
    // governance state compare exactly in both stages.
    record.key == input.key
        && record.title == input.title
        && record.kind == input.kind
        && record.value == input.value
        && record.tags == input.tags
        && record.metadata == input.metadata
        && record.locked == input.locked
}

fn entry_inputs_match(left: &EntryInput, right: &EntryInput) -> bool {
    left.key == right.key
        && left.title == right.title
        && left.kind == right.kind
        && left.value == right.value
        && left.tags == right.tags
        && left.metadata == right.metadata
        && left.locked == right.locked
}

fn source_import_request_id(preview: &SourceImportPreview) -> ContextResult<String> {
    let canonical = json!({
        "contract": "source_import_apply_v2",
        "preview_fingerprint": preview.preview_fingerprint,
        "destination": preview.destination,
        "pack_name": preview.pack_name,
        "candidates": preview.candidates.iter().map(|candidate| {
            json!({
                "candidate_index": candidate.candidate_index,
                "document_index": candidate.document_index,
                "source_path": candidate.source_path,
                "detected_source_kind": candidate.detected_source_kind,
                "entry": {
                    "key": candidate.entry.key,
                    "title": candidate.entry.title,
                    "kind": candidate.entry.kind,
                    "value": candidate.entry.value,
                    "tags": candidate.entry.tags,
                    "metadata": candidate.entry.metadata,
                    "locked": candidate.entry.locked,
                },
            })
        }).collect::<Vec<_>>(),
    });
    let mut hasher = Sha256::new();
    hasher.update(serde_json::to_vec(&canonicalize_json_value(canonical))?);
    Ok(format!("source-import-{:x}", hasher.finalize()))
}

fn source_import_candidate_request_id(
    actor: &str,
    preview: &SourceImportPreview,
    candidate: &SourceImportCandidate,
) -> ContextResult<String> {
    let canonical = json!({
        "contract": "source_import_candidate_v2",
        "preview_fingerprint": preview.preview_fingerprint,
        "destination": preview.destination,
        "pack_name": preview.pack_name,
        "actor": actor,
        "review_mode": preview.review_mode,
        "destination_pack": preview.destination_pack,
        "candidate": candidate,
    });
    let mut hasher = Sha256::new();
    hasher.update(serde_json::to_vec(&canonicalize_json_value(canonical))?);
    Ok(format!("source-import-candidate-{:x}", hasher.finalize()))
}

fn redacted_commit_request(request: &CommitWorkRequest) -> Value {
    json!({
        "request_id": &request.request_id,
        "actor": &request.actor,
        "run": request.run.as_ref().map(|run| {
            json!({
                "id": &run.id,
                "project_scope_id": &run.project_scope_id,
                "task_scope_id": &run.task_scope_id,
                "source": &run.source,
                "has_metadata": !value_is_empty(&run.metadata),
            })
        }),
        "proposal_count": request.proposals.len(),
        "proposals": request.proposals.iter().map(|proposal| {
            json!({
                "scope_kind": proposal.scope.kind.as_str(),
                "scope_id": &proposal.scope.id,
                "pack_name": &proposal.pack_name,
                "entry_key": &proposal.entry.key,
                "entry_kind": &proposal.entry.kind,
                "locked": proposal.entry.locked,
            })
        }).collect::<Vec<_>>(),
    })
}

fn value_is_empty(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::Object(map) => map.is_empty(),
        Value::Array(items) => items.is_empty(),
        Value::String(text) => text.is_empty(),
        _ => false,
    }
}

fn ensure_private_dir_chain(path: &Path) -> ContextResult<()> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut created = Vec::new();
    let mut current = absolute.as_path();
    while !current.exists() {
        created.push(current.to_path_buf());
        let Some(parent) = current.parent() else {
            break;
        };
        current = parent;
    }
    fs::create_dir_all(&absolute)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for dir in created.iter().rev() {
            fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
        }
    }
    Ok(())
}

fn set_private_file_permissions(path: &Path) -> ContextResult<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if path.exists() {
            fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
        }
    }
    Ok(())
}

fn summarize_commit_status(items: &[CommitItemResult]) -> CommitStatus {
    let mut saw_applied = false;
    let mut saw_pending = false;
    let mut saw_rejected = false;
    for item in items {
        match item.disposition {
            CommitDisposition::Applied | CommitDisposition::Duplicate => saw_applied = true,
            CommitDisposition::Pending => saw_pending = true,
            CommitDisposition::Rejected => saw_rejected = true,
        }
    }
    match (saw_applied, saw_pending, saw_rejected) {
        (true, false, false) => CommitStatus::Applied,
        (false, true, false) => CommitStatus::Pending,
        (false, false, true) => CommitStatus::Rejected,
        _ => CommitStatus::Partial,
    }
}

fn commit_status_str(status: &CommitStatus) -> &'static str {
    match status {
        CommitStatus::Applied => "applied",
        CommitStatus::Pending => "pending",
        CommitStatus::Partial => "partial",
        CommitStatus::Rejected => "rejected",
        CommitStatus::Spooled => "spooled",
    }
}

fn compose_filters(request: &ComposeRequest) -> ContextResult<Vec<String>> {
    let mut filters = vec![format!(
        "(p.scope_kind = 'global' AND p.scope_id = {})",
        sql_quote(GLOBAL_SCOPE_ID)
    )];
    if let Some(project_scope_id) = &request.project_scope_id {
        let project = ScopeKind::Project.normalize_id(project_scope_id)?;
        filters.push(format!(
            "(p.scope_kind = 'project' AND p.scope_id = {})",
            sql_quote(&project)
        ));
    }
    if let Some(task_scope_id) = &request.task_scope_id {
        let task = ScopeKind::Task.normalize_id(task_scope_id)?;
        filters.push(format!(
            "(p.scope_kind = 'task' AND p.scope_id = {})",
            sql_quote(&task)
        ));
    }
    Ok(filters)
}

fn request_to_filters(request: &ExportRequest) -> ContextResult<Vec<String>> {
    let mut filters = Vec::new();
    if let Some(scope) = &request.scope {
        let normalized = scope.normalize()?;
        filters.push(format!(
            "(scope_kind = {} AND scope_id = {})",
            sql_quote(normalized.kind.as_str()),
            sql_quote(&normalized.id)
        ));
        return Ok(filters);
    }
    if request.project_scope_id.is_none() && request.task_scope_id.is_none() {
        return Ok(filters);
    }
    filters.push(format!(
        "(scope_kind = 'global' AND scope_id = {})",
        sql_quote(GLOBAL_SCOPE_ID)
    ));
    if let Some(project) = &request.project_scope_id {
        let normalized = ScopeKind::Project.normalize_id(project)?;
        filters.push(format!(
            "(scope_kind = 'project' AND scope_id = {})",
            sql_quote(&normalized)
        ));
    }
    if let Some(task) = &request.task_scope_id {
        let normalized = ScopeKind::Task.normalize_id(task)?;
        filters.push(format!(
            "(scope_kind = 'task' AND scope_id = {})",
            sql_quote(&normalized)
        ));
    }
    Ok(filters)
}

fn markdown_body(value: &EntryValue) -> Option<String> {
    match value {
        EntryValue::Markdown { body } => Some(body.clone()),
        EntryValue::Json { .. } => None,
    }
}

fn json_body(value: &EntryValue) -> Option<String> {
    match value {
        EntryValue::Json { value } => Some(value.to_string()),
        EntryValue::Markdown { .. } => None,
    }
}

fn parse_ts(text: &str) -> ContextResult<DateTime<Utc>> {
    Ok(DateTime::parse_from_rfc3339(text)
        .map_err(|err| ContextError::validation(format!("invalid timestamp {text}: {err}")))?
        .with_timezone(&Utc))
}

fn to_json_text<T: Serialize>(value: &T) -> ContextResult<String> {
    Ok(serde_json::to_string(value)?)
}

fn optional_json_text<T: Serialize>(value: Option<&T>) -> ContextResult<Option<String>> {
    value.map(to_json_text).transpose()
}

fn from_json_text<T: serde::de::DeserializeOwned>(text: &str) -> ContextResult<T> {
    Ok(serde_json::from_str(text)?)
}

fn int_to_bool(value: i64) -> bool {
    value != 0
}

fn bool_to_int(value: bool) -> i64 {
    if value { 1 } else { 0 }
}

fn to_sql_err(err: ContextError) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(err))
}

fn collect_rows<T>(
    rows: rusqlite::MappedRows<'_, impl FnMut(&Row<'_>) -> rusqlite::Result<T>>,
) -> ContextResult<Vec<T>> {
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

fn sql_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

fn validate_non_empty(label: &str, value: &str) -> ContextResult<()> {
    if value.trim().is_empty() {
        Err(ContextError::validation(format!(
            "{label} must not be empty"
        )))
    } else {
        Ok(())
    }
}

fn validate_actor(actor: &str) -> ContextResult<()> {
    validate_non_empty("actor", actor)
}

fn validate_pack_name(pack_name: &str) -> ContextResult<()> {
    validate_non_empty("pack name", pack_name)
}

fn validate_entry_key(entry_key: &str) -> ContextResult<()> {
    validate_non_empty("entry key", entry_key)
}

fn validate_request_id(request_id: &str) -> ContextResult<()> {
    validate_non_empty("request_id", request_id)
}

fn validate_review_id(review_id: &str) -> ContextResult<()> {
    validate_non_empty("review_id", review_id)
}

fn validate_run_id(run_id: &str) -> ContextResult<()> {
    validate_non_empty("run_id", run_id)
}

fn validate_imported_review(review: &ReviewItem) -> ContextResult<()> {
    validate_review_id(&review.id)?;
    validate_request_id(&review.request_id)?;
    review.scope.normalize()?;
    validate_pack_name(&review.pack_name)?;
    validate_entry_key(&review.entry_key)?;
    review.proposed_entry.validate()?;
    if let Some(existing) = &review.existing_entry {
        existing.scope.normalize()?;
        validate_pack_name(&existing.pack_name)?;
        validate_entry_key(&existing.key)?;
    }
    Ok(())
}

fn fts_query(query: &str) -> String {
    query
        .split_whitespace()
        .filter(|token| !token.is_empty())
        .map(|token| format!("\"{}\"", token.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(" AND ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::tempdir;

    fn temp_store() -> ContextStore {
        let dir = tempdir().expect("tempdir");
        let db_path = dir.path().join("context.db");
        ContextStore::open(&db_path).expect("store")
    }

    fn sample_entry(key: &str, body: &str) -> EntryInput {
        EntryInput {
            key: key.to_string(),
            title: Some(format!("title-{key}")),
            kind: "note".to_string(),
            value: EntryValue::Markdown {
                body: body.to_string(),
            },
            tags: vec!["alpha".to_string()],
            metadata: json!({"k": key}),
            locked: false,
            provenance: None,
        }
    }

    fn pending_global_review(
        store: &ContextStore,
        request_id: &str,
        key: &str,
        body: &str,
    ) -> ReviewItem {
        let result = store
            .commit_work(CommitWorkRequest {
                request_id: request_id.to_string(),
                actor: "agent".to_string(),
                run: None,
                proposals: vec![CommitProposal {
                    scope: ScopeRef::global(),
                    pack_name: "main".to_string(),
                    entry: sample_entry(key, body),
                }],
            })
            .expect("pending commit");
        let review_id = result.items[0].review_id.as_deref().expect("review id");
        store
            .review_list(Some(ReviewState::Pending))
            .expect("pending reviews")
            .into_iter()
            .find(|review| review.id == review_id)
            .expect("pending review")
    }

    fn find_review(store: &ContextStore, review_id: &str) -> ReviewItem {
        store
            .review_list(None)
            .expect("reviews")
            .into_iter()
            .find(|review| review.id == review_id)
            .expect("review")
    }

    fn synthetic_secret(prefix: &str) -> String {
        [prefix, "abcdefghijklmnopqrstuvwxyz123456"].concat()
    }

    fn instruction_source_import_request(
        destination: ScopeRef,
        payload: &str,
    ) -> SourceImportApplyRequest {
        SourceImportApplyRequest {
            source_kind: SourceImportKind::AgentsMd,
            documents: vec![SourceImportDocument {
                path: Some("AGENTS.md".to_string()),
                payload: payload.to_string(),
            }],
            destination,
            pack_name: Some("instructions".to_string()),
            actor: "importer".to_string(),
            expected_preview_fingerprint: None,
        }
    }

    fn locked_source_import_request(destination: ScopeRef) -> SourceImportApplyRequest {
        let source = ContextStore::open_in_memory().expect("source store");
        source
            .put_entry(PutEntryRequest {
                scope: ScopeRef::global(),
                pack_name: "main".to_string(),
                entry: EntryInput {
                    locked: true,
                    ..sample_entry("governed", "unchanged body")
                },
                actor: "source-author".to_string(),
            })
            .expect("source entry");
        let payload = source
            .export_json(ExportRequest {
                project_scope_id: None,
                task_scope_id: None,
                scope: None,
                pack_name: None,
                include_deleted: false,
                include_reviews: false,
                include_runs: false,
            })
            .expect("source export");

        SourceImportApplyRequest {
            source_kind: SourceImportKind::UcmJson,
            documents: vec![SourceImportDocument {
                path: Some("locked-bundle.json".to_string()),
                payload,
            }],
            destination,
            pack_name: Some("imports".to_string()),
            actor: "importer".to_string(),
            expected_preview_fingerprint: None,
        }
    }

    fn seed_unlocked_source_import_candidate(
        store: &ContextStore,
        request: &SourceImportApplyRequest,
    ) -> (EntrySelector, SourceImportPreview) {
        let initial = store
            .preview_source_import(request.into())
            .expect("initial preview");
        assert_eq!(initial.candidates.len(), 1);
        assert_eq!(
            initial.candidates[0].disposition,
            SourceImportDisposition::New
        );
        assert!(initial.candidates[0].entry.locked);

        let mut existing = initial.candidates[0].entry.clone();
        existing.locked = false;
        store
            .put_entry(PutEntryRequest {
                scope: initial.destination.clone(),
                pack_name: initial.pack_name.clone(),
                entry: existing,
                actor: "seed".to_string(),
            })
            .expect("seed unlocked candidate");

        let selector = EntrySelector {
            scope: initial.destination,
            pack_name: initial.pack_name,
            entry_key: initial.candidates[0].entry.key.clone(),
        };
        let conflict = store
            .preview_source_import(request.into())
            .expect("conflict preview");
        assert_eq!(
            conflict.candidates[0].disposition,
            SourceImportDisposition::Conflict
        );
        (selector, conflict)
    }

    fn set_review_mode(store: &ContextStore, mode: ReviewMode) {
        store
            .set_review_policy(SetReviewPolicyRequest {
                mode,
                metadata: json!({}),
                actor: "admin".to_string(),
            })
            .expect("review policy");
    }

    fn create_import_pack(store: &ContextStore, scope: ScopeRef) -> PackRecord {
        store
            .create_pack(CreatePackRequest {
                scope,
                name: "instructions".to_string(),
                description: None,
                metadata: json!({}),
                locked: false,
                lock_reason: None,
                actor: "pack-owner".to_string(),
            })
            .expect("destination pack")
    }

    fn assert_lock_only_source_import_is_reviewed(mode: ReviewMode) {
        let store = temp_store();
        set_review_mode(&store, mode);
        let request = locked_source_import_request(
            ScopeRef::normalized(ScopeKind::Project, "proj").expect("scope"),
        );
        let (selector, preview) = seed_unlocked_source_import_candidate(&store, &request);
        assert_eq!(preview.review_mode, mode);

        let result = store.apply_source_import(request).expect("apply import");
        assert_eq!(result.applied_count, 0);
        assert_eq!(result.pending_count, 1);
        assert_eq!(result.items[0].disposition, CommitDisposition::Pending);
        assert_eq!(
            result.items[0].reason.as_deref(),
            Some(ReviewReason::Conflict.as_str())
        );
        assert!(!store.get_entry(&selector).expect("existing entry").locked);

        let reviews = store
            .review_list(Some(ReviewState::Pending))
            .expect("pending reviews");
        assert_eq!(reviews.len(), 1);
        assert!(reviews[0].proposed_entry.locked);
        assert_eq!(
            reviews[0].existing_entry.as_ref().map(|entry| entry.locked),
            Some(false)
        );
    }

    fn ts(value: &str) -> DateTime<Utc> {
        chrono::DateTime::parse_from_rfc3339(value)
            .expect("timestamp")
            .with_timezone(&chrono::Utc)
    }

    #[test]
    fn configures_wal_and_schema() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("context.db");
        let store = ContextStore::open(&path).expect("store");
        let conn = store.lock_conn().expect("conn");
        let mode: String = conn
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .expect("pragma");
        assert_eq!(mode.to_lowercase(), "wal");
        let strict_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_list WHERE strict = 1 AND name IN ('packs','entries','revisions','runs','commits','review_items','review_policy')",
                [],
                |row| row.get(0),
            )
            .expect("strict count");
        assert_eq!(strict_count, 7);
        assert_eq!(
            current_schema_version(&conn).expect("schema version"),
            LATEST_SCHEMA_VERSION
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&path).expect("metadata").permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
    }

    #[test]
    fn health_reports_the_requested_component_version() {
        let store = ContextStore::open_in_memory().expect("store");
        let report = store
            .health_with_component_version("contextd-test-version")
            .expect("health");

        assert_eq!(
            report.component_version.as_deref(),
            Some("contextd-test-version")
        );
        assert_eq!(report.api_version, Some(CONTEXT_API_VERSION));
        assert_eq!(report.schema_version, LATEST_SCHEMA_VERSION);
    }

    #[test]
    fn open_preserves_existing_parent_dir_permissions() {
        let dir = tempdir().expect("tempdir");
        let parent = dir.path().join("project");
        fs::create_dir_all(&parent).expect("mkdir");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&parent, fs::Permissions::from_mode(0o755)).expect("chmod");
        }

        let path = parent.join("context.db");
        let _store = ContextStore::open(&path).expect("store");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let parent_mode = fs::metadata(&parent)
                .expect("metadata")
                .permissions()
                .mode()
                & 0o777;
            let file_mode = fs::metadata(&path).expect("file").permissions().mode() & 0o777;
            assert_eq!(parent_mode, 0o755);
            assert_eq!(file_mode, 0o600);
        }
    }

    #[test]
    fn compose_and_search_across_layers() {
        let store = temp_store();
        let _ = store
            .put_entry(PutEntryRequest {
                scope: ScopeRef::global(),
                pack_name: "main".to_string(),
                entry: sample_entry("g", "global hello"),
                actor: "tester".to_string(),
            })
            .expect("global");
        let _ = store
            .put_entry(PutEntryRequest {
                scope: ScopeRef::normalized(ScopeKind::Project, "proj").expect("scope"),
                pack_name: "main".to_string(),
                entry: sample_entry("p", "project specific hello"),
                actor: "tester".to_string(),
            })
            .expect("project");
        let _ = store
            .put_entry(PutEntryRequest {
                scope: ScopeRef::normalized(ScopeKind::Task, "task1").expect("scope"),
                pack_name: "main".to_string(),
                entry: sample_entry("t", "task scoped signal"),
                actor: "tester".to_string(),
            })
            .expect("task");

        let composed = store
            .compose_context(ComposeRequest {
                project_scope_id: Some("proj".to_string()),
                task_scope_id: Some("task1".to_string()),
                include_archived: false,
            })
            .expect("compose");
        assert_eq!(composed.sections.len(), 3);
        assert!(composed.rendered_markdown.contains("global hello"));
        assert!(
            composed
                .rendered_markdown
                .contains("project specific hello")
        );
        assert!(composed.rendered_markdown.contains("task scoped signal"));

        let conn = store.lock_conn().expect("conn");
        let fts_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM entry_fts WHERE entry_fts MATCH 'specific'",
                [],
                |row| row.get(0),
            )
            .expect("fts count");
        drop(conn);
        assert_eq!(fts_count, 1);
        let search = store
            .search_context(SearchRequest {
                query: "specific".to_string(),
                project_scope_id: Some("proj".to_string()),
                task_scope_id: None,
                limit: 10,
            })
            .expect("search");
        assert_eq!(search.hits.len(), 1);
        assert_eq!(search.hits[0].entry.key, "p");
    }

    #[test]
    fn commit_work_is_idempotent_and_routes_pending_items() {
        let store = temp_store();
        let project_scope = ScopeRef::normalized(ScopeKind::Project, "proj").expect("scope");
        let _ = store
            .put_entry(PutEntryRequest {
                scope: project_scope.clone(),
                pack_name: "main".to_string(),
                entry: sample_entry("conflict", "existing"),
                actor: "tester".to_string(),
            })
            .expect("seed");
        let _ = store
            .put_entry(PutEntryRequest {
                scope: project_scope.clone(),
                pack_name: "main".to_string(),
                entry: EntryInput {
                    locked: true,
                    ..sample_entry("locked", "locked body")
                },
                actor: "tester".to_string(),
            })
            .expect("locked");
        let result = store
            .commit_work(CommitWorkRequest {
                request_id: "req-1".to_string(),
                actor: "agent".to_string(),
                run: Some(RunInput {
                    id: Some("run-1".to_string()),
                    project_scope_id: Some("proj".to_string()),
                    task_scope_id: Some("task1".to_string()),
                    source: "test".to_string(),
                    metadata: json!({}),
                }),
                proposals: vec![
                    CommitProposal {
                        scope: ScopeRef::global(),
                        pack_name: "main".to_string(),
                        entry: sample_entry("g", "global pending"),
                    },
                    CommitProposal {
                        scope: project_scope.clone(),
                        pack_name: "main".to_string(),
                        entry: sample_entry("new", "project applied"),
                    },
                    CommitProposal {
                        scope: project_scope.clone(),
                        pack_name: "main".to_string(),
                        entry: sample_entry("conflict", "project changed"),
                    },
                    CommitProposal {
                        scope: project_scope.clone(),
                        pack_name: "main".to_string(),
                        entry: sample_entry("locked", "locked change"),
                    },
                ],
            })
            .expect("commit");
        assert_eq!(result.status, CommitStatus::Partial);
        assert_eq!(
            result
                .items
                .iter()
                .filter(|item| item.disposition == CommitDisposition::Pending)
                .count(),
            3
        );
        assert_eq!(
            result
                .items
                .iter()
                .filter(|item| item.disposition == CommitDisposition::Applied)
                .count(),
            1
        );
        let duplicate = store
            .commit_work(CommitWorkRequest {
                request_id: "req-1".to_string(),
                actor: "ignored".to_string(),
                run: None,
                proposals: vec![CommitProposal {
                    scope: project_scope,
                    pack_name: "main".to_string(),
                    entry: sample_entry("new", "different but ignored"),
                }],
            })
            .expect("duplicate");
        assert_eq!(duplicate, result);
    }

    #[test]
    fn commit_work_rejects_secret_request_metadata_without_writes() {
        let store = temp_store();
        let err = store
            .commit_work(CommitWorkRequest {
                request_id: "req-meta".to_string(),
                actor: synthetic_secret("xoxb-1234567890-"),
                run: None,
                proposals: vec![CommitProposal {
                    scope: ScopeRef::normalized(ScopeKind::Project, "proj").expect("scope"),
                    pack_name: "main".to_string(),
                    entry: sample_entry("k", "body"),
                }],
            })
            .expect_err("secret actor");
        assert!(matches!(err, ContextError::SecretDetected(_)));

        let stats = store.stats().expect("stats");
        assert_eq!(stats.packs, 0);
        assert_eq!(stats.entries, 0);
        assert_eq!(stats.reviews, 0);
        assert_eq!(stats.runs, 0);
    }

    #[test]
    fn secret_rejected_proposal_does_not_create_pack() {
        let store = temp_store();
        let result = store
            .commit_work(CommitWorkRequest {
                request_id: "req-secret-proposal".to_string(),
                actor: "agent".to_string(),
                run: None,
                proposals: vec![CommitProposal {
                    scope: ScopeRef::normalized(ScopeKind::Project, "proj-new").expect("scope"),
                    pack_name: "main".to_string(),
                    entry: sample_entry("k", &synthetic_secret("token = sk-")),
                }],
            })
            .expect("commit result");
        assert_eq!(result.status, CommitStatus::Rejected);
        assert_eq!(result.items.len(), 1);
        assert_eq!(result.items[0].disposition, CommitDisposition::Rejected);

        let stats = store.stats().expect("stats");
        assert_eq!(stats.packs, 0);
        assert_eq!(stats.entries, 0);
        assert_eq!(stats.reviews, 0);
    }

    #[test]
    fn allows_normal_absolute_project_paths_with_tokenish_names() {
        let store = temp_store();
        let result = store
            .commit_work(CommitWorkRequest {
                request_id: "req-abs-path".to_string(),
                actor: "agent".to_string(),
                run: Some(RunInput {
                    id: Some("run-abs-path".to_string()),
                    project_scope_id: Some("/Users/alice/src/secret-token-service".to_string()),
                    task_scope_id: None,
                    source: "codex".to_string(),
                    metadata: json!({}),
                }),
                proposals: vec![CommitProposal {
                    scope: ScopeRef::normalized(
                        ScopeKind::Project,
                        "/Users/alice/src/secret-token-service",
                    )
                    .expect("scope"),
                    pack_name: "main".to_string(),
                    entry: sample_entry("k", "body"),
                }],
            })
            .expect("commit");
        assert_eq!(result.status, CommitStatus::Applied);
    }

    #[test]
    fn review_approve_edit_reject_flow() {
        let store = temp_store();
        let commit = store
            .commit_work(CommitWorkRequest {
                request_id: "req-review".to_string(),
                actor: "agent".to_string(),
                run: None,
                proposals: vec![CommitProposal {
                    scope: ScopeRef::global(),
                    pack_name: "main".to_string(),
                    entry: sample_entry("g", "pending review body"),
                }],
            })
            .expect("commit");
        let review_id = commit.items[0].review_id.clone().expect("review id");
        let edited = store
            .review_edit(ReviewEditRequest {
                review_id: review_id.clone(),
                title: Some("edited".to_string()),
                kind: None,
                value: Some(EntryValue::Markdown {
                    body: "approved body".to_string(),
                }),
                tags: None,
                metadata: None,
                locked: None,
                actor: "reviewer".to_string(),
            })
            .expect("edit review");
        assert_eq!(edited.proposed_entry.title.as_deref(), Some("edited"));
        let approved = store
            .review_approve(ReviewDecisionRequest {
                review_id,
                actor: "reviewer".to_string(),
                note: Some("looks good".to_string()),
            })
            .expect("approve review");
        assert_eq!(approved.state, ReviewState::Approved);
        let entry = store
            .get_entry(&EntrySelector {
                scope: ScopeRef::global(),
                pack_name: "main".to_string(),
                entry_key: "g".to_string(),
            })
            .expect("entry");
        assert_eq!(entry.title.as_deref(), Some("edited"));
        assert!(entry.value.render_markdown().contains("approved body"));

        let commit = store
            .commit_work(CommitWorkRequest {
                request_id: "req-reject".to_string(),
                actor: "agent".to_string(),
                run: None,
                proposals: vec![CommitProposal {
                    scope: ScopeRef::global(),
                    pack_name: "main".to_string(),
                    entry: sample_entry("g2", "reject body"),
                }],
            })
            .expect("commit");
        let rejected = store
            .review_reject(ReviewDecisionRequest {
                review_id: commit.items[0].review_id.clone().expect("review id"),
                actor: "reviewer".to_string(),
                note: Some("nope".to_string()),
            })
            .expect("reject review");
        assert_eq!(rejected.state, ReviewState::Rejected);
    }

    #[test]
    fn review_edit_and_approve_applies_atomically() {
        let store = temp_store();
        let original =
            pending_global_review(&store, "req-atomic-review", "atomic", "original body");

        let approved = store
            .review_edit_and_approve(ReviewEditAndApproveRequest {
                review_id: original.id.clone(),
                title: Some("Approved title".to_string()),
                kind: Some("decision".to_string()),
                value: Some(EntryValue::Markdown {
                    body: "approved body".to_string(),
                }),
                tags: Some(vec!["approved".to_string()]),
                metadata: Some(json!({"approved": true})),
                locked: Some(true),
                actor: "reviewer".to_string(),
                note: Some("edited and approved".to_string()),
            })
            .expect("atomic edit and approve");

        assert_eq!(approved.state, ReviewState::Approved);
        assert_eq!(approved.revision_no, original.revision_no + 1);
        assert_eq!(
            approved.proposed_entry.title.as_deref(),
            Some("Approved title")
        );
        assert_eq!(approved.proposed_entry.kind, "decision");
        assert_eq!(
            approved.proposed_entry.value.render_markdown(),
            "approved body"
        );
        assert_eq!(approved.proposed_entry.tags, vec!["approved"]);
        assert_eq!(approved.proposed_entry.metadata, json!({"approved": true}));
        assert!(approved.proposed_entry.locked);
        assert_eq!(
            approved.resolution_note.as_deref(),
            Some("edited and approved")
        );

        let entry = store
            .get_entry(&EntrySelector {
                scope: ScopeRef::global(),
                pack_name: "main".to_string(),
                entry_key: "atomic".to_string(),
            })
            .expect("approved entry");
        assert_eq!(entry.title, approved.proposed_entry.title);
        assert_eq!(entry.kind, approved.proposed_entry.kind);
        assert_eq!(entry.value, approved.proposed_entry.value);
        assert_eq!(entry.tags, approved.proposed_entry.tags);
        assert_eq!(entry.metadata, approved.proposed_entry.metadata);
        assert_eq!(entry.locked, approved.proposed_entry.locked);
    }

    #[test]
    fn review_edit_and_approve_validation_failure_changes_nothing() {
        let store = temp_store();
        let original =
            pending_global_review(&store, "req-atomic-invalid", "invalid", "original body");

        let err = store
            .review_edit_and_approve(ReviewEditAndApproveRequest {
                review_id: original.id.clone(),
                title: Some("must not persist".to_string()),
                kind: Some(" ".to_string()),
                value: None,
                tags: None,
                metadata: None,
                locked: None,
                actor: "reviewer".to_string(),
                note: Some("invalid edit".to_string()),
            })
            .expect_err("invalid atomic edit");
        assert!(matches!(err, ContextError::Validation(_)));
        assert_eq!(find_review(&store, &original.id), original);
        assert_eq!(store.stats().expect("stats").entries, 0);
    }

    #[test]
    fn review_edit_and_approve_rejects_secret_note_without_partial_writes() {
        let store = temp_store();
        let original =
            pending_global_review(&store, "req-atomic-secret", "secret", "original body");

        let err = store
            .review_edit_and_approve(ReviewEditAndApproveRequest {
                review_id: original.id.clone(),
                title: Some("must roll back".to_string()),
                kind: None,
                value: Some(EntryValue::Markdown {
                    body: "safe approved body".to_string(),
                }),
                tags: None,
                metadata: None,
                locked: None,
                actor: "reviewer".to_string(),
                note: Some(synthetic_secret("token = sk-")),
            })
            .expect_err("secret resolution note");
        assert!(matches!(err, ContextError::SecretDetected(_)));
        assert_eq!(find_review(&store, &original.id), original);
        assert_eq!(store.stats().expect("stats").entries, 0);
    }

    #[test]
    fn review_edit_and_approve_rolls_back_late_database_failure() {
        let store = temp_store();
        let original =
            pending_global_review(&store, "req-atomic-rollback", "rollback", "original body");
        {
            let conn = store.lock_conn().expect("connection");
            conn.execute_batch(
                "CREATE TRIGGER fail_atomic_review_approval
                 BEFORE UPDATE OF state ON review_items
                 WHEN NEW.state = 'approved'
                 BEGIN
                   SELECT RAISE(ABORT, 'forced atomic approval failure');
                 END;",
            )
            .expect("failure trigger");
        }

        let err = store
            .review_edit_and_approve(ReviewEditAndApproveRequest {
                review_id: original.id.clone(),
                title: Some("must roll back".to_string()),
                kind: None,
                value: Some(EntryValue::Markdown {
                    body: "must roll back".to_string(),
                }),
                tags: None,
                metadata: None,
                locked: Some(true),
                actor: "reviewer".to_string(),
                note: Some("valid note".to_string()),
            })
            .expect_err("forced late failure");
        assert!(matches!(err, ContextError::Sql(_)));
        assert_eq!(find_review(&store, &original.id), original);
        assert_eq!(store.stats().expect("stats").entries, 0);

        let conn = store.lock_conn().expect("connection");
        let entry_revisions: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM revisions WHERE entity_type = 'entry'",
                [],
                |row| row.get(0),
            )
            .expect("entry revision count");
        assert_eq!(entry_revisions, 0);
    }

    #[test]
    fn review_transition_rejects_secret_actor() {
        let store = temp_store();
        let commit = store
            .commit_work(CommitWorkRequest {
                request_id: "req-review-actor".to_string(),
                actor: "agent".to_string(),
                run: None,
                proposals: vec![CommitProposal {
                    scope: ScopeRef::global(),
                    pack_name: "main".to_string(),
                    entry: sample_entry("g", "pending review body"),
                }],
            })
            .expect("commit");
        let review_id = commit.items[0].review_id.clone().expect("review id");
        let err = store
            .review_reject(ReviewDecisionRequest {
                review_id: review_id.clone(),
                actor: synthetic_secret("ghp_"),
                note: Some("safe note".to_string()),
            })
            .expect_err("secret actor");
        assert!(matches!(err, ContextError::SecretDetected(_)));

        let review = store
            .review_list(Some(ReviewState::Pending))
            .expect("reviews")
            .into_iter()
            .find(|item| item.id == review_id)
            .expect("pending review");
        assert_eq!(review.state, ReviewState::Pending);
    }

    #[test]
    fn rejects_secrets_and_can_revert() {
        let store = temp_store();
        let err = store
            .put_entry(PutEntryRequest {
                scope: ScopeRef::global(),
                pack_name: "main".to_string(),
                entry: sample_entry("bad", &synthetic_secret("token = sk-")),
                actor: "tester".to_string(),
            })
            .expect_err("secret rejection");
        assert!(matches!(err, ContextError::SecretDetected(_)));

        let scope = ScopeRef::normalized(ScopeKind::Project, "proj").expect("scope");
        let _ = store
            .put_entry(PutEntryRequest {
                scope: scope.clone(),
                pack_name: "main".to_string(),
                entry: sample_entry("k", "v1"),
                actor: "tester".to_string(),
            })
            .expect("put v1");
        let _ = store
            .put_entry(PutEntryRequest {
                scope: scope.clone(),
                pack_name: "main".to_string(),
                entry: sample_entry("k", "v2"),
                actor: "tester".to_string(),
            })
            .expect("put v2");
        let reverted = store
            .revert_entry(RevertEntryRequest {
                selector: EntrySelector {
                    scope,
                    pack_name: "main".to_string(),
                    entry_key: "k".to_string(),
                },
                revision_no: Some(1),
                actor: "tester".to_string(),
            })
            .expect("revert");
        assert!(reverted.value.render_markdown().contains("v1"));
    }

    #[test]
    fn rejects_secrets_in_entry_tags_and_provenance() {
        let store = temp_store();
        let mut entry = sample_entry("tagged", "body");
        entry.tags = vec![synthetic_secret("xoxb-1234567890-")];
        let err = store
            .put_entry(PutEntryRequest {
                scope: ScopeRef::normalized(ScopeKind::Project, "proj").expect("scope"),
                pack_name: "main".to_string(),
                entry,
                actor: "tester".to_string(),
            })
            .expect_err("secret tag rejection");
        assert!(matches!(err, ContextError::SecretDetected(_)));

        let mut entry = sample_entry("prov", "body");
        entry.provenance = Some(Provenance {
            actor: "tester".to_string(),
            source: "manual".to_string(),
            source_ref: None,
            run_id: None,
            request_id: None,
            note: Some(synthetic_secret("github_pat_")),
        });
        let err = store
            .put_entry(PutEntryRequest {
                scope: ScopeRef::normalized(ScopeKind::Project, "proj").expect("scope"),
                pack_name: "main".to_string(),
                entry,
                actor: "tester".to_string(),
            })
            .expect_err("secret provenance rejection");
        assert!(matches!(err, ContextError::SecretDetected(_)));
    }

    #[test]
    fn rejects_secrets_in_pack_and_run_metadata() {
        let store = temp_store();
        let err = store
            .create_pack(CreatePackRequest {
                scope: ScopeRef::normalized(ScopeKind::Project, "proj").expect("scope"),
                name: "main".to_string(),
                description: Some(synthetic_secret("api_key = sk-proj-")),
                metadata: json!({}),
                locked: false,
                lock_reason: None,
                actor: "tester".to_string(),
            })
            .expect_err("secret pack description");
        assert!(matches!(err, ContextError::SecretDetected(_)));

        let err = store
            .create_pack(CreatePackRequest {
                scope: ScopeRef::normalized(ScopeKind::Project, synthetic_secret("ghp_"))
                    .expect("scope"),
                name: "main".to_string(),
                description: None,
                metadata: json!({}),
                locked: false,
                lock_reason: None,
                actor: "tester".to_string(),
            })
            .expect_err("secret pack scope");
        assert!(matches!(err, ContextError::SecretDetected(_)));

        let err = store
            .create_pack(CreatePackRequest {
                scope: ScopeRef::normalized(ScopeKind::Project, "proj-name").expect("scope"),
                name: synthetic_secret("github_pat_"),
                description: None,
                metadata: json!({}),
                locked: false,
                lock_reason: None,
                actor: "tester".to_string(),
            })
            .expect_err("secret pack name");
        assert!(matches!(err, ContextError::SecretDetected(_)));

        let _ = store
            .create_pack(CreatePackRequest {
                scope: ScopeRef::normalized(ScopeKind::Project, "proj").expect("scope"),
                name: "main".to_string(),
                description: None,
                metadata: json!({}),
                locked: false,
                lock_reason: None,
                actor: "tester".to_string(),
            })
            .expect("safe pack");
        let err = store
            .update_pack(UpdatePackRequest {
                selector: PackSelector {
                    scope: ScopeRef::normalized(ScopeKind::Project, "proj").expect("scope"),
                    name: "main".to_string(),
                },
                description: None,
                metadata: Some(json!({"safe": true})),
                status: None,
                locked: None,
                lock_reason: None,
                actor: synthetic_secret("ghp_"),
            })
            .expect_err("secret pack actor");
        assert!(matches!(err, ContextError::SecretDetected(_)));

        let err = store
            .create_run(RunInput {
                id: Some(synthetic_secret("sk-run-")),
                project_scope_id: Some("proj".to_string()),
                task_scope_id: None,
                source: "test".to_string(),
                metadata: json!({}),
            })
            .expect_err("secret run id");
        assert!(matches!(err, ContextError::SecretDetected(_)));

        let err = store
            .create_run(RunInput {
                id: None,
                project_scope_id: Some("proj".to_string()),
                task_scope_id: None,
                source: "test".to_string(),
                metadata: json!({"openai_key": synthetic_secret("sk-ant-")}),
            })
            .expect_err("secret run metadata");
        assert!(matches!(err, ContextError::SecretDetected(_)));
    }

    #[test]
    fn rejects_empty_identifiers_and_duplicate_public_pack_creates() {
        let store = temp_store();
        let err = store
            .create_pack(CreatePackRequest {
                scope: ScopeRef::normalized(ScopeKind::Project, "proj").expect("scope"),
                name: "   ".to_string(),
                description: None,
                metadata: json!({}),
                locked: false,
                lock_reason: None,
                actor: "tester".to_string(),
            })
            .expect_err("empty pack name");
        assert!(matches!(err, ContextError::Validation(_)));

        let created = store
            .create_pack(CreatePackRequest {
                scope: ScopeRef::normalized(ScopeKind::Project, "proj").expect("scope"),
                name: "main".to_string(),
                description: None,
                metadata: json!({}),
                locked: false,
                lock_reason: None,
                actor: "tester".to_string(),
            })
            .expect("create pack");
        let duplicate = store
            .create_pack(CreatePackRequest {
                scope: ScopeRef::normalized(ScopeKind::Project, "proj").expect("scope"),
                name: "main".to_string(),
                description: None,
                metadata: json!({}),
                locked: false,
                lock_reason: None,
                actor: "tester".to_string(),
            })
            .expect_err("duplicate create");
        assert!(matches!(duplicate, ContextError::Conflict(_)));

        let fetched = store.list_packs().expect("packs");
        assert_eq!(fetched.len(), 1);
        assert_eq!(fetched[0].id, created.id);

        let err = store
            .put_entry(PutEntryRequest {
                scope: ScopeRef::normalized(ScopeKind::Project, "proj").expect("scope"),
                pack_name: "".to_string(),
                entry: sample_entry("k", "body"),
                actor: "tester".to_string(),
            })
            .expect_err("empty pack name on put");
        assert!(matches!(err, ContextError::Validation(_)));

        let err = store
            .review_edit(ReviewEditRequest {
                review_id: " ".to_string(),
                title: None,
                kind: None,
                value: None,
                tags: None,
                metadata: None,
                locked: None,
                actor: "reviewer".to_string(),
            })
            .expect_err("empty review id");
        assert!(matches!(err, ContextError::Validation(_)));

        let err = store
            .create_run(RunInput {
                id: Some(" ".to_string()),
                project_scope_id: Some("proj".to_string()),
                task_scope_id: None,
                source: "test".to_string(),
                metadata: json!({}),
            })
            .expect_err("empty run id");
        assert!(matches!(err, ContextError::Validation(_)));
    }

    #[test]
    fn exports_and_imports_json_and_markdown() {
        let source = temp_store();
        let _ = source
            .put_entry(PutEntryRequest {
                scope: ScopeRef::global(),
                pack_name: "main".to_string(),
                entry: sample_entry("g", "hello export"),
                actor: "tester".to_string(),
            })
            .expect("put");
        let json = source
            .export_json(ExportRequest {
                project_scope_id: None,
                task_scope_id: None,
                scope: None,
                pack_name: None,
                include_deleted: false,
                include_reviews: true,
                include_runs: true,
            })
            .expect("json export");
        let markdown = source
            .export_markdown(ExportRequest {
                project_scope_id: None,
                task_scope_id: None,
                scope: None,
                pack_name: None,
                include_deleted: false,
                include_reviews: false,
                include_runs: false,
            })
            .expect("md export");

        let imported_json = temp_store();
        imported_json
            .import_data(ImportRequest {
                actor: "importer".to_string(),
                format: ImportFormat::Json,
                payload: json,
            })
            .expect("json import");
        let entry = imported_json
            .get_entry(&EntrySelector {
                scope: ScopeRef::global(),
                pack_name: "main".to_string(),
                entry_key: "g".to_string(),
            })
            .expect("entry");
        assert!(entry.value.render_markdown().contains("hello export"));

        let imported_markdown = temp_store();
        imported_markdown
            .import_data(ImportRequest {
                actor: "importer".to_string(),
                format: ImportFormat::Markdown,
                payload: markdown,
            })
            .expect("markdown import");
        let entry = imported_markdown
            .get_entry(&EntrySelector {
                scope: ScopeRef::global(),
                pack_name: "main".to_string(),
                entry_key: "g".to_string(),
            })
            .expect("entry");
        assert!(entry.value.render_markdown().contains("hello export"));
    }

    #[test]
    fn json_import_preserves_review_metadata_round_trip() {
        let source = temp_store();
        let commit = source
            .commit_work(CommitWorkRequest {
                request_id: "req-review-export".to_string(),
                actor: "agent".to_string(),
                run: None,
                proposals: vec![CommitProposal {
                    scope: ScopeRef::global(),
                    pack_name: "main".to_string(),
                    entry: sample_entry("review", "round trip"),
                }],
            })
            .expect("commit");
        let review_id = commit.items[0].review_id.clone().expect("review id");
        let _ = source
            .review_reject(ReviewDecisionRequest {
                review_id,
                actor: "reviewer".to_string(),
                note: Some("keep out".to_string()),
            })
            .expect("reject");

        let exported = source
            .export_bundle(ExportRequest {
                project_scope_id: None,
                task_scope_id: None,
                scope: None,
                pack_name: None,
                include_deleted: false,
                include_reviews: true,
                include_runs: false,
            })
            .expect("export");

        let imported = temp_store();
        imported
            .import_data(ImportRequest {
                actor: "importer".to_string(),
                format: ImportFormat::Json,
                payload: serde_json::to_string(&exported).expect("json"),
            })
            .expect("import");

        let imported_reviews = imported.review_list(None).expect("reviews");
        assert_eq!(imported_reviews, exported.reviews);
    }

    #[test]
    fn export_bundle_filters_reviews_and_runs_by_scope_and_pack() {
        let store = temp_store();
        let project_a = ScopeRef::normalized(ScopeKind::Project, "proj-a").expect("scope");
        let project_b = ScopeRef::normalized(ScopeKind::Project, "proj-b").expect("scope");
        for (scope, pack_name) in [
            (project_a.clone(), "main"),
            (project_a.clone(), "alt"),
            (project_b.clone(), "main"),
        ] {
            store
                .create_pack(CreatePackRequest {
                    scope,
                    name: pack_name.to_string(),
                    description: None,
                    metadata: json!({}),
                    locked: true,
                    lock_reason: Some("locked".to_string()),
                    actor: "tester".to_string(),
                })
                .expect("create pack");
        }

        let _ = store
            .commit_work(CommitWorkRequest {
                request_id: "req-export-global".to_string(),
                actor: "agent".to_string(),
                run: None,
                proposals: vec![CommitProposal {
                    scope: ScopeRef::global(),
                    pack_name: "main".to_string(),
                    entry: sample_entry("global", "global review"),
                }],
            })
            .expect("global review");
        let _ = store
            .commit_work(CommitWorkRequest {
                request_id: "req-export-proj-a-main".to_string(),
                actor: "agent".to_string(),
                run: None,
                proposals: vec![CommitProposal {
                    scope: project_a.clone(),
                    pack_name: "main".to_string(),
                    entry: sample_entry("proj-a-main", "project a main review"),
                }],
            })
            .expect("project a main review");
        let _ = store
            .commit_work(CommitWorkRequest {
                request_id: "req-export-proj-a-alt".to_string(),
                actor: "agent".to_string(),
                run: None,
                proposals: vec![CommitProposal {
                    scope: project_a.clone(),
                    pack_name: "alt".to_string(),
                    entry: sample_entry("proj-a-alt", "project a alt review"),
                }],
            })
            .expect("project a alt review");
        let _ = store
            .commit_work(CommitWorkRequest {
                request_id: "req-export-proj-b-main".to_string(),
                actor: "agent".to_string(),
                run: None,
                proposals: vec![CommitProposal {
                    scope: project_b.clone(),
                    pack_name: "main".to_string(),
                    entry: sample_entry("proj-b-main", "project b main review"),
                }],
            })
            .expect("project b main review");

        let _ = store
            .create_run(RunInput {
                id: Some("run-a".to_string()),
                project_scope_id: Some("proj-a".to_string()),
                task_scope_id: None,
                source: "test".to_string(),
                metadata: json!({"scope": "proj-a"}),
            })
            .expect("run a");
        let _ = store
            .create_run(RunInput {
                id: Some("run-b".to_string()),
                project_scope_id: Some("proj-b".to_string()),
                task_scope_id: None,
                source: "test".to_string(),
                metadata: json!({"scope": "proj-b"}),
            })
            .expect("run b");

        let scoped = store
            .export_bundle(ExportRequest {
                project_scope_id: Some("proj-a".to_string()),
                task_scope_id: None,
                scope: None,
                pack_name: None,
                include_deleted: false,
                include_reviews: true,
                include_runs: true,
            })
            .expect("scoped export");
        assert_eq!(scoped.reviews.len(), 3);
        assert!(
            scoped
                .reviews
                .iter()
                .any(|review| review.scope == ScopeRef::global())
        );
        assert!(
            scoped
                .reviews
                .iter()
                .any(|review| review.scope == project_a && review.pack_name == "main")
        );
        assert!(
            scoped
                .reviews
                .iter()
                .any(|review| review.scope == project_a && review.pack_name == "alt")
        );
        assert!(
            !scoped
                .reviews
                .iter()
                .any(|review| review.scope == project_b)
        );
        assert_eq!(scoped.runs.len(), 1);
        assert_eq!(scoped.runs[0].id, "run-a");

        let pack_filtered = store
            .export_bundle(ExportRequest {
                project_scope_id: Some("proj-a".to_string()),
                task_scope_id: None,
                scope: None,
                pack_name: Some("main".to_string()),
                include_deleted: false,
                include_reviews: true,
                include_runs: true,
            })
            .expect("pack-filtered export");
        assert!(pack_filtered.runs.is_empty());
        assert!(
            pack_filtered
                .reviews
                .iter()
                .all(|review| review.pack_name == "main")
        );
        assert_eq!(pack_filtered.reviews.len(), 2);
    }

    #[test]
    fn import_bundle_upserts_pack_and_run_state_and_allows_archived_entries() {
        let store = temp_store();
        let scope = ScopeRef::normalized(ScopeKind::Project, "proj").expect("scope");
        let imported_created_at = ts("2026-08-16T00:00:00Z");
        let imported_updated_at = ts("2026-08-16T00:05:00Z");
        let new_scope = ScopeRef::normalized(ScopeKind::Task, "task-import").expect("scope");
        let new_created_at = ts("2026-08-16T00:02:00Z");
        let new_updated_at = ts("2026-08-16T00:03:00Z");
        let _ = store
            .create_pack(CreatePackRequest {
                scope: scope.clone(),
                name: "main".to_string(),
                description: Some("old description".to_string()),
                metadata: json!({"version": "old"}),
                locked: false,
                lock_reason: None,
                actor: "tester".to_string(),
            })
            .expect("seed pack");
        let _ = store
            .create_run(RunInput {
                id: Some("run-import".to_string()),
                project_scope_id: Some("old-proj".to_string()),
                task_scope_id: None,
                source: "old-source".to_string(),
                metadata: json!({"old": true}),
            })
            .expect("seed run");

        store
            .import_bundle(
                ContextExportBundle {
                    exported_at: ts("2026-08-16T00:00:00Z"),
                    packs: vec![
                        PackRecord {
                            id: "pack-import".to_string(),
                            scope: scope.clone(),
                            name: "main".to_string(),
                            description: Some("new description".to_string()),
                            metadata: json!({"policy": "strict"}),
                            status: PackStatus::Archived,
                            locked: true,
                            lock_reason: Some("frozen".to_string()),
                            created_at: imported_created_at,
                            updated_at: imported_updated_at,
                            revision_no: 7,
                        },
                        PackRecord {
                            id: "pack-import-new".to_string(),
                            scope: new_scope.clone(),
                            name: "secondary".to_string(),
                            description: Some("new pack description".to_string()),
                            metadata: json!({"tier": "secondary"}),
                            status: PackStatus::Active,
                            locked: false,
                            lock_reason: None,
                            created_at: new_created_at,
                            updated_at: new_updated_at,
                            revision_no: 3,
                        },
                    ],
                    entries: vec![EntryRecord {
                        id: "entry-import".to_string(),
                        scope: scope.clone(),
                        pack_name: "main".to_string(),
                        key: "archived-entry".to_string(),
                        title: Some("archived entry".to_string()),
                        kind: "note".to_string(),
                        value: EntryValue::Markdown {
                            body: "still import me".to_string(),
                        },
                        tags: vec!["imported".to_string()],
                        metadata: json!({"source": "bundle"}),
                        provenance: Provenance::system("import-source", "bundle"),
                        locked: false,
                        status: EntryStatus::Active,
                        created_at: ts("2026-08-16T00:06:00Z"),
                        updated_at: ts("2026-08-16T00:06:00Z"),
                        revision_no: 1,
                    }],
                    reviews: Vec::new(),
                    runs: vec![RunRecord {
                        id: "run-import".to_string(),
                        project_scope_id: Some("proj".to_string()),
                        task_scope_id: Some("task-1".to_string()),
                        source: "import-source".to_string(),
                        metadata: json!({"fresh": true}),
                        started_at: ts("2026-08-16T00:07:00Z"),
                    }],
                },
                "importer",
            )
            .expect("import bundle");

        let pack = store
            .list_packs()
            .expect("packs")
            .into_iter()
            .find(|pack| pack.scope == scope && pack.name == "main")
            .expect("pack");
        assert_eq!(pack.description.as_deref(), Some("new description"));
        assert_eq!(pack.metadata, json!({"policy": "strict"}));
        assert_eq!(pack.status, PackStatus::Archived);
        assert!(pack.locked);
        assert_eq!(pack.lock_reason.as_deref(), Some("frozen"));
        assert_eq!(pack.created_at, imported_created_at);
        assert_eq!(pack.updated_at, imported_updated_at);

        let new_pack = store
            .list_packs()
            .expect("packs")
            .into_iter()
            .find(|pack| pack.scope == new_scope && pack.name == "secondary")
            .expect("new pack");
        assert_eq!(
            new_pack.description.as_deref(),
            Some("new pack description")
        );
        assert_eq!(new_pack.metadata, json!({"tier": "secondary"}));
        assert_eq!(new_pack.created_at, new_created_at);
        assert_eq!(new_pack.updated_at, new_updated_at);

        let entry = store
            .get_entry(&EntrySelector {
                scope: scope.clone(),
                pack_name: "main".to_string(),
                entry_key: "archived-entry".to_string(),
            })
            .expect("imported entry");
        assert_eq!(entry.value.render_markdown(), "still import me");

        let run = store
            .list_runs()
            .expect("runs")
            .into_iter()
            .find(|run| run.id == "run-import")
            .expect("run");
        assert_eq!(run.project_scope_id.as_deref(), Some("proj"));
        assert_eq!(run.task_scope_id.as_deref(), Some("task-1"));
        assert_eq!(run.source, "import-source");
        assert_eq!(run.metadata, json!({"fresh": true}));
        assert_eq!(run.started_at, ts("2026-08-16T00:07:00Z"));
    }

    #[test]
    fn import_rejects_empty_review_and_run_ids() {
        let invalid_run = ContextExportBundle {
            exported_at: ts("2026-08-16T00:00:00Z"),
            packs: Vec::new(),
            entries: Vec::new(),
            reviews: Vec::new(),
            runs: vec![RunRecord {
                id: " ".to_string(),
                project_scope_id: Some("proj".to_string()),
                task_scope_id: None,
                source: "import-source".to_string(),
                metadata: json!({}),
                started_at: ts("2026-08-16T00:00:01Z"),
            }],
        };
        let err = temp_store()
            .import_bundle(invalid_run, "importer")
            .expect_err("empty imported run id");
        assert!(matches!(err, ContextError::Validation(_)));

        let invalid_review = ContextExportBundle {
            exported_at: ts("2026-08-16T00:00:00Z"),
            packs: Vec::new(),
            entries: Vec::new(),
            reviews: vec![ReviewItem {
                id: " ".to_string(),
                request_id: "req-review-import".to_string(),
                scope: ScopeRef::global(),
                pack_name: "main".to_string(),
                entry_key: "entry".to_string(),
                state: ReviewState::Pending,
                reason: ReviewReason::Locked,
                proposed_entry: sample_entry("entry", "body"),
                existing_entry: None,
                resolution_note: None,
                created_at: ts("2026-08-16T00:00:02Z"),
                updated_at: ts("2026-08-16T00:00:02Z"),
                revision_no: 1,
            }],
            runs: Vec::new(),
        };
        let err = temp_store()
            .import_bundle(invalid_review, "importer")
            .expect_err("empty imported review id");
        assert!(matches!(err, ContextError::Validation(_)));
    }

    #[test]
    fn rejects_secret_review_notes_and_imported_review_records() {
        let store = temp_store();
        let commit = store
            .commit_work(CommitWorkRequest {
                request_id: "req-review-note".to_string(),
                actor: "agent".to_string(),
                run: None,
                proposals: vec![CommitProposal {
                    scope: ScopeRef::global(),
                    pack_name: "main".to_string(),
                    entry: sample_entry("g", "pending review body"),
                }],
            })
            .expect("commit");
        let err = store
            .review_reject(ReviewDecisionRequest {
                review_id: commit.items[0].review_id.clone().expect("review id"),
                actor: "reviewer".to_string(),
                note: Some(synthetic_secret("token = sk-")),
            })
            .expect_err("secret review note");
        assert!(matches!(err, ContextError::SecretDetected(_)));

        let mut exported = store
            .export_bundle(ExportRequest {
                project_scope_id: None,
                task_scope_id: None,
                scope: None,
                pack_name: None,
                include_deleted: false,
                include_reviews: true,
                include_runs: false,
            })
            .expect("export");
        exported.reviews[0].resolution_note = Some("password = hunter2hunter2".to_string());
        let imported = temp_store();
        let err = imported
            .import_data(ImportRequest {
                actor: "importer".to_string(),
                format: ImportFormat::Json,
                payload: serde_json::to_string(&exported).expect("json"),
            })
            .expect_err("secret imported review");
        assert!(matches!(err, ContextError::SecretDetected(_)));
    }

    #[test]
    fn commit_audit_log_redacts_entry_bodies() {
        let store = temp_store();
        let body = "sensitive but non-secret note body";
        let _ = store
            .commit_work(CommitWorkRequest {
                request_id: "req-redacted-log".to_string(),
                actor: "agent".to_string(),
                run: Some(RunInput {
                    id: Some("run-redacted".to_string()),
                    project_scope_id: Some("proj".to_string()),
                    task_scope_id: None,
                    source: "test".to_string(),
                    metadata: json!({"summary": "keep out of commit log"}),
                }),
                proposals: vec![CommitProposal {
                    scope: ScopeRef::normalized(ScopeKind::Project, "proj").expect("scope"),
                    pack_name: "main".to_string(),
                    entry: sample_entry("k", body),
                }],
            })
            .expect("commit");
        let conn = store.lock_conn().expect("conn");
        let request_json: String = conn
            .query_row(
                "SELECT request_json FROM commits WHERE request_id = 'req-redacted-log'",
                [],
                |row| row.get(0),
            )
            .expect("request json");
        drop(conn);
        assert!(!request_json.contains(body));
        assert!(!request_json.contains("keep out of commit log"));
        assert!(request_json.contains("\"proposal_count\":1"));
    }

    #[test]
    fn archived_packs_reject_mutations() {
        let store = temp_store();
        let scope = ScopeRef::normalized(ScopeKind::Project, "proj").expect("scope");
        let _ = store
            .put_entry(PutEntryRequest {
                scope: scope.clone(),
                pack_name: "main".to_string(),
                entry: sample_entry("existing", "body"),
                actor: "tester".to_string(),
            })
            .expect("seed");
        let _ = store
            .update_pack(UpdatePackRequest {
                selector: PackSelector {
                    scope: scope.clone(),
                    name: "main".to_string(),
                },
                description: None,
                metadata: None,
                status: Some(PackStatus::Archived),
                locked: None,
                lock_reason: None,
                actor: "tester".to_string(),
            })
            .expect("archive pack");

        let put_err = store
            .put_entry(PutEntryRequest {
                scope: scope.clone(),
                pack_name: "main".to_string(),
                entry: sample_entry("new", "body"),
                actor: "tester".to_string(),
            })
            .expect_err("put should fail");
        assert!(matches!(put_err, ContextError::Conflict(_)));

        let delete_err = store
            .delete_entry(DeleteEntryRequest {
                selector: EntrySelector {
                    scope: scope.clone(),
                    pack_name: "main".to_string(),
                    entry_key: "existing".to_string(),
                },
                actor: "tester".to_string(),
            })
            .expect_err("delete should fail");
        assert!(matches!(delete_err, ContextError::Conflict(_)));

        let commit = store
            .commit_work(CommitWorkRequest {
                request_id: "req-archived".to_string(),
                actor: "agent".to_string(),
                run: None,
                proposals: vec![CommitProposal {
                    scope,
                    pack_name: "main".to_string(),
                    entry: sample_entry("proposal", "body"),
                }],
            })
            .expect("commit");
        assert_eq!(commit.status, CommitStatus::Rejected);
        assert_eq!(commit.items.len(), 1);
        assert_eq!(commit.items[0].disposition, CommitDisposition::Rejected);
    }

    #[test]
    fn migrates_v4_databases_and_defaults_review_policy_to_balanced() {
        let mut conn = Connection::open_in_memory().expect("connection");
        ContextStore::configure_connection(&mut conn).expect("configure");
        conn.execute(
            "CREATE TABLE schema_migrations (version INTEGER PRIMARY KEY NOT NULL, applied_at TEXT NOT NULL) STRICT",
            [],
        )
        .expect("migration table");
        for version in 1..=4 {
            let tx = conn.transaction().expect("transaction");
            match version {
                1 => migration_v1(&tx).expect("v1"),
                2 => migration_v2(&tx).expect("v2"),
                3 => migration_v3(&tx).expect("v3"),
                4 => migration_v4(&tx).expect("v4"),
                _ => unreachable!(),
            }
            tx.execute(
                "INSERT INTO schema_migrations (version, applied_at) VALUES (?, ?)",
                params![version, now_utc().to_rfc3339()],
            )
            .expect("record migration");
            tx.commit().expect("commit migration");
        }
        assert_eq!(current_schema_version(&conn).expect("version"), 4);
        conn.execute(
            "INSERT INTO revisions (id, entity_type, entity_id, revision_no, action, snapshot_json, provenance_json, commit_request_id, run_id, created_at) VALUES ('rev-v4', 'entry', 'entry-v4', 1, 'create', '{}', '{}', NULL, NULL, ?)",
            params![now_utc().to_rfc3339()],
        )
        .expect("seed revision");
        conn.execute(
            "INSERT INTO review_items (id, request_id, scope_kind, scope_id, pack_name, entry_key, state, reason, proposed_entry_json, existing_entry_json, resolution_note, created_at, updated_at, current_revision_no) VALUES ('review-v4', 'request-v4', 'project', 'proj', 'main', 'key', 'pending', 'locked', '{}', NULL, NULL, ?, ?, 0)",
            params![now_utc().to_rfc3339(), now_utc().to_rfc3339()],
        )
        .expect("seed review");

        ContextStore::migrate(&mut conn).expect("migrate to latest");
        assert_eq!(
            current_schema_version(&conn).expect("latest version"),
            LATEST_SCHEMA_VERSION
        );
        let policy = get_review_policy_tx(&conn).expect("policy");
        assert_eq!(policy.mode, ReviewMode::Balanced);
        assert_eq!(policy.revision_no, 1);
        let preserved_revision: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM revisions WHERE id = 'rev-v4' AND entity_type = 'entry'",
                [],
                |row| row.get(0),
            )
            .expect("preserved revision");
        let preserved_review: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM review_items WHERE id = 'review-v4' AND reason = 'locked'",
                [],
                |row| row.get(0),
            )
            .expect("preserved review");
        assert_eq!(preserved_revision, 1);
        assert_eq!(preserved_review, 1);
    }

    #[test]
    fn review_policy_is_persisted_audited_and_secret_checked() {
        let store = temp_store();
        let default_policy = store.get_review_policy().expect("default policy");
        assert_eq!(default_policy.mode, ReviewMode::Balanced);

        let updated = store
            .set_review_policy(SetReviewPolicyRequest {
                mode: ReviewMode::Fast,
                metadata: json!({"reason": "interactive workflow"}),
                actor: "admin".to_string(),
            })
            .expect("set policy");
        assert_eq!(updated.mode, ReviewMode::Fast);
        assert_eq!(updated.revision_no, default_policy.revision_no + 1);
        assert_eq!(store.get_review_policy().expect("persisted"), updated);

        let unchanged = store
            .set_review_policy(SetReviewPolicyRequest {
                mode: ReviewMode::Fast,
                metadata: json!({"reason": "interactive workflow"}),
                actor: "another-admin".to_string(),
            })
            .expect("idempotent set");
        assert_eq!(unchanged.revision_no, updated.revision_no);

        let secret_err = store
            .set_review_policy(SetReviewPolicyRequest {
                mode: ReviewMode::Strict,
                metadata: json!({"credential": synthetic_secret("sk-")}),
                actor: "admin".to_string(),
            })
            .expect_err("secret metadata");
        assert!(matches!(secret_err, ContextError::SecretDetected(_)));
        let actor_err = store
            .set_review_policy(SetReviewPolicyRequest {
                mode: ReviewMode::Strict,
                metadata: json!({}),
                actor: synthetic_secret("xoxb-1234567890-"),
            })
            .expect_err("secret actor");
        assert!(matches!(actor_err, ContextError::SecretDetected(_)));
        assert_eq!(
            store.get_review_policy().expect("policy unchanged").mode,
            ReviewMode::Fast
        );

        let conn = store.lock_conn().expect("conn");
        let (revision_count, source): (i64, String) = conn
            .query_row(
                "SELECT COUNT(*), (SELECT json_extract(provenance_json, '$.source') FROM revisions WHERE entity_type = 'policy' AND entity_id = 'review' ORDER BY revision_no DESC LIMIT 1) FROM revisions WHERE entity_type = 'policy' AND entity_id = 'review'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("policy revisions");
        assert_eq!(revision_count, 2);
        assert_eq!(source, "review_policy_update");
    }

    #[test]
    fn review_modes_enforce_strict_balanced_and_fast_semantics() {
        let project = ScopeRef::normalized(ScopeKind::Project, "proj").expect("scope");

        let strict = temp_store();
        strict
            .set_review_policy(SetReviewPolicyRequest {
                mode: ReviewMode::Strict,
                metadata: json!({}),
                actor: "admin".to_string(),
            })
            .expect("strict policy");
        let strict_result = strict
            .commit_work(CommitWorkRequest {
                request_id: "strict-new".to_string(),
                actor: "agent".to_string(),
                run: None,
                proposals: vec![CommitProposal {
                    scope: project.clone(),
                    pack_name: "main".to_string(),
                    entry: sample_entry("new", "strict review"),
                }],
            })
            .expect("strict commit");
        assert_eq!(
            strict_result.items[0].disposition,
            CommitDisposition::Pending
        );
        assert_eq!(
            strict_result.items[0].reason.as_deref(),
            Some(ReviewReason::StrictPolicy.as_str())
        );
        let duplicate_entry = sample_entry("duplicate", "same");
        strict
            .put_entry(PutEntryRequest {
                scope: project.clone(),
                pack_name: "main".to_string(),
                entry: duplicate_entry.clone(),
                actor: "tester".to_string(),
            })
            .expect("seed duplicate");
        let strict_duplicate = strict
            .commit_work(CommitWorkRequest {
                request_id: "strict-duplicate".to_string(),
                actor: "agent".to_string(),
                run: None,
                proposals: vec![CommitProposal {
                    scope: project.clone(),
                    pack_name: "main".to_string(),
                    entry: duplicate_entry,
                }],
            })
            .expect("strict duplicate");
        assert_eq!(
            strict_duplicate.items[0].disposition,
            CommitDisposition::Duplicate
        );

        let balanced = temp_store();
        balanced
            .put_entry(PutEntryRequest {
                scope: project.clone(),
                pack_name: "main".to_string(),
                entry: sample_entry("conflict", "old"),
                actor: "tester".to_string(),
            })
            .expect("seed balanced conflict");
        let balanced_result = balanced
            .commit_work(CommitWorkRequest {
                request_id: "balanced".to_string(),
                actor: "agent".to_string(),
                run: None,
                proposals: vec![
                    CommitProposal {
                        scope: project.clone(),
                        pack_name: "main".to_string(),
                        entry: sample_entry("safe", "direct"),
                    },
                    CommitProposal {
                        scope: project.clone(),
                        pack_name: "main".to_string(),
                        entry: sample_entry("conflict", "changed"),
                    },
                ],
            })
            .expect("balanced commit");
        assert_eq!(
            balanced_result.items[0].disposition,
            CommitDisposition::Applied
        );
        assert_eq!(
            balanced_result.items[1].disposition,
            CommitDisposition::Pending
        );

        let fast = temp_store();
        fast.set_review_policy(SetReviewPolicyRequest {
            mode: ReviewMode::Fast,
            metadata: json!({}),
            actor: "admin".to_string(),
        })
        .expect("fast policy");
        fast.put_entry(PutEntryRequest {
            scope: project.clone(),
            pack_name: "main".to_string(),
            entry: sample_entry("conflict", "old"),
            actor: "tester".to_string(),
        })
        .expect("seed fast conflict");
        fast.put_entry(PutEntryRequest {
            scope: project.clone(),
            pack_name: "main".to_string(),
            entry: EntryInput {
                locked: true,
                ..sample_entry("locked", "old")
            },
            actor: "tester".to_string(),
        })
        .expect("seed locked");
        let fast_result = fast
            .commit_work(CommitWorkRequest {
                request_id: "fast".to_string(),
                actor: "agent".to_string(),
                run: None,
                proposals: vec![
                    CommitProposal {
                        scope: project.clone(),
                        pack_name: "main".to_string(),
                        entry: sample_entry("conflict", "changed"),
                    },
                    CommitProposal {
                        scope: project.clone(),
                        pack_name: "main".to_string(),
                        entry: sample_entry("locked", "changed"),
                    },
                    CommitProposal {
                        scope: ScopeRef::global(),
                        pack_name: "main".to_string(),
                        entry: sample_entry("global", "review"),
                    },
                    CommitProposal {
                        scope: project,
                        pack_name: "main".to_string(),
                        entry: sample_entry("secret", &synthetic_secret("token = sk-")),
                    },
                ],
            })
            .expect("fast commit");
        assert_eq!(fast_result.items[0].disposition, CommitDisposition::Applied);
        assert_eq!(fast_result.items[1].disposition, CommitDisposition::Pending);
        assert_eq!(fast_result.items[2].disposition, CommitDisposition::Pending);
        assert_eq!(
            fast_result.items[3].disposition,
            CommitDisposition::Rejected
        );
        assert_eq!(
            fast.get_entry(&EntrySelector {
                scope: ScopeRef::normalized(ScopeKind::Project, "proj").expect("scope"),
                pack_name: "main".to_string(),
                entry_key: "conflict".to_string(),
            })
            .expect("updated conflict")
            .value
            .render_markdown(),
            "changed"
        );
    }

    #[test]
    fn source_import_candidate_equality_matches_preview_semantics() {
        let store = temp_store();
        let scope = ScopeRef::normalized(ScopeKind::Project, "proj").expect("scope");
        let mut candidate = sample_entry("candidate", "exact body");
        candidate.title = Some("Exact title".to_string());
        candidate.kind = "instructions".to_string();
        candidate.tags = vec!["alpha".to_string(), "beta".to_string()];
        candidate.metadata = json!({"nested": {"enabled": true}});
        candidate.locked = true;
        candidate.provenance = Some(Provenance {
            actor: "importer".to_string(),
            source: "source_import:ucm_json".to_string(),
            source_ref: Some("bundle.json#candidate".to_string()),
            run_id: None,
            request_id: None,
            note: Some("imported".to_string()),
        });
        let record = store
            .put_entry(PutEntryRequest {
                scope,
                pack_name: "main".to_string(),
                entry: candidate.clone(),
                actor: "seed".to_string(),
            })
            .expect("stored candidate");
        assert!(entry_record_matches_input(&record, &candidate));

        let mut changed = candidate.clone();
        changed.title = Some("Changed title".to_string());
        assert!(!entry_record_matches_input(&record, &changed));

        let mut changed = candidate.clone();
        changed.kind = "decision".to_string();
        assert!(!entry_record_matches_input(&record, &changed));

        let mut changed = candidate.clone();
        changed.value = EntryValue::Markdown {
            body: "exact body\n".to_string(),
        };
        assert!(!entry_record_matches_input(&record, &changed));

        let mut changed = candidate.clone();
        changed.tags.reverse();
        assert!(!entry_record_matches_input(&record, &changed));

        let mut changed = candidate.clone();
        changed.metadata = json!({"nested": {"enabled": false}});
        assert!(!entry_record_matches_input(&record, &changed));

        let mut changed = candidate.clone();
        changed.locked = false;
        assert!(!entry_record_matches_input(&record, &changed));

        let mut provenance_only = candidate;
        provenance_only.provenance = Some(Provenance::system("other-importer", "retry"));
        assert!(entry_record_matches_input(&record, &provenance_only));
    }

    #[test]
    fn strict_source_import_reviews_lock_only_conflicts() {
        assert_lock_only_source_import_is_reviewed(ReviewMode::Strict);
    }

    #[test]
    fn balanced_source_import_reviews_lock_only_conflicts() {
        assert_lock_only_source_import_is_reviewed(ReviewMode::Balanced);
    }

    #[test]
    fn fast_source_import_applies_lock_only_conflicts() {
        let store = temp_store();
        set_review_mode(&store, ReviewMode::Fast);
        let request = locked_source_import_request(
            ScopeRef::normalized(ScopeKind::Project, "proj").expect("scope"),
        );
        let (selector, preview) = seed_unlocked_source_import_candidate(&store, &request);
        assert_eq!(preview.review_mode, ReviewMode::Fast);

        let result = store.apply_source_import(request).expect("apply import");
        assert_eq!(result.applied_count, 1);
        assert_eq!(result.pending_count, 0);
        assert_eq!(result.items[0].disposition, CommitDisposition::Applied);
        assert!(store.get_entry(&selector).expect("updated entry").locked);
        assert!(
            store
                .review_list(Some(ReviewState::Pending))
                .expect("pending reviews")
                .is_empty()
        );
    }

    #[test]
    fn source_import_preview_fingerprint_uses_canonical_full_state() {
        let candidate = SourceImportCandidate {
            candidate_index: 0,
            document_index: 0,
            source_path: Some("AGENTS.md".to_string()),
            detected_source_kind: SourceImportKind::AgentsMd,
            entry: sample_entry("agents-instructions", "body"),
            disposition: SourceImportDisposition::New,
            existing_entry_id: None,
            existing_revision_no: None,
            warnings: vec!["candidate warning".to_string()],
        };
        let left = SourceImportPreview {
            destination: ScopeRef::normalized(ScopeKind::Project, "a").expect("scope"),
            pack_name: "b::c".to_string(),
            review_mode: ReviewMode::Balanced,
            destination_pack: SourceImportPackGovernance::default(),
            preview_fingerprint: None,
            candidates: vec![candidate],
            warnings: vec!["preview warning".to_string()],
            apply_allowed: true,
        };
        let right = SourceImportPreview {
            destination: ScopeRef::normalized(ScopeKind::Project, "a::b").expect("scope"),
            pack_name: "c".to_string(),
            ..left.clone()
        };
        let left_fingerprint =
            source_import_preview_fingerprint("actor", &left).expect("left fingerprint");
        assert_ne!(
            left_fingerprint,
            source_import_preview_fingerprint("actor", &right).expect("right fingerprint")
        );
        assert_ne!(
            left_fingerprint,
            source_import_preview_fingerprint("other-actor", &left).expect("actor fingerprint")
        );

        let mut changed = left.clone();
        changed.review_mode = ReviewMode::Fast;
        assert_ne!(
            left_fingerprint,
            source_import_preview_fingerprint("actor", &changed).expect("mode fingerprint")
        );

        let mut changed = left.clone();
        changed.destination_pack = SourceImportPackGovernance {
            exists: true,
            status: Some(PackStatus::Active),
            locked: true,
            lock_reason: Some("hold".to_string()),
            revision_no: Some(3),
        };
        assert_ne!(
            left_fingerprint,
            source_import_preview_fingerprint("actor", &changed).expect("pack fingerprint")
        );

        let mut changed = left.clone();
        changed.candidates[0].entry.locked = true;
        changed.candidates[0].disposition = SourceImportDisposition::Conflict;
        changed.candidates[0].existing_entry_id = Some("entry-1".to_string());
        changed.candidates[0].existing_revision_no = Some(7);
        changed.candidates[0]
            .warnings
            .push("state changed".to_string());
        assert_ne!(
            left_fingerprint,
            source_import_preview_fingerprint("actor", &changed).expect("candidate fingerprint")
        );
    }

    #[test]
    fn source_import_expected_fingerprint_rejects_destination_state_change() {
        let store = temp_store();
        let destination = ScopeRef::normalized(ScopeKind::Project, "proj").expect("scope");
        let mut request =
            instruction_source_import_request(destination.clone(), "# Rules\n\nPreviewed.");
        let preview = store
            .preview_source_import((&request).into())
            .expect("preview");
        let candidate_key = preview.candidates[0].entry.key.clone();
        request.expected_preview_fingerprint = preview.preview_fingerprint;

        store
            .put_entry(PutEntryRequest {
                scope: destination.clone(),
                pack_name: "instructions".to_string(),
                entry: sample_entry(&candidate_key, "concurrent destination content"),
                actor: "other-actor".to_string(),
            })
            .expect("concurrent write");

        let err = store
            .apply_source_import(request)
            .expect_err("stale preview must fail");
        assert!(matches!(err, ContextError::Conflict(_)));
        assert_eq!(
            store
                .get_entry(&EntrySelector {
                    scope: destination,
                    pack_name: "instructions".to_string(),
                    entry_key: candidate_key,
                })
                .expect("concurrent entry")
                .value
                .render_markdown(),
            "concurrent destination content"
        );
    }

    #[test]
    fn source_import_expected_fingerprint_rejects_review_mode_change() {
        let store = temp_store();
        let mut request = instruction_source_import_request(
            ScopeRef::normalized(ScopeKind::Project, "proj").expect("scope"),
            "# Rules\n\nPreviewed.",
        );
        let preview = store
            .preview_source_import((&request).into())
            .expect("preview");
        request.expected_preview_fingerprint = preview.preview_fingerprint;
        set_review_mode(&store, ReviewMode::Strict);

        let err = store
            .apply_source_import(request)
            .expect_err("review mode change must fail");
        assert!(matches!(err, ContextError::Conflict(_)));
        assert_eq!(store.stats().expect("stats").entries, 0);
    }

    #[test]
    fn source_import_reapplies_after_imported_entry_is_deleted() {
        let store = temp_store();
        let destination = ScopeRef::normalized(ScopeKind::Project, "proj").expect("scope");
        let request = instruction_source_import_request(
            destination.clone(),
            "# Rules\n\nRestore this import.",
        );

        let first = store
            .apply_source_import(request.clone())
            .expect("first apply");
        assert_eq!(first.applied_count, 1);
        let selector = EntrySelector {
            scope: destination,
            pack_name: "instructions".to_string(),
            entry_key: "agents-instructions".to_string(),
        };
        let deleted = store
            .delete_entry(DeleteEntryRequest {
                selector: selector.clone(),
                actor: "operator".to_string(),
            })
            .expect("delete imported entry");
        assert_eq!(deleted.status, EntryStatus::Deleted);

        let preview = store
            .preview_source_import((&request).into())
            .expect("reapply preview");
        assert_eq!(
            preview.candidates[0].disposition,
            SourceImportDisposition::New
        );
        let second = store
            .apply_source_import(request)
            .expect("reapply deleted entry");
        assert_eq!(second.applied_count, 1);
        assert_ne!(second.request_id, first.request_id);
        let restored = store.get_entry(&selector).expect("restored entry");
        assert_eq!(restored.status, EntryStatus::Active);
        assert_eq!(
            restored.value.render_markdown(),
            "# Rules\n\nRestore this import."
        );
    }

    #[test]
    fn source_import_policy_change_does_not_replay_cached_pending_result() {
        let store = temp_store();
        let destination = ScopeRef::normalized(ScopeKind::Project, "proj").expect("scope");
        let request =
            instruction_source_import_request(destination.clone(), "# Rules\n\nDesired content.");
        let preview = store
            .preview_source_import((&request).into())
            .expect("preview");
        let candidate_key = preview.candidates[0].entry.key.clone();
        store
            .put_entry(PutEntryRequest {
                scope: destination.clone(),
                pack_name: "instructions".to_string(),
                entry: sample_entry(&candidate_key, "existing content"),
                actor: "seed".to_string(),
            })
            .expect("seed conflict");

        let balanced = store
            .apply_source_import(request.clone())
            .expect("balanced apply");
        assert_eq!(balanced.pending_count, 1);
        set_review_mode(&store, ReviewMode::Fast);
        let fast = store.apply_source_import(request).expect("fast reapply");
        assert_eq!(fast.applied_count, 1);
        assert_ne!(fast.request_id, balanced.request_id);
        assert_eq!(
            store
                .get_entry(&EntrySelector {
                    scope: destination,
                    pack_name: "instructions".to_string(),
                    entry_key: candidate_key,
                })
                .expect("updated entry")
                .value
                .render_markdown(),
            "# Rules\n\nDesired content."
        );
    }

    #[test]
    fn source_import_entry_revision_change_does_not_replay_cached_applied_result() {
        let store = temp_store();
        set_review_mode(&store, ReviewMode::Fast);
        let destination = ScopeRef::normalized(ScopeKind::Project, "proj").expect("scope");
        let request =
            instruction_source_import_request(destination.clone(), "# Rules\n\nDesired content.");
        let preview = store
            .preview_source_import((&request).into())
            .expect("preview");
        let candidate_key = preview.candidates[0].entry.key.clone();
        store
            .put_entry(PutEntryRequest {
                scope: destination.clone(),
                pack_name: "instructions".to_string(),
                entry: sample_entry(&candidate_key, "first conflict"),
                actor: "seed".to_string(),
            })
            .expect("seed conflict");

        let first = store
            .apply_source_import(request.clone())
            .expect("first fast apply");
        assert_eq!(first.applied_count, 1);
        store
            .put_entry(PutEntryRequest {
                scope: destination.clone(),
                pack_name: "instructions".to_string(),
                entry: sample_entry(&candidate_key, "later concurrent content"),
                actor: "other-writer".to_string(),
            })
            .expect("later write");

        let second = store
            .apply_source_import(request)
            .expect("reapply after revision change");
        assert_eq!(second.applied_count, 1);
        assert_ne!(second.request_id, first.request_id);
        assert_eq!(
            store
                .get_entry(&EntrySelector {
                    scope: destination,
                    pack_name: "instructions".to_string(),
                    entry_key: candidate_key,
                })
                .expect("restored desired entry")
                .value
                .render_markdown(),
            "# Rules\n\nDesired content."
        );
    }

    #[test]
    fn source_import_stable_pending_retry_does_not_duplicate_review() {
        let store = temp_store();
        let request = instruction_source_import_request(
            ScopeRef::global(),
            "# Rules\n\nNeeds global review.",
        );

        let first = store
            .apply_source_import(request.clone())
            .expect("first pending apply");
        assert_eq!(first.pending_count, 1);
        let second = store.apply_source_import(request).expect("stable retry");
        assert_eq!(second.pending_count, 1);
        assert_eq!(
            second.items[0].review_id.as_deref(),
            first.items[0].review_id.as_deref()
        );
        assert_eq!(
            store
                .review_list(Some(ReviewState::Pending))
                .expect("pending reviews")
                .len(),
            1
        );
    }

    #[test]
    fn source_import_pack_lock_invalidates_preview_and_routes_to_review() {
        let store = temp_store();
        let destination = ScopeRef::normalized(ScopeKind::Project, "proj").expect("scope");
        let created = create_import_pack(&store, destination.clone());
        let mut request =
            instruction_source_import_request(destination.clone(), "# Rules\n\nLocked import.");
        let preview = store
            .preview_source_import((&request).into())
            .expect("unlocked preview");
        assert_eq!(
            preview.destination_pack,
            SourceImportPackGovernance {
                exists: true,
                status: Some(PackStatus::Active),
                locked: false,
                lock_reason: None,
                revision_no: Some(created.revision_no),
            }
        );
        request.expected_preview_fingerprint = preview.preview_fingerprint;

        let locked = store
            .update_pack(UpdatePackRequest {
                selector: PackSelector {
                    scope: destination.clone(),
                    name: "instructions".to_string(),
                },
                description: None,
                metadata: None,
                status: None,
                locked: Some(true),
                lock_reason: Some("operator hold".to_string()),
                actor: "pack-owner".to_string(),
            })
            .expect("lock destination pack");
        let err = store
            .apply_source_import(request.clone())
            .expect_err("stale unlocked preview");
        assert!(matches!(err, ContextError::Conflict(_)));

        let fresh = store
            .preview_source_import((&request).into())
            .expect("locked preview");
        assert_eq!(
            fresh.destination_pack,
            SourceImportPackGovernance {
                exists: true,
                status: Some(PackStatus::Active),
                locked: true,
                lock_reason: Some("operator hold".to_string()),
                revision_no: Some(locked.revision_no),
            }
        );
        assert!(fresh.apply_allowed);
        assert!(
            fresh
                .warnings
                .iter()
                .any(|warning| warning.contains("locked"))
        );
        request.expected_preview_fingerprint = fresh.preview_fingerprint;
        let result = store
            .apply_source_import(request)
            .expect("locked pack apply");
        assert_eq!(result.pending_count, 1);
        assert_eq!(
            result.items[0].reason.as_deref(),
            Some(ReviewReason::Locked.as_str())
        );
        assert_eq!(store.stats().expect("stats").entries, 0);
    }

    #[test]
    fn source_import_pack_archive_invalidates_and_blocks_preview() {
        let store = temp_store();
        let destination = ScopeRef::normalized(ScopeKind::Project, "proj").expect("scope");
        create_import_pack(&store, destination.clone());
        let mut request =
            instruction_source_import_request(destination.clone(), "# Rules\n\nArchived import.");
        let preview = store
            .preview_source_import((&request).into())
            .expect("active preview");
        request.expected_preview_fingerprint = preview.preview_fingerprint;

        let archived = store
            .update_pack(UpdatePackRequest {
                selector: PackSelector {
                    scope: destination,
                    name: "instructions".to_string(),
                },
                description: None,
                metadata: None,
                status: Some(PackStatus::Archived),
                locked: None,
                lock_reason: None,
                actor: "pack-owner".to_string(),
            })
            .expect("archive destination pack");
        let err = store
            .apply_source_import(request.clone())
            .expect_err("stale active preview");
        assert!(matches!(err, ContextError::Conflict(_)));

        let fresh = store
            .preview_source_import((&request).into())
            .expect("archived preview");
        assert_eq!(
            fresh.destination_pack,
            SourceImportPackGovernance {
                exists: true,
                status: Some(PackStatus::Archived),
                locked: false,
                lock_reason: None,
                revision_no: Some(archived.revision_no),
            }
        );
        assert!(!fresh.apply_allowed);
        assert!(
            fresh
                .warnings
                .iter()
                .any(|warning| warning.contains("archived"))
        );
        request.expected_preview_fingerprint = fresh.preview_fingerprint;
        let err = store
            .apply_source_import(request)
            .expect_err("archived pack must block apply");
        assert!(matches!(err, ContextError::Validation(_)));
        assert_eq!(store.stats().expect("stats").entries, 0);
    }

    #[test]
    fn stale_fast_preview_cannot_overwrite_concurrent_import() {
        let dir = tempdir().expect("tempdir");
        let db_path = dir.path().join("concurrent-source-import.db");
        let setup = ContextStore::open(&db_path).expect("setup store");
        set_review_mode(&setup, ReviewMode::Fast);
        drop(setup);

        let store_a = ContextStore::open(&db_path).expect("store a");
        let store_b = ContextStore::open(&db_path).expect("store b");
        let destination = ScopeRef::normalized(ScopeKind::Project, "proj").expect("scope");
        let mut request_a =
            instruction_source_import_request(destination.clone(), "# Rules\n\nFirst writer.");
        let mut request_b =
            instruction_source_import_request(destination.clone(), "# Rules\n\nSecond writer.");
        let preview_a = store_a
            .preview_source_import((&request_a).into())
            .expect("preview a");
        let preview_b = store_b
            .preview_source_import((&request_b).into())
            .expect("preview b");
        let candidate_key = preview_a.candidates[0].entry.key.clone();
        request_a.expected_preview_fingerprint = preview_a.preview_fingerprint;
        request_b.expected_preview_fingerprint = preview_b.preview_fingerprint;

        let (first_done_tx, first_done_rx) = std::sync::mpsc::channel();
        let first = std::thread::spawn(move || {
            let result = store_a.apply_source_import(request_a);
            first_done_tx.send(()).expect("signal first completion");
            result
        });
        let second = std::thread::spawn(move || {
            first_done_rx.recv().expect("wait for first import");
            store_b.apply_source_import(request_b)
        });

        assert_eq!(
            first
                .join()
                .expect("first thread")
                .expect("first apply")
                .applied_count,
            1
        );
        let second_error = second
            .join()
            .expect("second thread")
            .expect_err("stale second apply");
        assert!(matches!(second_error, ContextError::Conflict(_)));

        let verifier = ContextStore::open(&db_path).expect("verifier");
        assert_eq!(
            verifier
                .get_entry(&EntrySelector {
                    scope: destination,
                    pack_name: "instructions".to_string(),
                    entry_key: candidate_key,
                })
                .expect("winning entry")
                .value
                .render_markdown(),
            "# Rules\n\nFirst writer."
        );
    }

    #[test]
    fn source_import_apply_rolls_back_all_candidates_on_late_failure() {
        let store = temp_store();
        let destination = ScopeRef::normalized(ScopeKind::Project, "proj").expect("scope");
        let mut request = SourceImportApplyRequest {
            source_kind: SourceImportKind::Auto,
            documents: vec![
                SourceImportDocument {
                    path: Some("AGENTS.md".to_string()),
                    payload: "# Agent rules".to_string(),
                },
                SourceImportDocument {
                    path: Some("CLAUDE.md".to_string()),
                    payload: "# Claude rules".to_string(),
                },
            ],
            destination,
            pack_name: Some("instructions".to_string()),
            actor: "importer".to_string(),
            expected_preview_fingerprint: None,
        };
        let preview = store
            .preview_source_import((&request).into())
            .expect("preview");
        request.expected_preview_fingerprint = preview.preview_fingerprint;
        {
            let conn = store.lock_conn().expect("connection");
            conn.execute_batch(
                "CREATE TRIGGER fail_second_source_import
                 BEFORE INSERT ON entries
                 WHEN NEW.entry_key = 'claude-instructions'
                 BEGIN
                   SELECT RAISE(ABORT, 'forced source import failure');
                 END;",
            )
            .expect("failure trigger");
        }

        let err = store
            .apply_source_import(request)
            .expect_err("late candidate failure");
        assert!(matches!(err, ContextError::Sql(_)));
        let stats = store.stats().expect("stats");
        assert_eq!(stats.packs, 0);
        assert_eq!(stats.entries, 0);
        let conn = store.lock_conn().expect("connection");
        let source_import_commits: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM commits WHERE request_id LIKE 'source-import-%'",
                [],
                |row| row.get(0),
            )
            .expect("commit count");
        assert_eq!(source_import_commits, 0);
    }

    #[test]
    fn source_import_preview_reports_duplicates_and_conflicts_and_apply_is_idempotent() {
        let store = temp_store();
        let destination = ScopeRef::normalized(ScopeKind::Project, "proj").expect("scope");
        let request = SourceImportApplyRequest {
            source_kind: SourceImportKind::Auto,
            documents: vec![SourceImportDocument {
                path: Some("AGENTS.md".to_string()),
                payload: "# Project instructions\n\nRun focused tests.".to_string(),
            }],
            destination: destination.clone(),
            pack_name: Some("instructions".to_string()),
            actor: "importer".to_string(),
            expected_preview_fingerprint: None,
        };
        let first = store
            .apply_source_import(request.clone())
            .expect("first apply");
        assert_eq!(first.candidate_count, 1);
        assert_eq!(first.imported_count, 1);
        assert_eq!(first.applied_count, 1);
        assert_eq!(first.pending_count, 0);
        let entry = store
            .get_entry(&EntrySelector {
                scope: destination.clone(),
                pack_name: "instructions".to_string(),
                entry_key: "agents-instructions".to_string(),
            })
            .expect("imported entry");
        assert_eq!(entry.provenance.source_ref.as_deref(), Some("AGENTS.md"));
        let revision_no = entry.revision_no;

        let duplicate_preview = store
            .preview_source_import((&request).into())
            .expect("duplicate preview");
        assert_eq!(
            duplicate_preview.candidates[0].disposition,
            SourceImportDisposition::Duplicate
        );
        let second = store
            .apply_source_import(request.clone())
            .expect("idempotent apply");
        assert_eq!(second.imported_count, 0);
        assert_eq!(second.skipped_count, 1);
        assert_eq!(second.items[0].disposition, CommitDisposition::Duplicate);
        assert_eq!(
            store
                .get_entry(&EntrySelector {
                    scope: destination.clone(),
                    pack_name: "instructions".to_string(),
                    entry_key: "agents-instructions".to_string(),
                })
                .expect("unchanged entry")
                .revision_no,
            revision_no
        );

        let mut changed = request;
        changed.documents[0].payload.push_str("\nUse clippy.");
        let conflict_preview = store
            .preview_source_import((&changed).into())
            .expect("conflict preview");
        assert_eq!(
            conflict_preview.candidates[0].disposition,
            SourceImportDisposition::Conflict
        );
        assert_eq!(conflict_preview.destination, destination);
        assert_eq!(conflict_preview.pack_name, "instructions");
        assert!(conflict_preview.apply_allowed);
    }

    #[test]
    fn mixed_source_import_retries_do_not_duplicate_pending_reviews() {
        let store = temp_store();
        let destination = ScopeRef::normalized(ScopeKind::Project, "proj").expect("scope");
        store
            .put_entry(PutEntryRequest {
                scope: destination.clone(),
                pack_name: "main".to_string(),
                entry: sample_entry("claude-instructions", "existing"),
                actor: "tester".to_string(),
            })
            .expect("seed conflict");
        let request = SourceImportApplyRequest {
            source_kind: SourceImportKind::Auto,
            documents: vec![
                SourceImportDocument {
                    path: Some("AGENTS.md".to_string()),
                    payload: "# Agent instructions".to_string(),
                },
                SourceImportDocument {
                    path: Some("CLAUDE.md".to_string()),
                    payload: "# Claude instructions".to_string(),
                },
            ],
            destination,
            pack_name: None,
            actor: "importer".to_string(),
            expected_preview_fingerprint: None,
        };

        let first = store
            .apply_source_import(request.clone())
            .expect("first apply");
        assert_eq!(first.applied_count, 1);
        assert_eq!(first.pending_count, 1);
        assert_eq!(store.review_list(None).expect("reviews").len(), 1);

        let retry = store.apply_source_import(request).expect("retry");
        assert_eq!(retry.applied_count, 0);
        assert_eq!(retry.pending_count, 1);
        assert_eq!(retry.skipped_count, 1);
        assert_eq!(store.review_list(None).expect("reviews").len(), 1);
    }

    #[test]
    fn staged_source_import_accepts_ucm_json_and_markdown_exports() {
        let source = temp_store();
        source
            .put_entry(PutEntryRequest {
                scope: ScopeRef::global(),
                pack_name: "main".to_string(),
                entry: sample_entry("exported", "portable instructions"),
                actor: "tester".to_string(),
            })
            .expect("seed export");
        let export_request = ExportRequest {
            project_scope_id: None,
            task_scope_id: None,
            scope: None,
            pack_name: None,
            include_deleted: false,
            include_reviews: false,
            include_runs: false,
        };
        let json_payload = source
            .export_json(export_request.clone())
            .expect("json export");
        let markdown_payload = source
            .export_markdown(export_request)
            .expect("markdown export");
        let destination = ScopeRef::normalized(ScopeKind::Project, "proj").expect("scope");
        let target = temp_store();

        for (source_kind, payload, path, pack_name) in [
            (
                SourceImportKind::UcmJson,
                json_payload,
                "ucm-export.json",
                "json-import",
            ),
            (
                SourceImportKind::UcmMarkdown,
                markdown_payload,
                "ucm-export.md",
                "markdown-import",
            ),
        ] {
            let request = SourceImportApplyRequest {
                source_kind,
                documents: vec![SourceImportDocument {
                    path: Some(path.to_string()),
                    payload,
                }],
                destination: destination.clone(),
                pack_name: Some(pack_name.to_string()),
                actor: "importer".to_string(),
                expected_preview_fingerprint: None,
            };
            let preview = target
                .preview_source_import((&request).into())
                .expect("preview");
            assert_eq!(preview.candidates.len(), 1);
            assert_eq!(preview.candidates[0].detected_source_kind, source_kind);
            assert_eq!(preview.candidates[0].entry.key, "exported");
            let applied = target.apply_source_import(request).expect("apply");
            assert_eq!(applied.applied_count, 1);
            assert_eq!(
                target
                    .get_entry(&EntrySelector {
                        scope: destination.clone(),
                        pack_name: pack_name.to_string(),
                        entry_key: "exported".to_string(),
                    })
                    .expect("imported entry")
                    .value
                    .render_markdown(),
                "portable instructions"
            );
        }
    }

    #[test]
    fn source_import_rejects_secrets_without_creating_records() {
        let store = temp_store();
        let err = store
            .apply_source_import(SourceImportApplyRequest {
                source_kind: SourceImportKind::PlainMarkdown,
                documents: vec![SourceImportDocument {
                    path: Some("notes.md".to_string()),
                    payload: format!("# Notes\n\n{}", synthetic_secret("token = sk-")),
                }],
                destination: ScopeRef::normalized(ScopeKind::Project, "proj").expect("scope"),
                pack_name: None,
                actor: "importer".to_string(),
                expected_preview_fingerprint: None,
            })
            .expect_err("secret source");
        assert!(matches!(err, ContextError::SecretDetected(_)));
        let stats = store.stats().expect("stats");
        assert_eq!(stats.packs, 0);
        assert_eq!(stats.entries, 0);
        assert_eq!(stats.reviews, 0);
    }

    #[test]
    fn compose_reports_metrics_and_ordered_exclusions() {
        let store = temp_store();
        let scope = ScopeRef::normalized(ScopeKind::Project, "proj").expect("scope");
        store
            .put_entry(PutEntryRequest {
                scope: scope.clone(),
                pack_name: "main".to_string(),
                entry: sample_entry("included", "visible"),
                actor: "tester".to_string(),
            })
            .expect("included");
        store
            .put_entry(PutEntryRequest {
                scope: scope.clone(),
                pack_name: "main".to_string(),
                entry: sample_entry("deleted", "hidden"),
                actor: "tester".to_string(),
            })
            .expect("deleted seed");
        store
            .delete_entry(DeleteEntryRequest {
                selector: EntrySelector {
                    scope: scope.clone(),
                    pack_name: "main".to_string(),
                    entry_key: "deleted".to_string(),
                },
                actor: "tester".to_string(),
            })
            .expect("delete");
        store
            .put_entry(PutEntryRequest {
                scope: scope.clone(),
                pack_name: "archived".to_string(),
                entry: sample_entry("archived", "hidden"),
                actor: "tester".to_string(),
            })
            .expect("archived seed");
        store
            .update_pack(UpdatePackRequest {
                selector: PackSelector {
                    scope,
                    name: "archived".to_string(),
                },
                description: None,
                metadata: None,
                status: Some(PackStatus::Archived),
                locked: None,
                lock_reason: None,
                actor: "tester".to_string(),
            })
            .expect("archive");

        let composed = store
            .compose_context(ComposeRequest {
                project_scope_id: Some("proj".to_string()),
                task_scope_id: None,
                include_archived: false,
            })
            .expect("compose");
        assert_eq!(
            composed.metrics.rendered_bytes,
            composed.rendered_markdown.len()
        );
        assert_eq!(composed.metrics.included_entries, 1);
        assert_eq!(composed.metrics.excluded_entries, 2);
        assert!(composed.metrics.estimated_tokens > 0);
        assert_eq!(composed.sections[0].entries[0].key, "included");
        assert_eq!(
            composed
                .exclusions
                .iter()
                .map(|item| item.reason.clone())
                .collect::<Vec<_>>(),
            vec![
                ComposeExclusionReason::ArchivedPack,
                ComposeExclusionReason::DeletedEntry,
            ]
        );
    }
}
