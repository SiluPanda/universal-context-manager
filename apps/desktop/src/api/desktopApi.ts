import type {
  AdapterStatus,
  ContextPack,
  ContextPreview,
  DashboardSnapshot,
  ImportExportSummary,
  RestoreRevisionResult,
  ReviewDecisionInput,
  RevisionEntry,
  SavePackInput,
  SearchResult,
  Settings,
} from '../types'
import { MockDesktopApi } from './mockClient'

export interface DesktopApi {
  loadDashboard(): Promise<DashboardSnapshot>
  listPacks(scopeId?: string): Promise<ContextPack[]>
  savePack(input: SavePackInput): Promise<ContextPack>
  composePreview(scopeId: string): Promise<ContextPreview>
  searchIndex(query: string): Promise<SearchResult[]>
  listRevisions(entityId?: string): Promise<RevisionEntry[]>
  reviewDecision(input: ReviewDecisionInput): Promise<void>
  restoreRevision(revisionId: string): Promise<RestoreRevisionResult>
  listAdapters(): Promise<AdapterStatus[]>
  toggleAdapter(adapterId: string, enabled: boolean): Promise<AdapterStatus>
  loadSettings(): Promise<Settings>
  saveSettings(settings: Settings): Promise<Settings>
  exportArchive(path: string): Promise<ImportExportSummary>
  importArchive(path: string): Promise<ImportExportSummary>
}

interface CreateDesktopApiOptions {
  forceMock?: boolean
  seed?: DashboardSnapshot
}

function hasTauriRuntime() {
  return typeof window !== 'undefined' && Object.prototype.hasOwnProperty.call(window, '__TAURI_INTERNALS__')
}

async function invokeTauri<T>(command: string, args?: Record<string, unknown>) {
  const module = await import('@tauri-apps/api/core')
  return module.invoke<T>(command, args)
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

  composePreview(scopeId: string) {
    return invokeTauri<ContextPreview>('compose_preview', { scopeId })
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

  restoreRevision(revisionId: string) {
    return invokeTauri<RestoreRevisionResult>('restore_revision', { revisionId })
  }

  listAdapters() {
    return invokeTauri<AdapterStatus[]>('list_adapters')
  }

  toggleAdapter(adapterId: string, enabled: boolean) {
    return invokeTauri<AdapterStatus>('toggle_adapter', { adapterId, enabled })
  }

  loadSettings() {
    return invokeTauri<Settings>('load_settings')
  }

  saveSettings(settings: Settings) {
    return invokeTauri<Settings>('save_settings', { settings })
  }

  exportArchive(path: string) {
    return invokeTauri<ImportExportSummary>('export_archive', { path })
  }

  importArchive(path: string) {
    return invokeTauri<ImportExportSummary>('import_archive', { path })
  }
}

export function createDesktopApi(options: CreateDesktopApiOptions = {}): DesktopApi {
  if (!options.forceMock && hasTauriRuntime()) {
    return new TauriDesktopApi()
  }

  return new MockDesktopApi(options.seed)
}

export const desktopApi = createDesktopApi()
