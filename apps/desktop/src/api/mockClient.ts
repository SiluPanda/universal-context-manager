import { DesktopApiError } from './errors'
import {
  cloneMockDashboard,
  MOCK_GLOBAL_SCOPE_ID,
  MOCK_PROJECT_SCOPE_ID,
} from './mockData'
import type {
  AdapterStatus,
  BulkReviewDecisionInput,
  BulkReviewDecisionResult,
  BundleImportApplyInput,
  BundleImportPreview,
  CommitDisposition,
  ComposeContextInput,
  ContextEntry,
  ContextPack,
  ContextPreview,
  DaemonControlResult,
  DashboardSnapshot,
  DesktopErrorCode,
  DiagnosticState,
  EntryFormat,
  ForgetScopeInput,
  ForgetScopeResult,
  ImportExportSummary,
  PathGrantPurpose,
  PathGrantSelection,
  ProjectRegistration,
  RestoreRevisionResult,
  ReviewDecisionInput,
  ReviewItem,
  ReviewMode,
  ReviewPolicy,
  ReviewReason,
  RevisionEntry,
  SaveEntryInput,
  SavePackInput,
  SearchResult,
  SetReviewPolicyInput,
  Settings,
  SourceImportApplyInput,
  SourceImportApplyResult,
  SourceImportCandidate,
  SourceImportKind,
  SourceImportPreviewInput,
  SourceImportPreviewResult,
  SpoolRetryResult,
  WorkspaceNode,
} from '../types'

export interface MockDialogSelections {
  projectFolder: string | null
  projectFolders?: Array<string | null>
  sourceFiles: string[]
  bundleFile: string | null
  archiveDestination: string | null
}

interface EntryRevisionRecord {
  revision: RevisionEntry
  snapshot: ContextEntry
}

interface SourcePreviewRecord {
  input: SourceImportPreviewInput
  result: SourceImportPreviewResult
}

interface MockBundleArtifact {
  snapshot: DashboardSnapshot
  preview: BundleImportPreview
}

interface MockPathGrant {
  purpose: PathGrantPurpose
  paths: string[]
  expiresAtMs: number
}

interface MockStore {
  dashboard: DashboardSnapshot
  entryRevisions: EntryRevisionRecord[]
  sourcePreviews: Map<string, SourcePreviewRecord>
  bundleArtifacts: Map<string, MockBundleArtifact>
  pathGrants: Map<string, MockPathGrant>
  onboardingCompleteSetting?: boolean
  onboardingCompletedAtSetting?: string
  nextEntryId: number
  nextPackId: number
  nextRevisionId: number
  nextRequestId: number
  nextGrantId: number
  diagnosticsRefreshes: number
}

const defaultDialogs: MockDialogSelections = {
  projectFolder: '/Users/mock/Atlas',
  sourceFiles: [
    '/Users/mock/Atlas/AGENTS.md',
    '/Users/mock/Atlas/.github/copilot-instructions.md',
  ],
  bundleFile: '/Users/mock/Desktop/ucm-backup.json',
  archiveDestination: '/Users/mock/Desktop/universal-context-manager-backup.json',
}

function clone<T>(value: T): T {
  return structuredClone(value)
}

function nowIso() {
  return new Date().toISOString()
}

function withLatency<T>(value: T): Promise<T> {
  return new Promise((resolve) => {
    window.setTimeout(() => resolve(clone(value)), 8)
  })
}

function fail(code: DesktopErrorCode, message: string, retryable = false): never {
  throw new DesktopApiError({ code, message, retryable })
}

function issuePathGrant(
  store: MockStore,
  purpose: PathGrantPurpose,
  paths: string[],
): PathGrantSelection {
  const grantToken = `mock-path-grant-${store.nextGrantId++}`
  const expiresAtMs = Date.now() + 10 * 60 * 1_000
  store.pathGrants.set(grantToken, {
    purpose,
    paths: [...paths],
    expiresAtMs,
  })
  return {
    grantToken,
    purpose,
    paths: [...paths],
    expiresAt: new Date(expiresAtMs).toISOString(),
  }
}

function consumePathGrant(
  store: MockStore,
  purpose: PathGrantPurpose,
  grantToken: string | undefined,
  paths: string[],
) {
  if (!grantToken?.trim()) {
    fail('path_grant_required', 'Path grant is required for this operation.')
  }
  const grant = store.pathGrants.get(grantToken)
  store.pathGrants.delete(grantToken)
  if (!grant) {
    fail('path_grant_invalid', 'Path grant is invalid, expired, or already used.')
  }
  if (grant.expiresAtMs <= Date.now()) {
    fail('path_grant_expired', 'Path grant has expired.')
  }
  if (grant.purpose !== purpose) {
    fail('path_grant_invalid', 'Path grant does not authorize this operation.')
  }
  if (
    grant.paths.length !== paths.length ||
    grant.paths.some((path, index) => path !== paths[index])
  ) {
    fail('path_grant_invalid', 'Path grant does not match the requested path selection.')
  }
}

function hasPotentialSecret(value: string) {
  return /(sk-[a-z0-9_-]{8,}|xox[baprs]-[a-z0-9-]{8,}|api[_-]?key\s*[:=]|password\s*[:=])/iu.test(
    value,
  )
}

function assertNoSecret(value: string) {
  if (hasPotentialSecret(value)) {
    fail('secret_detected', 'Potential secret detected.')
  }
}

function opaqueFingerprint(value: string) {
  let hash = 2_166_136_261
  for (let index = 0; index < value.length; index += 1) {
    hash ^= value.charCodeAt(index)
    hash = Math.imul(hash, 16_777_619)
  }
  return `mock-${(hash >>> 0).toString(16).padStart(8, '0')}`
}

function mockSha256(value: string) {
  return opaqueFingerprint(value).slice(5).repeat(8)
}

function sourceFileChecksum(input: SourceImportPreviewInput) {
  return opaqueFingerprint(
    JSON.stringify({
      sourceKind: input.sourceKind ?? 'auto',
      paths: input.paths,
    }),
  )
}

function sourcePreviewFingerprint(
  dashboard: DashboardSnapshot,
  input: SourceImportPreviewInput,
  packName: string,
  reviewMode: ReviewMode,
  candidates: SourceImportCandidate[],
) {
  const destinationState = dashboard.entries
    .filter((entry) => entry.scopeId === input.destinationScopeId)
    .map((entry) => ({
      id: entry.id,
      packId: entry.packId,
      key: entry.key,
      revision: entry.revision,
      status: entry.status,
      body: entry.body,
      locked: entry.locked,
    }))
    .sort((left, right) => left.id.localeCompare(right.id))
  const packState = dashboard.packs
    .filter((pack) => pack.scopeId === input.destinationScopeId)
    .map((pack) => ({
      id: pack.id,
      name: pack.name,
      status: pack.status,
      revision: pack.revision,
    }))
    .sort((left, right) => left.id.localeCompare(right.id))
  return opaqueFingerprint(
    JSON.stringify({
      previewId: sourceFileChecksum(input),
      destinationScopeId: input.destinationScopeId,
      packName,
      reviewMode,
      candidates,
      destinationState,
      packState,
    }),
  )
}

function bundlePreviewForSnapshot(
  path: string,
  dashboard: DashboardSnapshot,
  exportedAt: string,
): BundleImportPreview {
  const scopeIds = [
    ...new Set([
      ...dashboard.packs.map((pack) => pack.scopeId),
      ...dashboard.entries.map((entry) => entry.scopeId),
    ]),
  ].sort()
  const payload = JSON.stringify({
    packs: dashboard.packs,
    entries: dashboard.entries,
    reviews: dashboard.reviewQueue,
    runs: dashboard.activity,
  })
  return {
    path,
    applyGrantToken: '',
    format: path.toLocaleLowerCase().endsWith('.md') ? 'ucm_markdown' : 'ucm_json',
    valid: true,
    fileSizeBytes: new TextEncoder().encode(payload).byteLength,
    checksumSha256: mockSha256(payload),
    exportedAt,
    packCount: dashboard.packs.length,
    entryCount: dashboard.entries.length,
    reviewCount: dashboard.reviewQueue.length,
    runCount: dashboard.activity.length,
    scopeIds,
    warnings: [],
    requiresConfirmation: true,
  }
}

function estimateTokens(body: string) {
  return Math.max(1, Math.ceil(body.trim().split(/\s+/u).filter(Boolean).length * 1.35))
}

function scopePath(nodes: WorkspaceNode[], targetId: string): WorkspaceNode[] {
  for (const node of nodes) {
    if (node.id === targetId) {
      return [node]
    }
    const nested = scopePath(node.children, targetId)
    if (nested.length > 0) {
      return [node, ...nested]
    }
  }
  return []
}

