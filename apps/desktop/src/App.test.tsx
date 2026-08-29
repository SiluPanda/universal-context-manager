import { fireEvent, render, screen, waitFor, within } from '@testing-library/react'
import { describe, expect, it, vi } from 'vitest'
import App from './App'
import { createDesktopApi, DesktopApiError } from './api/desktopApi'
import {
  cloneMockDashboard,
  MOCK_GLOBAL_SCOPE_ID,
  MOCK_PROJECT_SCOPE_ID,
} from './api/mockData'
import type {
  ContextPreview,
  DashboardSnapshot,
  SourceImportApplyResult,
  SourceImportPreviewResult,
} from './types'

function freshOnboardingSeed(): DashboardSnapshot {
  const seed = cloneMockDashboard()
  seed.packs = []
  seed.entries = []
  seed.reviewQueue = []
  seed.activity = []
  seed.revisions = []
  seed.selectedScopeId = MOCK_GLOBAL_SCOPE_ID
  seed.onboarding = {
    complete: false,
    inferred: false,
    durableContext: false,
  }
  seed.settings.onboarding = seed.onboarding
  seed.settings.lastProjectPath = undefined
  seed.settings.lastSelectedScopeId = MOCK_GLOBAL_SCOPE_ID
  return seed
}

async function openView(name: string | RegExp) {
  const navigation = await screen.findByRole('navigation', { name: 'Primary navigation' })
  fireEvent.click(within(navigation).getByRole('button', { name }))
}

function deferred<T>() {
  let resolve!: (value: T) => void
  let reject!: (reason?: unknown) => void
  const promise = new Promise<T>((resolvePromise, rejectPromise) => {
    resolve = resolvePromise
    reject = rejectPromise
  })
  return { promise, resolve, reject }
}

async function openFreshOnboardingSources() {
  fireEvent.click(await screen.findByRole('button', { name: 'Begin setup' }))
  fireEvent.click(screen.getByRole('button', { name: 'Choose project folder…' }))
  await screen.findByRole('heading', {
    name: /Import what already exists—or write one entry/i,
  })
}

