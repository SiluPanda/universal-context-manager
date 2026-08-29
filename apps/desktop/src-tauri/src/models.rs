use serde::{Deserialize, Serialize};
use serde_json::Value;

pub type ReviewMode = context_core::ReviewMode;
pub type ReviewReason = context_core::ReviewReason;
pub type ReviewState = context_core::ReviewState;
pub type CommitDisposition = context_core::CommitDisposition;
pub type ContextExclusionReason = context_core::ComposeExclusionReason;
pub type SourceImportDisposition = context_core::SourceImportDisposition;
pub type SourceImportKind = context_core::SourceImportKind;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceNode {
    pub id: String,
    pub label: String,
    pub kind: ScopeKind,
    pub description: String,
    pub status: String,
    pub children: Vec<WorkspaceNode>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ScopeKind {
    #[default]
    Global,
    Project,
    Task,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum PackStatus {
    Active,
    Draft,
    Review,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextPack {
    pub id: String,
    pub scope_id: String,
    pub scope_kind: ScopeKind,
    pub scope_label: String,
    pub name: String,
    pub status: PackStatus,
    pub token_estimate: u32,
    pub updated_at: String,
    pub summary: String,
    pub tags: Vec<String>,
    pub body: String,
    pub provenance: Vec<String>,
    pub revision: u32,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EntryFormat {
    #[default]
    Markdown,
    Json,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EntryStatus {
    Active,
    Deleted,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct EntryProvenance {
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

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ContextEntry {
    pub id: String,
    pub pack_id: String,
    pub pack_name: String,
    pub pack_key: String,
    pub scope_id: String,
    pub scope_kind: ScopeKind,
    pub scope_label: String,
    pub key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub kind: String,
    pub format: EntryFormat,
    pub body: String,
    pub rendered_body: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub json_value: Option<Value>,
    #[serde(default)]
    pub tags: Vec<String>,
    pub locked: bool,
    pub status: EntryStatus,
    pub provenance: EntryProvenance,
    pub revision: i64,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewSection {
    pub id: String,
    #[serde(default)]
    pub order: u32,
    #[serde(default)]
    pub layer: String,
    pub title: String,
    pub pack_name: String,
    #[serde(default)]
    pub scope_id: String,
    pub scope_label: String,
    pub scope_kind: ScopeKind,
    pub tokens: u32,
    pub body: String,
    #[serde(default)]
    pub entry_ids: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewSource {
    pub pack_id: String,
    pub pack_name: String,
    pub scope_label: String,
    pub excerpt: String,
    pub tokens: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextPreview {
    pub scope_id: String,
    pub headline: String,
    pub total_tokens: u32,
    pub warnings: Vec<String>,
    pub sections: Vec<PreviewSection>,
    pub sources: Vec<PreviewSource>,
    #[serde(default = "default_destination_adapter")]
    pub destination_adapter: String,
    #[serde(default)]
    pub generated_at: String,
    #[serde(default)]
    pub rendered_markdown: String,
    #[serde(default)]
    pub metrics: ContextMetrics,
    #[serde(default)]
    pub exclusions: Vec<ContextExclusion>,
    #[serde(default)]
    pub included_entries: Vec<IncludedContextEntry>,
}

fn default_destination_adapter() -> String {
    "generic".to_string()
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ContextMetrics {
    pub rendered_bytes: usize,
    pub estimated_tokens: usize,
    pub included_entries: usize,
    pub excluded_entries: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ContextExclusion {
    pub entry_id: String,
    pub scope_id: String,
    pub scope_kind: ScopeKind,
    pub scope_label: String,
    pub pack_name: String,
    pub entry_key: String,
    pub revision: i64,
    pub reason: ContextExclusionReason,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct IncludedContextEntry {
    pub order: u32,
    pub entry_id: String,
    pub pack_name: String,
    pub scope_id: String,
    pub scope_kind: ScopeKind,
    pub scope_label: String,
    pub key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub kind: String,
    pub format: EntryFormat,
    pub provenance: EntryProvenance,
    pub revision: i64,
    pub token_estimate: u32,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ComposeContextInput {
    pub scope_id: String,
    #[serde(default)]
    pub destination_adapter: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchResult {
    pub id: String,
    pub kind: SearchKind,
    pub title: String,
    pub excerpt: String,
    pub scope_label: String,
    pub score: u32,
    pub updated_at: String,
    pub tags: Vec<String>,
    #[serde(default)]
    pub target: SearchTarget,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SearchTarget {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pack_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub adapter_id: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SearchKind {
    Entry,
    Pack,
    Review,
    Run,
    Revision,
    Adapter,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewItem {
    pub id: String,
    #[serde(default)]
    pub request_id: String,
    pub pack_id: String,
    pub pack_name: String,
    pub scope_id: String,
    #[serde(default)]
    pub scope_kind: ScopeKind,
    pub scope_label: String,
    #[serde(default)]
    pub entry_key: String,
    pub title: String,
    pub summary: String,
    pub requested_by: String,
    pub requested_at: String,
    #[serde(default)]
    pub age_seconds: u64,
    pub risk: RiskLevel,
    #[serde(default)]
    pub reason: Option<ReviewReason>,
    pub diff: String,
    #[serde(default)]
    pub diff_sides: ReviewDiff,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub existing_content: Option<String>,
    #[serde(default)]
    pub proposed_content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<EntryProvenance>,
    #[serde(default)]
    pub source: String,
    pub suggested_edit: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewDiff {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before: Option<String>,
    pub after: String,
    pub format: EntryFormat,
    pub changed: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RiskLevel {
    Low,
    Medium,
    High,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivityRun {
    pub id: String,
    pub actor: String,
    pub summary: String,
    pub status: RunStatus,
    pub started_at: String,
    pub duration_ms: u64,
    pub step_count: u32,
    pub context_pack_ids: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum RunStatus {
    Running,
    Completed,
    Blocked,
    Failed,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RevisionEntry {
    pub id: String,
    pub entity_id: String,
    pub entity_label: String,
    pub author: String,
    pub created_at: String,
    pub note: String,
    pub change_summary: String,
    pub restorable: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdapterStatus {
    pub id: String,
    pub name: String,
    pub kind: AdapterKind,
    pub enabled: bool,
    pub health: AdapterHealth,
    pub last_checked_at: String,
    pub queue_depth: u32,
    pub path: String,
    pub note: String,
    #[serde(default)]
    pub state: DiagnosticState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detected_version: Option<String>,
    #[serde(default)]
    pub remediation: Vec<DiagnosticAction>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AdapterKind {
    Filesystem,
    Git,
    Terminal,
    Api,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum AdapterHealth {
    Healthy,
    Degraded,
    Offline,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticState {
    NotInstalled,
    Stopped,
    Starting,
    Healthy,
    #[default]
    Degraded,
    Incompatible,
    MigrationRequired,
    Failed,
    Ignored,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticActionKind {
    Refresh,
    StartDaemon,
    RestartDaemon,
    RetrySpool,
    OpenPath,
    Manual,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticAction {
    pub id: String,
    pub label: String,
    pub kind: DiagnosticActionKind,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticCheck {
    pub id: String,
    pub label: String,
    pub component: String,
    pub state: DiagnosticState,
    pub summary: String,
    #[serde(default)]
    pub details: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detected_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_version: Option<String>,
    #[serde(default)]
    pub remediation: Vec<DiagnosticAction>,
    pub checked_at: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticsReport {
    #[serde(default)]
    pub generated_at: String,
    pub overall_state: DiagnosticState,
    #[serde(default)]
    pub daemon_reachable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub component_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_version: Option<u32>,
    #[serde(default)]
    pub expected_api_version: u32,
    #[serde(default)]
    pub schema_version: Option<i64>,
    #[serde(default)]
    pub expected_schema_version: i64,
    #[serde(default)]
    pub spool_backlog: usize,
    #[serde(default)]
    pub checks: Vec<DiagnosticCheck>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SpoolRetryResult {
    pub attempted: usize,
    pub delivered: usize,
    pub retained: usize,
    #[serde(default)]
    pub errors: Vec<String>,
    pub diagnostics: DiagnosticsReport,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DaemonControlResult {
    pub action: String,
    pub performed: bool,
    pub message: String,
    pub diagnostics: DiagnosticsReport,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    pub theme: ThemeMode,
    pub auto_compose: bool,
    pub review_mode: ReviewMode,
    pub socket_path: String,
    pub launch_on_login: bool,
    pub telemetry: bool,
    pub max_preview_tokens: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_policy: Option<ReviewPolicy>,
    #[serde(default)]
    pub onboarding: OnboardingState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_selected_scope_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_project_path: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemeMode {
    System,
    Light,
    Dark,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewPolicy {
    pub mode: ReviewMode,
    #[serde(default)]
    pub metadata: Value,
    pub updated_at: String,
    pub updated_by: String,
    pub revision: i64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OnboardingState {
    pub complete: bool,
    pub inferred: bool,
    pub durable_context: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_project_path: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DashboardStats {
    pub active_packs: u32,
    pub pending_reviews: u32,
    pub healthy_adapters: u32,
    pub running_agents: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DashboardSnapshot {
    pub workspace: Vec<WorkspaceNode>,
    pub packs: Vec<ContextPack>,
    #[serde(default)]
    pub entries: Vec<ContextEntry>,
    pub review_queue: Vec<ReviewItem>,
    pub activity: Vec<ActivityRun>,
    pub revisions: Vec<RevisionEntry>,
    pub adapters: Vec<AdapterStatus>,
    pub settings: Settings,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_policy: Option<ReviewPolicy>,
    #[serde(default)]
    pub onboarding: OnboardingState,
    #[serde(default)]
    pub diagnostics: DiagnosticsReport,
    #[serde(default)]
    pub privacy: PrivacySummary,
    pub stats: DashboardStats,
    pub selected_scope_id: String,
    pub connected: bool,
    pub last_sync_at: String,
    pub notices: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SavePackInput {
    pub id: Option<String>,
    pub scope_id: String,
    pub name: String,
    pub status: PackStatus,
    pub summary: String,
    pub tags: Vec<String>,
    pub body: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveEntryInput {
    #[serde(default)]
    pub id: Option<String>,
    pub scope_id: String,
    #[serde(default)]
    pub pack_id: Option<String>,
    #[serde(default)]
    pub pack_name: Option<String>,
    pub key: String,
    #[serde(default)]
    pub title: Option<String>,
    pub kind: String,
    #[serde(default)]
    pub format: EntryFormat,
    pub body: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub locked: bool,
    #[serde(default)]
    pub actor: Option<String>,
    #[serde(default)]
    pub note: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RevertEntryInput {
    pub entry_id: String,
    #[serde(default)]
    pub revision: Option<i64>,
    #[serde(default)]
    pub actor: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewDecisionInput {
    pub item_id: String,
    pub decision: ReviewDecision,
    pub edited_content: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ReviewDecision {
    Approve,
    Reject,
    Edit,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BulkReviewDecisionInput {
    pub item_ids: Vec<String>,
    pub decision: ReviewDecision,
    #[serde(default)]
    pub confirmation: bool,
    #[serde(default)]
    pub edited_content: Option<String>,
    #[serde(default)]
    pub actor: Option<String>,
    #[serde(default)]
    pub note: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewDecisionResult {
    pub item_id: String,
    pub success: bool,
    #[serde(default)]
    pub requires_follow_up: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<ReviewState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<DesktopError>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BulkReviewDecisionResult {
    pub decision: ReviewDecision,
    pub attempted: usize,
    pub completed: usize,
    pub stopped: bool,
    pub results: Vec<ReviewDecisionResult>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetReviewPolicyInput {
    pub mode: ReviewMode,
    pub actor: String,
    #[serde(default)]
    pub note: Option<String>,
    #[serde(default)]
    pub request_id: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportExportSummary {
    pub path: String,
    pub packs_imported: usize,
    pub adapters_touched: usize,
    pub revision_id: String,
    pub exported_at: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RestoreRevisionResult {
    pub revision_id: String,
    pub entity_id: String,
    pub restored_at: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DesktopErrorCode {
    SecretDetected,
    Unavailable,
    InvalidImport,
    Conflict,
    PermissionDenied,
    NotFound,
    InvalidInput,
    Incompatible,
    ConfirmationRequired,
    PathGrantRequired,
    PathGrantInvalid,
    PathGrantExpired,
    Internal,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DesktopError {
    pub code: DesktopErrorCode,
    pub message: String,
    pub retryable: bool,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum PathGrantPurpose {
    ProjectRegistration,
    SourceImportPreview,
    SourceImportApply,
    BundleImportPreview,
    BundleImportApply,
    ExportArchive,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PathGrantSelection {
    pub grant_token: String,
    pub purpose: PathGrantPurpose,
    pub paths: Vec<String>,
    pub expires_at: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscoveredInstructionSource {
    pub path: String,
    pub relative_path: String,
    pub source_kind: SourceImportKind,
    pub readable: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectRegistration {
    pub input_path: String,
    pub normalized_path: String,
    pub scope_id: String,
    pub scope_kind: ScopeKind,
    pub label: String,
    pub instruction_sources: Vec<DiscoveredInstructionSource>,
    pub durable: bool,
    pub selected: bool,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceImportPreviewInput {
    pub paths: Vec<String>,
    #[serde(default)]
    pub grant_token: Option<String>,
    pub destination_scope_id: String,
    #[serde(default)]
    pub pack_name: Option<String>,
    #[serde(default)]
    pub source_kind: SourceImportKind,
    #[serde(default)]
    pub actor: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceImportApplyInput {
    pub paths: Vec<String>,
    #[serde(default)]
    pub grant_token: Option<String>,
    pub destination_scope_id: String,
    #[serde(default)]
    pub pack_name: Option<String>,
    #[serde(default)]
    pub source_kind: SourceImportKind,
    #[serde(default)]
    pub actor: Option<String>,
    pub preview_id: String,
    #[serde(default)]
    pub expected_preview_fingerprint: Option<String>,
    pub confirmation: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceImportCandidate {
    pub candidate_index: usize,
    pub document_index: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_path: Option<String>,
    pub detected_source_kind: SourceImportKind,
    pub key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub kind: String,
    pub format: EntryFormat,
    pub rendered_body: String,
    #[serde(default)]
    pub tags: Vec<String>,
    pub locked: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<EntryProvenance>,
    pub disposition: SourceImportDisposition,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub existing_entry_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub existing_revision: Option<i64>,
    #[serde(default)]
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceImportPreviewResult {
    pub preview_id: String,
    #[serde(default)]
    pub preview_fingerprint: String,
    #[serde(default)]
    pub apply_grant_token: String,
    pub destination_scope_id: String,
    pub pack_name: String,
    pub review_mode: ReviewMode,
    pub candidates: Vec<SourceImportCandidate>,
    #[serde(default)]
    pub conflicts: usize,
    #[serde(default)]
    pub duplicates: usize,
    #[serde(default)]
    pub warnings: Vec<String>,
    pub apply_allowed: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
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

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceImportApplyResult {
    pub request_id: String,
    pub destination_scope_id: String,
    pub pack_name: String,
    pub navigation_scope_id: String,
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

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BundleFormat {
    UcmJson,
    UcmMarkdown,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BundleImportPreview {
    pub path: String,
    #[serde(default)]
    pub apply_grant_token: String,
    pub format: BundleFormat,
    pub valid: bool,
    pub file_size_bytes: u64,
    pub checksum_sha256: String,
    pub exported_at: String,
    pub pack_count: usize,
    pub entry_count: usize,
    pub review_count: usize,
    pub run_count: usize,
    #[serde(default)]
    pub scope_ids: Vec<String>,
    #[serde(default)]
    pub warnings: Vec<String>,
    pub requires_confirmation: bool,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BundleImportApplyInput {
    pub path: String,
    #[serde(default)]
    pub grant_token: Option<String>,
    pub checksum_sha256: String,
    pub confirmation: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrivacyDataCounts {
    pub packs: usize,
    pub entries: usize,
    pub reviews: usize,
    pub runs: usize,
    pub spool_backlog: usize,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrivacySummary {
    pub data_path: String,
    pub database_path: String,
    pub socket_path: String,
    pub spool_path: String,
    pub settings_path: String,
    pub local_only_statement: String,
    pub downstream_adapter_disclosure: String,
    pub secret_scanning_statement: String,
    pub application_encryption_boundary: String,
    #[serde(default)]
    pub counts: PrivacyDataCounts,
    #[serde(default)]
    pub counts_available: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub counts_source: Option<String>,
    pub telemetry_enabled: bool,
    pub network_egress_enabled: bool,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ForgetScopeInput {
    pub scope_id: String,
    pub confirmation: bool,
    #[serde(default)]
    pub actor: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ForgetScopeFailure {
    pub pack_id: String,
    pub pack_name: String,
    pub error: DesktopError,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ForgetScopeResult {
    pub scope_id: String,
    pub scopes_matched: usize,
    pub packs_archived: usize,
    pub packs_already_archived: usize,
    pub entries_affected: usize,
    pub reversible: bool,
    pub stopped: bool,
    #[serde(default)]
    pub failures: Vec<ForgetScopeFailure>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn legacy_settings_payload_gets_safe_additive_defaults() {
        let settings: Settings = serde_json::from_value(json!({
            "theme": "system",
            "autoCompose": true,
            "reviewMode": "balanced",
            "socketPath": "/local/contextd.sock",
            "launchOnLogin": false,
            "telemetry": false,
            "maxPreviewTokens": 1400
        }))
        .expect("legacy settings");
        assert_eq!(settings.review_mode, ReviewMode::Balanced);
        assert!(settings.review_policy.is_none());
        assert!(!settings.onboarding.complete);
        assert!(settings.last_selected_scope_id.is_none());
    }

    #[test]
    fn legacy_context_preview_payload_gets_safe_additive_defaults() {
        let preview: ContextPreview = serde_json::from_value(json!({
            "scopeId": "global:global",
            "headline": "Preview",
            "totalTokens": 0,
            "warnings": [],
            "sections": [],
            "sources": []
        }))
        .expect("legacy preview");
        assert_eq!(preview.destination_adapter, "generic");
        assert!(preview.rendered_markdown.is_empty());
        assert!(preview.included_entries.is_empty());
    }

    #[test]
    fn typed_error_and_review_policy_use_frontend_casing() {
        let error = serde_json::to_value(DesktopError {
            code: DesktopErrorCode::SecretDetected,
            message: "Secret rejected".to_string(),
            retryable: false,
        })
        .expect("error json");
        assert_eq!(error["code"], "secret_detected");
        assert_eq!(error["retryable"], false);

        let policy = serde_json::to_value(ReviewPolicy {
            mode: ReviewMode::Strict,
            metadata: json!({}),
            updated_at: "2026-08-29T00:00:00Z".to_string(),
            updated_by: "operator".to_string(),
            revision: 2,
        })
        .expect("policy json");
        assert_eq!(policy["mode"], "strict");
        assert_eq!(policy["updatedAt"], "2026-08-29T00:00:00Z");
        assert!(policy.get("updated_at").is_none());
    }

    #[test]
    fn search_target_serializes_navigation_ids_without_parsing() {
        let target = serde_json::to_value(SearchTarget {
            scope_id: Some("project:atlas".to_string()),
            pack_id: Some("pack-1".to_string()),
            entry_id: Some("entry-1".to_string()),
            review_id: None,
            revision_id: None,
            adapter_id: None,
        })
        .expect("search target json");
        assert_eq!(target["scopeId"], "project:atlas");
        assert_eq!(target["packId"], "pack-1");
        assert_eq!(target["entryId"], "entry-1");
        assert!(target.get("reviewId").is_none());
        assert_eq!(
            serde_json::to_value(SearchKind::Entry).expect("entry kind"),
            "entry"
        );
        let legacy: SearchResult = serde_json::from_value(json!({
            "id": "pack-1",
            "kind": "pack",
            "title": "Pack",
            "excerpt": "Excerpt",
            "scopeLabel": "Atlas",
            "score": 80,
            "updatedAt": "2026-08-29T00:00:00Z",
            "tags": []
        }))
        .expect("legacy search result");
        assert_eq!(legacy.target, SearchTarget::default());
    }

    #[test]
    fn bulk_review_confirmation_defaults_false_for_legacy_callers() {
        let input: BulkReviewDecisionInput = serde_json::from_value(json!({
            "itemIds": ["review-1"],
            "decision": "approve"
        }))
        .expect("legacy bulk input");
        assert!(!input.confirmation);
    }

    #[test]
    fn source_import_apply_fingerprint_is_additive_but_runtime_required() {
        let input: SourceImportApplyInput = serde_json::from_value(json!({
            "paths": ["AGENTS.md"],
            "destinationScopeId": "project:atlas",
            "sourceKind": "auto",
            "previewId": "file-checksum",
            "confirmation": true
        }))
        .expect("legacy source import apply");
        assert!(input.expected_preview_fingerprint.is_none());
    }
}
