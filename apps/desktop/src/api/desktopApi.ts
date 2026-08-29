import type {
  AdapterStatus,
  BulkReviewDecisionInput,
  BulkReviewDecisionResult,
  BundleImportApplyInput,
  BundleImportPreview,
  ComposeContextInput,
  ContextEntry,
  ContextPack,
  ContextPreview,
  DaemonControlResult,
  DashboardSnapshot,
  DiagnosticsReport,
  ForgetScopeInput,
  ForgetScopeResult,
  ImportExportSummary,
  OnboardingState,
  PathGrantSelection,
  PrivacySummary,
  ProjectRegistration,
  RestoreRevisionResult,
  ReviewDecisionInput,
  ReviewPolicy,
  RevisionEntry,
  SaveEntryInput,
  SavePackInput,
  SearchResult,
  SetReviewPolicyInput,
  Settings,
  SourceImportApplyInput,
  SourceImportApplyResult,
  SourceImportPreviewInput,
  SourceImportPreviewResult,
  SpoolRetryResult,
} from '../types'
import { normalizeDesktopError } from './errors'
import { MockDesktopApi, type MockDialogSelections } from './mockClient'
export { DesktopApiError, friendlyDesktopError, normalizeDesktopError } from './errors'

export interface DesktopApi {
  loadDashboard(): Promise<DashboardSnapshot>
  listPacks(scopeId?: string): Promise<ContextPack[]>
  savePack(input: SavePackInput): Promise<ContextPack>
  listEntries(scopeId?: string, packId?: string): Promise<ContextEntry[]>
  saveEntry(input: SaveEntryInput): Promise<ContextEntry>
  archiveEntry(entryId: string): Promise<ContextEntry>
  deleteEntry(entryId: string): Promise<ContextEntry>
  restoreEntry(entryId: string): Promise<ContextEntry>
  revertEntryRevision(input: { entryId: string; revision?: number; actor?: string }): Promise<ContextEntry>
  composePreview(scopeId: string): Promise<ContextPreview>
  composeEffectiveContext(input: ComposeContextInput): Promise<ContextPreview>
  searchIndex(query: string): Promise<SearchResult[]>
  listRevisions(entityId?: string): Promise<RevisionEntry[]>
  reviewDecision(input: ReviewDecisionInput): Promise<void>
  bulkReviewDecision(input: BulkReviewDecisionInput): Promise<BulkReviewDecisionResult>
  setReviewPolicy(input: SetReviewPolicyInput): Promise<ReviewPolicy>
  restoreRevision(revisionId: string): Promise<RestoreRevisionResult>
  listAdapters(): Promise<AdapterStatus[]>
  toggleAdapter(adapterId: string, enabled: boolean): Promise<AdapterStatus>
  loadDiagnostics(): Promise<DiagnosticsReport>
  refreshDiagnostics(): Promise<DiagnosticsReport>
  startDaemon(): Promise<DaemonControlResult>
  restartDaemon(): Promise<DaemonControlResult>
  retrySpool(): Promise<SpoolRetryResult>
  loadSettings(): Promise<Settings>
  saveSettings(settings: Settings): Promise<Settings>
  completeOnboarding(): Promise<OnboardingState>
  resetOnboarding(): Promise<OnboardingState>
  registerProject(path: string, grantToken: string): Promise<ProjectRegistration>
  setSelectedScope(scopeId: string, projectPath?: string): Promise<Settings>
  previewSourceImport(input: SourceImportPreviewInput): Promise<SourceImportPreviewResult>
  applySourceImport(input: SourceImportApplyInput): Promise<SourceImportApplyResult>
  previewBundleImport(path: string, grantToken: string): Promise<BundleImportPreview>
  applyBundleImport(input: BundleImportApplyInput): Promise<ImportExportSummary>
  loadPrivacySummary(): Promise<PrivacySummary>
  forgetScope(input: ForgetScopeInput): Promise<ForgetScopeResult>
  archiveScope(input: ForgetScopeInput): Promise<ForgetScopeResult>
  exportArchive(path: string, grantToken: string): Promise<ImportExportSummary>
  selectProjectDirectory(): Promise<PathGrantSelection | null>
  selectSourceImportFiles(): Promise<PathGrantSelection | null>
  selectBundleImportFile(): Promise<PathGrantSelection | null>
  selectExportDestination(): Promise<PathGrantSelection | null>
}

export interface CreateDesktopApiOptions {
  forceMock?: boolean
  seed?: DashboardSnapshot
  dialogs?: Partial<MockDialogSelections>
}

function hasTauriRuntime() {
  return (
    typeof window !== 'undefined' &&
    Object.prototype.hasOwnProperty.call(window, '__TAURI_INTERNALS__')
  )
}

async function invokeTauri<T>(command: string, args?: Record<string, unknown>): Promise<T> {
  try {
    const module = await import('@tauri-apps/api/core')
    return await module.invoke<T>(command, args)
  } catch (error) {
    throw normalizeDesktopError(error)
  }
}

class TauriDesktopApi implements DesktopApi {
  loadDashboard() {
    return invokeTauri<DashboardSnapshot>('load_dashboard')
  }

  listPacks(scopeId?: string) {
    return invokeTauri<ContextPack[]>('list_packs', { scopeId })
  }