function flattenScopes(nodes: WorkspaceNode[]): WorkspaceNode[] {
  return nodes.flatMap((node) => [node, ...flattenScopes(node.children)])
}

function relevantScopes(dashboard: DashboardSnapshot, scopeId: string) {
  const selectedPath = scopePath(dashboard.workspace, scopeId)
  const globals = flattenScopes(dashboard.workspace).filter((scope) => scope.kind === 'global')
  const ordered = [...globals, ...selectedPath.filter((scope) => scope.kind !== 'global')]
  return ordered.filter(
    (scope, index) => ordered.findIndex((candidate) => candidate.id === scope.id) === index,
  )
}

function renderJson(body: string) {
  try {
    return JSON.stringify(JSON.parse(body), null, 2)
  } catch {
    return body
  }
}

function renderEntryBody(format: EntryFormat, body: string) {
  return format === 'json' ? renderJson(body) : body
}

function displayEntryTitle(entry: ContextEntry) {
  return entry.title?.trim() || entry.key
}

function activeComposableEntries(dashboard: DashboardSnapshot) {
  const activePackIds = new Set(
    dashboard.packs
      .filter((pack) => pack.status !== 'draft')
      .map((pack) => pack.id),
  )
  return dashboard.entries.filter(
    (entry) => entry.status === 'active' && activePackIds.has(entry.packId),
  )
}

function scopeHasComposableEntries(dashboard: DashboardSnapshot, scopeId: string) {
  const relevantIds = new Set(relevantScopes(dashboard, scopeId).map((scope) => scope.id))
  return activeComposableEntries(dashboard).some((entry) => relevantIds.has(entry.scopeId))
}

function recalculate(store: MockStore) {
  const { dashboard } = store
  dashboard.stats = {
    activePacks: dashboard.packs.filter((pack) => pack.status === 'active').length,
    pendingReviews: dashboard.reviewQueue.length,
    healthyAdapters: dashboard.adapters.filter((adapter) => adapter.health === 'healthy').length,
    runningAgents: dashboard.activity.filter((run) => run.status === 'running').length,
  }
  const durableContext = activeComposableEntries(dashboard).length > 0
  const inferredReady =
    dashboard.connected &&
    Boolean(dashboard.selectedScopeId) &&
    scopeHasComposableEntries(dashboard, dashboard.selectedScopeId)
  const complete =
    durableContext && (store.onboardingCompleteSetting ?? inferredReady)
  dashboard.onboarding = {
    complete,
    inferred: store.onboardingCompleteSetting === undefined,
    durableContext,
    completedAt: complete ? store.onboardingCompletedAtSetting : undefined,
    lastProjectPath:
      dashboard.onboarding.lastProjectPath ?? dashboard.settings.lastProjectPath,
  }
  dashboard.settings = {
    ...dashboard.settings,
    reviewMode: dashboard.reviewPolicy?.mode ?? dashboard.settings.reviewMode,
    reviewPolicy: dashboard.reviewPolicy,
    onboarding: dashboard.onboarding,
    lastSelectedScopeId: dashboard.selectedScopeId,
  }
  dashboard.privacy = {
    ...dashboard.privacy,
    counts: dashboard.privacy.countsAvailable
      ? {
          packs: dashboard.packs.length,
          entries: dashboard.entries.length,
          reviews: dashboard.reviewQueue.length,
          runs: dashboard.activity.length,
          spoolBacklog: dashboard.diagnostics.spoolBacklog,
        }
      : {
          ...dashboard.privacy.counts,
          spoolBacklog: dashboard.diagnostics.spoolBacklog,
        },
    telemetryEnabled: dashboard.settings.telemetry,
  }
  dashboard.lastSyncAt = nowIso()
}

function normalizeSeed(seed?: DashboardSnapshot): DashboardSnapshot {
  const dashboard = seed ? clone(seed) : cloneMockDashboard()
  dashboard.entries ??= []
  dashboard.reviewPolicy ??= dashboard.settings.reviewPolicy
  dashboard.onboarding ??= dashboard.settings.onboarding
  dashboard.diagnostics ??= cloneMockDashboard().diagnostics
  dashboard.diagnostics.apiVersion ??= null
  dashboard.diagnostics.expectedApiVersion ??= 1
  dashboard.privacy ??= cloneMockDashboard().privacy
  dashboard.privacy.countsAvailable ??= false
  if (!dashboard.privacy.countsAvailable) dashboard.privacy.countsSource = undefined
  dashboard.settings.onboarding ??= dashboard.onboarding
  return dashboard
}

function buildStore(seed?: DashboardSnapshot): MockStore {
  const dashboard = normalizeSeed(seed)
  const entryRevisions = dashboard.revisions.flatMap<EntryRevisionRecord>((revision) => {
    const entry = dashboard.entries.find((candidate) => candidate.id === revision.entityId)
    return entry ? [{ revision: clone(revision), snapshot: clone(entry) }] : []
  })
  const store: MockStore = {
    dashboard,
    entryRevisions,
    sourcePreviews: new Map(),
    bundleArtifacts: new Map(),
    pathGrants: new Map(),
    onboardingCompleteSetting: dashboard.onboarding.inferred
      ? undefined
      : dashboard.onboarding.complete,
    onboardingCompletedAtSetting: dashboard.onboarding.completedAt,
    nextEntryId: 100,
    nextPackId: 100,
    nextRevisionId: 900,
    nextRequestId: 100,
    nextGrantId: 100,
    diagnosticsRefreshes: 0,
  }
  recalculate(store)
  return store
}

function pushEntryRevision(
  store: MockStore,
  snapshot: ContextEntry,
  note: string,
  changeSummary: string,
) {
  const revision: RevisionEntry = {
    id: `rev-mock-${store.nextRevisionId++}`,
    entityId: snapshot.id,
    entityLabel: displayEntryTitle(snapshot),
    author: 'desktop-operator',
    createdAt: nowIso(),
    note,
    changeSummary,
    restorable: true,
  }
  store.dashboard.revisions = [revision, ...store.dashboard.revisions]
  store.entryRevisions = [{ revision, snapshot: clone(snapshot) }, ...store.entryRevisions]
}

function replaceEntry(store: MockStore, entry: ContextEntry) {
  const index = store.dashboard.entries.findIndex((candidate) => candidate.id === entry.id)
  if (index >= 0) {
    store.dashboard.entries[index] = entry
  } else {
    store.dashboard.entries = [entry, ...store.dashboard.entries]
  }

  const pack = store.dashboard.packs.find((candidate) => candidate.id === entry.packId)
  if (pack) {
    const packEntries = store.dashboard.entries.filter(
      (candidate) => candidate.packId === pack.id && candidate.status === 'active',
    )
    pack.updatedAt = entry.updatedAt
    pack.revision += 1
    pack.tokenEstimate = packEntries.reduce(
      (total, candidate) => total + estimateTokens(candidate.renderedBody),
      0,
    )
    pack.body = packEntries.map((candidate) => candidate.renderedBody).join('\n\n')
  }
  recalculate(store)
}

function findPack(
  dashboard: DashboardSnapshot,
  scopeId: string,
  packId?: string,
  packName?: string,
) {
  if (packId) {
    return dashboard.packs.find(
      (candidate) => candidate.id === packId && candidate.scopeId === scopeId,
    )
  }
  if (packName) {
    return dashboard.packs.find(
      (candidate) =>
        candidate.scopeId === scopeId &&
        (candidate.name === packName || candidate.id === packName),
    )
  }
  return undefined
}

function makeEntry(
  store: MockStore,
  input: SaveEntryInput,
  pack: ContextPack,
  existing?: ContextEntry,
): ContextEntry {
  let jsonValue: unknown
  let body = input.body
  if (input.format === 'json') {
    try {
      jsonValue = JSON.parse(input.body)
      body = JSON.stringify(jsonValue, null, 2)
    } catch {
      fail('invalid_input', 'invalid JSON entry body')
    }
  }

  const timestamp = nowIso()
  return {
    id: existing?.id ?? `entry-mock-${store.nextEntryId++}`,
    packId: pack.id,
    packName: pack.name,
    packKey: pack.name.toLocaleLowerCase().replace(/[^a-z0-9]+/gu, '-'),
    scopeId: input.scopeId,
    scopeKind: pack.scopeKind,
    scopeLabel: pack.scopeLabel,
    key: input.key.trim(),
    title: input.title?.trim() || undefined,
    kind: input.kind.trim(),
    format: input.format,
    body,
    renderedBody: renderEntryBody(input.format, body),
    jsonValue,
    tags: [...input.tags],
    locked: input.locked,
    status: 'active',
    provenance: {
      actor: input.actor?.trim() || 'desktop-operator',
      source: 'desktop_editor',
      note: input.note,
    },
    revision: (existing?.revision ?? 0) + 1,
    createdAt: existing?.createdAt ?? timestamp,
    updatedAt: timestamp,
  }
}

