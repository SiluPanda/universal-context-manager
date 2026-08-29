use crate::error::{ContextError, ContextResult};
use crate::markdown::import_markdown;
use crate::model::{
    ContextExportBundle, EntryInput, EntryValue, Provenance, SourceImportDocument,
    SourceImportKind, SourceImportPreviewRequest,
};
use serde_json::{Map, Value, json};

#[derive(Clone, Debug)]
pub(crate) struct ParsedSourceCandidate {
    pub document_index: usize,
    pub source_path: Option<String>,
    pub detected_source_kind: SourceImportKind,
    pub entry: EntryInput,
    pub warnings: Vec<String>,
}

pub(crate) fn parse_source_import(
    request: &SourceImportPreviewRequest,
) -> ContextResult<(Vec<ParsedSourceCandidate>, Vec<String>)> {
    if request.documents.is_empty() {
        return Err(ContextError::validation(
            "source import requires at least one document",
        ));
    }

    let mut candidates = Vec::new();
    let mut warnings = Vec::new();
    for (document_index, document) in request.documents.iter().enumerate() {
        if document.payload.trim().is_empty() {
            warnings.push(format!(
                "document {} is empty and was skipped",
                document_label(document_index, document)
            ));
            continue;
        }
        let detected = if request.source_kind == SourceImportKind::Auto {
            detect_source_kind(document)
        } else {
            request.source_kind
        };
        let mut parsed = parse_document(document_index, document, detected, &request.actor)?;
        candidates.append(&mut parsed);
    }

    if candidates.is_empty() {
        return Err(ContextError::validation(
            "source import produced no candidate entries",
        ));
    }
    Ok((candidates, warnings))
}

pub(crate) fn detect_source_kind(document: &SourceImportDocument) -> SourceImportKind {
    if serde_json::from_str::<ContextExportBundle>(&document.payload).is_ok() {
        return SourceImportKind::UcmJson;
    }
    if document.payload.contains("<!-- UCM_ENTRY") {
        return SourceImportKind::UcmMarkdown;
    }

    let path = document
        .path
        .as_deref()
        .unwrap_or_default()
        .replace('\\', "/")
        .to_ascii_lowercase();
    let file_name = path.rsplit('/').next().unwrap_or(path.as_str());
    if file_name == "agents.md" {
        return SourceImportKind::AgentsMd;
    }
    if file_name == "claude.md" || file_name == "claude.local.md" {
        return SourceImportKind::ClaudeMd;
    }
    if file_name == "copilot-instructions.md"
        || file_name.ends_with(".instructions.md")
        || path.ends_with("/.github/copilot-instructions.md")
    {
        return SourceImportKind::CopilotInstructions;
    }
    if file_name == ".cursorrules"
        || ((path.starts_with(".cursor/rules/") || path.contains("/.cursor/rules/"))
            && file_name.ends_with(".mdc"))
    {
        return SourceImportKind::CursorRule;
    }
    if (path.starts_with(".continue/rules/") || path.contains("/.continue/rules/"))
        && file_name.ends_with(".md")
    {
        return SourceImportKind::ContinueRule;
    }

    let content_prefix = document
        .payload
        .trim_start()
        .chars()
        .take(512)
        .collect::<String>()
        .to_ascii_lowercase();
    if content_prefix.starts_with("# agents.md") {
        return SourceImportKind::AgentsMd;
    }
    if content_prefix.starts_with("# claude.md")
        || content_prefix.starts_with("# claude instructions")
    {
        return SourceImportKind::ClaudeMd;
    }
    if content_prefix.starts_with("# github copilot")
        || content_prefix.starts_with("# copilot instructions")
    {
        return SourceImportKind::CopilotInstructions;
    }

    SourceImportKind::PlainMarkdown
}

