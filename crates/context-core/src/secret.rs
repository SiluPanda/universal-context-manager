use regex::Regex;
use serde::Serialize;
use serde_json::{Value, json};

use crate::error::{ContextError, ContextResult};
use crate::model::{
    CommitWorkRequest, EntryInput, Provenance, ReviewItem, ReviewMode, RunInput, ScopeRef,
};

pub fn reject_if_secret(text: &str) -> ContextResult<()> {
    for (pattern, label) in secret_patterns() {
        if pattern.is_match(text) {
            return Err(ContextError::SecretDetected(label.to_string()));
        }
    }
    Ok(())
}

fn secret_patterns() -> &'static [(Regex, &'static str)] {
    use std::sync::OnceLock;

    static PATTERNS: OnceLock<Vec<(Regex, &'static str)>> = OnceLock::new();
    PATTERNS
        .get_or_init(|| {
            vec![
                (
                    Regex::new(r"(?i)-----BEGIN [A-Z ]*PRIVATE KEY-----").expect("valid regex"),
                    "private key block",
                ),
                (
                    Regex::new(r"\bAKIA[0-9A-Z]{16}\b").expect("valid regex"),
                    "aws access key",
                ),
                (
                    Regex::new(r"\bsk-[A-Za-z0-9][A-Za-z0-9_-]{20,}\b")
                        .expect("valid regex"),
                    "OpenAI-style API key",
                ),
                (
                    Regex::new(r"\b(?:gh[pousr]_[A-Za-z0-9]{20,}|github_pat_[A-Za-z0-9_]{20,})\b")
                        .expect("valid regex"),
                    "GitHub token",
                ),
                (
                    Regex::new(r"\bAIza[0-9A-Za-z\-_]{35}\b").expect("valid regex"),
                    "Google API key",
                ),
                (
                    Regex::new(r"\bxox[baprs]-[A-Za-z0-9-]{10,}\b").expect("valid regex"),
                    "Slack token",
                ),
                (
                    Regex::new(r"(?i)\bBearer\s+[A-Za-z0-9\-._~+/]+=*\b").expect("valid regex"),
                    "bearer token",
                ),
                (
                    Regex::new(r#"(?i)(api[_ -]?key|token|secret|password)\s*[:=]\s*['\"]?[A-Za-z0-9\-._~+/=]{12,}['\"]?"#)
                        .expect("valid regex"),
                    "credential assignment",
                ),
            ]
        })
        .as_slice()
}

pub fn reject_entry_for_storage(entry: &EntryInput) -> ContextResult<()> {
    reject_serialized_secret("entry", entry)
}

pub fn reject_pack_for_storage(
    description: Option<&str>,
    metadata: &Value,
    lock_reason: Option<&str>,
) -> ContextResult<()> {
    reject_serialized_secret(
        "pack",
        &json!({
            "description": description,
            "metadata": metadata,
            "lock_reason": lock_reason,
        }),
    )
}

pub fn reject_pack_write_for_storage(
    scope: &ScopeRef,
    name: &str,
    actor: &str,
    description: Option<&str>,
    metadata: &Value,
    lock_reason: Option<&str>,
) -> ContextResult<()> {
    reject_serialized_secret(
        "pack write",
        &json!({
            "scope": scope,
            "name": name,
            "actor": actor,
            "description": description,
            "metadata": metadata,
            "lock_reason": lock_reason,
        }),
    )
}

pub fn reject_run_for_storage(run: &RunInput) -> ContextResult<()> {
    reject_serialized_secret(
        "run",
        &json!({
            "id": run.id,
            "project_scope_id": run.project_scope_id,
            "task_scope_id": run.task_scope_id,
            "source": run.source,
            "metadata": run.metadata,
        }),
    )
}

pub fn reject_review_for_storage(review: &ReviewItem) -> ContextResult<()> {
    reject_serialized_secret("review", review)
}

pub fn reject_review_transition_for_storage(review: &ReviewItem, actor: &str) -> ContextResult<()> {
    reject_serialized_secret(
        "review transition",
        &json!({
            "review": review,
            "actor": actor,
        }),
    )
}

pub fn reject_commit_metadata_for_storage(request: &CommitWorkRequest) -> ContextResult<()> {
    reject_serialized_secret(
        "commit metadata",
        &json!({
            "request_id": request.request_id,
            "actor": request.actor,
            "run": request.run,
            "proposals": request.proposals.iter().map(|proposal| {
                json!({
                    "scope": proposal.scope,
                    "pack_name": proposal.pack_name,
                    "entry_key": proposal.entry.key,
                    "entry_kind": proposal.entry.kind,
                    "locked": proposal.entry.locked,
                })
            }).collect::<Vec<_>>(),
        }),
    )
}

pub fn reject_commit_request_for_storage(request: &CommitWorkRequest) -> ContextResult<()> {
    reject_serialized_secret("commit request", request)
}

pub fn reject_entry_write_for_storage(
    scope: &ScopeRef,
    pack_name: &str,
    actor: &str,
    entry: &EntryInput,
    provenance: &Provenance,
    request_id: Option<&str>,
    run_id: Option<&str>,
) -> ContextResult<()> {
    reject_serialized_secret(
        "entry write",
        &json!({
            "scope": scope,
            "pack_name": pack_name,
            "actor": actor,
            "entry": entry,
            "provenance": provenance,
            "request_id": request_id,
            "run_id": run_id,
        }),
    )
}

pub fn reject_actor_for_storage(actor: &str) -> ContextResult<()> {
    reject_serialized_secret("actor", &json!({ "actor": actor }))
}

pub fn reject_review_policy_write_for_storage(
    mode: ReviewMode,
    metadata: &Value,
    actor: &str,
) -> ContextResult<()> {
    reject_serialized_secret(
        "review policy",
        &json!({
            "mode": mode,
            "metadata": metadata,
            "actor": actor,
        }),
    )
}

pub fn reject_revision_metadata_for_storage(
    provenance: &Provenance,
    commit_request_id: Option<&str>,
    run_id: Option<&str>,
) -> ContextResult<()> {
    reject_serialized_secret(
        "revision metadata",
        &json!({
            "provenance": provenance,
            "commit_request_id": commit_request_id,
            "run_id": run_id,
        }),
    )
}

fn reject_serialized_secret<T: Serialize>(context: &str, value: &T) -> ContextResult<()> {
    let serialized = serde_json::to_string(value).unwrap_or_default();
    reject_if_secret(&serialized).map_err(|err| match err {
        ContextError::SecretDetected(label) => {
            ContextError::SecretDetected(format!("{context}: {label}"))
        }
        other => other,
    })
}
