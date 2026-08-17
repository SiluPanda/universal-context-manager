use serde::{Deserialize, Serialize};

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

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ScopeKind {
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

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewSection {
    pub id: String,
    pub title: String,
    pub pack_name: String,
    pub scope_label: String,
    pub scope_kind: ScopeKind,
    pub tokens: u32,
    pub body: String,
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
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SearchKind {
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
    pub pack_id: String,
    pub pack_name: String,
    pub scope_id: String,
    pub scope_label: String,
    pub title: String,
    pub summary: String,
    pub requested_by: String,
    pub requested_at: String,
    pub risk: RiskLevel,
    pub diff: String,
    pub suggested_edit: String,
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
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemeMode {
    System,
    Light,
    Dark,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReviewMode {
    Strict,
    Balanced,
    Fast,
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
    pub review_queue: Vec<ReviewItem>,
    pub activity: Vec<ActivityRun>,
    pub revisions: Vec<RevisionEntry>,
    pub adapters: Vec<AdapterStatus>,
    pub settings: Settings,
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
pub struct ReviewDecisionInput {
    pub item_id: String,
    pub decision: ReviewDecision,
    pub edited_content: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReviewDecision {
    Approve,
    Reject,
    Edit,
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
