import { cloneMockDashboard } from './mockData'
import { composePreviewFromPacks, findScopePath, summarizeExcerpt } from '../lib/contextUtils'
import type {
  AdapterStatus,
  ContextPack,
  DashboardSnapshot,
  ImportExportSummary,
  RestoreRevisionResult,
  ReviewDecisionInput,
  RevisionEntry,
  SavePackInput,
  SearchResult,
  Settings,
} from '../types'

interface RevisionRecord {
  entry: RevisionEntry
  snapshot: ContextPack
}

interface MockStore {
  dashboard: DashboardSnapshot
  revisionRecords: RevisionRecord[]
  exports: Map<string, DashboardSnapshot>
  nextPackId: number
  nextRevisionId: number
}

function clonePack(pack: ContextPack) {
  return structuredClone(pack)
}

function estimateTokens(body: string) {
  const wordCount = body.trim().split(/\s+/u).filter(Boolean).length
  return Math.max(72, Math.ceil(wordCount * 1.35))
}

function nowIso() {
  return new Date().toISOString()
}

function buildStore(): MockStore {
  const dashboard = cloneMockDashboard()
  const revisionRecords = dashboard.revisions.map((entry) => {
    const pack = dashboard.packs.find((candidate) => candidate.id === entry.entityId)

    return {
      entry,
      snapshot: clonePack(
        pack ?? {
          id: entry.entityId,
          scopeId: dashboard.selectedScopeId,
          scopeKind: 'task',
          scopeLabel: 'Recovered context',
          name: entry.entityLabel,
          status: 'active',
          tokenEstimate: 96,
          updatedAt: entry.createdAt,
          summary: entry.note,
          tags: ['restored'],
          body: entry.changeSummary,
          provenance: ['restored/from-revision'],
          revision: 1,
        },
      ),
    }
  })

  return {
    dashboard,
    revisionRecords,
    exports: new Map<string, DashboardSnapshot>(),
    nextPackId: 100,
    nextRevisionId: 800,
  }
}

function recomputeStats(store: MockStore) {
  const { dashboard } = store
  dashboard.stats = {
    activePacks: dashboard.packs.filter((pack) => pack.status === 'active').length,
    pendingReviews: dashboard.reviewQueue.length,
    healthyAdapters: dashboard.adapters.filter((adapter) => adapter.health === 'healthy').length,
    runningAgents: dashboard.activity.filter((run) => run.status === 'running').length,
  }
  dashboard.lastSyncAt = nowIso()
}

function pushRevision(
  store: MockStore,
  pack: ContextPack,
  author: string,
  note: string,
  changeSummary: string,
) {
  const entry: RevisionEntry = {
    id: `rev-${store.nextRevisionId}`,
    entityId: pack.id,
    entityLabel: pack.name,
    author,
    createdAt: nowIso(),
    note,
    changeSummary,
    restorable: true,
  }

  store.nextRevisionId += 1
  store.dashboard.revisions = [entry, ...store.dashboard.revisions]
  store.revisionRecords = [{ entry, snapshot: clonePack(pack) }, ...store.revisionRecords]
}

function applyPackUpdate(
  store: MockStore,
  pack: ContextPack,
  author: string,
  note: string,
  changeSummary: string,
) {
  const index = store.dashboard.packs.findIndex((candidate) => candidate.id === pack.id)
  if (index >= 0) {
    store.dashboard.packs[index] = pack
  } else {
    store.dashboard.packs = [pack, ...store.dashboard.packs]
  }

  pushRevision(store, pack, author, note, changeSummary)
  recomputeStats(store)
}

function withLatency<T>(value: T) {
  return new Promise<T>((resolve) => {
    window.setTimeout(() => resolve(structuredClone(value)), 30)
  })
}

export class MockDesktopApi {
  private readonly store: MockStore

  constructor(seed?: DashboardSnapshot) {
    this.store = buildStore()
    if (seed) {
      this.store.dashboard = structuredClone(seed)
      recomputeStats(this.store)
    }
  }

  async loadDashboard() {
    recomputeStats(this.store)
    return withLatency(this.store.dashboard)
  }

  async listPacks(scopeId?: string) {
    const packs = scopeId
      ? this.store.dashboard.packs.filter((pack) => pack.scopeId === scopeId)
      : this.store.dashboard.packs

    return withLatency(
      [...packs].sort((left, right) => right.updatedAt.localeCompare(left.updatedAt)),
    )
  }

