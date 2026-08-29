export type ScopeKind = 'global' | 'project' | 'task'
export type PackStatus = 'active' | 'draft' | 'review'
export type EntryFormat = 'markdown' | 'json'
export type EntryStatus = 'active' | 'deleted'
export type RiskLevel = 'low' | 'medium' | 'high'
export type RunStatus = 'running' | 'completed' | 'blocked' | 'failed'
export type AdapterKind = 'filesystem' | 'git' | 'terminal' | 'api'
export type AdapterHealth = 'healthy' | 'degraded' | 'offline'
export type ThemeMode = 'system' | 'light' | 'dark'
export type ReviewMode = 'strict' | 'balanced' | 'fast'
export type ReviewDecision = 'approve' | 'reject' | 'edit'
export type ReviewState = 'pending' | 'approved' | 'rejected'
export type ReviewReason = 'global_scope' | 'conflict' | 'locked' | 'strict_policy'
export type SearchKind = 'entry' | 'pack' | 'review' | 'run' | 'revision' | 'adapter'
export type ContextExclusionReason = 'deleted_entry' | 'archived_pack'
export type DiagnosticState =
  | 'not_installed'
  | 'stopped'
  | 'starting'
  | 'healthy'
  | 'degraded'
  | 'incompatible'
  | 'migration_required'
  | 'failed'
  | 'ignored'
export type DiagnosticActionKind =
  | 'refresh'
  | 'start_daemon'
  | 'restart_daemon'
  | 'retry_spool'
  | 'open_path'
  | 'manual'
export type DesktopErrorCode =
  | 'secret_detected'
  | 'unavailable'
  | 'invalid_import'
  | 'conflict'
  | 'permission_denied'
  | 'not_found'
  | 'invalid_input'
  | 'incompatible'
  | 'confirmation_required'
  | 'path_grant_required'
  | 'path_grant_invalid'
  | 'path_grant_expired'
  | 'internal'
export type PathGrantPurpose =
  | 'project_registration'
  | 'source_import_preview'
  | 'source_import_apply'
  | 'bundle_import_preview'
  | 'bundle_import_apply'
  | 'export_archive'
export type SourceImportKind =
  | 'auto'
  | 'ucm_json'
  | 'ucm_markdown'
  | 'agents_md'
  | 'claude_md'
  | 'copilot_instructions'
  | 'cursor_rule'
  | 'continue_rule'
  | 'plain_markdown'
export type SourceImportDisposition = 'new' | 'duplicate' | 'conflict'
export type CommitDisposition = 'applied' | 'pending' | 'duplicate' | 'rejected'
export type BundleFormat = 'ucm_json' | 'ucm_markdown'

export interface WorkspaceNode {
  id: string
  label: string
  kind: ScopeKind
  description: string
  status: string
  children: WorkspaceNode[]
}

export interface PathGrantSelection {
  grantToken: string
  purpose: PathGrantPurpose
  paths: string[]
  expiresAt: string
}

export interface ContextPack {
  id: string
  scopeId: string
  scopeKind: ScopeKind
  scopeLabel: string
  name: string
  status: PackStatus
  tokenEstimate: number
  updatedAt: string
  summary: string
  tags: string[]
  body: string
  provenance: string[]
  revision: number
}

export interface EntryProvenance {
  actor: string
  source: string
  sourceRef?: string
  runId?: string
  requestId?: string
  note?: string
}

export interface ContextEntry {
  id: string
  packId: string
  packName: string
  packKey: string
  scopeId: string
  scopeKind: ScopeKind
  scopeLabel: string
  key: string
  title?: string
  kind: string
  format: EntryFormat
  body: string
  renderedBody: string
  jsonValue?: unknown
  tags: string[]
  locked: boolean
  status: EntryStatus
  provenance: EntryProvenance
  revision: number
  createdAt: string
  updatedAt: string
}

export interface PreviewSection {
  id: string
  order: number
  layer: string
  title: string
  packName: string
  scopeId: string
  scopeLabel: string
  scopeKind: ScopeKind
  tokens: number
  body: string
  entryIds: string[]
}

export interface PreviewSource {
  packId: string
  packName: string
  scopeLabel: string
  excerpt: string
  tokens: number
}

export interface ContextMetrics {
  renderedBytes: number
  estimatedTokens: number
  includedEntries: number
  excludedEntries: number
}

export interface ContextExclusion {
  entryId: string
  scopeId: string
  scopeKind: ScopeKind
  scopeLabel: string
  packName: string
  entryKey: string
  revision: number
  reason: ContextExclusionReason
}

export interface IncludedContextEntry {
  order: number
  entryId: string
  packName: string
  scopeId: string
  scopeKind: ScopeKind
  scopeLabel: string
  key: string
  title?: string
  kind: string
  format: EntryFormat
  provenance: EntryProvenance
  revision: number
  tokenEstimate: number
}

