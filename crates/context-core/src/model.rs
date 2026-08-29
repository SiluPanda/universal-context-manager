use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::fmt::{Display, Formatter};
use std::str::FromStr;
use uuid::Uuid;

use crate::error::{ContextError, ContextResult};

pub const GLOBAL_SCOPE_ID: &str = "global";
pub const DEFAULT_PACK_NAME: &str = "main";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ScopeKind {
    Global,
    Project,
    Task,
}

impl ScopeKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Global => "global",
            Self::Project => "project",
            Self::Task => "task",
        }
    }

    pub fn normalize_id(&self, raw: impl AsRef<str>) -> ContextResult<String> {
        let raw = raw.as_ref().trim();
        match self {
            Self::Global => Ok(GLOBAL_SCOPE_ID.to_string()),
            Self::Project | Self::Task => {
                if raw.is_empty() {
                    Err(ContextError::validation(format!(
                        "scope id is required for {}",
                        self.as_str()
                    )))
                } else {
                    Ok(raw.to_string())
                }
            }
        }
    }
}

impl Display for ScopeKind {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for ScopeKind {
    type Err = ContextError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "global" => Ok(Self::Global),
            "project" => Ok(Self::Project),
            "task" => Ok(Self::Task),
            other => Err(ContextError::validation(format!(
                "unknown scope kind: {other}"
            ))),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct ScopeRef {
    pub kind: ScopeKind,
    #[serde(default)]
    pub id: String,
}

impl ScopeRef {
    pub fn normalized(kind: ScopeKind, id: impl AsRef<str>) -> ContextResult<Self> {
        let id = kind.normalize_id(id)?;
        Ok(Self { kind, id })
    }

    pub fn global() -> Self {
        Self {
            kind: ScopeKind::Global,
            id: GLOBAL_SCOPE_ID.to_string(),
        }
    }

    pub fn normalize(&self) -> ContextResult<Self> {
        Self::normalized(self.kind.clone(), &self.id)
    }

    pub fn label(&self) -> String {
        format!("{}:{}", self.kind, self.id)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "format", rename_all = "snake_case")]
pub enum EntryValue {
    Markdown { body: String },
    Json { value: Value },
}

impl EntryValue {
    pub fn format_name(&self) -> &'static str {
        match self {
            Self::Markdown { .. } => "markdown",
            Self::Json { .. } => "json",
        }
    }

    pub fn search_text(&self) -> String {
        match self {
            Self::Markdown { body } => body.clone(),
            Self::Json { value } => {
                serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
            }
        }
    }

    pub fn render_markdown(&self) -> String {
        match self {
            Self::Markdown { body } => body.clone(),
            Self::Json { value } => format!(
                "```json\n{}\n```",
                serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
            ),
        }
    }

    pub fn normalized_hash(&self) -> String {
        let normalized = match self {
            Self::Markdown { body } => format!("markdown:{}", body.trim()),
            Self::Json { value } => format!(
                "json:{}",
                serde_json::to_string(value).unwrap_or_else(|_| value.to_string())
            ),
        };
        let mut hasher = Sha256::new();
        hasher.update(normalized.as_bytes());
        format!("{:x}", hasher.finalize())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PackStatus {
    Active,
    Archived,
}

impl PackStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Archived => "archived",
        }
    }
}

impl FromStr for PackStatus {
    type Err = ContextError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "active" => Ok(Self::Active),
            "archived" => Ok(Self::Archived),
            other => Err(ContextError::validation(format!(
                "unknown pack status: {other}"
            ))),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EntryStatus {
    Active,
    Deleted,
}

impl EntryStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Deleted => "deleted",
        }
    }
}

impl FromStr for EntryStatus {
    type Err = ContextError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "active" => Ok(Self::Active),
            "deleted" => Ok(Self::Deleted),
            other => Err(ContextError::validation(format!(
                "unknown entry status: {other}"
            ))),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReviewState {
    Pending,
    Approved,
    Rejected,
}

impl ReviewState {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Approved => "approved",
            Self::Rejected => "rejected",
        }
    }
}

impl FromStr for ReviewState {
    type Err = ContextError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "pending" => Ok(Self::Pending),
            "approved" => Ok(Self::Approved),
            "rejected" => Ok(Self::Rejected),
            other => Err(ContextError::validation(format!(
                "unknown review state: {other}"
            ))),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReviewReason {
    GlobalScope,
    Conflict,
    Locked,
    StrictPolicy,
}

impl ReviewReason {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::GlobalScope => "global_scope",
            Self::Conflict => "conflict",
            Self::Locked => "locked",
            Self::StrictPolicy => "strict_policy",
        }
    }
}