fn parse_document(
    document_index: usize,
    document: &SourceImportDocument,
    detected: SourceImportKind,
    actor: &str,
) -> ContextResult<Vec<ParsedSourceCandidate>> {
    match detected {
        SourceImportKind::Auto => Err(ContextError::validation(
            "auto source kind must be resolved before parsing",
        )),
        SourceImportKind::UcmJson => {
            let bundle: ContextExportBundle = serde_json::from_str(&document.payload)?;
            candidates_from_bundle(document_index, document, detected, actor, bundle)
        }
        SourceImportKind::UcmMarkdown => {
            let bundle = import_markdown(&document.payload)?;
            candidates_from_bundle(document_index, document, detected, actor, bundle)
        }
        SourceImportKind::AgentsMd
        | SourceImportKind::ClaudeMd
        | SourceImportKind::CopilotInstructions
        | SourceImportKind::CursorRule
        | SourceImportKind::ContinueRule
        | SourceImportKind::PlainMarkdown => Ok(vec![common_markdown_candidate(
            document_index,
            document,
            detected,
            actor,
        )]),
    }
}

fn candidates_from_bundle(
    document_index: usize,
    document: &SourceImportDocument,
    detected: SourceImportKind,
    actor: &str,
    bundle: ContextExportBundle,
) -> ContextResult<Vec<ParsedSourceCandidate>> {
    if bundle.entries.is_empty() {
        return Err(ContextError::validation(format!(
            "{} contains no entries",
            document_label(document_index, document)
        )));
    }

    Ok(bundle
        .entries
        .into_iter()
        .map(|record| {
            let source_ref = source_ref(document_index, document, Some(record.key.as_str()));
            ParsedSourceCandidate {
                document_index,
                source_path: document.path.clone(),
                detected_source_kind: detected,
                entry: EntryInput {
                    key: record.key,
                    title: record.title,
                    kind: record.kind,
                    value: record.value,
                    tags: record.tags,
                    metadata: record.metadata,
                    locked: record.locked,
                    provenance: Some(Provenance {
                        actor: actor.to_string(),
                        source: format!("source_import:{}", detected.as_str()),
                        source_ref: Some(source_ref),
                        run_id: None,
                        request_id: None,
                        note: Some(format!("imported from {}", record.provenance.source)),
                    }),
                },
                warnings: Vec::new(),
            }
        })
        .collect())
}

fn common_markdown_candidate(
    document_index: usize,
    document: &SourceImportDocument,
    detected: SourceImportKind,
    actor: &str,
) -> ParsedSourceCandidate {
    let (frontmatter, warnings) = parse_frontmatter(&document.payload);
    let title = frontmatter
        .get("title")
        .or_else(|| frontmatter.get("name"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .or_else(|| default_title(document, detected));
    let mut tags = vec!["imported".to_string(), detected.as_str().to_string()];
    if let Some(frontmatter_tags) = frontmatter.get("tags") {
        match frontmatter_tags {
            Value::Array(values) => tags.extend(
                values
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(ToString::to_string),
            ),
            Value::String(value) if !value.trim().is_empty() => {
                tags.push(value.trim().to_string());
            }
            _ => {}
        }
    }
    tags.sort();
    tags.dedup();

    let source_ref = source_ref(document_index, document, None);
    ParsedSourceCandidate {
        document_index,
        source_path: document.path.clone(),
        detected_source_kind: detected,
        entry: EntryInput {
            key: source_entry_key(document_index, document, detected),
            title,
            kind: match detected {
                SourceImportKind::PlainMarkdown => "document",
                _ => "instructions",
            }
            .to_string(),
            value: EntryValue::Markdown {
                body: document.payload.clone(),
            },
            tags,
            metadata: json!({
                "source_import": {
                    "kind": detected.as_str(),
                    "path": document.path,
                    "frontmatter": Value::Object(frontmatter),
                }
            }),
            locked: false,
            provenance: Some(Provenance {
                actor: actor.to_string(),
                source: format!("source_import:{}", detected.as_str()),
                source_ref: Some(source_ref),
                run_id: None,
                request_id: None,
                note: None,
            }),
        },
        warnings,
    }
}

fn parse_frontmatter(payload: &str) -> (Map<String, Value>, Vec<String>) {
    let normalized = payload.replace("\r\n", "\n");
    let mut lines = normalized.lines();
    if lines.next() != Some("---") {
        return (Map::new(), Vec::new());
    }

    let mut values = Map::new();
    let mut warnings = Vec::new();
    let mut closed = false;
    for line in lines.take(100) {
        if line == "---" {
            closed = true;
            break;
        }
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let Some((key, raw_value)) = trimmed.split_once(':') else {
            warnings.push(format!("ignored unsupported frontmatter line `{trimmed}`"));
            continue;
        };
        let key = key.trim();
        if key.is_empty() {
            warnings.push("ignored frontmatter field with an empty key".to_string());
            continue;
        }
        values.insert(key.to_string(), parse_frontmatter_value(raw_value.trim()));
    }
    if !closed {
        warnings.push("ignored unterminated YAML frontmatter".to_string());
        return (Map::new(), warnings);
    }
    (values, warnings)
}

fn parse_frontmatter_value(raw: &str) -> Value {
    let unquoted = raw
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .or_else(|| {
            raw.strip_prefix('\'')
                .and_then(|value| value.strip_suffix('\''))
        })
        .unwrap_or(raw)
        .trim();
    if unquoted.eq_ignore_ascii_case("true") {
        return Value::Bool(true);
    }
    if unquoted.eq_ignore_ascii_case("false") {
        return Value::Bool(false);
    }
    if let Some(inner) = unquoted
        .strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))
    {
        return Value::Array(
            inner
                .split(',')
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(|value| {
                    Value::String(
                        value
                            .trim_matches('"')
                            .trim_matches('\'')
                            .trim()
                            .to_string(),
                    )
                })
                .collect(),
        );
    }
    Value::String(unquoted.to_string())
}