function diagnosticRank(state: DiagnosticState) {
  if (state === 'failed') return 6
  if (state === 'incompatible') return 5
  if (state === 'migration_required') return 4
  if (state === 'degraded') return 3
  if (state === 'starting') return 2
  if (state === 'not_installed' || state === 'stopped') return 1
  return 0
}

function restampDiagnostics(store: MockStore) {
  store.diagnosticsRefreshes += 1
  const checkedAt = new Date(Date.now() + store.diagnosticsRefreshes * 1_000).toISOString()
  store.dashboard.diagnostics = {
    ...store.dashboard.diagnostics,
    generatedAt: checkedAt,
    checks: store.dashboard.diagnostics.checks.map((check) => ({ ...check, checkedAt })),
  }
  store.dashboard.diagnostics.overallState =
    store.dashboard.diagnostics.checks
      .filter((check) => check.state !== 'ignored')
      .sort((left, right) => diagnosticRank(right.state) - diagnosticRank(left.state))[0]?.state ??
    'healthy'
  store.dashboard.adapters = store.dashboard.adapters.map((adapter) => ({
    ...adapter,
    lastCheckedAt: checkedAt,
  }))
  recalculate(store)
  return store.dashboard.diagnostics
}

function sourceKindForPath(path: string): SourceImportKind {
  const lower = path.toLocaleLowerCase()
  if (lower.endsWith('agents.md')) return 'agents_md'
  if (lower.endsWith('claude.md')) return 'claude_md'
  if (lower.includes('copilot-instructions')) return 'copilot_instructions'
  if (lower.endsWith('.mdc')) return 'cursor_rule'
  return 'plain_markdown'
}

function importCandidate(
  dashboard: DashboardSnapshot,
  path: string,
  index: number,
  destinationScopeId: string,
  packName: string,
): SourceImportCandidate {
  const kind = sourceKindForPath(path)
  const key =
    kind === 'copilot_instructions'
      ? 'focused-testing'
      : `project-instructions-${index + 1}`
  const title =
    kind === 'copilot_instructions'
      ? 'Focused testing'
      : index === 0
        ? 'Project instructions'
        : 'Imported guidance'
  const renderedBody =
    kind === 'copilot_instructions'
      ? 'Run focused tests first, then lint and build after the targeted checks pass.'
      : 'Use the repository instructions discovered during onboarding and keep changes focused.'
  const tags = ['imported', kind]
  const pack = dashboard.packs.find(
    (candidate) =>
      candidate.scopeId === destinationScopeId && candidate.name === packName,
  )
  const existing = pack
    ? dashboard.entries.find(
        (entry) =>
          entry.scopeId === destinationScopeId &&
          entry.packId === pack.id &&
          entry.key === key,
      )
    : undefined
  const disposition =
    existing?.status === 'active' &&
    existing.renderedBody === renderedBody &&
    existing.title === title &&
    existing.kind === 'instruction' &&
    existing.format === 'markdown' &&
    existing.locked === false &&
    existing.tags.join('\u0000') === tags.join('\u0000')
      ? 'duplicate'
      : existing
        ? 'conflict'
        : 'new'
  return {
    candidateIndex: index,
    documentIndex: index,
    sourcePath: path,
    detectedSourceKind: kind,
    key,
    title,
    kind: 'instruction',
    format: 'markdown',
    renderedBody,
    tags,
    locked: false,
    provenance: {
      actor: 'source-import',
      source: kind,
      sourceRef: path,
    },
    disposition,
    existingEntryId: existing?.id,
    existingRevision: existing?.revision,
    warnings:
      disposition === 'conflict'
        ? ['An entry with this key already exists in the destination pack.']
        : [],
  }
}

function ensureImportPack(store: MockStore, scopeId: string, packName: string) {
  const existing = findPack(store.dashboard, scopeId, undefined, packName)
  if (existing) {
    return existing
  }
  const scope = flattenScopes(store.dashboard.workspace).find((candidate) => candidate.id === scopeId)
  if (!scope) {
    fail('not_found', `Unknown scope: ${scopeId}`)
  }
  const pack: ContextPack = {
    id: `pack-import-${store.nextPackId++}`,
    scopeId,
    scopeKind: scope.kind,
    scopeLabel: scope.label,
    name: packName,
    status: 'active',
    tokenEstimate: 0,
    updatedAt: nowIso(),
    summary: 'Imported instruction sources.',
    tags: ['imported'],
    body: '',
    provenance: ['source-import'],
    revision: 1,
  }
  store.dashboard.packs = [pack, ...store.dashboard.packs]
  return pack
}

function reviewFromCandidate(
  store: MockStore,
  candidate: SourceImportCandidate,
  scopeId: string,
  pack: ContextPack,
  requestId: string,
  reason: ReviewReason,
) {
  const existing = candidate.existingEntryId
    ? store.dashboard.entries.find((entry) => entry.id === candidate.existingEntryId)
    : undefined
  const timestamp = nowIso()
  const review: ReviewItem = {
    id: `review-import-${store.nextRequestId++}`,
    requestId,
    packId: pack.id,
    packName: pack.name,
    scopeId,
    scopeKind: pack.scopeKind,
    scopeLabel: pack.scopeLabel,
    entryKey: candidate.key,
    title: candidate.title ?? candidate.key,
    summary: 'Imported context is waiting for the configured review gate.',
    requestedBy: candidate.provenance?.actor ?? 'source-import',
    requestedAt: timestamp,
    ageSeconds: 0,
    risk: reason === 'conflict' ? 'medium' : 'low',
    reason,
    diff: existing ? 'Existing and proposed content differ.' : 'A new entry is proposed.',
    diffSides: {
      before: existing?.renderedBody,
      after: candidate.renderedBody,
      format: candidate.format,
      changed: existing?.renderedBody !== candidate.renderedBody,
    },
    existingContent: existing?.renderedBody,
    proposedContent: candidate.renderedBody,
    provenance: candidate.provenance,
    source: candidate.provenance?.source ?? 'source-import',
    suggestedEdit: candidate.renderedBody,
  }
  store.dashboard.reviewQueue.push(review)
  return review
}

function applyReview(store: MockStore, review: ReviewItem, editedContent?: string) {
  const body = editedContent ?? review.proposedContent
  assertNoSecret(body)
  const existing = store.dashboard.entries.find(
    (entry) =>
      entry.scopeId === review.scopeId &&
      entry.packId === review.packId &&
      entry.key === review.entryKey,
  )
  const pack = store.dashboard.packs.find((candidate) => candidate.id === review.packId)
  if (!pack) {
    fail('not_found', `Pack not found for review item: ${review.id}`)
  }
  if (existing) {
    pushEntryRevision(store, existing, 'Review approved.', 'Stored the previous entry value.')
  }
  const entry = makeEntry(
    store,
    {
      id: existing?.id,
      scopeId: review.scopeId,
      packId: review.packId,
      key: review.entryKey,
      title: review.title,
      kind: existing?.kind ?? 'instruction',
      format: review.diffSides.format,
      body,
      tags: existing?.tags ?? ['reviewed'],
      locked: existing?.locked ?? false,
      actor: 'review-operator',
      note: 'Approved from the review queue.',
    },
    pack,
    existing,
  )
  replaceEntry(store, entry)
}

export class MockDesktopApi {
  private readonly store: MockStore
  private readonly dialogs: MockDialogSelections
  private readonly projectFolders: Array<string | null>

  constructor(seed?: DashboardSnapshot, dialogs: Partial<MockDialogSelections> = {}) {
    this.store = buildStore(seed)
    this.dialogs = { ...defaultDialogs, ...dialogs }
    this.projectFolders = dialogs.projectFolders
      ? [...dialogs.projectFolders]
      : [this.dialogs.projectFolder]
  }