impl FromStr for ReviewReason {
    type Err = ContextError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "global_scope" => Ok(Self::GlobalScope),
            "conflict" => Ok(Self::Conflict),
            "locked" => Ok(Self::Locked),
            "strict_policy" => Ok(Self::StrictPolicy),
            other => Err(ContextError::validation(format!(
                "unknown review reason: {other}"
            ))),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReviewMode {
    Strict,
    #[default]
    Balanced,
    Fast,
}

impl ReviewMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Strict => "strict",
            Self::Balanced => "balanced",
            Self::Fast => "fast",
        }
    }
}

impl Display for ReviewMode {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for ReviewMode {
    type Err = ContextError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "strict" => Ok(Self::Strict),
            "balanced" => Ok(Self::Balanced),
            "fast" => Ok(Self::Fast),
            other => Err(ContextError::validation(format!(
                "unknown review mode: {other}"
            ))),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CommitDisposition {
    Applied,
    Pending,
    Duplicate,
    Rejected,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CommitStatus {
    Applied,
    Pending,
    Partial,
    Rejected,
    Spooled,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Provenance {
    pub actor: String,
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

impl Provenance {
    pub fn system(actor: impl Into<String>, source: impl Into<String>) -> Self {
        Self {
            actor: actor.into(),
            source: source.into(),
            source_ref: None,
            run_id: None,
            request_id: None,
            note: None,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct EntryInput {
    pub key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub kind: String,
    #[serde(flatten)]
    pub value: EntryValue,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default = "default_json_object")]
    pub metadata: Value,
    #[serde(default)]
    pub locked: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<Provenance>,
}

impl EntryInput {
    pub fn validate(&self) -> ContextResult<()> {
        if self.key.trim().is_empty() {
            return Err(ContextError::validation("entry key must not be empty"));
        }
        if self.kind.trim().is_empty() {
            return Err(ContextError::validation("entry kind must not be empty"));
        }
        Ok(())
    }

    pub fn content_hash(&self) -> String {
        let mut hasher = Sha256::new();
        hasher.update(self.key.trim().as_bytes());
        hasher.update(self.kind.trim().as_bytes());
        if let Some(title) = &self.title {
            hasher.update(title.trim().as_bytes());
        }
        hasher.update(self.value.normalized_hash().as_bytes());
        for tag in &self.tags {
            hasher.update(tag.trim().as_bytes());
        }
        hasher.update(
            serde_json::to_string(&self.metadata)
                .unwrap_or_default()
                .as_bytes(),
        );
        format!("{:x}", hasher.finalize())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct EntryRecord {
    pub id: String,
    pub scope: ScopeRef,
    pub pack_name: String,
    pub key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub kind: String,
    #[serde(flatten)]
    pub value: EntryValue,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default = "default_json_object")]
    pub metadata: Value,
    pub provenance: Provenance,
    pub locked: bool,
    pub status: EntryStatus,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub revision_no: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct PutEntryRequest {
    pub scope: ScopeRef,
    #[serde(default = "default_pack_name")]
    pub pack_name: String,
    pub entry: EntryInput,
    pub actor: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct EntrySelector {
    pub scope: ScopeRef,
    #[serde(default = "default_pack_name")]
    pub pack_name: String,
    pub entry_key: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct DeleteEntryRequest {
    pub selector: EntrySelector,
    pub actor: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct RevertEntryRequest {
    pub selector: EntrySelector,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision_no: Option<i64>,
    pub actor: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct PackRecord {
    pub id: String,
    pub scope: ScopeRef,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default = "default_json_object")]
    pub metadata: Value,
    pub status: PackStatus,
    pub locked: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lock_reason: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub revision_no: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ReviewPolicy {
    pub mode: ReviewMode,
    #[serde(default = "default_json_object")]
    pub metadata: Value,
    pub updated_at: DateTime<Utc>,
    pub updated_by: String,
    pub revision_no: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SetReviewPolicyRequest {
    pub mode: ReviewMode,
    #[serde(default = "default_json_object")]
    pub metadata: Value,
    pub actor: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct CreatePackRequest {
    pub scope: ScopeRef,
    #[serde(default = "default_pack_name")]
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default = "default_json_object")]
    pub metadata: Value,
    #[serde(default)]
    pub locked: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lock_reason: Option<String>,
    pub actor: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct PackSelector {
    pub scope: ScopeRef,
    #[serde(default = "default_pack_name")]
    pub name: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct UpdatePackRequest {
    pub selector: PackSelector,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<PackStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub locked: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lock_reason: Option<String>,
    pub actor: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ComposeRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_scope_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_scope_id: Option<String>,
    #[serde(default)]
    pub include_archived: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ComposeSection {
    pub scope: ScopeRef,
    pub pack_name: String,
    pub entries: Vec<EntryRecord>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ComposeMetrics {
    pub rendered_bytes: usize,
    pub estimated_tokens: usize,
    pub included_entries: usize,
    pub excluded_entries: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ComposeExclusionReason {
    DeletedEntry,
    ArchivedPack,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ComposeExclusion {
    pub entry_id: String,
    pub scope: ScopeRef,
    pub pack_name: String,
    pub entry_key: String,
    pub revision_no: i64,
    pub reason: ComposeExclusionReason,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ComposeResponse {
    pub generated_at: DateTime<Utc>,
    pub sections: Vec<ComposeSection>,
    pub rendered_markdown: String,
    #[serde(default)]
    pub metrics: ComposeMetrics,
    #[serde(default)]
    pub exclusions: Vec<ComposeExclusion>,
    #[serde(default)]
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SearchRequest {
    pub query: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_scope_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_scope_id: Option<String>,
    #[serde(default = "default_search_limit")]
    pub limit: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SearchHit {
    pub score: f64,
    pub snippet: String,
    pub entry: EntryRecord,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SearchResponse {
    pub query: String,
    pub hits: Vec<SearchHit>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct RunInput {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_scope_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_scope_id: Option<String>,
    pub source: String,
    #[serde(default = "default_json_object")]
    pub metadata: Value,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct RunRecord {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_scope_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_scope_id: Option<String>,
    pub source: String,
    #[serde(default = "default_json_object")]
    pub metadata: Value,
    pub started_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct CommitProposal {
    pub scope: ScopeRef,
    #[serde(default = "default_pack_name")]
    pub pack_name: String,
    pub entry: EntryInput,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct CommitWorkRequest {
    pub request_id: String,
    pub actor: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<RunInput>,
    pub proposals: Vec<CommitProposal>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct CommitItemResult {
    pub scope: ScopeRef,
    pub pack_name: String,
    pub entry_key: String,
    pub disposition: CommitDisposition,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_id: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct CommitWorkResult {
    pub request_id: String,
    pub status: CommitStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    pub items: Vec<CommitItemResult>,
    #[serde(default)]
    pub spooled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spool_path: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ReviewItem {
    pub id: String,
    pub request_id: String,
    pub scope: ScopeRef,
    pub pack_name: String,
    pub entry_key: String,
    pub state: ReviewState,
    pub reason: ReviewReason,
    pub proposed_entry: EntryInput,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub existing_entry: Option<EntryRecord>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolution_note: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub revision_no: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ReviewEditRequest {
    pub review_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<EntryValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub locked: Option<bool>,
    pub actor: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ReviewEditAndApproveRequest {
    pub review_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<EntryValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub locked: Option<bool>,
    pub actor: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

impl From<ReviewEditAndApproveRequest> for ReviewEditRequest {
    fn from(value: ReviewEditAndApproveRequest) -> Self {
        Self {
            review_id: value.review_id,
            title: value.title,
            kind: value.kind,
            value: value.value,
            tags: value.tags,
            metadata: value.metadata,
            locked: value.locked,
            actor: value.actor,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ReviewDecisionRequest {
    pub review_id: String,
    pub actor: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ExportRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_scope_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_scope_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<ScopeRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pack_name: Option<String>,
    #[serde(default)]
    pub include_deleted: bool,
    #[serde(default)]
    pub include_reviews: bool,
    #[serde(default)]
    pub include_runs: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ContextExportBundle {
    pub exported_at: DateTime<Utc>,
    pub packs: Vec<PackRecord>,
    pub entries: Vec<EntryRecord>,
    #[serde(default)]
    pub reviews: Vec<ReviewItem>,
    #[serde(default)]
    pub runs: Vec<RunRecord>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct StoreStats {
    pub schema_version: i64,
    pub packs: usize,
    pub entries: usize,
    pub reviews: usize,
    pub runs: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct HealthReport {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub component_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_version: Option<u32>,
    pub schema_version: i64,
    pub packs: usize,
    pub entries: usize,
    pub reviews: usize,
    pub runs: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ImportRequest {
    pub actor: String,
    pub format: ImportFormat,
    pub payload: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ImportFormat {
    Json,
    Markdown,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SourceImportKind {
    #[default]
    Auto,
    UcmJson,
    UcmMarkdown,
    AgentsMd,
    ClaudeMd,
    CopilotInstructions,
    CursorRule,
    ContinueRule,
    PlainMarkdown,
}

impl SourceImportKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::UcmJson => "ucm_json",
            Self::UcmMarkdown => "ucm_markdown",
            Self::AgentsMd => "agents_md",
            Self::ClaudeMd => "claude_md",
            Self::CopilotInstructions => "copilot_instructions",
            Self::CursorRule => "cursor_rule",
            Self::ContinueRule => "continue_rule",
            Self::PlainMarkdown => "plain_markdown",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SourceImportDocument {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    pub payload: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SourceImportPreviewRequest {
    #[serde(default)]
    pub source_kind: SourceImportKind,
    pub documents: Vec<SourceImportDocument>,
    pub destination: ScopeRef,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pack_name: Option<String>,
    pub actor: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SourceImportDisposition {
    New,
    Duplicate,
    Conflict,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SourceImportCandidate {
    pub candidate_index: usize,
    pub document_index: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_path: Option<String>,
    pub detected_source_kind: SourceImportKind,
    pub entry: EntryInput,
    pub disposition: SourceImportDisposition,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub existing_entry_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub existing_revision_no: Option<i64>,
    #[serde(default)]
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct SourceImportPackGovernance {
    pub exists: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<PackStatus>,
    #[serde(default)]
    pub locked: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lock_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision_no: Option<i64>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SourceImportPreview {
    pub destination: ScopeRef,
    pub pack_name: String,
    pub review_mode: ReviewMode,
    #[serde(default)]
    pub destination_pack: SourceImportPackGovernance,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview_fingerprint: Option<String>,
    pub candidates: Vec<SourceImportCandidate>,
    #[serde(default)]
    pub warnings: Vec<String>,
    pub apply_allowed: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SourceImportApplyRequest {
    #[serde(default)]
    pub source_kind: SourceImportKind,
    pub documents: Vec<SourceImportDocument>,
    pub destination: ScopeRef,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pack_name: Option<String>,
    pub actor: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_preview_fingerprint: Option<String>,
}

impl From<&SourceImportApplyRequest> for SourceImportPreviewRequest {
    fn from(value: &SourceImportApplyRequest) -> Self {
        Self {
            source_kind: value.source_kind,
            documents: value.documents.clone(),
            destination: value.destination.clone(),
            pack_name: value.pack_name.clone(),
            actor: value.actor.clone(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SourceImportApplyItem {
    pub candidate_index: usize,
    pub document_index: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_path: Option<String>,
    pub entry_key: String,
    pub disposition: CommitDisposition,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_id: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SourceImportApplyResult {
    pub request_id: String,
    pub candidate_count: usize,
    pub imported_count: usize,
    pub applied_count: usize,
    pub pending_count: usize,
    pub skipped_count: usize,
    pub rejected_count: usize,
    pub items: Vec<SourceImportApplyItem>,
    #[serde(default)]
    pub affected_entry_ids: Vec<String>,
    #[serde(default)]
    pub affected_review_ids: Vec<String>,
    #[serde(default)]
    pub affected_entry_keys: Vec<String>,
}

pub fn default_json_object() -> Value {
    Value::Object(Default::default())
}

pub fn default_pack_name() -> String {
    DEFAULT_PACK_NAME.to_string()
}

pub fn default_search_limit() -> u32 {
    20
}

pub fn now_utc() -> DateTime<Utc> {
    Utc::now()
}

pub fn new_id() -> String {
    Uuid::new_v4().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn health_report_serializes_component_version() {
        let report = HealthReport {
            component_version: Some("1.2.3".to_string()),
            api_version: Some(crate::CONTEXT_API_VERSION),
            schema_version: 5,
            packs: 1,
            entries: 2,
            reviews: 3,
            runs: 4,
        };

        assert_eq!(
            serde_json::to_value(report).expect("health report"),
            json!({
                "component_version": "1.2.3",
                "api_version": crate::CONTEXT_API_VERSION,
                "schema_version": 5,
                "packs": 1,
                "entries": 2,
                "reviews": 3,
                "runs": 4,
            })
        );
    }

    #[test]
    fn health_report_deserializes_legacy_payload_without_component_version() {
        let report: HealthReport = serde_json::from_value(json!({
            "schema_version": 4,
            "packs": 1,
            "entries": 2,
            "reviews": 3,
            "runs": 4,
        }))
        .expect("legacy health report");

        assert_eq!(report.component_version, None);
        assert_eq!(report.api_version, None);
        assert_eq!(report.schema_version, 4);
    }

    #[test]
    fn source_import_fingerprint_fields_are_additive() {
        let preview: SourceImportPreview = serde_json::from_value(json!({
            "destination": {"kind": "project", "id": "proj"},
            "pack_name": "main",
            "review_mode": "balanced",
            "candidates": [],
            "warnings": [],
            "apply_allowed": true,
        }))
        .expect("legacy preview");
        assert_eq!(
            preview.destination_pack,
            SourceImportPackGovernance::default()
        );
        assert_eq!(preview.preview_fingerprint, None);

        let apply: SourceImportApplyRequest = serde_json::from_value(json!({
            "documents": [{"path": "AGENTS.md", "payload": "# Rules"}],
            "destination": {"kind": "project", "id": "proj"},
            "pack_name": "main",
            "actor": "tester",
        }))
        .expect("legacy apply request");
        assert_eq!(apply.expected_preview_fingerprint, None);
    }
}