  async savePack(input: SavePackInput) {
    const scopePath = findScopePath(this.store.dashboard.workspace, input.scopeId)
    const scope = scopePath.at(-1)

    if (!scope) {
      throw new Error(`Unknown scope: ${input.scopeId}`)
    }

    const existing = input.id
      ? this.store.dashboard.packs.find((pack) => pack.id === input.id)
      : undefined

    const pack: ContextPack = {
      id: existing?.id ?? `pack-custom-${this.store.nextPackId++}`,
      scopeId: input.scopeId,
      scopeKind: scope.kind,
      scopeLabel: scope.label,
      name: input.name.trim() || 'Untitled context pack',
      status: input.status,
      tokenEstimate: estimateTokens(input.body),
      updatedAt: nowIso(),
      summary: input.summary.trim() || 'No summary provided yet.',
      tags: input.tags,
      body: input.body.trim(),
      provenance: existing?.provenance ?? ['desktop/manual-edit'],
      revision: (existing?.revision ?? 0) + 1,
    }

    applyPackUpdate(
      this.store,
      pack,
      'desktop-operator',
      existing ? 'Updated context pack from the desktop editor.' : 'Created a new context pack from the desktop editor.',
      existing ? 'Refreshed summary, body, or tags.' : 'Added a new pack for the selected scope.',
    )

    return withLatency(pack)
  }

  async composePreview(scopeId: string) {
    return withLatency(
      composePreviewFromPacks(
        this.store.dashboard.workspace,
        this.store.dashboard.packs,
        scopeId,
        this.store.dashboard.settings.maxPreviewTokens,
      ),
    )
  }

  async searchIndex(query: string) {
    const needle = query.trim().toLowerCase()
    if (!needle) {
      return withLatency([] as SearchResult[])
    }

    const packResults = this.store.dashboard.packs
      .filter((pack) => `${pack.name} ${pack.summary} ${pack.body}`.toLowerCase().includes(needle))
      .map<SearchResult>((pack) => ({
        id: pack.id,
        kind: 'pack',
        title: pack.name,
        excerpt: summarizeExcerpt(pack.summary),
        scopeLabel: pack.scopeLabel,
        score: 96,
        updatedAt: pack.updatedAt,
        tags: pack.tags,
      }))

    const reviewResults = this.store.dashboard.reviewQueue
      .filter((item) => `${item.title} ${item.summary} ${item.suggestedEdit}`.toLowerCase().includes(needle))
      .map<SearchResult>((item) => ({
        id: item.id,
        kind: 'review',
        title: item.title,
        excerpt: summarizeExcerpt(item.summary),
        scopeLabel: item.scopeLabel,
        score: 91,
        updatedAt: item.requestedAt,
        tags: [item.risk, 'review'],
      }))

    const runResults = this.store.dashboard.activity
      .filter((run) => `${run.actor} ${run.summary}`.toLowerCase().includes(needle))
      .map<SearchResult>((run) => ({
        id: run.id,
        kind: 'run',
        title: run.summary,
        excerpt: summarizeExcerpt(`${run.actor} · ${run.status}`),
        scopeLabel: 'Activity',
        score: 84,
        updatedAt: run.startedAt,
        tags: [run.status],
      }))

    const revisionResults = this.store.dashboard.revisions
      .filter((revision) => `${revision.entityLabel} ${revision.note} ${revision.changeSummary}`.toLowerCase().includes(needle))
      .map<SearchResult>((revision) => ({
        id: revision.id,
        kind: 'revision',
        title: revision.entityLabel,
        excerpt: summarizeExcerpt(revision.changeSummary),
        scopeLabel: 'Revision history',
        score: 80,
        updatedAt: revision.createdAt,
        tags: ['revision'],
      }))

    const adapterResults = this.store.dashboard.adapters
      .filter((adapter) => `${adapter.name} ${adapter.note}`.toLowerCase().includes(needle))
      .map<SearchResult>((adapter) => ({
        id: adapter.id,
        kind: 'adapter',
        title: adapter.name,
        excerpt: summarizeExcerpt(adapter.note),
        scopeLabel: 'Adapters',
        score: 76,
        updatedAt: adapter.lastCheckedAt,
        tags: [adapter.health, adapter.kind],
      }))

    return withLatency(
      [...packResults, ...reviewResults, ...runResults, ...revisionResults, ...adapterResults]
        .sort((left, right) => right.score - left.score || right.updatedAt.localeCompare(left.updatedAt))
        .slice(0, 12),
    )
  }