fn source_entry_key(
    document_index: usize,
    document: &SourceImportDocument,
    detected: SourceImportKind,
) -> String {
    let path_file_name = document.path.as_deref().and_then(|path| {
        path.replace('\\', "/")
            .rsplit('/')
            .next()
            .map(str::to_string)
    });
    let file_name = path_file_name.as_deref().unwrap_or_default();
    let base = file_name
        .strip_suffix(".instructions.md")
        .or_else(|| file_name.strip_suffix(".md"))
        .or_else(|| file_name.strip_suffix(".mdc"))
        .unwrap_or(file_name);
    let slug = slugify(base);
    match detected {
        SourceImportKind::AgentsMd => "agents-instructions".to_string(),
        SourceImportKind::ClaudeMd if file_name.eq_ignore_ascii_case("CLAUDE.local.md") => {
            "claude-local-instructions".to_string()
        }
        SourceImportKind::ClaudeMd => "claude-instructions".to_string(),
        SourceImportKind::CopilotInstructions if slug == "copilot-instructions" => {
            "copilot-instructions".to_string()
        }
        SourceImportKind::CopilotInstructions if !slug.is_empty() => {
            format!("copilot-{slug}-instructions")
        }
        SourceImportKind::CopilotInstructions => "copilot-instructions".to_string(),
        SourceImportKind::CursorRule if file_name == ".cursorrules" => "cursor-rules".to_string(),
        SourceImportKind::CursorRule if !slug.is_empty() => format!("cursor-{slug}"),
        SourceImportKind::CursorRule => format!("cursor-rule-{}", document_index + 1),
        SourceImportKind::ContinueRule if !slug.is_empty() => format!("continue-{slug}"),
        SourceImportKind::ContinueRule => format!("continue-rule-{}", document_index + 1),
        SourceImportKind::PlainMarkdown if !slug.is_empty() => slug,
        SourceImportKind::PlainMarkdown => format!("imported-markdown-{}", document_index + 1),
        SourceImportKind::UcmJson | SourceImportKind::UcmMarkdown | SourceImportKind::Auto => {
            format!("imported-entry-{}", document_index + 1)
        }
    }
}