  savePack(input: SavePackInput) {
    return invokeTauri<ContextPack>('save_pack', { input })
  }

  listEntries(scopeId?: string, packId?: string) {
    return invokeTauri<ContextEntry[]>('list_entries', { scopeId, packId })
  }

  saveEntry(input: SaveEntryInput) {
    return invokeTauri<ContextEntry>('save_entry', { input })
  }

  archiveEntry(entryId: string) {
    return invokeTauri<ContextEntry>('archive_entry', { entryId })
  }

  deleteEntry(entryId: string) {
    return invokeTauri<ContextEntry>('delete_entry', { entryId })
  }

  restoreEntry(entryId: string) {
    return invokeTauri<ContextEntry>('restore_entry', { entryId })
  }

  revertEntryRevision(input: { entryId: string; revision?: number; actor?: string }) {
    return invokeTauri<ContextEntry>('revert_entry_revision', { input })
  }

  composePreview(scopeId: string) {
    return invokeTauri<ContextPreview>('compose_preview', { scopeId })
  }

  composeEffectiveContext(input: ComposeContextInput) {
    return invokeTauri<ContextPreview>('compose_effective_context', { input })
  }

  searchIndex(query: string) {
    return invokeTauri<SearchResult[]>('search_index', { query })
  }

  listRevisions(entityId?: string) {
    return invokeTauri<RevisionEntry[]>('list_revisions', { entityId })
  }

  reviewDecision(input: ReviewDecisionInput) {
    return invokeTauri<void>('review_decision', { input })
  }

  bulkReviewDecision(input: BulkReviewDecisionInput) {
    return invokeTauri<BulkReviewDecisionResult>('bulk_review_decision', { input })
  }

  setReviewPolicy(input: SetReviewPolicyInput) {
    return invokeTauri<ReviewPolicy>('set_review_policy', { input })
  }

  restoreRevision(revisionId: string) {
    return invokeTauri<RestoreRevisionResult>('restore_revision', { revisionId })
  }

  listAdapters() {
    return invokeTauri<AdapterStatus[]>('list_adapters')
  }

  toggleAdapter(adapterId: string, enabled: boolean) {
    return invokeTauri<AdapterStatus>('toggle_adapter', { adapterId, enabled })
  }

  loadDiagnostics() {
    return invokeTauri<DiagnosticsReport>('load_diagnostics')
  }

  refreshDiagnostics() {
    return invokeTauri<DiagnosticsReport>('refresh_diagnostics')
  }

  startDaemon() {
    return invokeTauri<DaemonControlResult>('start_daemon')
  }

  restartDaemon() {
    return invokeTauri<DaemonControlResult>('restart_daemon')
  }

  retrySpool() {
    return invokeTauri<SpoolRetryResult>('retry_spool')
  }

  loadSettings() {
    return invokeTauri<Settings>('load_settings')
  }

  saveSettings(settings: Settings) {
    return invokeTauri<Settings>('save_settings', { settings })
  }

  completeOnboarding() {
    return invokeTauri<OnboardingState>('complete_onboarding')
  }

  resetOnboarding() {
    return invokeTauri<OnboardingState>('reset_onboarding')
  }

  registerProject(path: string, grantToken: string) {
    return invokeTauri<ProjectRegistration>('register_project', { path, grantToken })
  }

  setSelectedScope(scopeId: string, projectPath?: string) {
    return invokeTauri<Settings>('set_selected_scope', { scopeId, projectPath })
  }

  previewSourceImport(input: SourceImportPreviewInput) {
    return invokeTauri<SourceImportPreviewResult>('preview_source_import', { input })
  }

  applySourceImport(input: SourceImportApplyInput) {
    return invokeTauri<SourceImportApplyResult>('apply_source_import', { input })
  }

  previewBundleImport(path: string, grantToken: string) {
    return invokeTauri<BundleImportPreview>('preview_bundle_import', { path, grantToken })
  }

  applyBundleImport(input: BundleImportApplyInput) {
    return invokeTauri<ImportExportSummary>('apply_bundle_import', { input })
  }

  loadPrivacySummary() {
    return invokeTauri<PrivacySummary>('load_privacy_summary')
  }

  forgetScope(input: ForgetScopeInput) {
    return invokeTauri<ForgetScopeResult>('forget_scope', { input })
  }

  archiveScope(input: ForgetScopeInput) {
    return invokeTauri<ForgetScopeResult>('archive_scope', { input })
  }

  exportArchive(path: string, grantToken: string) {
    return invokeTauri<ImportExportSummary>('export_archive', { path, grantToken })
  }

  selectProjectDirectory() {
    return invokeTauri<PathGrantSelection | null>('select_project_directory')
  }

  selectSourceImportFiles() {
    return invokeTauri<PathGrantSelection | null>('select_source_import_files')
  }

  selectBundleImportFile() {
    return invokeTauri<PathGrantSelection | null>('select_bundle_import_file')
  }

  selectExportDestination() {
    return invokeTauri<PathGrantSelection | null>('select_export_destination')
  }
}

export function createDesktopApi(options: CreateDesktopApiOptions = {}): DesktopApi {
  if (!options.forceMock && hasTauriRuntime()) {
    return new TauriDesktopApi()
  }

  return new MockDesktopApi(options.seed, options.dialogs)
}

export const desktopApi = createDesktopApi()
