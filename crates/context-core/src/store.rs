use crate::error::{ContextError, ContextResult};
use crate::markdown::{
    export_markdown as bundle_to_markdown, import_markdown as bundle_from_markdown,
};
use crate::model::*;
use crate::secret::{
    reject_commit_metadata_for_storage, reject_entry_write_for_storage,
    reject_pack_write_for_storage, reject_review_for_storage, reject_review_transition_for_storage,
    reject_revision_metadata_for_storage, reject_run_for_storage,
};
use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension, Row, Transaction, params};
use serde::Serialize;
use serde_json::{Value, json};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tracing::debug;

const LATEST_SCHEMA_VERSION: i64 = 4;

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
        Ok(ComposeResponse {
            generated_at: now_utc(),
            sections,
            rendered_markdown,
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
        validate_request_id(&request.request_id)?;
        validate_actor(&request.actor)?;
        if request.proposals.is_empty() {
            return Err(ContextError::validation("proposals must not be empty"));
        }
        reject_commit_metadata_for_storage(&request)?;
        let mut conn = self.lock_conn()?;
        let tx = conn.transaction()?;
        if let Some(existing) = get_commit_result_tx(&tx, &request.request_id)? {
            tx.commit()?;
            return Ok(existing);
        }
        let run_id = if let Some(run) = request.run.clone() {
            Some(ensure_run_tx(&tx, run)?.id)
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
                &tx,
                &request.request_id,
                run_id.as_deref(),
                &scope,
                &proposal.pack_name,
                &proposal.entry,
                &request.actor,
                &provenance,
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
        if let Some(title) = request.title {
            review.record.proposed_entry.title = Some(title);
        }
        if let Some(kind) = request.kind {
            review.record.proposed_entry.kind = kind;
        }
        if let Some(value) = request.value {
            review.record.proposed_entry.value = value;
        }
        if let Some(tags) = request.tags {
            review.record.proposed_entry.tags = tags;
        }
        if let Some(metadata) = request.metadata {
            review.record.proposed_entry.metadata = metadata;
        }
        if let Some(locked) = request.locked {
            review.record.proposed_entry.locked = locked;
        }
        review.record.proposed_entry.validate()?;
        reject_review_for_storage(&review.record)?;
        review.record.updated_at = now_utc();
        review.record.revision_no = update_review_tx(&tx, &review.record, &request.actor, "edit")?;
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
        let stats = self.stats()?;
        Ok(HealthReport {
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
    if let Err(err) = reject_archived_pack_mutation(&pack, "write entries to") {
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
    if scope.kind == ScopeKind::Global {
        let review = insert_review_item_tx(
            tx,
            request_id,
            scope,
            pack_name,
            entry,
            existing.map(|row| row.record),
            ReviewReason::GlobalScope,
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
    if pack.locked
        || existing
            .as_ref()
            .map(|row| row.record.locked)
            .unwrap_or(false)
    {
        let review = insert_review_item_tx(
            tx,
            request_id,
            scope,
            pack_name,
            entry,
            existing.map(|row| row.record),
            ReviewReason::Locked,
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
    let content_hash = entry.content_hash();
    if let Some(existing) = existing {
        if existing.record.status == EntryStatus::Active && existing.content_hash == content_hash {
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
        if existing.record.status == EntryStatus::Active && existing.content_hash != content_hash {
            let review = insert_review_item_tx(
                tx,
                request_id,
                scope,
                pack_name,
                entry,
                Some(existing.record),
                ReviewReason::Conflict,
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

    fn synthetic_secret(prefix: &str) -> String {
        [prefix, "abcdefghijklmnopqrstuvwxyz123456"].concat()
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
                "SELECT COUNT(*) FROM pragma_table_list WHERE strict = 1 AND name IN ('packs','entries','revisions','runs','commits','review_items')",
                [],
                |row| row.get(0),
            )
            .expect("strict count");
        assert_eq!(strict_count, 6);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&path).expect("metadata").permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
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
}
