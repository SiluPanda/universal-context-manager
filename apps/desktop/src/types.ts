export type ScopeKind = 'global' | 'project' | 'task'
export type PackStatus = 'active' | 'draft' | 'review'
export type RiskLevel = 'low' | 'medium' | 'high'
export type RunStatus = 'running' | 'completed' | 'blocked' | 'failed'
export type AdapterKind = 'filesystem' | 'git' | 'terminal' | 'api'
export type AdapterHealth = 'healthy' | 'degraded' | 'offline'
export type ThemeMode = 'system' | 'light' | 'dark'
export type ReviewMode = 'strict' | 'balanced' | 'fast'
export type ReviewDecision = 'approve' | 'reject' | 'edit'

export interface WorkspaceNode {
  id: string
  label: string
  kind: ScopeKind
  description: string
  status: string
  children: WorkspaceNode[]
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

export interface PreviewSection {
  id: string
  title: string
  packName: string
  scopeLabel: string
  scopeKind: ScopeKind
  tokens: number
  body: string
}

export interface PreviewSource {
  packId: string
  packName: string
  scopeLabel: string
  excerpt: string
  tokens: number
}

export interface ContextPreview {
  scopeId: string
  headline: string
  totalTokens: number
  warnings: string[]
  sections: PreviewSection[]
  sources: PreviewSource[]
}

export interface SearchResult {
  id: string
  kind: 'pack' | 'review' | 'run' | 'revision' | 'adapter'
  title: string
  excerpt: string
  scopeLabel: string
  score: number
  updatedAt: string
  tags: string[]
}

export interface ReviewItem {
  id: string
  packId: string
  packName: string
  scopeId: string
  scopeLabel: string
  title: string
  summary: string
  requestedBy: string
  requestedAt: string
  risk: RiskLevel
  diff: string
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
}

export interface Settings {
  theme: ThemeMode
  autoCompose: boolean
  reviewMode: ReviewMode
  socketPath: string
  launchOnLogin: boolean
  telemetry: boolean
  maxPreviewTokens: number
}

export interface DashboardStats {
  activePacks: number
  pendingReviews: number
  healthyAdapters: number
  runningAgents: number
}

export interface DashboardSnapshot {
  workspace: WorkspaceNode[]
  packs: ContextPack[]
  reviewQueue: ReviewItem[]
  activity: ActivityRun[]
  revisions: RevisionEntry[]
  adapters: AdapterStatus[]
  settings: Settings
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

export interface ReviewDecisionInput {
  itemId: string
  decision: ReviewDecision
  editedContent?: string
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