export interface ContextPreview {
  scopeId: string
  headline: string
  totalTokens: number
  warnings: string[]
  sections: PreviewSection[]
  sources: PreviewSource[]
  destinationAdapter: string
  generatedAt: string
  renderedMarkdown: string
  metrics: ContextMetrics
  exclusions: ContextExclusion[]
  includedEntries: IncludedContextEntry[]
}

export interface ComposeContextInput {
  scopeId: string
  destinationAdapter?: string
}

export interface SearchResult {
  id: string
  kind: SearchKind
  title: string
  excerpt: string
  scopeLabel: string
  score: number
  updatedAt: string
  tags: string[]
  target: SearchTarget
}

export interface SearchTarget {
  scopeId?: string
  packId?: string
  entryId?: string
  reviewId?: string
  revisionId?: string
  adapterId?: string
}

export interface ReviewDiff {
  before?: string
  after: string
  format: EntryFormat
  changed: boolean
}

export interface ReviewItem {
  id: string
  requestId: string
  packId: string
  packName: string
  scopeId: string
  scopeKind: ScopeKind
  scopeLabel: string
  entryKey: string
  title: string
  summary: string
  requestedBy: string
  requestedAt: string
  ageSeconds: number
  risk: RiskLevel
  reason: ReviewReason | null
  diff: string
  diffSides: ReviewDiff
  existingContent?: string
  proposedContent: string
  provenance?: EntryProvenance
  source: string
  suggestedEdit: string
}

export interface ActivityRun {
  id: string
  actor: string
  summary: string
  status: RunStatus
  startedAt: string
  durationMs: number
  stepCount: number
  contextPackIds: string[]
}

export interface RevisionEntry {
  id: string
  entityId: string
  entityLabel: string
  author: string
  createdAt: string
  note: string
  changeSummary: string
  restorable: boolean
}

export interface DiagnosticAction {
  id: string
  label: string
  kind: DiagnosticActionKind
}

export interface DiagnosticCheck {
  id: string
  label: string
  component: string
  state: DiagnosticState
  summary: string
  details: string[]
  path?: string
  detectedVersion?: string
  expectedVersion?: string
  remediation: DiagnosticAction[]
  checkedAt: string
}

export interface DiagnosticsReport {
  generatedAt: string
  overallState: DiagnosticState
  daemonReachable: boolean
  componentVersion?: string
  apiVersion: number | null
  expectedApiVersion: number
  schemaVersion: number | null
  expectedSchemaVersion: number
  spoolBacklog: number
  checks: DiagnosticCheck[]
}

export interface AdapterStatus {
  id: string
  name: string
  kind: AdapterKind
  enabled: boolean
  health: AdapterHealth
  lastCheckedAt: string
  queueDepth: number
  path: string
  note: string
  state: DiagnosticState
  detectedVersion?: string
  remediation: DiagnosticAction[]
}

export interface SpoolRetryResult {
  attempted: number
  delivered: number
  retained: number
  errors: string[]
  diagnostics: DiagnosticsReport
}

export interface DaemonControlResult {
  action: string
  performed: boolean
  message: string
  diagnostics: DiagnosticsReport
}

export interface ReviewPolicy {
  mode: ReviewMode
  metadata: unknown
  updatedAt: string
  updatedBy: string
  revision: number
}

export interface OnboardingState {
  complete: boolean
  inferred: boolean
  durableContext: boolean
  completedAt?: string
  lastProjectPath?: string
}

export interface Settings {
  theme: ThemeMode
  autoCompose: boolean
  reviewMode: ReviewMode
  socketPath: string
  launchOnLogin: boolean
  telemetry: boolean
  maxPreviewTokens: number
  reviewPolicy?: ReviewPolicy
  onboarding: OnboardingState
  lastSelectedScopeId?: string
  lastProjectPath?: string
}

export interface DashboardStats {
  activePacks: number
  pendingReviews: number
  healthyAdapters: number
  runningAgents: number
}

export interface PrivacyDataCounts {
  packs: number
  entries: number
  reviews: number
  runs: number
  spoolBacklog: number
}

export interface PrivacySummary {
  dataPath: string
  databasePath: string
  socketPath: string
  spoolPath: string
  settingsPath: string
  localOnlyStatement: string
  downstreamAdapterDisclosure: string
  secretScanningStatement: string
  applicationEncryptionBoundary: string
  counts: PrivacyDataCounts
  countsAvailable: boolean
  countsSource?: 'store_stats' | 'daemon_health' | 'read_only_database'
  telemetryEnabled: boolean
  networkEgressEnabled: boolean
}

export interface DashboardSnapshot {
  workspace: WorkspaceNode[]
  packs: ContextPack[]
  entries: ContextEntry[]
  reviewQueue: ReviewItem[]
  activity: ActivityRun[]
  revisions: RevisionEntry[]
  adapters: AdapterStatus[]
  settings: Settings
  reviewPolicy?: ReviewPolicy
  onboarding: OnboardingState
  diagnostics: DiagnosticsReport
  privacy: PrivacySummary
  stats: DashboardStats
  selectedScopeId: string
  connected: boolean
  lastSyncAt: string
  notices: string[]
}