  async loadDashboard() {
    recalculate(this.store)
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
    assertNoSecret(`${input.name}\n${input.summary}\n${input.body}\n${input.tags.join('\n')}`)
    const scope = flattenScopes(this.store.dashboard.workspace).find(
      (candidate) => candidate.id === input.scopeId,
    )
    if (!scope) {
      fail('not_found', `Unknown scope: ${input.scopeId}`)
    }
    const existing = input.id
      ? this.store.dashboard.packs.find((pack) => pack.id === input.id)
      : undefined
    if (input.id && !existing) {
      fail('not_found', `Unknown pack id: ${input.id}`)
    }
    if (existing && existing.scopeId !== input.scopeId) {
      fail('conflict', 'An existing pack cannot be moved to another scope.')
    }
    const timestamp = nowIso()
    const pack: ContextPack = {
      id: existing?.id ?? `pack-mock-${this.store.nextPackId++}`,
      scopeId: input.scopeId,
      scopeKind: scope.kind,
      scopeLabel: scope.label,
      name: input.name.trim() || 'Manual context',
      status: input.status,
      tokenEstimate: estimateTokens(input.body),
      updatedAt: timestamp,
      summary: input.summary.trim() || 'Manual context entry.',
      tags: [...input.tags],
      body: input.body,
      provenance: existing?.provenance ?? ['desktop/manual'],
      revision: (existing?.revision ?? 0) + 1,
    }
    if (existing) {
      this.store.dashboard.packs = this.store.dashboard.packs.map((candidate) =>
        candidate.id === pack.id ? pack : candidate,
      )
    } else {
      this.store.dashboard.packs = [pack, ...this.store.dashboard.packs]
      const entry = makeEntry(
        this.store,
        {
          scopeId: input.scopeId,
          packId: pack.id,
          key: 'manual-context',
          title: input.name.trim() || 'Manual context',
          kind: 'instruction',
          format: 'markdown',
          body: input.body,
          tags: input.tags,
          locked: false,
          actor: 'desktop-operator',
          note: 'Created during onboarding.',
        },
        pack,
      )
      replaceEntry(this.store, entry)
    }
    recalculate(this.store)
    return withLatency(pack)
  }

  async listEntries(scopeId?: string, packId?: string) {
    const entries = this.store.dashboard.entries.filter(
      (entry) => (!scopeId || entry.scopeId === scopeId) && (!packId || entry.packId === packId),
    )
    return withLatency(
      [...entries].sort(
        (left, right) =>
          right.updatedAt.localeCompare(left.updatedAt) || left.key.localeCompare(right.key),
      ),
    )
  }

  async saveEntry(input: SaveEntryInput) {
    assertNoSecret(
      `${input.key}\n${input.title ?? ''}\n${input.kind}\n${input.body}\n${input.tags.join('\n')}`,
    )
    if (!input.key.trim()) {
      fail('invalid_input', 'entry key must not be empty')
    }
    if (!input.kind.trim()) {
      fail('invalid_input', 'entry kind must not be empty')
    }

    const existing = input.id
      ? this.store.dashboard.entries.find((entry) => entry.id === input.id)
      : undefined
    if (input.id && !existing) {
      fail('not_found', `Unknown entry id: ${input.id}`)
    }
    if (existing && existing.scopeId !== input.scopeId) {
      fail('conflict', 'An existing entry cannot be moved to another scope.')
    }
    if (existing && existing.key !== input.key.trim()) {
      fail('conflict', 'An existing entry key cannot be changed; create a new entry instead.')
    }
    if (existing && input.packId && existing.packId !== input.packId) {
      fail('conflict', 'An existing entry cannot be moved to another pack.')
    }

    let pack = findPack(
      this.store.dashboard,
      input.scopeId,
      existing?.packId ?? input.packId,
      input.packName,
    )
    if (!pack) {
      if (input.packId) {
        fail('not_found', 'Unknown pack for the selected scope.')
      }
      const scope = flattenScopes(this.store.dashboard.workspace).find(
        (candidate) => candidate.id === input.scopeId,
      )
      if (!scope) {
        fail('not_found', `Unknown scope: ${input.scopeId}`)
      }
      const requestedName = input.packName?.trim() || 'default'
      pack = {
        id: `pack-entry-${this.store.nextPackId++}`,
        scopeId: input.scopeId,
        scopeKind: scope.kind,
        scopeLabel: scope.label,
        name: requestedName,
        status: 'active',
        tokenEstimate: 0,
        updatedAt: nowIso(),
        summary: 'Created by the desktop entry editor.',
        tags: [],
        body: '',
        provenance: ['desktop_editor'],
        revision: 1,
      }
      this.store.dashboard.packs = [pack, ...this.store.dashboard.packs]
    }
    const duplicate = this.store.dashboard.entries.find(
      (entry) =>
        entry.id !== existing?.id &&
        entry.scopeId === input.scopeId &&
        entry.packId === pack.id &&
        entry.key === input.key.trim(),
    )
    if (duplicate) {
      fail(
        'conflict',
        `Entry ${input.key.trim()} already exists or is archived in ${pack.name}.`,
      )
    }
    if (existing) {
      pushEntryRevision(
        this.store,
        existing,
        'Saved from the entry editor.',
        'Stored the previous entry value before the update.',
      )
    }
    const entry = makeEntry(this.store, input, pack, existing)
    replaceEntry(this.store, entry)
    return withLatency(entry)
  }

  async archiveEntry(entryId: string) {
    const entry = this.store.dashboard.entries.find((candidate) => candidate.id === entryId)
    if (!entry) {
      fail('not_found', `Unknown entry id: ${entryId}`)
    }
    if (entry.status === 'deleted') {
      return withLatency(entry)
    }
    pushEntryRevision(this.store, entry, 'Archived entry.', 'Stored the active entry before archive.')
    const archived = { ...entry, status: 'deleted' as const, revision: entry.revision + 1, updatedAt: nowIso() }
    replaceEntry(this.store, archived)
    return withLatency(archived)
  }

  deleteEntry(entryId: string) {
    return this.archiveEntry(entryId)
  }

  async restoreEntry(entryId: string) {
    const entry = this.store.dashboard.entries.find((candidate) => candidate.id === entryId)
    if (!entry) {
      fail('not_found', `Unknown entry id: ${entryId}`)
    }
    if (entry.status !== 'deleted') {
      fail(
        'conflict',
        'restore_entry requires an entry whose current status is deleted',
      )
    }
    pushEntryRevision(this.store, entry, 'Restored entry.', 'Stored the archived state before restore.')
    const restored = {
      ...entry,
      status: 'active' as const,
      revision: entry.revision + 1,
      updatedAt: nowIso(),
    }
    replaceEntry(this.store, restored)
    return withLatency(restored)
  }

  async revertEntryRevision(input: { entryId: string; revision?: number; actor?: string }) {
    const current = this.store.dashboard.entries.find((entry) => entry.id === input.entryId)
    if (!current) {
      fail('not_found', `Unknown entry id: ${input.entryId}`)
    }
    const record = this.store.entryRevisions.find(
      (candidate) =>
        candidate.snapshot.id === input.entryId &&
        (input.revision === undefined || candidate.snapshot.revision === input.revision),
    )
    if (
      !record &&
      input.revision !== undefined &&
      input.revision !== Math.max(1, current.revision - 1)
    ) {
      fail('not_found', `Unknown entry revision: ${input.revision}`)
    }
    const snapshot =
      record?.snapshot ??
      (input.revision === Math.max(1, current.revision - 1)
        ? { ...current, revision: input.revision }
        : current)
    pushEntryRevision(this.store, current, 'Reverted entry.', 'Stored the value before revert.')
    const restored = {
      ...clone(snapshot),
      status: 'active' as const,
      revision: current.revision + 1,
      updatedAt: nowIso(),
      provenance: {
        ...snapshot.provenance,
        actor: input.actor?.trim() || 'desktop-operator',
        note: 'Reverted from entry history.',
      },
    }
    replaceEntry(this.store, restored)
    return withLatency(restored)
  }

  composePreview(scopeId: string) {
    return this.composeEffectiveContext({ scopeId, destinationAdapter: 'generic' })
  }

