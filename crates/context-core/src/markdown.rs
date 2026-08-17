use crate::error::{ContextError, ContextResult};
use crate::model::{
    ContextExportBundle, CreatePackRequest, EntryInput, EntryValue, ExportRequest, PackRecord,
    ScopeKind, ScopeRef,
};
use serde_json::Value;

const ENTRY_BEGIN: &str = "<!-- UCM_ENTRY";
const ENTRY_END: &str = "<!-- /UCM_ENTRY -->";

pub fn export_markdown(bundle: &ContextExportBundle) -> ContextResult<String> {
    let mut out = String::from("# Universal Context Export\n\n");
    for pack in &bundle.packs {
        out.push_str(&format!(
            "## Pack `{}` ({})\n\n",
            pack.name,
            pack.scope.label()
        ));
        if let Some(description) = &pack.description {
            out.push_str(description);
            out.push_str("\n\n");
        }
        let entries = bundle
            .entries
            .iter()
            .filter(|entry| entry.scope == pack.scope && entry.pack_name == pack.name)
            .collect::<Vec<_>>();
        for entry in entries {
            let metadata = serde_json::json!({
                "scope_kind": entry.scope.kind.as_str(),
                "scope_id": entry.scope.id,
                "pack_name": entry.pack_name,
                "key": entry.key,
                "title": entry.title,
                "kind": entry.kind,
                "tags": entry.tags,
                "locked": entry.locked,
                "metadata": entry.metadata,
                "format": entry.value.format_name(),
            });
            out.push_str(ENTRY_BEGIN);
            out.push(' ');
            out.push_str(&serde_json::to_string(&metadata)?);
            out.push_str(" -->\n");
            out.push_str(&entry.value.render_markdown());
            if !out.ends_with('\n') {
                out.push('\n');
            }
            out.push_str(ENTRY_END);
            out.push_str("\n\n");
        }
    }
    Ok(out)
}

pub fn import_markdown(markdown: &str) -> ContextResult<ContextExportBundle> {
    let mut packs: Vec<PackRecord> = Vec::new();
    let mut entries = Vec::new();
    let mut rest = markdown;
    while let Some(start) = rest.find(ENTRY_BEGIN) {
        rest = &rest[start + ENTRY_BEGIN.len()..];
        let end_marker = rest
            .find("-->")
            .ok_or_else(|| ContextError::validation("unterminated markdown export metadata"))?;
        let metadata_str = rest[..end_marker].trim();
        let metadata: Value = serde_json::from_str(metadata_str)?;
        rest = &rest[end_marker + 3..];
        let body_end = rest
            .find(ENTRY_END)
            .ok_or_else(|| ContextError::validation("unterminated markdown export body"))?;
        let body = rest[..body_end].trim().to_string();
        rest = &rest[body_end + ENTRY_END.len()..];

        let scope_kind = metadata
            .get("scope_kind")
            .and_then(Value::as_str)
            .ok_or_else(|| ContextError::validation("missing scope_kind"))?
            .parse::<ScopeKind>()?;
        let scope_id = metadata
            .get("scope_id")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let scope = ScopeRef::normalized(scope_kind, scope_id)?;
        let pack_name = metadata
            .get("pack_name")
            .and_then(Value::as_str)
            .unwrap_or("main")
            .to_string();
        if packs
            .iter()
            .all(|pack| !(pack.scope == scope && pack.name == pack_name))
        {
            packs.push(PackRecord {
                id: format!("imported-pack:{}:{}", scope.label(), pack_name),
                scope: scope.clone(),
                name: pack_name.clone(),
                description: None,
                metadata: serde_json::json!({}),
                status: crate::model::PackStatus::Active,
                locked: false,
                lock_reason: None,
                created_at: crate::model::now_utc(),
                updated_at: crate::model::now_utc(),
                revision_no: 1,
            });
        }
        let format = metadata
            .get("format")
            .and_then(Value::as_str)
            .unwrap_or("markdown");
        let value = if format == "json" {
            let trimmed = body.trim();
            let json_body = if trimmed.starts_with("```json") && trimmed.ends_with("```") {
                trimmed
                    .trim_start_matches("```json")
                    .trim_end_matches("```")
                    .trim()
                    .to_string()
            } else {
                trimmed.to_string()
            };
            EntryValue::Json {
                value: serde_json::from_str(&json_body)?,
            }
        } else {
            EntryValue::Markdown { body }
        };
        let title = metadata
            .get("title")
            .and_then(Value::as_str)
            .map(ToString::to_string);
        let tags = metadata
            .get("tags")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let locked = metadata
            .get("locked")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let item_metadata = metadata
            .get("metadata")
            .cloned()
            .unwrap_or_else(|| serde_json::json!({}));
        entries.push(crate::model::EntryRecord {
            id: format!(
                "imported-entry:{}:{}:{}",
                scope.label(),
                pack_name,
                metadata
                    .get("key")
                    .and_then(Value::as_str)
                    .unwrap_or("entry")
            ),
            scope,
            pack_name,
            key: metadata
                .get("key")
                .and_then(Value::as_str)
                .unwrap_or("entry")
                .to_string(),
            title,
            kind: metadata
                .get("kind")
                .and_then(Value::as_str)
                .unwrap_or("note")
                .to_string(),
            value,
            tags,
            metadata: item_metadata,
            provenance: crate::model::Provenance::system("import", "markdown_import"),
            locked,
            status: crate::model::EntryStatus::Active,
            created_at: crate::model::now_utc(),
            updated_at: crate::model::now_utc(),
            revision_no: 1,
        });
    }

    Ok(ContextExportBundle {
        exported_at: crate::model::now_utc(),
        packs,
        entries,
        reviews: Vec::new(),
        runs: Vec::new(),
    })
}

#[allow(dead_code)]
pub fn export_request_to_pack_requests(request: &ExportRequest) -> Vec<CreatePackRequest> {
    request
        .scope
        .iter()
        .map(|scope| CreatePackRequest {
            scope: scope.clone(),
            name: request
                .pack_name
                .clone()
                .unwrap_or_else(|| "main".to_string()),
            description: None,
            metadata: serde_json::json!({}),
            locked: false,
            lock_reason: None,
            actor: "import".to_string(),
        })
        .collect()
}

#[allow(dead_code)]
pub fn entry_input_from_record(record: &crate::model::EntryRecord) -> EntryInput {
    EntryInput {
        key: record.key.clone(),
        title: record.title.clone(),
        kind: record.kind.clone(),
        value: record.value.clone(),
        tags: record.tags.clone(),
        metadata: record.metadata.clone(),
        locked: record.locked,
        provenance: Some(record.provenance.clone()),
    }
}
