import { describe, expect, it, vi } from 'vitest'
import {
  createDesktopApi,
  DesktopApiError,
  friendlyDesktopError,
  type DesktopApi,
} from './desktopApi'
import { cloneMockDashboard, MOCK_PROJECT_SCOPE_ID } from './mockData'

async function requireSourceGrant(api: DesktopApi) {
  const grant = await api.selectSourceImportFiles()
  expect(grant).not.toBeNull()
  return grant!
}

async function requireBundleGrant(api: DesktopApi) {
  const grant = await api.selectBundleImportFile()
  expect(grant).not.toBeNull()
  return grant!
}

async function requireProjectGrant(api: DesktopApi) {
  const grant = await api.selectProjectDirectory()
  expect(grant).not.toBeNull()
  return grant!
}

async function requireExportGrant(api: DesktopApi) {
  const grant = await api.selectExportDestination()
  expect(grant).not.toBeNull()
  return grant!
}

describe('DesktopApi mock/live contract parity', () => {
  it('mutates one entry without changing a sibling in the same pack', async () => {
    const api = createDesktopApi({ forceMock: true })
    const before = await api.listEntries(MOCK_PROJECT_SCOPE_ID, 'pack-project-workflow')
    const target = before.find((entry) => entry.id === 'entry-project-testing')!
    const sibling = before.find((entry) => entry.id === 'entry-project-tools')!

    await api.saveEntry({
      id: target.id,
      scopeId: target.scopeId,
      packId: target.packId,
      key: target.key,
      title: target.title,
      kind: target.kind,
      format: target.format,
      body: 'Updated only the Markdown testing entry.',
      tags: target.tags,
      locked: target.locked,
    })

    const after = await api.listEntries(MOCK_PROJECT_SCOPE_ID, 'pack-project-workflow')
    expect(after.find((entry) => entry.id === target.id)?.body).toBe(
      'Updated only the Markdown testing entry.',
    )
    expect(after.find((entry) => entry.id === sibling.id)?.body).toBe(sibling.body)
  })

  it('returns typed secret errors without echoing content and preserves stored data', async () => {
    const api = createDesktopApi({ forceMock: true })
    const entry = (await api.listEntries()).find(
      (candidate) => candidate.id === 'entry-project-testing',
    )!

    await expect(
      api.saveEntry({
        id: entry.id,
        scopeId: entry.scopeId,
        packId: entry.packId,
        key: entry.key,
        title: entry.title,
        kind: entry.kind,
        format: entry.format,
        body: 'api_key = sk-examplecredential',
        tags: entry.tags,
        locked: entry.locked,
      }),
    ).rejects.toMatchObject({ code: 'secret_detected' })

    const stored = (await api.listEntries()).find((candidate) => candidate.id === entry.id)
    expect(stored?.body).toBe(entry.body)
    expect(friendlyDesktopError(new DesktopApiError({
      code: 'secret_detected',
      message: 'hidden content',
      retryable: false,
    }))).toMatch(/Nothing was stored.*secret manager/i)
  })

  it('reports truthful partial bulk results and leaves the failed item queued', async () => {
    const api = createDesktopApi({ forceMock: true })
    await expect(
      api.bulkReviewDecision({
        itemIds: ['review-a-conflict', 'review-b-strict'],
        decision: 'approve',
        confirmation: false,
      }),
    ).rejects.toMatchObject({ code: 'confirmation_required' })

    const result = await api.bulkReviewDecision({
      itemIds: ['review-a-conflict', 'review-b-strict', 'review-c-partial-offline'],
      decision: 'approve',
      confirmation: true,
    })

    expect(result).toMatchObject({
      attempted: 3,
      completed: 2,
      stopped: true,
    })
    expect(result.results.map((item) => item.success)).toEqual([true, true, false])
    expect((await api.loadDashboard()).reviewQueue.map((item) => item.id)).toContain(
      'review-c-partial-offline',
    )
  })

  it('provides deterministic native-dialog substitutes and persists policy', async () => {
    const api = createDesktopApi({
      forceMock: true,
      dialogs: {
        projectFolder: '/Users/mock/No-Files',
        sourceFiles: ['/Users/mock/No-Files/CLAUDE.md'],
        bundleFile: '/Users/mock/Desktop/backup.json',
        archiveDestination: '/Users/mock/Desktop/export.json',
      },
    })

    expect(await api.selectProjectDirectory()).toMatchObject({
      purpose: 'project_registration',
      paths: ['/Users/mock/No-Files'],
    })
    expect(await api.selectSourceImportFiles()).toMatchObject({
      purpose: 'source_import_preview',
      paths: ['/Users/mock/No-Files/CLAUDE.md'],
    })
    expect(await api.selectBundleImportFile()).toMatchObject({
      purpose: 'bundle_import_preview',
      paths: ['/Users/mock/Desktop/backup.json'],
    })
    expect(await api.selectExportDestination()).toMatchObject({
      purpose: 'export_archive',
      paths: ['/Users/mock/Desktop/export.json'],
    })

    await api.setReviewPolicy({ mode: 'fast', actor: 'tester' })
    expect((await api.loadSettings()).reviewMode).toBe('fast')

    const [result] = await api.searchIndex('tool preferences')
    expect(result).toMatchObject({
      kind: 'entry',
      target: {
        scopeId: MOCK_PROJECT_SCOPE_ID,
        packId: 'pack-project-workflow',
        entryId: 'entry-project-tools',
      },
    })

    await api.setReviewPolicy({ mode: 'strict', actor: 'tester' })
    const missingGrant = await requireSourceGrant(api)
    const importInput = {
      paths: missingGrant.paths,
      grantToken: missingGrant.grantToken,
      destinationScopeId: MOCK_PROJECT_SCOPE_ID,
      packName: 'Policy-bound preview',
      sourceKind: 'auto' as const,
    }
    const preview = await api.previewSourceImport(importInput)
    expect(preview.previewFingerprint).toMatch(/^mock-/)
    await expect(
      api.applySourceImport({
        ...importInput,
        previewId: preview.previewId,
        grantToken: preview.applyGrantToken,
        expectedPreviewFingerprint: '',
        confirmation: true,
      }),
    ).rejects.toMatchObject({ code: 'invalid_input' })
    const staleSelection = await requireSourceGrant(api)
    const staleInput = {
      ...importInput,
      paths: staleSelection.paths,
      grantToken: staleSelection.grantToken,
    }
    const stalePreview = await api.previewSourceImport(staleInput)
    await api.setReviewPolicy({ mode: 'balanced', actor: 'tester' })
    await expect(
      api.applySourceImport({
        ...staleInput,
        grantToken: stalePreview.applyGrantToken,
        previewId: stalePreview.previewId,
        expectedPreviewFingerprint: stalePreview.previewFingerprint,
        confirmation: true,
      }),
    ).rejects.toMatchObject({ code: 'conflict' })
  })

  it('enforces one-time path grants across replay, mismatch, purpose, and expiry errors', async () => {
    const api = createDesktopApi({
      forceMock: true,
      dialogs: {
        projectFolders: ['/Users/mock/Atlas', '/Users/mock/Atlas'],
        sourceFiles: ['/Users/mock/Atlas/AGENTS.md'],
      },
    })
    const mismatchGrant = await requireProjectGrant(api)
    await expect(
      api.registerProject('/Users/mock/Other', mismatchGrant.grantToken),
    ).rejects.toMatchObject({ code: 'path_grant_invalid' })
    await expect(
      api.registerProject('/Users/mock/Atlas', mismatchGrant.grantToken),
    ).rejects.toMatchObject({ code: 'path_grant_invalid' })
    await expect(api.registerProject('/Users/mock/Atlas', '')).rejects.toMatchObject({
      code: 'path_grant_required',
    })

    const purposeGrant = await requireProjectGrant(api)
    await expect(
      api.previewBundleImport('/Users/mock/Atlas', purposeGrant.grantToken),
    ).rejects.toMatchObject({ code: 'path_grant_invalid' })

    const previewGrant = await requireSourceGrant(api)
    const input = {
      paths: previewGrant.paths,
      grantToken: previewGrant.grantToken,
      destinationScopeId: MOCK_PROJECT_SCOPE_ID,
      packName: 'Grant tests',
      sourceKind: 'auto' as const,
    }
    const preview = await api.previewSourceImport(input)
    await expect(api.previewSourceImport(input)).rejects.toMatchObject({
      code: 'path_grant_invalid',
    })
    await expect(
      api.applySourceImport({
        ...input,
        paths: ['/Users/mock/Atlas/other.md'],
        grantToken: preview.applyGrantToken,
        previewId: preview.previewId,
        expectedPreviewFingerprint: preview.previewFingerprint,
        confirmation: true,
      }),
    ).rejects.toMatchObject({ code: 'path_grant_invalid' })
    await expect(
      api.applySourceImport({
        ...input,
        grantToken: preview.applyGrantToken,
        previewId: preview.previewId,
        expectedPreviewFingerprint: preview.previewFingerprint,
        confirmation: true,
      }),
    ).rejects.toMatchObject({ code: 'path_grant_invalid' })

    const expiringGrant = await requireProjectGrant(api)
    vi.useFakeTimers()
    vi.setSystemTime(Date.now() + 11 * 60 * 1_000)
    await expect(
      api.registerProject(expiringGrant.paths[0], expiringGrant.grantToken),
    ).rejects.toMatchObject({ code: 'path_grant_expired' })
    vi.useRealTimers()

    expect(
      friendlyDesktopError(
        new DesktopApiError({
          code: 'path_grant_invalid',
          message: 'hidden token',
          retryable: false,
        }),
      ),
    ).toMatch(/one-time path authorization.*choose the path again/i)
  })

  it('invalidates source fingerprints when destination state changes', async () => {
    const api = createDesktopApi({
      forceMock: true,
      dialogs: { sourceFiles: ['/Users/mock/Atlas/AGENTS.md'] },
    })
    const selection = await requireSourceGrant(api)
    const importInput = {
      paths: selection.paths,
      grantToken: selection.grantToken,
      destinationScopeId: MOCK_PROJECT_SCOPE_ID,
      packName: 'State-bound preview',
      sourceKind: 'auto' as const,
    }
    const preview = await api.previewSourceImport(importInput)
    const entry = (await api.listEntries()).find(
      (candidate) => candidate.id === 'entry-project-testing',
    )!
    await api.saveEntry({
      id: entry.id,
      scopeId: entry.scopeId,
      packId: entry.packId,
      key: entry.key,
      title: entry.title,
      kind: entry.kind,
      format: entry.format,
      body: `${entry.body}\n\nConcurrent destination update.`,
      tags: entry.tags,
      locked: entry.locked,
    })

    await expect(
      api.applySourceImport({
        ...importInput,
        grantToken: preview.applyGrantToken,
        previewId: preview.previewId,
        expectedPreviewFingerprint: preview.previewFingerprint,
        confirmation: true,
      }),
    ).rejects.toMatchObject({ code: 'conflict' })
  })

  it('exposes only the guarded bundle import path', async () => {
    const api = createDesktopApi({ forceMock: true })
    expect('importArchive' in api).toBe(false)
    const selection = await requireBundleGrant(api)
    const preview = await api.previewBundleImport(
      selection.paths[0],
      selection.grantToken,
    )

    await expect(
      api.applyBundleImport({
        path: preview.path,
        grantToken: preview.applyGrantToken,
        checksumSha256: preview.checksumSha256,
        confirmation: false,
      }),
    ).rejects.toMatchObject({ code: 'confirmation_required' })

    await expect(
      api.applyBundleImport({
        path: preview.path,
        grantToken: preview.applyGrantToken,
        checksumSha256: preview.checksumSha256,
        confirmation: true,
      }),
    ).resolves.toMatchObject({ path: preview.path })
  })

  it('returns atomic edit approval without follow-up and restores only deleted entries', async () => {
    const api = createDesktopApi({ forceMock: true })
    const edit = await api.bulkReviewDecision({
      itemIds: ['review-a-conflict'],
      decision: 'edit',
      confirmation: false,
      editedContent: 'Atomically edited and approved content.',
    })
    expect(edit.results[0]).toMatchObject({
      success: true,
      state: 'approved',
      requiresFollowUp: false,
    })

    await expect(api.restoreEntry('entry-project-testing')).rejects.toMatchObject({
      code: 'conflict',
    })
    const restored = await api.restoreEntry('entry-project-retired')
    expect(restored.status).toBe('active')
  })

  it('mirrors inferred onboarding readiness and excludes archived packs', async () => {
    const inferred = cloneMockDashboard()
    inferred.onboarding = {
      complete: false,
      inferred: true,
      durableContext: true,
      lastProjectPath: '/Users/mock/Atlas',
    }
    inferred.settings.onboarding = inferred.onboarding
    const inferredApi = createDesktopApi({ forceMock: true, seed: inferred })
    expect((await inferredApi.loadDashboard()).onboarding).toMatchObject({
      complete: true,
      inferred: true,
      durableContext: true,
    })

    const offline = cloneMockDashboard()
    offline.connected = false
    offline.onboarding = {
      complete: false,
      inferred: true,
      durableContext: true,
    }
    offline.settings.onboarding = offline.onboarding
    const offlineApi = createDesktopApi({ forceMock: true, seed: offline })
    expect((await offlineApi.loadDashboard()).onboarding).toMatchObject({
      complete: false,
      inferred: true,
      durableContext: true,
    })

    const archived = cloneMockDashboard()
    archived.packs = archived.packs.map((pack) => ({ ...pack, status: 'draft' }))
    archived.onboarding = {
      complete: true,
      inferred: true,
      durableContext: true,
    }
    archived.settings.onboarding = archived.onboarding
    const archivedApi = createDesktopApi({ forceMock: true, seed: archived })
    expect((await archivedApi.loadDashboard()).onboarding).toMatchObject({
      complete: false,
      inferred: true,
      durableContext: false,
    })
  })

  it('uses current strict settings and scope-bound conflict lookup for imports', async () => {
    const strictSeed = cloneMockDashboard()
    strictSeed.reviewPolicy = undefined
    strictSeed.settings.reviewPolicy = undefined
    strictSeed.settings.reviewMode = 'strict'
    const strictApi = createDesktopApi({
      forceMock: true,
      seed: strictSeed,
      dialogs: { sourceFiles: ['/Users/mock/Atlas/AGENTS.md'] },
    })
    const strictGrant = await requireSourceGrant(strictApi)
    const strictInput = {
      paths: strictGrant.paths,
      grantToken: strictGrant.grantToken,
      destinationScopeId: MOCK_PROJECT_SCOPE_ID,
      packName: 'New strict pack',
      sourceKind: 'auto' as const,
    }
    const strictPreview = await strictApi.previewSourceImport(strictInput)
    const strictResult = await strictApi.applySourceImport({
      ...strictInput,
      grantToken: strictPreview.applyGrantToken,
      previewId: strictPreview.previewId,
      expectedPreviewFingerprint: strictPreview.previewFingerprint,
      confirmation: true,
    })
    expect(strictResult).toMatchObject({ appliedCount: 0, pendingCount: 1 })

    const api = createDesktopApi({
      forceMock: true,
      dialogs: {
        sourceFiles: ['/Users/mock/Atlas/.github/copilot-instructions.md'],
      },
    })
    const atlasGrant = await requireSourceGrant(api)
    const atlasPreview = await api.previewSourceImport({
      paths: atlasGrant.paths,
      grantToken: atlasGrant.grantToken,
      destinationScopeId: MOCK_PROJECT_SCOPE_ID,
      packName: 'Repository workflow',
      sourceKind: 'auto',
    })
    expect(atlasPreview.candidates[0]).toMatchObject({
      disposition: 'conflict',
      existingEntryId: 'entry-project-testing',
    })
    const existing = (await api.listEntries()).find(
      (entry) => entry.id === 'entry-project-testing',
    )!
    await api.saveEntry({
      id: existing.id,
      scopeId: existing.scopeId,
      packId: existing.packId,
      key: existing.key,
      title: existing.title,
      kind: existing.kind,
      format: existing.format,
      body: 'Run focused tests first, then lint and build after the targeted checks pass.',
      tags: ['imported', 'copilot_instructions'],
      locked: existing.locked,
    })
    const duplicateGrant = await requireSourceGrant(api)
    const duplicatePreview = await api.previewSourceImport({
      paths: duplicateGrant.paths,
      grantToken: duplicateGrant.grantToken,
      destinationScopeId: MOCK_PROJECT_SCOPE_ID,
      packName: 'Repository workflow',
      sourceKind: 'auto',
    })
    expect(duplicatePreview.candidates[0].disposition).toBe('duplicate')
    const duplicateResult = await api.applySourceImport({
      paths: duplicateGrant.paths,
      grantToken: duplicatePreview.applyGrantToken,
      destinationScopeId: MOCK_PROJECT_SCOPE_ID,
      packName: 'Repository workflow',
      sourceKind: 'auto',
      previewId: duplicatePreview.previewId,
      expectedPreviewFingerprint: duplicatePreview.previewFingerprint,
      confirmation: true,
    })
    expect(duplicateResult).toMatchObject({
      importedCount: 0,
      skippedCount: 1,
      appliedCount: 0,
      pendingCount: 0,
    })

    const otherApi = createDesktopApi({
      forceMock: true,
      dialogs: {
        projectFolder: '/Users/mock/Other',
        sourceFiles: ['/Users/mock/Other/.github/copilot-instructions.md'],
      },
    })
    const otherProjectGrant = await requireProjectGrant(otherApi)
    const other = await otherApi.registerProject(
      otherProjectGrant.paths[0],
      otherProjectGrant.grantToken,
    )
    const otherSourceGrant = await requireSourceGrant(otherApi)
    const otherPreview = await otherApi.previewSourceImport({
      paths: otherSourceGrant.paths,
      grantToken: otherSourceGrant.grantToken,
      destinationScopeId: other.scopeId,
      packName: 'Repository workflow',
      sourceKind: 'auto',
    })
    expect(otherPreview.candidates[0]).toMatchObject({ disposition: 'new' })
    expect(otherPreview.candidates[0].existingEntryId).toBeUndefined()
  })

  it('registers normalized project paths without aliasing Atlas', async () => {
    const api = createDesktopApi({
      forceMock: true,
      dialogs: {
        projectFolders: ['/Users/mock/Atlas', '/Users/mock/Other'],
      },
    })
    const atlasGrant = await requireProjectGrant(api)
    const atlas = await api.registerProject(
      atlasGrant.paths[0],
      atlasGrant.grantToken,
    )
    const otherGrant = await requireProjectGrant(api)
    const other = await api.registerProject(
      otherGrant.paths[0],
      otherGrant.grantToken,
    )

    expect(atlas.scopeId).toBe('project:/Users/mock/Atlas')
    expect(other.scopeId).toBe('project:/Users/mock/Other')
    expect(other.scopeId).not.toBe(atlas.scopeId)
    expect(other.label).toBe('Other')
  })

  it('reports project durability only for active entries in composable packs', async () => {
    const archived = cloneMockDashboard()
    archived.packs = archived.packs.map((pack) =>
      pack.scopeId === MOCK_PROJECT_SCOPE_ID
        ? { ...pack, status: 'draft' }
        : pack,
    )
    archived.entries = archived.entries.map((entry) =>
      entry.scopeId === MOCK_PROJECT_SCOPE_ID
        ? { ...entry, status: 'deleted' }
        : entry,
    )
    const archivedApi = createDesktopApi({ forceMock: true, seed: archived })
    const archivedGrant = await requireProjectGrant(archivedApi)
    expect(
      (
        await archivedApi.registerProject(
          archivedGrant.paths[0],
          archivedGrant.grantToken,
        )
      ).durable,
    ).toBe(false)

    const activeApi = createDesktopApi({ forceMock: true })
    const activeGrant = await requireProjectGrant(activeApi)
    expect(
      (
        await activeApi.registerProject(
          activeGrant.paths[0],
          activeGrant.grantToken,
        )
      ).durable,
    ).toBe(true)
  })

  it('keeps mock export/import and archive summaries truthful', async () => {
    const path = '/Users/mock/Desktop/round-trip.json'
    const api = createDesktopApi({
      forceMock: true,
      dialogs: {
        archiveDestination: path,
        bundleFile: path,
      },
    })
    const before = await api.loadDashboard()
    const exportGrant = await requireExportGrant(api)
    await api.exportArchive(path, exportGrant.grantToken)
    const previewGrant = await requireBundleGrant(api)
    const preview = await api.previewBundleImport(path, previewGrant.grantToken)
    expect(preview).toMatchObject({
      packCount: before.packs.length,
      entryCount: before.entries.filter((entry) => entry.status === 'active').length,
      reviewCount: before.reviewQueue.length,
      runCount: before.activity.length,
    })
    const duplicateImport = await api.applyBundleImport({
      path,
      grantToken: preview.applyGrantToken,
      checksumSha256: preview.checksumSha256,
      confirmation: true,
    })
    expect(duplicateImport.packsImported).toBe(0)

    const externalPath = '/Users/mock/Desktop/external.json'
    const externalApi = createDesktopApi({
      forceMock: true,
      dialogs: { bundleFile: externalPath },
    })
    const externalGrant = await requireBundleGrant(externalApi)
    const externalPreview = await externalApi.previewBundleImport(
      externalPath,
      externalGrant.grantToken,
    )
    const firstImport = await externalApi.applyBundleImport({
      path: externalPreview.path,
      grantToken: externalPreview.applyGrantToken,
      checksumSha256: externalPreview.checksumSha256,
      confirmation: true,
    })
    const secondGrant = await requireBundleGrant(externalApi)
    const secondPreview = await externalApi.previewBundleImport(
      externalPath,
      secondGrant.grantToken,
    )
    const secondImport = await externalApi.applyBundleImport({
      path: secondPreview.path,
      grantToken: secondPreview.applyGrantToken,
      checksumSha256: secondPreview.checksumSha256,
      confirmation: true,
    })
    expect(firstImport.packsImported).toBe(1)
    expect(secondImport.packsImported).toBe(0)

    const archiveApi = createDesktopApi({ forceMock: true })
    const entryStatuses = new Map(
      (await archiveApi.listEntries()).map((entry) => [entry.id, entry.status]),
    )
    const firstArchive = await archiveApi.archiveScope({
      scopeId: MOCK_PROJECT_SCOPE_ID,
      confirmation: true,
    })
    const secondArchive = await archiveApi.archiveScope({
      scopeId: MOCK_PROJECT_SCOPE_ID,
      confirmation: true,
    })
    expect(firstArchive).toMatchObject({
      packsArchived: 2,
      packsAlreadyArchived: 0,
      entriesAffected: 3,
    })
    expect(secondArchive).toMatchObject({
      packsArchived: 0,
      packsAlreadyArchived: 2,
      entriesAffected: 0,
    })
    expect(
      new Map((await archiveApi.listEntries()).map((entry) => [entry.id, entry.status])),
    ).toEqual(entryStatuses)
  })

  it('rejects onboarding completion when no active durable context can compose', async () => {
    const seed = cloneMockDashboard()
    seed.entries = []
    seed.packs = []
    seed.onboarding = { complete: false, inferred: false, durableContext: false }
    seed.settings.onboarding = seed.onboarding
    const api = createDesktopApi({ forceMock: true, seed })

    await expect(api.completeOnboarding()).rejects.toMatchObject({
      code: 'invalid_input',
    })
    expect((await api.loadDashboard()).onboarding.complete).toBe(false)

    const wrongScopeSeed = cloneMockDashboard()
    wrongScopeSeed.entries = wrongScopeSeed.entries.filter(
      (entry) => entry.scopeId === MOCK_PROJECT_SCOPE_ID,
    )
    wrongScopeSeed.packs = wrongScopeSeed.packs.filter(
      (pack) => pack.scopeId === MOCK_PROJECT_SCOPE_ID,
    )
    wrongScopeSeed.workspace.push({
      id: 'project:/Users/mock/Empty',
      label: 'Empty repository',
      kind: 'project',
      description: 'A selected project with no composable entries.',
      status: 'Registered',
      children: [],
    })
    wrongScopeSeed.selectedScopeId = 'project:/Users/mock/Empty'
    wrongScopeSeed.settings.lastSelectedScopeId = wrongScopeSeed.selectedScopeId
    wrongScopeSeed.onboarding = {
      complete: false,
      inferred: false,
      durableContext: true,
    }
    wrongScopeSeed.settings.onboarding = wrongScopeSeed.onboarding
    const wrongScopeApi = createDesktopApi({ forceMock: true, seed: wrongScopeSeed })

    await expect(wrongScopeApi.completeOnboarding()).rejects.toMatchObject({
      code: 'invalid_input',
    })
    expect((await wrongScopeApi.loadDashboard()).onboarding.complete).toBe(false)
  })
})