  async composeEffectiveContext(input: ComposeContextInput) {
    const scopes = relevantScopes(this.store.dashboard, input.scopeId)
    if (scopes.length === 0) {
      fail('not_found', `Unknown scope: ${input.scopeId}`)
    }
    const relevantIds = new Set(scopes.map((scope) => scope.id))
    const activePacks = scopes.flatMap((scope) =>
      this.store.dashboard.packs.filter(
        (pack) =>
          relevantIds.has(pack.scopeId) &&
          pack.scopeId === scope.id &&
          pack.status !== 'draft',
      ),
    )
    const sections = activePacks.flatMap((pack) => {
      const included = this.store.dashboard.entries
        .filter((entry) => entry.packId === pack.id && entry.status === 'active')
        .sort((left, right) => left.key.localeCompare(right.key))
      if (included.length === 0) return []
      const body = included
        .map((entry) => `### ${displayEntryTitle(entry)}\n\n${entry.renderedBody}`)
        .join('\n\n')
      return [
        {
          id: `preview:${pack.scopeId}:${pack.id}`,
          order: 0,
          layer: pack.scopeKind,
          title:
            pack.scopeKind === 'global'
              ? 'Global context'
              : pack.scopeKind === 'project'
                ? 'Project context'
                : 'Task context',
          packName: pack.name,
          scopeId: pack.scopeId,
          scopeLabel: pack.scopeLabel,
          scopeKind: pack.scopeKind,
          tokens: estimateTokens(body),
          body,
          entryIds: included.map((entry) => entry.id),
        },
      ]
    })
    sections.forEach((section, index) => {
      section.order = index
    })
    const includedEntries = sections.flatMap((section) =>
      section.entryIds.map((entryId) => {
        const entry = this.store.dashboard.entries.find((candidate) => candidate.id === entryId)!
        return {
          order: 0,
          entryId: entry.id,
          packName: entry.packName,
          scopeId: entry.scopeId,
          scopeKind: entry.scopeKind,
          scopeLabel: entry.scopeLabel,
          key: entry.key,
          title: entry.title,
          kind: entry.kind,
          format: entry.format,
          provenance: entry.provenance,
          revision: entry.revision,
          tokenEstimate: estimateTokens(entry.renderedBody),
        }
      }),
    )
    includedEntries.forEach((entry, index) => {
      entry.order = index
    })
    const exclusions = this.store.dashboard.entries
      .filter((entry) => relevantIds.has(entry.scopeId) && entry.status === 'deleted')
      .map((entry) => ({
        entryId: entry.id,
        scopeId: entry.scopeId,
        scopeKind: entry.scopeKind,
        scopeLabel: entry.scopeLabel,
        packName: entry.packName,
        entryKey: entry.key,
        revision: entry.revision,
        reason: 'deleted_entry' as const,
      }))
    const renderedMarkdown = sections
      .map(
        (section) =>
          `## ${section.title} · ${section.packName}\n\n${section.body}`,
      )
      .join('\n\n')
    const renderedBytes = new TextEncoder().encode(renderedMarkdown).byteLength
    const totalTokens = sections.reduce((total, section) => total + section.tokens, 0)
    const scope = scopes.at(-1)!
    const preview: ContextPreview = {
      scopeId: input.scopeId,
      headline: `${scope.label} composed preview`,
      totalTokens,
      warnings:
        totalTokens > this.store.dashboard.settings.maxPreviewTokens
          ? [
              `Preview exceeds the ${this.store.dashboard.settings.maxPreviewTokens.toLocaleString()} token budget; trim before export.`,
            ]
          : [],
      sections,
      sources: sections.map((section) => ({
        packId:
          this.store.dashboard.packs.find(
            (pack) => pack.scopeId === section.scopeId && pack.name === section.packName,
          )?.id ?? section.id,
        packName: section.packName,
        scopeLabel: section.scopeLabel,
        excerpt: section.body.slice(0, 140),
        tokens: section.tokens,
      })),
      destinationAdapter: input.destinationAdapter?.trim() || 'generic',
      generatedAt: nowIso(),
      renderedMarkdown,
      metrics: {
        renderedBytes,
        estimatedTokens: totalTokens,
        includedEntries: includedEntries.length,
        excludedEntries: exclusions.length,
      },
      exclusions,
      includedEntries,
    }
    return withLatency(preview)
  }

  async searchIndex(query: string) {
    const needle = query.trim().toLocaleLowerCase()
    if (!needle) return withLatency([] as SearchResult[])
    const results: SearchResult[] = []
    for (const entry of this.store.dashboard.entries) {
      if (
        `${entry.title ?? ''} ${entry.key} ${entry.kind} ${entry.body} ${entry.tags.join(' ')}`
          .toLocaleLowerCase()
          .includes(needle)
      ) {
        results.push({
          id: entry.id,
          kind: 'entry',
          title: `${entry.packName} / ${displayEntryTitle(entry)}`,
          excerpt: entry.renderedBody.slice(0, 140),
          scopeLabel: entry.scopeLabel,
          score: 98,
          updatedAt: entry.updatedAt,
          tags: [entry.format, ...entry.tags],
          target: {
            scopeId: entry.scopeId,
            packId: entry.packId,
            entryId: entry.id,
          },
        })
      }
    }
    for (const pack of this.store.dashboard.packs) {
      if (`${pack.name} ${pack.summary} ${pack.body}`.toLocaleLowerCase().includes(needle)) {
        results.push({
          id: pack.id,
          kind: 'pack',
          title: pack.name,
          excerpt: pack.summary,
          scopeLabel: pack.scopeLabel,
          score: 89,
          updatedAt: pack.updatedAt,
          tags: pack.tags,
          target: { scopeId: pack.scopeId, packId: pack.id },
        })
      }
    }
    for (const review of this.store.dashboard.reviewQueue) {
      if (
        `${review.title} ${review.summary} ${review.proposedContent}`
          .toLocaleLowerCase()
          .includes(needle)
      ) {
        results.push({
          id: review.id,
          kind: 'review',
          title: review.title,
          excerpt: review.summary,
          scopeLabel: review.scopeLabel,
          score: 91,
          updatedAt: review.requestedAt,
          tags: [review.risk, review.reason ?? 'review'],
          target: {
            scopeId: review.scopeId,
            packId: review.packId,
            reviewId: review.id,
          },
        })
      }
    }
    for (const revision of this.store.dashboard.revisions) {
      if (
        `${revision.entityLabel} ${revision.note} ${revision.changeSummary}`
          .toLocaleLowerCase()
          .includes(needle)
      ) {
        const entry = this.store.dashboard.entries.find(
          (candidate) => candidate.id === revision.entityId,
        )
        results.push({
          id: revision.id,
          kind: 'revision',
          title: revision.entityLabel,
          excerpt: revision.changeSummary,
          scopeLabel: 'Revision history',
          score: 80,
          updatedAt: revision.createdAt,
          tags: ['revision'],
          target: {
            scopeId: entry?.scopeId,
            packId: entry?.packId,
            revisionId: revision.id,
          },
        })
      }
    }
    for (const run of this.store.dashboard.activity) {
      if (`${run.actor} ${run.summary}`.toLocaleLowerCase().includes(needle)) {
        const referencedEntry = this.store.dashboard.entries.find((entry) =>
          run.contextPackIds.includes(entry.packId),
        )
        results.push({
          id: run.id,
          kind: 'run',
          title: run.summary,
          excerpt: `${run.actor} · ${run.status}`,
          scopeLabel: 'Runs',
          score: 84,
          updatedAt: run.startedAt,
          tags: [run.status],
          target: { scopeId: referencedEntry?.scopeId },
        })
      }
    }
    for (const adapter of this.store.dashboard.adapters) {
      if (`${adapter.name} ${adapter.note}`.toLocaleLowerCase().includes(needle)) {
        results.push({
          id: adapter.id,
          kind: 'adapter',
          title: adapter.name,
          excerpt: adapter.note,
          scopeLabel: 'Connections',
          score: 74,
          updatedAt: adapter.lastCheckedAt,
          tags: [adapter.state, adapter.kind],
          target: { adapterId: adapter.id },
        })
      }
    }
    return withLatency(
      results
        .sort(
          (left, right) =>
            right.score - left.score || right.updatedAt.localeCompare(left.updatedAt),
        )
        .slice(0, 12),
    )
  }

  async listRevisions(entityId?: string) {
    if (!entityId) return withLatency(this.store.dashboard.revisions)
    const packEntryIds = new Set(
      this.store.dashboard.entries
        .filter((entry) => entry.packId === entityId)
        .map((entry) => entry.id),
    )
    return withLatency(
      this.store.dashboard.revisions.filter(
        (revision) => revision.entityId === entityId || packEntryIds.has(revision.entityId),
      ),
    )
  }

  async reviewDecision(input: ReviewDecisionInput) {
    const result = await this.bulkReviewDecision({
      itemIds: [input.itemId],
      decision: input.decision,
      confirmation: false,
      editedContent: input.editedContent,
    })
    const failed = result.results.find((item) => !item.success)
    if (failed?.error) {
      fail(failed.error.code, failed.error.message, failed.error.retryable)
    }
    return withLatency(undefined)
  }