describe('desktop usability overhaul', () => {
  it('completes resumable manual onboarding only after a durable entry composes', async () => {
    const api = createDesktopApi({ forceMock: true, seed: freshOnboardingSeed() })
    render(<App api={api} />)

    expect(
      await screen.findByRole('heading', { name: /Know what is stored/i }),
    ).toBeInTheDocument()
    expect(screen.getByText('Local data path', { selector: 'span' })).toBeInTheDocument()
    fireEvent.click(screen.getByRole('button', { name: 'Begin setup' }))
    fireEvent.click(screen.getByRole('button', { name: 'Choose project folder…' }))

    expect(
      await screen.findByRole('heading', {
        name: /Import what already exists—or write one entry/i,
      }),
    ).toBeInTheDocument()
    fireEvent.click(screen.getByRole('tab', { name: 'Manual entry' }))
    fireEvent.change(screen.getByRole('textbox', { name: 'Manual first entry' }), {
      target: { value: 'Run focused desktop tests before lint and build.' },
    })
    fireEvent.click(screen.getByRole('button', { name: 'Continue to policy' }))

    const dirtyDialog = await screen.findByRole('dialog', { name: 'Unsaved changes' })
    fireEvent.click(within(dirtyDialog).getByRole('button', { name: 'Save' }))

    expect(
      await screen.findByRole('heading', { name: /Decide how agent proposals become durable/i }),
    ).toBeInTheDocument()
    fireEvent.click(screen.getByRole('radio', { name: /Balanced/i }))
    fireEvent.click(screen.getByRole('button', { name: 'Save policy' }))
    await waitFor(() =>
      expect(screen.getByRole('button', { name: 'Continue' })).not.toBeDisabled(),
    )
    fireEvent.click(screen.getByRole('button', { name: 'Continue' }))
    fireEvent.click(await screen.findByRole('button', { name: 'Compose and finish' }))

    expect(await screen.findByRole('heading', { name: 'Library' })).toBeInTheDocument()
    expect((await api.loadDashboard()).onboarding.complete).toBe(true)
    expect((await api.listEntries(MOCK_PROJECT_SCOPE_ID)).some((entry) => entry.status === 'active')).toBe(
      true,
    )
  })

  it('re-previews after policy changes, applies, and composes onboarding sources', async () => {
    const api = createDesktopApi({ forceMock: true, seed: freshOnboardingSeed() })
    const previewImport = vi.spyOn(api, 'previewSourceImport')
    const applyImport = vi.spyOn(api, 'applySourceImport')
    render(<App api={api} />)

    fireEvent.click(await screen.findByRole('button', { name: 'Begin setup' }))
    fireEvent.click(screen.getByRole('button', { name: 'Choose project folder…' }))
    fireEvent.click(await screen.findByRole('button', { name: 'Preview selected sources' }))
    expect(await screen.findByText(/2 candidates/i)).toBeInTheDocument()
    expect(screen.getAllByText('new').length).toBe(2)
    fireEvent.click(screen.getByRole('button', { name: 'Continue to policy' }))
    fireEvent.click(screen.getByRole('radio', { name: /Balanced/i }))
    expect(document.querySelector('.policy-preview-note')).toHaveTextContent(
      /fresh import preview under balanced/i,
    )
    fireEvent.click(await screen.findByRole('button', { name: 'Save policy' }))
    await waitFor(() => expect(previewImport).toHaveBeenCalledTimes(2))
    await waitFor(() =>
      expect(screen.getByRole('button', { name: 'Continue' })).not.toBeDisabled(),
    )
    fireEvent.click(screen.getByRole('button', { name: 'Continue' }))

    fireEvent.click(await screen.findByRole('button', { name: 'Apply selected import…' }))
    const importDialog = screen.getByRole('dialog', { name: 'Apply this source import?' })
    expect(applyImport).not.toHaveBeenCalled()
    fireEvent.click(within(importDialog).getByRole('button', { name: 'Apply import' }))
    await waitFor(() =>
      expect(applyImport).toHaveBeenCalledWith(
        expect.objectContaining({
          grantToken: expect.stringMatching(/^mock-path-grant-/),
          expectedPreviewFingerprint: expect.stringMatching(/^mock-/),
          confirmation: true,
        }),
      ),
    )
    await waitFor(() =>
      expect(screen.queryByRole('dialog', { name: 'Apply this source import?' })).not.toBeInTheDocument(),
    )
    fireEvent.click(screen.getByRole('button', { name: 'Compose and finish' }))

    expect(await screen.findByRole('heading', { name: 'Library' })).toBeInTheDocument()
    expect((await api.loadDashboard()).onboarding.complete).toBe(true)
  })

  it('forces a new source preview after an authoritative fingerprint conflict', async () => {
    const api = createDesktopApi({ forceMock: true, seed: freshOnboardingSeed() })
    render(<App api={api} />)

    fireEvent.click(await screen.findByRole('button', { name: 'Begin setup' }))
    fireEvent.click(screen.getByRole('button', { name: 'Choose project folder…' }))
    fireEvent.click(await screen.findByRole('button', { name: 'Preview selected sources' }))
    await screen.findByText(/2 candidates/i)
    fireEvent.click(screen.getByRole('button', { name: 'Continue to policy' }))
    fireEvent.click(await screen.findByRole('button', { name: 'Save policy' }))
    await waitFor(() =>
      expect(screen.getByRole('button', { name: 'Continue' })).not.toBeDisabled(),
    )
    fireEvent.click(screen.getByRole('button', { name: 'Continue' }))

    await api.setReviewPolicy({ mode: 'fast', actor: 'external-test' })
    fireEvent.click(await screen.findByRole('button', { name: 'Apply selected import…' }))
    fireEvent.click(
      within(screen.getByRole('dialog', { name: 'Apply this source import?' })).getByRole(
        'button',
        { name: 'Apply import' },
      ),
    )

    expect(
      await screen.findByRole('heading', {
        name: /Import what already exists—or write one entry/i,
      }),
    ).toBeInTheDocument()
    expect(screen.getByRole('heading', { name: 'Preview required' })).toBeInTheDocument()
    expect(screen.getByRole('alert')).toHaveTextContent(
      /Nothing was imported.*preview the selected sources again/i,
    )
  })

  it('resumes incomplete onboarding with existing durable context', async () => {
    const seed = cloneMockDashboard()
    seed.onboarding = {
      ...seed.onboarding,
      complete: false,
      inferred: false,
    }
    seed.settings.onboarding = seed.onboarding
    const api = createDesktopApi({ forceMock: true, seed })
    const entryCount = (await api.listEntries(MOCK_PROJECT_SCOPE_ID)).length
    render(<App api={api} />)

    expect(
      await screen.findByRole('heading', { name: /Choose the project you want to remember/i }),
    ).toBeInTheDocument()
    fireEvent.click(
      screen.getByRole('button', { name: 'Reauthorize this project folder…' }),
    )
    expect(
      await screen.findByRole('tab', { name: 'Use existing context' }),
    ).toHaveAttribute('aria-selected', 'true')
    fireEvent.click(screen.getByRole('button', { name: 'Continue to policy' }))
    fireEvent.click(await screen.findByRole('button', { name: 'Save policy' }))
    await waitFor(() =>
      expect(screen.getByRole('button', { name: 'Continue' })).not.toBeDisabled(),
    )
    fireEvent.click(screen.getByRole('button', { name: 'Continue' }))
    fireEvent.click(await screen.findByRole('button', { name: 'Compose and finish' }))

    expect(await screen.findByRole('heading', { name: 'Library' })).toBeInTheDocument()
    expect((await api.listEntries(MOCK_PROJECT_SCOPE_ID))).toHaveLength(entryCount)
  })

  it('falls back to a manual entry when a registered project has no instruction files', async () => {
    const api = createDesktopApi({
      forceMock: true,
      seed: freshOnboardingSeed(),
      dialogs: { projectFolder: '/Users/mock/No-Files' },
    })
    render(<App api={api} />)

    fireEvent.click(await screen.findByRole('button', { name: 'Begin setup' }))
    fireEvent.click(screen.getByRole('button', { name: 'Choose project folder…' }))
    expect(await screen.findByRole('tab', { name: 'Manual entry' })).toHaveAttribute(
      'aria-selected',
      'true',
    )
    expect(screen.getByRole('tab', { name: 'Import sources' })).toBeDisabled()
    expect(screen.getByRole('textbox', { name: 'Manual first entry' })).toBeInTheDocument()
  })

  it('keeps the wizard open with actionable copy when the backend rejects completion', async () => {
    const seed = cloneMockDashboard()
    seed.onboarding = { ...seed.onboarding, complete: false, inferred: false }
    seed.settings.onboarding = seed.onboarding
    const api = createDesktopApi({ forceMock: true, seed })
    vi.spyOn(api, 'completeOnboarding').mockRejectedValue(
      new DesktopApiError({
        code: 'invalid_input',
        message: 'onboarding requires composed durable context',
        retryable: false,
      }),
    )
    render(<App api={api} />)

    fireEvent.click(
      await screen.findByRole('button', { name: 'Reauthorize this project folder…' }),
    )
    fireEvent.click(await screen.findByRole('button', { name: 'Continue to policy' }))
    fireEvent.click(await screen.findByRole('button', { name: 'Save policy' }))
    await waitFor(() =>
      expect(screen.getByRole('button', { name: 'Continue' })).not.toBeDisabled(),
    )
    fireEvent.click(screen.getByRole('button', { name: 'Continue' }))
    fireEvent.click(await screen.findByRole('button', { name: 'Compose and finish' }))

    const alert = await screen.findByRole('alert')
    expect(alert).toHaveTextContent('Onboarding remains incomplete')
    expect(alert).toHaveTextContent(/Nothing was marked complete.*restore.*approve/i)
    expect(
      screen.getByRole('heading', { name: /Finish with durable context and exact output/i }),
    ).toBeInTheDocument()
    expect((await api.loadDashboard()).onboarding.complete).toBe(false)
  })

  it('edits one entry without mutating a sibling in the same pack', async () => {
    const api = createDesktopApi({ forceMock: true })
    const siblingBefore = (await api.listEntries()).find(
      (entry) => entry.id === 'entry-project-tools',
    )!
    render(<App api={api} />)

    const content = await screen.findByRole('textbox', { name: 'Entry content' })
    fireEvent.change(content, { target: { value: 'Only this Markdown entry should change.' } })
    fireEvent.click(screen.getByRole('button', { name: 'Save entry' }))

    expect(await screen.findByText(/Only this entry was updated/i)).toBeInTheDocument()
    const siblingAfter = (await api.listEntries()).find(
      (entry) => entry.id === 'entry-project-tools',
    )
    expect(siblingAfter?.body).toBe(siblingBefore.body)
  })

  it('protects dirty entry navigation with Save, Discard, and Stay choices', async () => {
    render(<App api={createDesktopApi({ forceMock: true })} />)

    fireEvent.change(await screen.findByRole('textbox', { name: 'Entry title' }), {
      target: { value: 'Unsaved title' },
    })
    await openView(/^Inbox/)
    let dialog = screen.getByRole('dialog', { name: 'Unsaved changes' })
    expect(within(dialog).getByRole('button', { name: 'Save' })).toBeInTheDocument()
    expect(within(dialog).getByRole('button', { name: 'Discard' })).toBeInTheDocument()
    expect(within(dialog).getByRole('button', { name: 'Save' })).toHaveFocus()
    fireEvent.click(within(dialog).getByRole('button', { name: 'Stay' }))
    expect(screen.getByRole('heading', { name: 'Library' })).toBeInTheDocument()

    await openView(/^Inbox/)
    dialog = screen.getByRole('dialog', { name: 'Unsaved changes' })
    fireEvent.click(within(dialog).getByRole('button', { name: 'Discard' }))
    expect(await screen.findByRole('heading', { name: 'Inbox' })).toBeInTheDocument()
  })

  it('renders backend Markdown byte-for-byte without client reconstruction', async () => {
    const api = createDesktopApi({ forceMock: true })
    const original = await api.composeEffectiveContext({
      scopeId: MOCK_PROJECT_SCOPE_ID,
      destinationAdapter: 'adapter-daemon',
    })
    const exact = '# Exact backend output\n\n- keep  two spaces  \n\n```json\n{"a":1}\n```\n'
    vi.spyOn(api, 'composeEffectiveContext').mockResolvedValue({
      ...original,
      renderedMarkdown: exact,
      metrics: { ...original.metrics, renderedBytes: new TextEncoder().encode(exact).byteLength },
    })
    render(<App api={api} />)

    await openView('Effective Context')
    const output = await screen.findByTestId('exact-rendered-markdown')
    expect(output.textContent).toBe(exact)
  })

  it('navigates from an actionable search result to the actual JSON entry', async () => {
    render(<App api={createDesktopApi({ forceMock: true })} />)

    await openView('Search')
    fireEvent.change(
      await screen.findByRole('searchbox', { name: 'Search local context' }),
      { target: { value: 'tool preferences' } },
    )
    const result = await screen.findByRole('button', {
      name: /Repository workflow \/ Tool preferences/i,
    })
    fireEvent.click(result)

    expect(await screen.findByRole('heading', { name: 'Library' })).toBeInTheDocument()
    expect(screen.getByRole('combobox', { name: 'Entry format' })).toHaveValue('json')
    expect(
      (screen.getByRole('textbox', { name: 'Entry content' }) as HTMLTextAreaElement).value,
    ).toContain('"packageManager": "pnpm"')
    await waitFor(() => expect(document.getElementById('entry-editor')).toHaveFocus())
  })

  it('filters reviews, compares both sides, and reports a partial bulk result truthfully', async () => {
    const api = createDesktopApi({ forceMock: true })
    const bulkDecision = vi.spyOn(api, 'bulkReviewDecision')
    render(<App api={api} />)
    await openView(/^Inbox/)

    expect(await screen.findByText('Existing')).toBeInTheDocument()
    expect(screen.getByText('Proposed')).toBeInTheDocument()
    const sourceFilter = screen.getByRole('combobox', { name: 'Source' })
    fireEvent.change(sourceFilter, { target: { value: 'spool' } })
    await waitFor(() =>
      expect(
        screen.getAllByText('Queued write from an offline adapter').length,
      ).toBeGreaterThan(0),
    )
    await waitFor(() =>
      expect(screen.queryByText('Expand focused test guidance')).not.toBeInTheDocument(),
    )

    fireEvent.change(sourceFilter, { target: { value: 'all' } })
    fireEvent.click(screen.getByRole('checkbox', { name: 'Select all visible reviews' }))
    fireEvent.click(screen.getByRole('button', { name: /^Approve 3/ }))
    const dialog = screen.getByRole('dialog', { name: 'Approve 3 reviews?' })
    expect(bulkDecision).not.toHaveBeenCalled()
    fireEvent.click(within(dialog).getByRole('button', { name: 'Approve selected' }))

    expect(
      await screen.findByRole('heading', { name: '2 of 3 attempted items completed' }),
    ).toBeInTheDocument()
    expect(screen.getByText(/Local service unavailable; the item remains queued/i)).toBeInTheDocument()
    expect(screen.getAllByText('Queued write from an offline adapter').length).toBeGreaterThan(0)
    expect(bulkDecision).toHaveBeenCalledWith(
      expect.objectContaining({ confirmation: true }),
    )
    const bulkNotice = screen
      .getByText(/2 of 3 attempted reviews completed/i, { selector: '.banner p' })
      .closest<HTMLElement>('.banner')
    fireEvent.click(within(bulkNotice!).getByRole('button', { name: 'Open history' }))
    await waitFor(() =>
      expect(screen.getByRole('textbox', { name: 'Entry key' })).toHaveValue(
        'focused-testing',
      ),
    )
  })

  it('omits review history when a successful outcome has no exact durable entry', async () => {
    render(<App api={createDesktopApi({ forceMock: true })} />)
    await openView(/^Inbox/)
    fireEvent.click(
      await screen.findByRole('button', {
        name: /Add accessibility check.*strict_policy/i,
      }),
    )
    fireEvent.click(screen.getByRole('button', { name: 'Reject' }))

    expect(
      (await screen.findAllByText('Add accessibility check was rejected.')).length,
    ).toBeGreaterThan(0)
    const notice = screen
      .getAllByText('Add accessibility check was rejected.')
      .map((element) => element.closest<HTMLElement>('.banner'))
      .find(Boolean)
    expect(within(notice!).queryByRole('button', { name: 'Open history' })).not.toBeInTheDocument()
  })

  it('locks single review actions and shortcuts while a decision is in flight', async () => {
    const api = createDesktopApi({ forceMock: true })
    const decision = deferred<void>()
    const originalDecision = api.reviewDecision.bind(api)
    const reviewDecision = vi
      .spyOn(api, 'reviewDecision')
      .mockImplementationOnce(async (input) => {
        await decision.promise
        return originalDecision(input)
      })
    render(<App api={api} />)
    await openView(/^Inbox/)
    const approve = await screen.findByRole('button', { name: 'Approve' })

    fireEvent.click(approve)
    fireEvent.click(approve)
    fireEvent.keyDown(window, { key: 'r' })
    expect(reviewDecision).toHaveBeenCalledTimes(1)
    expect(screen.getByRole('button', { name: 'Reject' })).toBeDisabled()
    expect(screen.getByRole('button', { name: 'Edit one' })).toBeDisabled()
    expect(screen.getByRole('button', { name: 'Approve' })).toBeDisabled()

    decision.resolve()
    await waitFor(() => expect(screen.queryByText('Expand focused test guidance')).not.toBeInTheDocument())
    expect(reviewDecision).toHaveBeenCalledTimes(1)
  })

  it('persists strict, balanced, and fast review policy semantics through Connections', async () => {
    const api = createDesktopApi({ forceMock: true })
    render(<App api={api} />)
    await openView('Connections')

    fireEvent.click(await screen.findByRole('radio', { name: /Fast/i }))
    fireEvent.click(screen.getByRole('button', { name: 'Save policy' }))
    await waitFor(async () => {
      expect((await api.loadSettings()).reviewMode).toBe('fast')
    })
    expect(
      (await screen.findAllByText(/Review policy saved as fast/i)).length,
    ).toBeGreaterThan(0)
  })

  it('re-runs diagnostics after a backend-supported repair', async () => {
    const api = createDesktopApi({ forceMock: true })
    const refresh = vi.spyOn(api, 'refreshDiagnostics')
    render(<App api={api} />)
    await openView('Connections')

    fireEvent.click(await screen.findByRole('button', { name: 'Retry queued writes' }))
    await waitFor(() => expect(refresh).toHaveBeenCalled())
    expect((await api.loadDashboard()).diagnostics.spoolBacklog).toBe(0)
    expect(
      screen.getAllByText(/Spool retry attempted 1; delivered 1; retained 0; errors 0/i)
        .length,
    ).toBeGreaterThan(0)
  })

  it('uses the deterministic native-dialog mock for backup preview', async () => {
    const api = createDesktopApi({
      forceMock: true,
      dialogs: { bundleFile: '/Users/mock/Desktop/native-dialog-backup.json' },
    })
    const picker = vi.spyOn(api, 'selectBundleImportFile')
    render(<App api={api} />)
    await openView('Connections')
    fireEvent.click(await screen.findByRole('tab', { name: 'Privacy & Data' }))
    fireEvent.click(screen.getByRole('button', { name: 'Choose backup…' }))

    expect(
      await screen.findByText('/Users/mock/Desktop/native-dialog-backup.json'),
    ).toBeInTheDocument()
    expect(picker).toHaveBeenCalledOnce()
    expect(screen.queryByRole('textbox', { name: /archive path/i })).not.toBeInTheDocument()
    expect(screen.getByText(/Preview only. No records have been imported/i)).toBeInTheDocument()
  })

  it('shows a secret-friendly error and confirms nothing was stored', async () => {
    const api = createDesktopApi({ forceMock: true })
    const before = (await api.listEntries()).find(
      (entry) => entry.id === 'entry-project-testing',
    )!
    render(<App api={api} />)

    fireEvent.change(await screen.findByRole('textbox', { name: 'Entry content' }), {
      target: { value: 'api_key = sk-supersecretvalue' },
    })
    fireEvent.click(screen.getByRole('button', { name: 'Save entry' }))
    expect(
      await screen.findByText(/Nothing was stored.*secret manager/i),
    ).toBeInTheDocument()
    expect(
      screen.getByRole('alert').textContent,
    ).not.toMatch(/sk-supersecretvalue/i)
    const after = (await api.listEntries()).find((entry) => entry.id === before.id)
    expect(after?.body).toBe(before.body)
  })

  it('requires explicit confirmation for entry archive and scoped forget', async () => {
    render(<App api={createDesktopApi({ forceMock: true })} />)

    fireEvent.click(await screen.findByRole('button', { name: 'Archive…' }))
    let dialog = screen.getByRole('dialog', { name: 'Archive this entry?' })
    expect(within(dialog).getByText(/Sibling entries.*remain unchanged/i)).toBeInTheDocument()
    fireEvent.click(within(dialog).getByRole('button', { name: 'Cancel' }))
    expect(screen.getAllByText('active').length).toBeGreaterThan(0)

    fireEvent.click(screen.getByRole('button', { name: 'Archive…' }))
    dialog = screen.getByRole('dialog', { name: 'Archive this entry?' })
    fireEvent.click(within(dialog).getByRole('button', { name: 'Archive entry' }))
    expect((await screen.findAllByText(/was archived/i)).length).toBeGreaterThan(0)
    expect(screen.getByRole('button', { name: 'Open history' })).toBeInTheDocument()

    await openView('Connections')
    fireEvent.click(await screen.findByRole('tab', { name: 'Privacy & Data' }))
    fireEvent.click(screen.getByRole('button', { name: 'Forget scope…' }))
    dialog = screen.getByRole('dialog', { name: /Forget This repository context/i })
    expect(within(dialog).getByText(/reports this operation as reversible/i)).toBeInTheDocument()
    fireEvent.click(within(dialog).getByRole('button', { name: 'Run forget workflow' }))
    expect(await screen.findByText(/backend reports this operation as reversible/i)).toBeInTheDocument()
  })

  it('supports keyboard triage, suppresses shortcuts while editing, and announces politely', async () => {
    const api = createDesktopApi({ forceMock: true })
    render(<App api={api} />)
    await openView(/^Inbox/)

    fireEvent.click(await screen.findByRole('button', { name: 'Edit one' }))
    const editor = screen.getByRole('textbox', { name: 'Edited proposed content' })
    fireEvent.keyDown(editor, { key: 'r' })
    expect((await api.loadDashboard()).reviewQueue).toHaveLength(3)
    fireEvent.click(screen.getByRole('button', { name: 'Cancel edit' }))

    fireEvent.keyDown(window, { key: 'a' })
    await waitFor(async () => {
      expect((await api.loadDashboard()).reviewQueue).toHaveLength(2)
    })
    expect(
      await screen.findByText('Expand focused test guidance review completed.'),
    ).toBeInTheDocument()
    expect(
      screen.getByText('Expand focused test guidance review completed.').closest('[aria-live]'),
    ).toHaveAttribute('aria-live', 'polite')
  })

  it('keeps Quick Open keyboard navigation and focus restoration', async () => {
    render(<App api={createDesktopApi({ forceMock: true })} />)
    const trigger = await screen.findByRole('button', { name: 'Open Quick Open' })
    trigger.focus()
    fireEvent.keyDown(window, { key: 'k', metaKey: true })
    let dialog = screen.getByRole('dialog', { name: 'Quick Open' })
    const input = within(dialog).getByRole('combobox', {
      name: 'Find context or a command',
    })
    expect(input).toHaveFocus()
    fireEvent.keyDown(input, { key: 'Escape' })
    await waitFor(() => expect(trigger).toHaveFocus())

    fireEvent.keyDown(window, { key: 'k', ctrlKey: true })
    dialog = screen.getByRole('dialog', { name: 'Quick Open' })
    const secondInput = within(dialog).getByRole('combobox', {
      name: 'Find context or a command',
    })
    fireEvent.change(secondInput, { target: { value: 'Tool preferences' } })
    fireEvent.keyDown(secondInput, { key: 'Enter' })
    expect(
      (await screen.findAllByRole('heading', { name: 'Tool preferences' })).length,
    ).toBeGreaterThan(0)
  })

  it('ignores stale onboarding preview and policy responses', async () => {
    const api = createDesktopApi({ forceMock: true, seed: freshOnboardingSeed() })
    const fixtureApi = createDesktopApi({ forceMock: true })
    const fixtureGrant = (await fixtureApi.selectSourceImportFiles())!
    const stalePreview = await fixtureApi.previewSourceImport({
      paths: fixtureGrant.paths,
      grantToken: fixtureGrant.grantToken,
      destinationScopeId: MOCK_PROJECT_SCOPE_ID,
      packName: 'Imported instructions',
      sourceKind: 'auto',
      actor: 'desktop-onboarding',
    })
    const previewDeferred = deferred<SourceImportPreviewResult>()
    const originalPreview = api.previewSourceImport.bind(api)
    vi.spyOn(api, 'previewSourceImport')
      .mockReturnValueOnce(previewDeferred.promise)
      .mockImplementation(originalPreview)
    render(<App api={api} />)
    await openFreshOnboardingSources()

    fireEvent.click(screen.getByRole('button', { name: 'Preview selected sources' }))
    fireEvent.click(screen.getByRole('checkbox', { name: /AGENTS\.md/i }))
    previewDeferred.resolve(stalePreview)
    await waitFor(() =>
      expect(screen.getByRole('heading', { name: 'Preview required' })).toBeInTheDocument(),
    )
    expect(screen.queryByText(/2 candidates/i)).not.toBeInTheDocument()

    fireEvent.click(screen.getByRole('button', { name: 'Preview selected sources' }))
    await screen.findByText(/2 candidates/i)
    fireEvent.click(screen.getByRole('button', { name: 'Continue to policy' }))

    const policyDeferred = deferred<NonNullable<DashboardSnapshot['reviewPolicy']>>()
    const originalPolicy = api.setReviewPolicy.bind(api)
    vi.spyOn(api, 'setReviewPolicy')
      .mockReturnValueOnce(policyDeferred.promise)
      .mockImplementation(originalPolicy)
    fireEvent.click(screen.getByRole('button', { name: 'Save policy' }))
    fireEvent.click(screen.getByRole('radio', { name: /Fast/i }))
    policyDeferred.resolve(cloneMockDashboard().reviewPolicy!)

    await waitFor(() =>
      expect(screen.getByRole('button', { name: 'Continue' })).toBeDisabled(),
    )
    expect(document.querySelector('.policy-preview-note')).toHaveTextContent(
      /fresh import preview under fast/i,
    )
  })

  it('ignores an apply response when onboarding inputs change in flight', async () => {
    const api = createDesktopApi({ forceMock: true, seed: freshOnboardingSeed() })
    const applyDeferred = deferred<SourceImportApplyResult>()
    render(<App api={api} />)
    await openFreshOnboardingSources()
    fireEvent.click(screen.getByRole('button', { name: 'Preview selected sources' }))
    await screen.findByText(/2 candidates/i)
    fireEvent.click(screen.getByRole('button', { name: 'Continue to policy' }))
    fireEvent.click(screen.getByRole('button', { name: 'Save policy' }))
    await waitFor(() =>
      expect(screen.getByRole('button', { name: 'Continue' })).not.toBeDisabled(),
    )
    fireEvent.click(screen.getByRole('button', { name: 'Continue' }))
    vi.spyOn(api, 'applySourceImport').mockReturnValueOnce(applyDeferred.promise)

    fireEvent.click(screen.getByRole('button', { name: 'Apply selected import…' }))
    fireEvent.click(
      within(screen.getByRole('dialog', { name: 'Apply this source import?' })).getByRole(
        'button',
        { name: 'Apply import' },
      ),
    )
    fireEvent.click(screen.getByRole('button', { name: 'Back' }))
    fireEvent.click(await screen.findByRole('radio', { name: /Fast/i }))
    applyDeferred.resolve({
      requestId: 'stale-apply',
      destinationScopeId: MOCK_PROJECT_SCOPE_ID,
      packName: 'Imported instructions',
      navigationScopeId: MOCK_PROJECT_SCOPE_ID,
      candidateCount: 2,
      importedCount: 2,
      appliedCount: 2,
      pendingCount: 0,
      skippedCount: 0,
      rejectedCount: 0,
      items: [],
      affectedEntryIds: ['stale-a', 'stale-b'],
      affectedReviewIds: [],
      affectedEntryKeys: ['a', 'b'],
    })

    await waitFor(() =>
      expect(screen.queryByRole('dialog', { name: 'Apply this source import?' })).not.toBeInTheDocument(),
    )
    expect(screen.getByRole('button', { name: 'Continue' })).toBeDisabled()
    expect(screen.queryByText(/2 applied/i)).not.toBeInTheDocument()
  })

  it('opens saved history through dirty protection with the saved entry draft', async () => {
    render(<App api={createDesktopApi({ forceMock: true })} />)
    const savedBody = 'Saved entry A body.'
    fireEvent.change(await screen.findByRole('textbox', { name: 'Entry content' }), {
      target: { value: savedBody },
    })
    fireEvent.click(screen.getByRole('button', { name: 'Save entry' }))
    const openHistory = await screen.findByRole('button', { name: 'Open history' })

    fireEvent.click(
      screen.getByRole('button', { name: /Tool preferences.*tool-preferences/i }),
    )
    fireEvent.change(screen.getByRole('textbox', { name: 'Entry content' }), {
      target: { value: 'Unsaved sibling B draft.' },
    })
    fireEvent.click(openHistory)
    const dialog = screen.getByRole('dialog', { name: 'Unsaved changes' })
    fireEvent.click(within(dialog).getByRole('button', { name: 'Discard' }))

    await waitFor(() =>
      expect(screen.getByRole('textbox', { name: 'Entry key' })).toHaveValue(
        'focused-testing',
      ),
    )
    expect(screen.getByRole('textbox', { name: 'Entry content' })).toHaveValue(savedBody)
  })

  it('creates and then updates one scope-bound manual onboarding entry', async () => {
    const api = createDesktopApi({
      forceMock: true,
      seed: freshOnboardingSeed(),
      dialogs: {
        projectFolders: ['/Users/mock/Atlas', '/Users/mock/Other'],
      },
    })
    const savePack = vi.spyOn(api, 'savePack')
    const saveEntry = vi.spyOn(api, 'saveEntry')
    render(<App api={api} />)
    await openFreshOnboardingSources()
    fireEvent.click(screen.getByRole('tab', { name: 'Manual entry' }))
    fireEvent.change(screen.getByRole('textbox', { name: 'Manual first entry' }), {
      target: { value: 'First manual body.' },
    })
    fireEvent.click(screen.getByRole('button', { name: 'Continue to policy' }))
    fireEvent.click(
      within(await screen.findByRole('dialog', { name: 'Unsaved changes' })).getByRole(
        'button',
        { name: 'Save' },
      ),
    )
    await screen.findByRole('heading', {
      name: /Decide how agent proposals become durable/i,
    })
    fireEvent.click(screen.getByRole('button', { name: 'Back' }))
    fireEvent.change(screen.getByRole('textbox', { name: 'Manual first entry' }), {
      target: { value: 'Updated manual body.' },
    })
    fireEvent.click(screen.getByRole('button', { name: 'Continue to policy' }))
    fireEvent.click(
      within(await screen.findByRole('dialog', { name: 'Unsaved changes' })).getByRole(
        'button',
        { name: 'Save' },
      ),
    )

    await waitFor(() => expect(saveEntry).toHaveBeenCalledTimes(2))
    expect(savePack).not.toHaveBeenCalled()
    expect(saveEntry.mock.calls[0][0].id).toBeUndefined()
    expect(saveEntry.mock.calls[1][0].id).toBeTruthy()

    fireEvent.click(await screen.findByRole('button', { name: 'Back' }))
    fireEvent.click(screen.getByRole('button', { name: 'Back' }))
    fireEvent.click(screen.getByRole('button', { name: 'Choose project folder…' }))
    await screen.findByRole('heading', {
      name: /Import what already exists—or write one entry/i,
    })
    fireEvent.click(screen.getByRole('tab', { name: 'Manual entry' }))
    expect(screen.getByRole('textbox', { name: 'Manual first entry' })).toHaveValue('')
    fireEvent.change(screen.getByRole('textbox', { name: 'Manual first entry' }), {
      target: { value: 'Other project manual body.' },
    })
    fireEvent.click(screen.getByRole('button', { name: 'Continue to policy' }))
    fireEvent.click(
      within(await screen.findByRole('dialog', { name: 'Unsaved changes' })).getByRole(
        'button',
        { name: 'Save' },
      ),
    )

    await waitFor(() => expect(saveEntry).toHaveBeenCalledTimes(3))
    expect(saveEntry.mock.calls[2][0]).toMatchObject({
      id: undefined,
      scopeId: 'project:/Users/mock/Other',
    })
    const atlasEntries = await api.listEntries(MOCK_PROJECT_SCOPE_ID)
    const otherEntries = await api.listEntries('project:/Users/mock/Other')
    expect(atlasEntries).toHaveLength(1)
    expect(atlasEntries[0].body).toBe('Updated manual body.')
    expect(otherEntries).toHaveLength(1)
    expect(otherEntries[0].body).toBe('Other project manual body.')
  })

  it('invalidates import preview state after manual destination changes', async () => {
    const api = createDesktopApi({ forceMock: true, seed: freshOnboardingSeed() })
    render(<App api={api} />)
    await openFreshOnboardingSources()
    fireEvent.click(screen.getByRole('button', { name: 'Preview selected sources' }))
    await screen.findByText(/2 candidates/i)
    fireEvent.click(screen.getByRole('tab', { name: 'Manual entry' }))
    fireEvent.change(screen.getByRole('textbox', { name: 'Manual first entry' }), {
      target: { value: 'Manual state change invalidates imports.' },
    })
    fireEvent.click(screen.getByRole('button', { name: 'Continue to policy' }))
    fireEvent.click(
      within(await screen.findByRole('dialog', { name: 'Unsaved changes' })).getByRole(
        'button',
        { name: 'Save' },
      ),
    )
    await screen.findByRole('heading', {
      name: /Decide how agent proposals become durable/i,
    })
    fireEvent.click(screen.getByRole('button', { name: 'Back' }))
    fireEvent.click(screen.getByRole('tab', { name: 'Import sources' }))

    expect(screen.getByRole('heading', { name: 'Preview required' })).toBeInTheDocument()
    expect(screen.queryByText(/2 candidates/i)).not.toBeInTheDocument()
    expect(screen.getByRole('button', { name: 'Continue to policy' })).toBeDisabled()
  })

  it('blocks source apply when the reviewed preview disallows it', async () => {
    const api = createDesktopApi({ forceMock: true, seed: freshOnboardingSeed() })
    const originalPreview = api.previewSourceImport.bind(api)
    vi.spyOn(api, 'previewSourceImport').mockImplementation(async (input) => ({
      ...(await originalPreview(input)),
      applyAllowed: false,
      warnings: ['Backend blocked this preview.'],
    }))
    const apply = vi.spyOn(api, 'applySourceImport')
    render(<App api={api} />)
    await openFreshOnboardingSources()
    fireEvent.click(screen.getByRole('button', { name: 'Preview selected sources' }))
    await screen.findByText('Backend blocked this preview.')
    fireEvent.click(screen.getByRole('button', { name: 'Continue to policy' }))
    fireEvent.click(screen.getByRole('button', { name: 'Save policy' }))
    await waitFor(() =>
      expect(screen.getByRole('button', { name: 'Continue' })).not.toBeDisabled(),
    )
    fireEvent.click(screen.getByRole('button', { name: 'Continue' }))

    expect(
      await screen.findByRole('button', { name: 'Import blocked by preview' }),
    ).toBeDisabled()
    fireEvent.click(screen.getByRole('button', { name: 'Import blocked by preview' }))
    expect(apply).not.toHaveBeenCalled()
    expect(screen.queryByRole('dialog', { name: 'Apply this source import?' })).not.toBeInTheDocument()
  })

  it('blocks invalid bundle apply before confirmation', async () => {
    const api = createDesktopApi({ forceMock: true })
    const originalPreview = api.previewBundleImport.bind(api)
    vi.spyOn(api, 'previewBundleImport').mockImplementation(async (path, grantToken) => ({
      ...(await originalPreview(path, grantToken)),
      valid: false,
      warnings: ['Checksum structure is invalid.'],
    }))
    const apply = vi.spyOn(api, 'applyBundleImport')
    render(<App api={api} />)
    await openView('Connections')
    fireEvent.click(await screen.findByRole('tab', { name: 'Privacy & Data' }))
    fireEvent.click(screen.getByRole('button', { name: 'Choose backup…' }))

    expect(await screen.findByRole('button', { name: 'Import blocked' })).toBeDisabled()
    fireEvent.click(screen.getByRole('button', { name: 'Import blocked' }))
    expect(apply).not.toHaveBeenCalled()
    expect(screen.queryByRole('dialog', { name: 'Import this local backup?' })).not.toBeInTheDocument()
  })

  it('does not enable onboarding completion for a fully rejected import', async () => {
    const api = createDesktopApi({ forceMock: true, seed: freshOnboardingSeed() })
    const complete = vi.spyOn(api, 'completeOnboarding')
    render(<App api={api} />)
    await openFreshOnboardingSources()
    fireEvent.click(screen.getByRole('button', { name: 'Preview selected sources' }))
    await screen.findByText(/2 candidates/i)
    fireEvent.click(screen.getByRole('button', { name: 'Continue to policy' }))
    fireEvent.click(screen.getByRole('button', { name: 'Save policy' }))
    await waitFor(() =>
      expect(screen.getByRole('button', { name: 'Continue' })).not.toBeDisabled(),
    )
    fireEvent.click(screen.getByRole('button', { name: 'Continue' }))
    vi.spyOn(api, 'applySourceImport').mockResolvedValue({
      requestId: 'rejected-import',
      destinationScopeId: MOCK_PROJECT_SCOPE_ID,
      packName: 'Imported instructions',
      navigationScopeId: MOCK_PROJECT_SCOPE_ID,
      candidateCount: 2,
      importedCount: 0,
      appliedCount: 0,
      pendingCount: 0,
      skippedCount: 0,
      rejectedCount: 2,
      items: [],
      affectedEntryIds: [],
      affectedReviewIds: [],
      affectedEntryKeys: [],
    })
    fireEvent.click(screen.getByRole('button', { name: 'Apply selected import…' }))
    fireEvent.click(
      within(screen.getByRole('dialog', { name: 'Apply this source import?' })).getByRole(
        'button',
        { name: 'Apply import' },
      ),
    )

    expect(await screen.findByText(/backend rejected every candidate/i)).toBeInTheDocument()
    expect(screen.getByText('rejected', { selector: '.status-pill' })).toBeInTheDocument()
    expect(screen.getByRole('button', { name: 'Compose and finish' })).toBeDisabled()
    expect(complete).not.toHaveBeenCalled()
  })

  it('uses backend privacy disclosures and actual onboarding egress flags', async () => {
    const seed = freshOnboardingSeed()
    seed.privacy = {
      ...seed.privacy,
      localOnlyStatement: 'CUSTOM LOCAL STORAGE DISCLOSURE',
      downstreamAdapterDisclosure: 'CUSTOM DOWNSTREAM DISCLOSURE',
      secretScanningStatement: 'CUSTOM SECRET SCANNING DISCLOSURE',
      applicationEncryptionBoundary: 'CUSTOM ENCRYPTION DISCLOSURE',
      telemetryEnabled: true,
      networkEgressEnabled: true,
    }
    seed.settings.telemetry = true
    render(<App api={createDesktopApi({ forceMock: true, seed })} />)

    expect(
      (await screen.findAllByText(/CUSTOM LOCAL STORAGE DISCLOSURE/)).length,
    ).toBeGreaterThan(0)
    expect(screen.getAllByText(/CUSTOM DOWNSTREAM DISCLOSURE/).length).toBeGreaterThan(0)
    expect(screen.getByText('CUSTOM SECRET SCANNING DISCLOSURE')).toBeInTheDocument()
    expect(screen.getByText('CUSTOM ENCRYPTION DISCLOSURE')).toBeInTheDocument()
    expect(screen.getByText('app telemetry enabled')).toBeInTheDocument()
    expect(screen.getByText('app network egress enabled')).toBeInTheDocument()
    expect(screen.queryByText(/Nothing leaves the app/i)).not.toBeInTheDocument()
  })

  it('clears and fences Effective Context to the latest scope and adapter pair', async () => {
    const api = createDesktopApi({ forceMock: true })
    const fixture = createDesktopApi({ forceMock: true })
    const oldPreview: ContextPreview = {
      ...(await fixture.composeEffectiveContext({
        scopeId: MOCK_PROJECT_SCOPE_ID,
        destinationAdapter: 'adapter-daemon',
      })),
      renderedMarkdown: 'STALE PROJECT OUTPUT',
    }
    const currentPreview: ContextPreview = {
      ...(await fixture.composeEffectiveContext({
        scopeId: MOCK_GLOBAL_SCOPE_ID,
        destinationAdapter: 'adapter-codex',
      })),
      renderedMarkdown: 'CURRENT GLOBAL CODEX OUTPUT',
    }
    const first = deferred<ContextPreview>()
    const second = deferred<ContextPreview>()
    const third = deferred<ContextPreview>()
    vi.spyOn(api, 'composeEffectiveContext')
      .mockReturnValueOnce(first.promise)
      .mockReturnValueOnce(second.promise)
      .mockReturnValueOnce(third.promise)
    render(<App api={api} />)
    await openView('Effective Context')
    await waitFor(() => expect(api.composeEffectiveContext).toHaveBeenCalledTimes(1))

    fireEvent.change(screen.getByRole('combobox', { name: 'Effective Context scope' }), {
      target: { value: MOCK_GLOBAL_SCOPE_ID },
    })
    expect(screen.queryByTestId('exact-rendered-markdown')).not.toBeInTheDocument()
    await waitFor(() => expect(api.composeEffectiveContext).toHaveBeenCalledTimes(2))
    fireEvent.change(screen.getByRole('combobox', { name: 'Destination adapter' }), {
      target: { value: 'adapter-codex' },
    })
    await waitFor(() => expect(api.composeEffectiveContext).toHaveBeenCalledTimes(3))
    first.resolve(oldPreview)
    second.resolve({
      ...currentPreview,
      destinationAdapter: 'adapter-daemon',
      renderedMarkdown: 'STALE GLOBAL DAEMON OUTPUT',
    })
    await Promise.resolve()
    expect(screen.queryByText('STALE PROJECT OUTPUT')).not.toBeInTheDocument()
    expect(screen.queryByText('STALE GLOBAL DAEMON OUTPUT')).not.toBeInTheDocument()
    third.resolve(currentPreview)

    expect(await screen.findByText('CURRENT GLOBAL CODEX OUTPUT')).toBeInTheDocument()
    expect(screen.queryByText('STALE PROJECT OUTPUT')).not.toBeInTheDocument()
  })

  it('binds Search results and Enter activation to the latest query request', async () => {
    const api = createDesktopApi({ forceMock: true })
    const fixture = createDesktopApi({ forceMock: true })
    const staleResults = await fixture.searchIndex('focused testing')
    const currentResults = await fixture.searchIndex('tool preferences')
    const first = deferred<typeof staleResults>()
    const second = deferred<typeof currentResults>()
    vi.spyOn(api, 'searchIndex')
      .mockReturnValueOnce(first.promise)
      .mockReturnValueOnce(second.promise)
    render(<App api={api} />)
    await openView('Search')
    const input = await screen.findByRole('searchbox', { name: 'Search local context' })

    fireEvent.change(input, { target: { value: 'focused testing' } })
    await waitFor(() => expect(api.searchIndex).toHaveBeenCalledTimes(1))
    fireEvent.change(input, { target: { value: 'tool preferences' } })
    expect(screen.queryByText(/Focused testing/)).not.toBeInTheDocument()
    fireEvent.keyDown(input, { key: 'Enter' })
    expect(screen.getByRole('heading', { name: 'Search' })).toBeInTheDocument()

    first.resolve(staleResults)
    await waitFor(() => expect(api.searchIndex).toHaveBeenCalledTimes(2))
    expect(screen.queryByText(/Focused testing/)).not.toBeInTheDocument()
    second.resolve(currentResults)
    await screen.findByRole('button', {
      name: /Repository workflow \/ Tool preferences/i,
    })
    fireEvent.keyDown(input, { key: 'Enter' })

    expect(await screen.findByRole('heading', { name: 'Library' })).toBeInTheDocument()
    expect(screen.getByRole('textbox', { name: 'Entry key' })).toHaveValue(
      'tool-preferences',
    )
  })

  it('keeps cross-scope selection, draft, persistence, and save input atomic', async () => {
    const api = createDesktopApi({ forceMock: true })
    const persistScope = vi.spyOn(api, 'setSelectedScope')
    const save = vi.spyOn(api, 'saveEntry')
    render(<App api={api} />)
    await screen.findByRole('heading', { name: 'Library' })

    fireEvent.keyDown(window, { key: 'k', metaKey: true })
    const quickOpen = screen.getByRole('dialog', { name: 'Quick Open' })
    const input = within(quickOpen).getByRole('combobox', {
      name: 'Find context or a command',
    })
    fireEvent.change(input, { target: { value: 'Release checklist' } })
    fireEvent.keyDown(input, { key: 'Enter' })

    await waitFor(() =>
      expect(screen.getByRole('textbox', { name: 'Entry key' })).toHaveValue(
        'release-checklist',
      ),
    )
    expect(persistScope).toHaveBeenCalledWith('task:release-candidate')
    fireEvent.change(screen.getByRole('textbox', { name: 'Entry content' }), {
      target: { value: 'Task-scoped saved draft.' },
    })
    fireEvent.click(screen.getByRole('button', { name: 'Save entry' }))
    await waitFor(() => expect(save).toHaveBeenCalled())
    expect(save.mock.calls.at(-1)?.[0]).toMatchObject({
      id: 'entry-task-checklist',
      scopeId: 'task:release-candidate',
      packId: 'pack-task-release',
    })
  })

  it('runs dirty protection before archive and entry revert confirmations', async () => {
    const api = createDesktopApi({ forceMock: true })
    const archive = vi.spyOn(api, 'archiveEntry')
    const revert = vi.spyOn(api, 'revertEntryRevision')
    render(<App api={api} />)
    fireEvent.change(await screen.findByRole('textbox', { name: 'Entry content' }), {
      target: { value: 'Unsaved destructive guard draft.' },
    })

    fireEvent.click(screen.getByRole('button', { name: 'Archive…' }))
    let dirty = screen.getByRole('dialog', { name: 'Unsaved changes' })
    expect(archive).not.toHaveBeenCalled()
    expect(screen.queryByRole('dialog', { name: 'Archive this entry?' })).not.toBeInTheDocument()
    fireEvent.click(within(dirty).getByRole('button', { name: 'Stay' }))

    fireEvent.click(screen.getByRole('button', { name: /Revert to revision 6/ }))
    dirty = screen.getByRole('dialog', { name: 'Unsaved changes' })
    expect(revert).not.toHaveBeenCalled()
    expect(screen.queryByRole('dialog', { name: /Revert to revision 6/ })).not.toBeInTheDocument()
    fireEvent.click(within(dirty).getByRole('button', { name: 'Stay' }))
  })

  it('protects edited review content across selection, filters, Quick Open, and leaving Inbox', async () => {
    render(<App api={createDesktopApi({ forceMock: true })} />)
    await openView(/^Inbox/)
    fireEvent.click(await screen.findByRole('button', { name: 'Edit one' }))
    fireEvent.change(screen.getByRole('textbox', { name: 'Edited proposed content' }), {
      target: { value: 'Unsaved edited review content.' },
    })

    fireEvent.click(
      screen.getByRole('button', { name: /Add accessibility check.*strict_policy/i }),
    )
    let dirty = screen.getByRole('dialog', { name: 'Unsaved changes' })
    fireEvent.click(within(dirty).getByRole('button', { name: 'Stay' }))
    expect(
      screen.getByRole('heading', { name: 'Expand focused test guidance' }),
    ).toBeInTheDocument()
    expect(screen.getByRole('textbox', { name: 'Edited proposed content' })).toHaveValue(
      'Unsaved edited review content.',
    )

    fireEvent.change(screen.getByRole('combobox', { name: 'Source' }), {
      target: { value: 'spool' },
    })
    dirty = screen.getByRole('dialog', { name: 'Unsaved changes' })
    fireEvent.click(within(dirty).getByRole('button', { name: 'Discard' }))
    await screen.findByRole('heading', { name: 'Queued write from an offline adapter' })

    fireEvent.click(screen.getByRole('button', { name: 'Edit one' }))
    fireEvent.change(screen.getByRole('textbox', { name: 'Edited proposed content' }), {
      target: { value: 'Another unsaved review edit.' },
    })
    fireEvent.keyDown(window, { key: 'k', metaKey: true })
    const quickOpen = screen.getByRole('dialog', { name: 'Quick Open' })
    const quickInput = within(quickOpen).getByRole('combobox', {
      name: 'Find context or a command',
    })
    fireEvent.change(quickInput, { target: { value: '>open library' } })
    fireEvent.keyDown(quickInput, { key: 'Enter' })
    dirty = await screen.findByRole('dialog', { name: 'Unsaved changes' })
    fireEvent.click(within(dirty).getByRole('button', { name: 'Discard' }))
    expect(await screen.findByRole('heading', { name: 'Library' })).toBeInTheDocument()
    expect(screen.getByRole('textbox', { name: 'Entry key' })).toHaveValue(
      'release-checklist',
    )
  })

  it('keeps a successful save committed when dashboard refresh fails', async () => {
    const api = createDesktopApi({ forceMock: true })
    const originalLoad = api.loadDashboard.bind(api)
    let loadCount = 0
    vi.spyOn(api, 'loadDashboard').mockImplementation(() => {
      loadCount += 1
      return loadCount === 1
        ? originalLoad()
        : Promise.reject(
            new DesktopApiError({
              code: 'unavailable',
              message: 'refresh unavailable',
              retryable: true,
            }),
          )
    })
    const save = vi.spyOn(api, 'saveEntry')
    render(<App api={api} />)
    fireEvent.change(await screen.findByRole('textbox', { name: 'Entry content' }), {
      target: { value: 'Committed despite refresh failure.' },
    })
    fireEvent.click(screen.getByRole('button', { name: 'Save entry' }))

    expect(
      await screen.findByText(/Saved Focused testing.*displayed state could not refresh/i),
    ).toBeInTheDocument()
    expect(screen.getByRole('button', { name: 'Refresh view' })).toBeInTheDocument()
    expect(screen.getByRole('textbox', { name: 'Entry content' })).toHaveValue(
      'Committed despite refresh failure.',
    )
    expect(screen.getByRole('button', { name: 'Save entry' })).toBeDisabled()
    expect(save).toHaveBeenCalledOnce()
  })

  it('closes bundle confirmation after import succeeds even when refresh fails', async () => {
    const api = createDesktopApi({ forceMock: true })
    const originalLoad = api.loadDashboard.bind(api)
    let loadCount = 0
    vi.spyOn(api, 'loadDashboard').mockImplementation(() => {
      loadCount += 1
      return loadCount === 1
        ? originalLoad()
        : Promise.reject(
            new DesktopApiError({
              code: 'unavailable',
              message: 'refresh unavailable',
              retryable: true,
            }),
          )
    })
    const apply = vi.spyOn(api, 'applyBundleImport')
    render(<App api={api} />)
    await openView('Connections')
    fireEvent.click(await screen.findByRole('tab', { name: 'Privacy & Data' }))
    fireEvent.click(screen.getByRole('button', { name: 'Choose backup…' }))
    fireEvent.click(await screen.findByRole('button', { name: 'Import this backup…' }))
    const dialog = screen.getByRole('dialog', { name: 'Import this local backup?' })
    fireEvent.click(within(dialog).getByRole('button', { name: 'Import backup' }))

    await waitFor(() =>
      expect(screen.queryByRole('dialog', { name: 'Import this local backup?' })).not.toBeInTheDocument(),
    )
    expect(apply).toHaveBeenCalledOnce()
    expect(screen.getByRole('alert')).toHaveTextContent(
      /Backup import completed.*may be stale.*do not repeat the mutation/i,
    )
  })

  it('does not repeat completed onboarding when the workspace refresh fails', async () => {
    const api = createDesktopApi({ forceMock: true, seed: freshOnboardingSeed() })
    const originalLoad = api.loadDashboard.bind(api)
    let loadCount = 0
    vi.spyOn(api, 'loadDashboard').mockImplementation(() => {
      loadCount += 1
      return loadCount === 1
        ? originalLoad()
        : Promise.reject(
            new DesktopApiError({
              code: 'unavailable',
              message: 'refresh unavailable',
              retryable: true,
            }),
          )
    })
    const complete = vi.spyOn(api, 'completeOnboarding')
    render(<App api={api} />)
    await openFreshOnboardingSources()
    fireEvent.click(screen.getByRole('tab', { name: 'Manual entry' }))
    fireEvent.change(screen.getByRole('textbox', { name: 'Manual first entry' }), {
      target: { value: 'Durable onboarding body.' },
    })
    fireEvent.click(screen.getByRole('button', { name: 'Continue to policy' }))
    fireEvent.click(
      within(await screen.findByRole('dialog', { name: 'Unsaved changes' })).getByRole(
        'button',
        { name: 'Save' },
      ),
    )
    fireEvent.click(await screen.findByRole('button', { name: 'Save policy' }))
    await waitFor(() =>
      expect(screen.getByRole('button', { name: 'Continue' })).not.toBeDisabled(),
    )
    fireEvent.click(screen.getByRole('button', { name: 'Continue' }))
    fireEvent.click(await screen.findByRole('button', { name: 'Compose and finish' }))

    expect(
      await screen.findByText(/Onboarding is complete, but the workspace could not refresh/i),
    ).toBeInTheDocument()
    expect(screen.getByRole('button', { name: 'Refresh workspace' })).toBeInTheDocument()
    expect(complete).toHaveBeenCalledOnce()
  })

  it('announces exact daemon and spool repair outcomes', async () => {
    const api = createDesktopApi({ forceMock: true })
    const dashboard = await api.loadDashboard()
    vi.spyOn(api, 'restartDaemon').mockResolvedValue({
      action: 'restart',
      performed: false,
      message: 'Daemon ownership prevented a process restart.',
      diagnostics: dashboard.diagnostics,
    })
    render(<App api={api} />)
    await openView('Connections')

    fireEvent.click(
      await screen.findByRole('button', { name: 'Recheck or restart daemon' }),
    )
    expect(
      (await screen.findAllByText(
        /Daemon ownership prevented a process restart\. Performed: no\./i,
      )).length,
    ).toBeGreaterThan(0)

    fireEvent.click(screen.getByRole('button', { name: 'Retry queued writes' }))
    expect(
      (await screen.findAllByText(
        /Spool retry attempted 1; delivered 1; retained 0; errors 0\./i,
      )).length,
    ).toBeGreaterThan(0)
  })

  it('shows matching, incompatible, and legacy Context API compatibility', async () => {
    const matchingRender = render(
      <App api={createDesktopApi({ forceMock: true })} />,
    )
    await openView('Connections')
    let summary = document.querySelector<HTMLElement>('.diagnostic-summary')!
    expect(within(summary).getByText(/1 \/ expected 1/i)).toBeInTheDocument()
    expect(within(summary).getByText('compatible')).toBeInTheDocument()
    matchingRender.unmount()

    const mismatch = cloneMockDashboard()
    mismatch.diagnostics = {
      ...mismatch.diagnostics,
      overallState: 'incompatible',
      apiVersion: 2,
      expectedApiVersion: 1,
    }
    const firstRender = render(
      <App api={createDesktopApi({ forceMock: true, seed: mismatch })} />,
    )
    await openView('Connections')
    summary = document.querySelector<HTMLElement>('.diagnostic-summary')!
    expect(within(summary).getByText(/2 \/ expected 1/i)).toBeInTheDocument()
    expect(within(summary).getByText('incompatible')).toBeInTheDocument()
    firstRender.unmount()

    const legacy = cloneMockDashboard()
    legacy.diagnostics = {
      ...legacy.diagnostics,
      overallState: 'degraded',
      apiVersion: null,
      expectedApiVersion: 1,
    }
    render(<App api={createDesktopApi({ forceMock: true, seed: legacy })} />)
    await openView('Connections')
    summary = document.querySelector<HTMLElement>('.diagnostic-summary')!
    expect(within(summary).getByText(/legacy \/ unknown \/ expected 1/i)).toBeInTheDocument()
    expect(within(summary).getByText('legacy · degraded')).toBeInTheDocument()
  })

  it('shows unavailable privacy counts without presenting zeroes as authoritative', async () => {
    const seed = cloneMockDashboard()
    seed.privacy = {
      ...seed.privacy,
      counts: {
        packs: 0,
        entries: 0,
        reviews: 0,
        runs: 0,
        spoolBacklog: 1,
      },
      countsAvailable: false,
      countsSource: undefined,
    }
    render(<App api={createDesktopApi({ forceMock: true, seed })} />)
    await openView('Connections')
    fireEvent.click(await screen.findByRole('tab', { name: 'Privacy & Data' }))

    const countCard = screen
      .getByRole('heading', { name: 'Local counts' })
      .closest<HTMLElement>('.settings-card')!
    expect(within(countCard).getByText('counts unavailable')).toBeInTheDocument()
    expect(within(countCard).getAllByText('Unavailable')).toHaveLength(4)
    expect(within(countCard).getByText('Record counts are currently unavailable')).toBeInTheDocument()
  })
})