export interface SavePackInput {
  id?: string
  scopeId: string
  name: string
  status: PackStatus
  summary: string
  tags: string[]
  body: string
}

export interface SaveEntryInput {
  id?: string
  scopeId: string
  packId?: string
  packName?: string
  key: string
  title?: string
  kind: string
  format: EntryFormat
  body: string
  tags: string[]
  locked: boolean
  actor?: string
  note?: string
}

export interface RevertEntryInput {
  entryId: string
  revision?: number
  actor?: string
}

export interface ReviewDecisionInput {
  itemId: string
  decision: ReviewDecision
  editedContent?: string
}

export interface BulkReviewDecisionInput {
  itemIds: string[]
  decision: ReviewDecision
  confirmation: boolean
  editedContent?: string
  actor?: string
  note?: string
}

export interface ReviewDecisionResult {
  itemId: string
  success: boolean
  requiresFollowUp: boolean
  state?: ReviewState
  error?: DesktopError
}

export interface BulkReviewDecisionResult {
  decision: ReviewDecision
  attempted: number
  completed: number
  stopped: boolean
  results: ReviewDecisionResult[]
}

export interface SetReviewPolicyInput {
  mode: ReviewMode
  actor: string
  note?: string
  requestId?: string
}

export interface ImportExportSummary {
  path: string
  packsImported: number
  adaptersTouched: number
  revisionId: string
  exportedAt: string
}

export interface RestoreRevisionResult {
  revisionId: string
  entityId: string
  restoredAt: string
}

export interface DesktopError {
  code: DesktopErrorCode
  message: string
  retryable: boolean
}

export interface DiscoveredInstructionSource {
  path: string
  relativePath: string
  sourceKind: SourceImportKind
  readable: boolean
}

export interface ProjectRegistration {
  inputPath: string
  normalizedPath: string
  scopeId: string
  scopeKind: ScopeKind
  label: string
  instructionSources: DiscoveredInstructionSource[]
  durable: boolean
  selected: boolean
}

export interface SourceImportPreviewInput {
  paths: string[]
  grantToken: string
  destinationScopeId: string
  packName?: string
  sourceKind?: SourceImportKind
  actor?: string
}

export interface SourceImportApplyInput extends SourceImportPreviewInput {
  previewId: string
  expectedPreviewFingerprint: string
  confirmation: boolean
}

export interface SourceImportCandidate {
  candidateIndex: number
  documentIndex: number
  sourcePath?: string
  detectedSourceKind: SourceImportKind
  key: string
  title?: string
  kind: string
  format: EntryFormat
  renderedBody: string
  tags: string[]
  locked: boolean
  provenance?: EntryProvenance
  disposition: SourceImportDisposition
  existingEntryId?: string
  existingRevision?: number
  warnings: string[]
}

export interface SourceImportPreviewResult {
  previewId: string
  previewFingerprint: string
  applyGrantToken: string
  destinationScopeId: string
  packName: string
  reviewMode: ReviewMode
  candidates: SourceImportCandidate[]
  conflicts: number
  duplicates: number
  warnings: string[]
  applyAllowed: boolean
}

export interface SourceImportApplyItem {
  candidateIndex: number
  documentIndex: number
  sourcePath?: string
  entryKey: string
  disposition: CommitDisposition
  reason?: string
  entryId?: string
  reviewId?: string
}

export interface SourceImportApplyResult {
  requestId: string
  destinationScopeId: string
  packName: string
  navigationScopeId: string
  candidateCount: number
  importedCount: number
  appliedCount: number
  pendingCount: number
  skippedCount: number
  rejectedCount: number
  items: SourceImportApplyItem[]
  affectedEntryIds: string[]
  affectedReviewIds: string[]
  affectedEntryKeys: string[]
}

export interface BundleImportPreview {
  path: string
  applyGrantToken: string
  format: BundleFormat
  valid: boolean
  fileSizeBytes: number
  checksumSha256: string
  exportedAt: string
  packCount: number
  entryCount: number
  reviewCount: number
  runCount: number
  scopeIds: string[]
  warnings: string[]
  requiresConfirmation: boolean
}

export interface BundleImportApplyInput {
  path: string
  grantToken: string
  checksumSha256: string
  confirmation: boolean
}

export interface ForgetScopeInput {
  scopeId: string
  confirmation: boolean
  actor?: string
}

export interface ForgetScopeFailure {
  packId: string
  packName: string
  error: DesktopError
}

export interface ForgetScopeResult {
  scopeId: string
  scopesMatched: number
  packsArchived: number
  packsAlreadyArchived: number
  entriesAffected: number
  reversible: boolean
  stopped: boolean
  failures: ForgetScopeFailure[]
}