  async bulkReviewDecision(input: BulkReviewDecisionInput) {
    const ids = [...new Set(input.itemIds)].sort()
    if (input.itemIds.length > 1 && !input.confirmation) {
      fail(
        'confirmation_required',
        'Confirmation is required before applying a bulk review decision.',
      )
    }
    if (ids.length === 0) {
      fail('invalid_input', 'review decision requires at least one item')
    }
    if (input.decision === 'edit' && ids.length !== 1) {
      fail('invalid_input', 'edit review decisions are limited to one item')
    }
    const results: BulkReviewDecisionResult['results'] = []
    let stopped = false
    for (const itemId of ids) {
      const review = this.store.dashboard.reviewQueue.find((item) => item.id === itemId)
      if (!review) {
        results.push({
          itemId,
          success: false,
          requiresFollowUp: false,
          error: {
            code: 'not_found',
            message: 'The review item is no longer available.',
            retryable: false,
          },
        })
        stopped = true
        break
      }
      if (review.id === 'review-c-partial-offline') {
        results.push({
          itemId,
          success: false,
          requiresFollowUp: false,
          error: {
            code: 'unavailable',
            message: 'The local adapter is unavailable.',
            retryable: true,
          },
        })
        stopped = true
        break
      }
      if (input.decision === 'approve' || input.decision === 'edit') {
        applyReview(
          this.store,
          review,
          input.decision === 'edit' ? input.editedContent : undefined,
        )
      }
      this.store.dashboard.reviewQueue = this.store.dashboard.reviewQueue.filter(
        (item) => item.id !== itemId,
      )
      results.push({
        itemId,
        success: true,
        requiresFollowUp: false,
        state: input.decision === 'reject' ? 'rejected' : 'approved',
      })
    }
    recalculate(this.store)
    return withLatency({
      decision: input.decision,
      attempted: results.length,
      completed: results.filter((result) => result.success).length,
      stopped,
      results,
    })
  }

  async setReviewPolicy(input: SetReviewPolicyInput) {
    assertNoSecret(`${input.actor}\n${input.note ?? ''}\n${input.requestId ?? ''}`)
    const existing = this.store.dashboard.reviewPolicy
    const unchanged = existing?.mode === input.mode
    const policy: ReviewPolicy = unchanged
      ? existing
      : {
          mode: input.mode,
          metadata: {
            source: 'desktop.governance',
            note: input.note,
            requestId: input.requestId,
          },
          updatedAt: nowIso(),
          updatedBy: input.actor,
          revision: (existing?.revision ?? 0) + 1,
        }
    this.store.dashboard.reviewPolicy = policy
    this.store.dashboard.settings.reviewMode = policy.mode
    this.store.dashboard.settings.reviewPolicy = policy
    recalculate(this.store)
    return withLatency(policy)
  }

  async restoreRevision(revisionId: string) {
    const record = this.store.entryRevisions.find(
      (candidate) => candidate.revision.id === revisionId,
    )
    if (!record) {
      fail('not_found', `Unknown revision: ${revisionId}`)
    }
    const current = this.store.dashboard.entries.find(
      (entry) => entry.id === record.snapshot.id,
    )
    if (current) {
      pushEntryRevision(this.store, current, 'Restored revision.', 'Stored the value before restore.')
    }
    const restored = {
      ...clone(record.snapshot),
      status: 'active' as const,
      revision: (current?.revision ?? record.snapshot.revision) + 1,
      updatedAt: nowIso(),
    }
    replaceEntry(this.store, restored)
    const result: RestoreRevisionResult = {
      revisionId,
      entityId: restored.id,
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
      fail('not_found', `Unknown adapter: ${adapterId}`)
    }
    const next: AdapterStatus = {
      ...adapter,
      enabled,
      state: enabled ? (adapter.state === 'ignored' ? 'degraded' : adapter.state) : 'ignored',
      health: enabled ? (adapter.health === 'offline' ? 'degraded' : adapter.health) : 'offline',
      note: enabled ? adapter.note : 'Ignored by local desktop settings.',
      lastCheckedAt: nowIso(),
    }
    this.store.dashboard.adapters = this.store.dashboard.adapters.map((candidate) =>
      candidate.id === adapterId ? next : candidate,
    )
    recalculate(this.store)
    return withLatency(next)
  }

  async loadDiagnostics() {
    return withLatency(this.store.dashboard.diagnostics)
  }

  async refreshDiagnostics() {
    return withLatency(restampDiagnostics(this.store))
  }

  async startDaemon() {
    const daemon = this.store.dashboard.diagnostics.checks.find(
      (check) => check.id === 'daemon-health',
    )
    const performed = daemon?.state !== 'healthy'
    if (daemon) {
      daemon.state = 'healthy'
      daemon.summary = 'The local context daemon is reachable and schema-compatible.'
    }
    this.store.dashboard.diagnostics.daemonReachable = true
    this.store.dashboard.connected = true
    const diagnostics = restampDiagnostics(this.store)
    const result: DaemonControlResult = {
      action: 'start',
      performed,
      message: performed
        ? 'The daemon was started through the local context client.'
        : 'The daemon was already running; its health was refreshed.',
      diagnostics,
    }
    return withLatency(result)
  }

  async restartDaemon() {
    const diagnostics = restampDiagnostics(this.store)
    return withLatency({
      action: 'restart',
      performed: false,
      message:
        'The existing daemon is healthy. The client does not terminate an unowned process, so no process restart was performed.',
      diagnostics,
    })
  }

  async retrySpool() {
    const attempted = this.store.dashboard.diagnostics.spoolBacklog
    this.store.dashboard.diagnostics.spoolBacklog = 0
    const check = this.store.dashboard.diagnostics.checks.find((item) => item.id === 'spool')
    if (check) {
      check.state = 'healthy'
      check.summary = 'No pending post-work commits are waiting for retry.'
      check.remediation = []
    }
    this.store.dashboard.adapters = this.store.dashboard.adapters.map((adapter) =>
      adapter.id === 'adapter-spool'
        ? { ...adapter, queueDepth: 0, state: 'healthy', health: 'healthy' }
        : adapter,
    )
    const diagnostics = restampDiagnostics(this.store)
    const result: SpoolRetryResult = {
      attempted,
      delivered: attempted,
      retained: 0,
      errors: [],
      diagnostics,
    }
    return withLatency(result)
  }

  async loadSettings() {
    return withLatency(this.store.dashboard.settings)
  }

  async saveSettings(settings: Settings) {
    this.store.dashboard.settings = clone(settings)
    await this.setReviewPolicy({
      mode: settings.reviewMode,
      actor: 'desktop-operator',
      note: 'Saved from desktop settings.',
    })
    recalculate(this.store)
    return withLatency(this.store.dashboard.settings)
  }

  async completeOnboarding() {
    const selectedScopeId = this.store.dashboard.selectedScopeId
    if (!selectedScopeId) {
      fail('invalid_input', 'Onboarding requires a persisted selected project or scope.')
    }
    if (activeComposableEntries(this.store.dashboard).length === 0) {
      fail('invalid_input', 'Onboarding requires at least one active durable context entry.')
    }
    const composed = await this.composeEffectiveContext({ scopeId: selectedScopeId })
    if (composed.metrics.includedEntries === 0) {
      fail(
        'invalid_input',
        'Onboarding requires the selected scope to compose at least one active entry.',
      )
    }
    this.store.onboardingCompleteSetting = true
    this.store.onboardingCompletedAtSetting = nowIso()
    recalculate(this.store)
    return withLatency(this.store.dashboard.onboarding)
  }

  async resetOnboarding() {
    this.store.onboardingCompleteSetting = false
    this.store.onboardingCompletedAtSetting = undefined
    recalculate(this.store)
    return withLatency(this.store.dashboard.onboarding)
  }

  async registerProject(path: string, grantToken: string) {
    consumePathGrant(
      this.store,
      'project_registration',
      grantToken,
      [path],
    )
    if (!path.trim()) {
      fail('invalid_input', 'project path must not be empty')
    }
    if (path.toLocaleLowerCase().includes('denied')) {
      fail('permission_denied', 'permission denied while resolving the project path')
    }
    const normalizedPath = path.trim().replace(/\/+$/u, '')
    const scopeId = `project:${normalizedPath}`
    const existingScope = flattenScopes(this.store.dashboard.workspace).find(
      (scope) => scope.kind === 'project' && scope.id === scopeId,
    )
    const label =
      normalizedPath.split('/').filter(Boolean).at(-1) || 'Selected repository'
    if (!existingScope) {
      this.store.dashboard.workspace.push({
        id: scopeId,
        label,
        kind: 'project',
        description: 'A project registered from the native folder chooser.',
        status: 'Registered',
        children: [],
      })
    }
    const noFiles = normalizedPath.toLocaleLowerCase().includes('no-files')
    const sources = noFiles
      ? []
      : [
          {
            path: `${normalizedPath}/AGENTS.md`,
            relativePath: 'AGENTS.md',
            sourceKind: 'agents_md' as const,
            readable: true,
          },
          {
            path: `${normalizedPath}/.github/copilot-instructions.md`,
            relativePath: '.github/copilot-instructions.md',
            sourceKind: 'copilot_instructions' as const,
            readable: true,
          },
        ]
    this.store.dashboard.selectedScopeId = scopeId
    this.store.dashboard.settings.lastProjectPath = normalizedPath
    this.store.dashboard.settings.lastSelectedScopeId = scopeId
    this.store.dashboard.onboarding.lastProjectPath = normalizedPath
    this.store.dashboard.settings.onboarding = this.store.dashboard.onboarding
    recalculate(this.store)
    const registration: ProjectRegistration = {
      inputPath: path,
      normalizedPath,
      scopeId,
      scopeKind: 'project',
      label: existingScope?.label ?? label,
      instructionSources: sources,
      durable: activeComposableEntries(this.store.dashboard).some(
        (entry) => entry.scopeId === scopeId,
      ),
      selected: true,
    }
    return withLatency(registration)
  }