  async listRevisions(entityId?: string) {
    const revisions = entityId
      ? this.store.dashboard.revisions.filter((entry) => entry.entityId === entityId)
      : this.store.dashboard.revisions

    return withLatency(revisions)
  }

  async reviewDecision(input: ReviewDecisionInput) {
    const item = this.store.dashboard.reviewQueue.find((candidate) => candidate.id === input.itemId)
    if (!item) {
      throw new Error(`Unknown review item: ${input.itemId}`)
    }

    if (input.decision === 'approve' || input.decision === 'edit') {
      const pack = this.store.dashboard.packs.find((candidate) => candidate.id === item.packId)
      if (!pack) {
        throw new Error(`Missing context pack for review item: ${input.itemId}`)
      }

      const nextBody =
        input.decision === 'edit' && input.editedContent?.trim()
          ? input.editedContent.trim()
          : item.suggestedEdit

      const updatedPack: ContextPack = {
        ...pack,
        body: nextBody,
        status: 'active',
        updatedAt: nowIso(),
        tokenEstimate: estimateTokens(nextBody),
        revision: pack.revision + 1,
      }

      applyPackUpdate(
        this.store,
        updatedPack,
        'review-operator',
        input.decision === 'approve'
          ? 'Approved a queued review update.'
          : 'Applied an edited review update from the queue.',
        input.decision === 'approve'
          ? 'Merged the suggested review edit.'
          : 'Merged a reviewer-adjusted draft.',
      )
    }

    this.store.dashboard.reviewQueue = this.store.dashboard.reviewQueue.filter(
      (candidate) => candidate.id !== input.itemId,
    )
    recomputeStats(this.store)

    return withLatency(undefined)
  }

  async restoreRevision(revisionId: string) {
    const record = this.store.revisionRecords.find((candidate) => candidate.entry.id === revisionId)
    if (!record) {
      throw new Error(`Unknown revision: ${revisionId}`)
    }

    const restoredPack: ContextPack = {
      ...clonePack(record.snapshot),
      updatedAt: nowIso(),
      revision: record.snapshot.revision + 1,
    }

    applyPackUpdate(
      this.store,
      restoredPack,
      'restore-operator',
      'Restored a pack from revision history.',
      `Restored from ${record.entry.id}.`,
    )

    const result: RestoreRevisionResult = {
      revisionId,
      entityId: restoredPack.id,
      restoredAt: nowIso(),
    }

    return withLatency(result)
  }

  async listAdapters() {
    return withLatency(this.store.dashboard.adapters)
  }

  async toggleAdapter(adapterId: string, enabled: boolean) {
    const adapter = this.store.dashboard.adapters.find((candidate) => candidate.id === adapterId)
    if (!adapter) {
      throw new Error(`Unknown adapter: ${adapterId}`)
    }

    const next: AdapterStatus = {
      ...adapter,
      enabled,
      health: enabled ? adapter.health : 'offline',
      lastCheckedAt: nowIso(),
      note: enabled ? adapter.note : 'Disabled locally from the desktop control plane.',
    }

    this.store.dashboard.adapters = this.store.dashboard.adapters.map((candidate) =>
      candidate.id === adapterId ? next : candidate,
    )
    recomputeStats(this.store)

    return withLatency(next)
  }

  async loadSettings() {
    return withLatency(this.store.dashboard.settings)
  }

  async saveSettings(settings: Settings) {
    this.store.dashboard.settings = { ...settings }
    recomputeStats(this.store)
    return withLatency(this.store.dashboard.settings)
  }

  async exportArchive(path: string) {
    const archive = structuredClone(this.store.dashboard)
    this.store.exports.set(path, archive)

    const result: ImportExportSummary = {
      path,
      packsImported: archive.packs.length,
      adaptersTouched: archive.adapters.length,
      revisionId: `export-${this.store.nextRevisionId}`,
      exportedAt: nowIso(),
    }

    return withLatency(result)
  }

  async importArchive(path: string) {
    const imported = this.store.exports.get(path)
    if (!imported) {
      throw new Error('No exported archive is available at that mock path yet.')
    }

    this.store.dashboard = structuredClone(imported)
    recomputeStats(this.store)

    const result: ImportExportSummary = {
      path,
      packsImported: imported.packs.length,
      adaptersTouched: imported.adapters.length,
      revisionId: `import-${this.store.nextRevisionId}`,
      exportedAt: nowIso(),
    }

    return withLatency(result)
  }
}