fn default_title(document: &SourceImportDocument, detected: SourceImportKind) -> Option<String> {
    document
        .path
        .as_deref()
        .and_then(|path| {
            path.replace('\\', "/")
                .rsplit('/')
                .next()
                .map(str::to_string)
        })
        .filter(|name| !name.is_empty())
        .or_else(|| Some(detected.as_str().replace('_', " ")))
}

fn source_ref(
    document_index: usize,
    document: &SourceImportDocument,
    fragment: Option<&str>,
) -> String {
    let mut reference = document
        .path
        .clone()
        .unwrap_or_else(|| format!("inline:{}", document_index + 1));
    if let Some(fragment) = fragment {
        reference.push('#');
        reference.push_str(fragment);
    }
    reference
}

fn document_label(document_index: usize, document: &SourceImportDocument) -> String {
    document
        .path
        .clone()
        .unwrap_or_else(|| format!("inline document {}", document_index + 1))
}

fn slugify(value: &str) -> String {
    let mut out = String::new();
    let mut previous_dash = false;
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            previous_dash = false;
        } else if !previous_dash && !out.is_empty() {
            out.push('-');
            previous_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::now_utc;

    fn document(path: &str, payload: &str) -> SourceImportDocument {
        SourceImportDocument {
            path: Some(path.to_string()),
            payload: payload.to_string(),
        }
    }

    #[test]
    fn detects_all_supported_source_families() {
        let json_bundle = serde_json::to_string(&ContextExportBundle {
            exported_at: now_utc(),
            packs: vec![],
            entries: vec![],
            reviews: vec![],
            runs: vec![],
        })
        .expect("json");
        let cases = [
            (
                document("export.json", &json_bundle),
                SourceImportKind::UcmJson,
            ),
            (
                document(
                    "export.md",
                    "<!-- UCM_ENTRY {\"scope_kind\":\"global\"} -->",
                ),
                SourceImportKind::UcmMarkdown,
            ),
            (
                document("AGENTS.md", "# Instructions"),
                SourceImportKind::AgentsMd,
            ),
            (
                document("CLAUDE.local.md", "# Instructions"),
                SourceImportKind::ClaudeMd,
            ),
            (
                document("CLAUDE.md", "# Instructions"),
                SourceImportKind::ClaudeMd,
            ),
            (
                document(".github/copilot-instructions.md", "# Instructions"),
                SourceImportKind::CopilotInstructions,
            ),
            (
                document(".github/testing.instructions.md", "# Instructions"),
                SourceImportKind::CopilotInstructions,
            ),
            (
                document(".cursor/rules/rust.mdc", "# Instructions"),
                SourceImportKind::CursorRule,
            ),
            (
                document(".cursorrules", "# Instructions"),
                SourceImportKind::CursorRule,
            ),
            (
                document(".continue/rules/rust.md", "# Instructions"),
                SourceImportKind::ContinueRule,
            ),
            (
                document("notes.md", "# Notes"),
                SourceImportKind::PlainMarkdown,
            ),
        ];

        for (document, expected) in cases {
            assert_eq!(
                detect_source_kind(&document),
                expected,
                "{:?}",
                document.path
            );
        }
    }

    #[test]
    fn common_markdown_preserves_body_and_safe_frontmatter() {
        let payload = "---\ntitle: Rust rules\ntags: [rust, testing]\nalwaysApply: true\n---\n# Body\nKeep this.";
        let candidate = common_markdown_candidate(
            0,
            &document(".cursor/rules/rust.mdc", payload),
            SourceImportKind::CursorRule,
            "importer",
        );
        assert_eq!(candidate.entry.title.as_deref(), Some("Rust rules"));
        assert!(candidate.entry.tags.contains(&"rust".to_string()));
        assert_eq!(candidate.entry.value.render_markdown(), payload);
        assert_eq!(
            candidate
                .entry
                .provenance
                .as_ref()
                .and_then(|value| value.source_ref.as_deref()),
            Some(".cursor/rules/rust.mdc")
        );
    }
}