  async setSelectedScope(scopeId: string, projectPath?: string) {
    const scope = flattenScopes(this.store.dashboard.workspace).find(
      (candidate) => candidate.id === scopeId,
    )
    if (!scope) {
      fail('not_found', `Unknown scope: ${scopeId}`)
    }
    if (
      projectPath &&
      scope.kind === 'project' &&
      scope.id !== `project:${projectPath.trim().replace(/\/+$/u, '')}`
    ) {
      fail('conflict', 'Selected project scope does not match the project path.')
    }
    this.store.dashboard.selectedScopeId = scopeId
    this.store.dashboard.settings.lastSelectedScopeId = scopeId
    if (projectPath) this.store.dashboard.settings.lastProjectPath = projectPath
    recalculate(this.store)
    return withLatency(this.store.dashboard.settings)
  }

  async previewSourceImport(input: SourceImportPreviewInput) {
    consumePathGrant(
      this.store,
      'source_import_preview',
      input.grantToken,
      input.paths,
    )
    if (input.paths.length === 0) {
      fail('invalid_input', 'source import requires at least one path')
    }
    const scope = flattenScopes(this.store.dashboard.workspace).find(
      (candidate) => candidate.id === input.destinationScopeId,
    )
    if (!scope) {
      fail('not_found', `Unknown scope: ${input.destinationScopeId}`)
    }
    const reviewMode =
      this.store.dashboard.reviewPolicy?.mode ?? this.store.dashboard.settings.reviewMode
    const previewId = sourceFileChecksum(input)
    const packName = input.packName?.trim() || 'Imported instructions'
    const candidates = input.paths.map((path, index) =>
      importCandidate(
        this.store.dashboard,
        path,
        index,
        input.destinationScopeId,
        packName,
      ),
    )
    const previewFingerprint = sourcePreviewFingerprint(
      this.store.dashboard,
      input,
      packName,
      reviewMode,
      candidates,
    )
    const result: SourceImportPreviewResult = {
      previewId,
      previewFingerprint,
      applyGrantToken: issuePathGrant(
        this.store,
        'source_import_apply',
        input.paths,
      ).grantToken,
      destinationScopeId: input.destinationScopeId,
      packName,
      reviewMode,
      candidates,
      conflicts: candidates.filter((candidate) => candidate.disposition === 'conflict').length,
      duplicates: candidates.filter((candidate) => candidate.disposition === 'duplicate').length,
      warnings: candidates.some((candidate) => candidate.disposition === 'conflict')
        ? ['Conflicting entries follow the selected review policy.']
        : [],
      applyAllowed: true,
    }
    this.store.sourcePreviews.set(previewId, { input: clone(input), result: clone(result) })
    return withLatency(result)
  }

  async applySourceImport(input: SourceImportApplyInput) {
    if (!input.confirmation) {
      fail('confirmation_required', 'Confirmation is required before applying a source import.')
    }
    if (!input.expectedPreviewFingerprint?.trim()) {
      fail(
        'invalid_input',
        'Source import apply requires expectedPreviewFingerprint from preview.',
      )
    }
    consumePathGrant(
      this.store,
      'source_import_apply',
      input.grantToken,
      input.paths,
    )
    if (sourceFileChecksum(input) !== input.previewId) {
      fail('conflict', 'Source files changed after preview; preview again.')
    }
    const previewRecord = this.store.sourcePreviews.get(input.previewId)
    if (!previewRecord) {
      fail('conflict', 'The source preview is no longer current; preview again.')
    }
    if (
      previewRecord.input.destinationScopeId !== input.destinationScopeId ||
      previewRecord.input.packName !== input.packName ||
      previewRecord.input.sourceKind !== input.sourceKind ||
      previewRecord.input.paths.join('\n') !== input.paths.join('\n')
    ) {
      fail('conflict', 'One or more source files changed after preview; preview again.')
    }
    const currentReviewMode =
      this.store.dashboard.reviewPolicy?.mode ?? this.store.dashboard.settings.reviewMode
    const currentCandidates = input.paths.map((path, index) =>
      importCandidate(
        this.store.dashboard,
        path,
        index,
        input.destinationScopeId,
        previewRecord.result.packName,
      ),
    )
    const currentFingerprint = sourcePreviewFingerprint(
      this.store.dashboard,
      input,
      previewRecord.result.packName,
      currentReviewMode,
      currentCandidates,
    )
    if (
      input.expectedPreviewFingerprint !== previewRecord.result.previewFingerprint ||
      input.expectedPreviewFingerprint !== currentFingerprint
    ) {
      fail(
        'conflict',
        'Source import preview fingerprint no longer matches authoritative state; preview again.',
      )
    }
    const requestId = `request-import-${this.store.nextRequestId++}`
    const pack = ensureImportPack(
      this.store,
      input.destinationScopeId,
      previewRecord.result.packName,
    )
    const mode =
      this.store.dashboard.reviewPolicy?.mode ?? this.store.dashboard.settings.reviewMode
    const items: SourceImportApplyResult['items'] = []
    const affectedEntryIds: string[] = []
    const affectedReviewIds: string[] = []
    const affectedEntryKeys: string[] = []

    for (const candidate of previewRecord.result.candidates) {
      let disposition: CommitDisposition
      let reason: string | undefined
      let entryId: string | undefined
      let reviewId: string | undefined
      if (candidate.disposition === 'duplicate') {
        disposition = 'duplicate'
        entryId = candidate.existingEntryId
      } else {
        const pendingReason =
          pack.scopeKind === 'global'
            ? 'global_scope'
            : mode === 'strict'
              ? candidate.disposition === 'conflict'
                ? 'conflict'
                : 'strict_policy'
              : mode === 'balanced' && candidate.disposition === 'conflict'
                ? 'conflict'
                : undefined
        if (pendingReason) {
          const review = reviewFromCandidate(
            this.store,
            candidate,
            input.destinationScopeId,
            pack,
            requestId,
            pendingReason,
          )
          disposition = 'pending'
          reason = pendingReason
          reviewId = review.id
          affectedReviewIds.push(review.id)
        } else {
          const existing = candidate.existingEntryId
            ? this.store.dashboard.entries.find(
                (entry) => entry.id === candidate.existingEntryId,
              )
            : undefined
          if (existing) {
            pushEntryRevision(
              this.store,
              existing,
              'Updated by source import.',
              'Stored the previous entry value.',
            )
          }
          const entry = makeEntry(
            this.store,
            {
              id: existing?.id,
              scopeId: input.destinationScopeId,
              packId: pack.id,
              key: candidate.key,
              title: candidate.title,
              kind: candidate.kind,
              format: candidate.format,
              body: candidate.renderedBody,
              tags: candidate.tags,
              locked: candidate.locked,
              actor: input.actor ?? 'source-import',
              note: 'Imported from a detected instruction source.',
            },
            pack,
            existing,
          )
          replaceEntry(this.store, entry)
          disposition = 'applied'
          entryId = entry.id
          affectedEntryIds.push(entry.id)
        }
      }
      affectedEntryKeys.push(candidate.key)
      items.push({
        candidateIndex: candidate.candidateIndex,
        documentIndex: candidate.documentIndex,
        sourcePath: candidate.sourcePath,
        entryKey: candidate.key,
        disposition,
        reason,
        entryId,
        reviewId,
      })
    }
    this.store.dashboard.selectedScopeId = input.destinationScopeId
    recalculate(this.store)
    const result: SourceImportApplyResult = {
      requestId,
      destinationScopeId: input.destinationScopeId,
      packName: pack.name,
      navigationScopeId: input.destinationScopeId,
      candidateCount: items.length,
      importedCount: items.filter((item) => item.disposition !== 'duplicate').length,
      appliedCount: items.filter((item) => item.disposition === 'applied').length,
      pendingCount: items.filter((item) => item.disposition === 'pending').length,
      skippedCount: items.filter((item) => item.disposition === 'duplicate').length,
      rejectedCount: items.filter((item) => item.disposition === 'rejected').length,
      items,
      affectedEntryIds,
      affectedReviewIds,
      affectedEntryKeys,
    }
    return withLatency(result)
  }

  async previewBundleImport(path: string, grantToken: string) {
    consumePathGrant(
      this.store,
      'bundle_import_preview',
      grantToken,
      [path],
    )
    if (!path.trim() || path.toLocaleLowerCase().includes('invalid')) {
      fail('invalid_import', 'The selected UCM bundle is invalid or unsupported.')
    }
    if (path.toLocaleLowerCase().includes('secret')) {
      fail('secret_detected', 'Potential secret detected.')
    }
    const artifact = this.store.bundleArtifacts.get(path)
    if (artifact) {
      return withLatency({
        ...artifact.preview,
        applyGrantToken: issuePathGrant(
          this.store,
          'bundle_import_apply',
          [path],
        ).grantToken,
      })
    }
    const preview: BundleImportPreview = {
      path,
      applyGrantToken: issuePathGrant(
        this.store,
        'bundle_import_apply',
        [path],
      ).grantToken,
      format: path.toLocaleLowerCase().endsWith('.md') ? 'ucm_markdown' : 'ucm_json',
      valid: true,
      fileSizeBytes: 4_096,
      checksumSha256: mockSha256(path),
      exportedAt: '2026-08-28T20:00:00Z',
      packCount: 2,
      entryCount: 4,
      reviewCount: 1,
      runCount: 2,
      scopeIds: [MOCK_GLOBAL_SCOPE_ID, MOCK_PROJECT_SCOPE_ID],
      warnings: ['Existing entry keys may be reviewed according to the current policy.'],
      requiresConfirmation: true,
    }
    return withLatency(preview)
  }

  async applyBundleImport(input: BundleImportApplyInput) {
    if (!input.confirmation) {
      fail('confirmation_required', 'Confirmation is required before importing a UCM bundle.')
    }
    consumePathGrant(
      this.store,
      'bundle_import_apply',
      input.grantToken,
      [input.path],
    )
    const artifact = this.store.bundleArtifacts.get(input.path)
    const expectedPreview =
      artifact?.preview ??
      {
        checksumSha256: mockSha256(input.path),
        valid: !input.path.toLocaleLowerCase().includes('invalid'),
      }
    if (expectedPreview.checksumSha256 !== input.checksumSha256) {
      fail('conflict', 'The UCM bundle changed after validation; preview it again.')
    }
    if (!expectedPreview.valid) {
      fail('invalid_import', 'The selected UCM bundle is not valid.')
    }
    let packsImported = 0
    if (artifact) {
      for (const pack of artifact.snapshot.packs) {
        if (!this.store.dashboard.packs.some((candidate) => candidate.id === pack.id)) {
          this.store.dashboard.packs.push(clone(pack))
          packsImported += 1
        }
      }
      for (const entry of artifact.snapshot.entries) {
        if (!this.store.dashboard.entries.some((candidate) => candidate.id === entry.id)) {
          this.store.dashboard.entries.push(clone(entry))
        }
      }
      for (const review of artifact.snapshot.reviewQueue) {
        if (!this.store.dashboard.reviewQueue.some((candidate) => candidate.id === review.id)) {
          this.store.dashboard.reviewQueue.push(clone(review))
        }
      }
      for (const run of artifact.snapshot.activity) {
        if (!this.store.dashboard.activity.some((candidate) => candidate.id === run.id)) {
          this.store.dashboard.activity.push(clone(run))
        }
      }
    } else {
      const existingPack = findPack(
        this.store.dashboard,
        MOCK_PROJECT_SCOPE_ID,
        undefined,
        'Imported backup',
      )
      const pack = ensureImportPack(this.store, MOCK_PROJECT_SCOPE_ID, 'Imported backup')
      if (!existingPack) packsImported = 1
      if (!this.store.dashboard.entries.some((entry) => entry.packId === pack.id)) {
        const entry = makeEntry(
          this.store,
          {
            scopeId: pack.scopeId,
            packId: pack.id,
            key: 'imported-backup-context',
            title: 'Imported backup context',
            kind: 'instruction',
            format: 'markdown',
            body: 'Context restored from the validated local UCM bundle.',
            tags: ['backup', 'imported'],
            locked: false,
            actor: 'bundle-import',
          },
          pack,
        )
        replaceEntry(this.store, entry)
      }
    }
    const summary: ImportExportSummary = {
      path: input.path,
      packsImported,
      adaptersTouched: 0,
      revisionId: `import-${this.store.nextRevisionId++}`,
      exportedAt: nowIso(),
    }
    recalculate(this.store)
    return withLatency(summary)
  }

  async loadPrivacySummary() {
    recalculate(this.store)
    return withLatency(this.store.dashboard.privacy)
  }

  private archiveScopeInternal(input: ForgetScopeInput): ForgetScopeResult {
    if (!input.confirmation) {
      fail('confirmation_required', 'Confirmation is required before archiving scoped context.')
    }
    const target = flattenScopes(this.store.dashboard.workspace).find(
      (scope) => scope.id === input.scopeId,
    )
    if (!target) {
      fail('not_found', `Unknown scope: ${input.scopeId}`)
    }
    const ids = new Set([target.id])
    if (target.kind === 'project') {
      for (const child of flattenScopes(target.children)) ids.add(child.id)
    }
    const matchedPacks = this.store.dashboard.packs.filter((pack) => ids.has(pack.scopeId))
    const transitioningPackIds = new Set(
      matchedPacks.filter((pack) => pack.status !== 'draft').map((pack) => pack.id),
    )
    const alreadyArchived = matchedPacks.filter((pack) => pack.status === 'draft').length
    const entriesAffected = this.store.dashboard.entries.filter(
      (entry) =>
        entry.status === 'active' && transitioningPackIds.has(entry.packId),
    ).length
    this.store.dashboard.packs = this.store.dashboard.packs.map((pack) =>
      transitioningPackIds.has(pack.id)
        ? { ...pack, status: 'draft', updatedAt: nowIso(), revision: pack.revision + 1 }
        : pack,
    )
    recalculate(this.store)
    return {
      scopeId: input.scopeId,
      scopesMatched: ids.size,
      packsArchived: transitioningPackIds.size,
      packsAlreadyArchived: alreadyArchived,
      entriesAffected,
      reversible: true,
      stopped: false,
      failures: [],
    }
  }

  async forgetScope(input: ForgetScopeInput) {
    return withLatency(this.archiveScopeInternal(input))
  }

  async archiveScope(input: ForgetScopeInput) {
    return withLatency(this.archiveScopeInternal(input))
  }

  async exportArchive(path: string, grantToken: string) {
    consumePathGrant(
      this.store,
      'export_archive',
      grantToken,
      [path],
    )
    if (!path.trim()) {
      fail('invalid_input', 'An export path is required.')
    }
    const exportedAt = nowIso()
    const snapshot = clone(this.store.dashboard)
    snapshot.entries = snapshot.entries.filter((entry) => entry.status === 'active')
    const preview = bundlePreviewForSnapshot(path, snapshot, exportedAt)
    this.store.bundleArtifacts.set(path, { snapshot, preview })
    return withLatency({
      path,
      packsImported: this.store.dashboard.packs.length,
      adaptersTouched: 0,
      revisionId: `export-${this.store.nextRevisionId++}`,
      exportedAt,
    })
  }

  selectProjectDirectory() {
    const selected =
      this.projectFolders.length > 1
        ? this.projectFolders.shift() ?? null
        : this.projectFolders[0] ?? null
    return withLatency(
      selected
        ? issuePathGrant(this.store, 'project_registration', [selected])
        : null,
    )
  }

  selectSourceImportFiles() {
    return withLatency(
      this.dialogs.sourceFiles.length > 0
        ? issuePathGrant(this.store, 'source_import_preview', this.dialogs.sourceFiles)
        : null,
    )
  }

  selectBundleImportFile() {
    return withLatency(
      this.dialogs.bundleFile
        ? issuePathGrant(this.store, 'bundle_import_preview', [this.dialogs.bundleFile])
        : null,
    )
  }

  selectExportDestination() {
    return withLatency(
      this.dialogs.archiveDestination
        ? issuePathGrant(this.store, 'export_archive', [this.dialogs.archiveDestination])
        : null,
    )
  }
}
