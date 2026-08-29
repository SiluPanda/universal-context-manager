import { useEffect, useMemo, useRef, useState } from 'react'
import type { DesktopApi } from '../api/desktopApi'
import { friendlyDesktopError, normalizeDesktopError } from '../api/desktopApi'
import type {
  ContextEntry,
  DashboardSnapshot,
  PathGrantSelection,
  ProjectRegistration,
  ReviewMode,
  SourceImportApplyResult,
  SourceImportPreviewResult,
} from '../types'
import {
  ConfirmationDialog,
  DirtyDecisionDialog,
  EmptyState,
  StatusPill,
} from './Common'

const steps = ['Welcome', 'Project', 'Sources', 'Policy', 'Finish']

const policyCopy: Record<ReviewMode, string> = {
  strict:
    'Every non-duplicate agent proposal waits for review. Global and locked changes still wait; recognized secrets are rejected.',
  balanced:
    'New project and task entries can apply directly. Conflicts, global changes, and locked changes wait; recognized secrets are rejected.',
  fast:
    'Project and task conflicts can apply directly. Global and locked changes still wait; recognized secrets are rejected.',
}

interface ManualDraft {
  title: string
  body: string
  tags: string
}

interface ManualEntryIdentity {
  id: string
  scopeId: string
  packId: string
  packName: string
  key: string
}

const emptyManual: ManualDraft = {
  title: 'Project operating notes',
  body: '',
  tags: 'onboarding, project',
}

function manualFingerprint(draft: ManualDraft) {
  return `${draft.title}\u0000${draft.body}\u0000${draft.tags}`
}

function onboardingInputFingerprint(
  registration: ProjectRegistration | null,
  selectedPaths: Set<string>,
  policyMode: ReviewMode,
  sourceMode: 'import' | 'manual' | 'existing',
) {
  return JSON.stringify({
    destinationScopeId: registration?.scopeId ?? '',
    projectPath: registration?.normalizedPath ?? '',
    selectedPaths: [...selectedPaths].sort(),
    policyMode,
    sourceMode,
  })
}

function importOutcome(result: SourceImportApplyResult | null) {
  if (!result) return 'not_applied'
  if (
    result.rejectedCount > 0 &&
    result.appliedCount === 0 &&
    result.pendingCount === 0 &&
    result.skippedCount === 0
  ) {
    return 'rejected'
  }
  if (result.pendingCount > 0) return 'review_required'
  if (result.appliedCount > 0) return 'applied'
  if (result.skippedCount > 0) return 'no_changes'
  return 'not_applied'
}

export function Onboarding({
  api,
  snapshot,
  onComplete,
  onAnnounce,
  onError,
}: {
  api: DesktopApi
  snapshot: DashboardSnapshot
  onComplete: () => Promise<void>
  onAnnounce: (message: string) => void
  onError: (message: string) => void
}) {
  const resumePath = snapshot.onboarding.lastProjectPath ?? snapshot.settings.lastProjectPath
  const [step, setStep] = useState(resumePath ? 1 : 0)
  const [registration, setRegistration] = useState<ProjectRegistration | null>(null)
  const [selectedPaths, setSelectedPaths] = useState<Set<string>>(new Set())
  const [sourceMode, setSourceMode] = useState<'import' | 'manual' | 'existing'>('import')
  const [manualDraft, setManualDraft] = useState<ManualDraft>(emptyManual)
  const [manualBaseline, setManualBaseline] = useState(manualFingerprint(emptyManual))
  const [manualEntry, setManualEntry] = useState<ManualEntryIdentity | null>(null)
  const [policyMode, setPolicyMode] = useState<ReviewMode>(
    snapshot.reviewPolicy?.mode ?? snapshot.settings.reviewMode,
  )
  const [policySaved, setPolicySaved] = useState(false)
  const [importPreview, setImportPreview] = useState<SourceImportPreviewResult | null>(null)
  const [applyResult, setApplyResult] = useState<SourceImportApplyResult | null>(null)
  const [composition, setComposition] = useState<
    Awaited<ReturnType<DesktopApi['composeEffectiveContext']>> | null
  >(null)
  const [completionIssue, setCompletionIssue] = useState('')
  const [completionCommitted, setCompletionCommitted] = useState(false)
  const [busyKey, setBusyKey] = useState('')
  const [confirm, setConfirm] = useState<{
    title: string
    description: string
    label: string
    action: () => Promise<void>
  } | null>(null)
  const [confirmBusy, setConfirmBusy] = useState(false)
  const [pendingStep, setPendingStep] = useState<number | null>(null)
  const registrationRef = useRef<ProjectRegistration | null>(null)
  const selectedPathsRef = useRef<Set<string>>(new Set())
  const sourceGrantRef = useRef<PathGrantSelection | null>(null)
  const policyModeRef = useRef(policyMode)
  const sourceModeRef = useRef(sourceMode)
  const manualEntryRef = useRef<ManualEntryIdentity | null>(null)
  const inputGenerationRef = useRef(0)
  const operationGenerationRef = useRef(0)
  const registrationRequestRef = useRef(0)

  const manualDirty =
    sourceMode === 'manual' && manualFingerprint(manualDraft) !== manualBaseline
  const readableSources =
    registration?.instructionSources.filter((source) => source.readable) ?? []
  const canPreview = Boolean(registration && selectedPaths.size > 0)
  const adapterWarning = snapshot.diagnostics.overallState !== 'healthy'
  const currentImportOutcome = importOutcome(applyResult)

  function currentInputFingerprint() {
    return onboardingInputFingerprint(
      registrationRef.current,
      selectedPathsRef.current,
      policyModeRef.current,
      sourceModeRef.current,
    )
  }

  function invalidateAsyncInputs() {
    inputGenerationRef.current += 1
    operationGenerationRef.current += 1
    setBusyKey('')
  }

  function operationIsCurrent(
    generation: number,
    fingerprint: string,
    operation?: number,
  ) {
    return (
      generation === inputGenerationRef.current &&
      fingerprint === currentInputFingerprint() &&
      (operation === undefined || operation === operationGenerationRef.current)
    )
  }

  function updateSourceMode(mode: 'import' | 'manual' | 'existing') {
    if (sourceModeRef.current === mode) return
    invalidateAsyncInputs()
    sourceModeRef.current = mode
    setSourceMode(mode)
  }

  function commitRegistration(result: ProjectRegistration) {
    invalidateAsyncInputs()
    registrationRef.current = result
    setRegistration(result)
    const paths = new Set(
      result.instructionSources.filter((source) => source.readable).map((source) => source.path),
    )
    selectedPathsRef.current = paths
    setSelectedPaths(paths)
    sourceGrantRef.current = null
    const mode = result.durable ? 'existing' : paths.size > 0 ? 'import' : 'manual'
    sourceModeRef.current = mode
    setSourceMode(mode)
    manualEntryRef.current = null
    setManualEntry(null)
    setManualDraft(emptyManual)
    setManualBaseline(manualFingerprint(emptyManual))
    setPolicySaved(false)
    setImportPreview(null)
    setApplyResult(null)
    setComposition(null)
    setCompletionIssue('')
    setCompletionCommitted(false)
  }

  useEffect(() => {
    function handleBeforeUnload(event: BeforeUnloadEvent) {
      if (!manualDirty) return
      event.preventDefault()
      event.returnValue = ''
    }
    window.addEventListener('beforeunload', handleBeforeUnload)
    return () => window.removeEventListener('beforeunload', handleBeforeUnload)
  }, [manualDirty])

  function invalidatePreview(message?: string) {
    if (importPreview && message) onAnnounce(message)
    setImportPreview(null)
    setApplyResult(null)
    setComposition(null)
  }

  function requestStep(nextStep: number) {
    if (manualDirty) {
      setPendingStep(nextStep)
      return
    }
    setStep(nextStep)
  }

  async function registerPath(path: string, grantToken: string) {
    const request = ++registrationRequestRef.current
    invalidateAsyncInputs()
    const operation = ++operationGenerationRef.current
    try {
      setBusyKey('register')
      const result = await api.registerProject(path, grantToken)
      if (request !== registrationRequestRef.current) return
      commitRegistration(result)
      setStep(2)
      const paths = result.instructionSources.filter((source) => source.readable)
      onAnnounce(
        paths.length > 0
          ? `Registered ${result.label} and detected ${paths.length} supported instruction source${paths.length === 1 ? '' : 's'}.`
          : `Registered ${result.label}. No supported instruction files were detected; create a manual first entry.`,
      )
    } catch (error) {
      if (request === registrationRequestRef.current) {
        onError(friendlyDesktopError(error))
      }
    } finally {
      if (operation === operationGenerationRef.current) setBusyKey('')
    }
  }

  async function chooseProject() {
    const generation = inputGenerationRef.current
    const fingerprint = currentInputFingerprint()
    const operation = ++operationGenerationRef.current
    try {
      setBusyKey('project-dialog')
      const selection = await api.selectProjectDirectory()
      if (!operationIsCurrent(generation, fingerprint, operation)) return
      if (!selection) {
        onAnnounce('Project folder selection cancelled.')
        return
      }
      const path = selection.paths[0]
      if (!path || selection.purpose !== 'project_registration') {
        onError('The native dialog returned an invalid project authorization. Choose again.')
        return
      }
      await registerPath(path, selection.grantToken)
    } catch (error) {
      if (operationIsCurrent(generation, fingerprint, operation)) {
        onError(friendlyDesktopError(error))
      }
    } finally {
      if (operation === operationGenerationRef.current) setBusyKey('')
    }
  }

  async function addSourceFiles(): Promise<PathGrantSelection | null> {
    const generation = inputGenerationRef.current
    const fingerprint = currentInputFingerprint()
    const operation = ++operationGenerationRef.current
    try {
      setBusyKey('source-dialog')
      const selection = await api.selectSourceImportFiles()
      if (!operationIsCurrent(generation, fingerprint, operation)) return null
      if (!selection) {
        onAnnounce('Instruction file selection cancelled.')
        return null
      }
      if (
        selection.purpose !== 'source_import_preview' ||
        selection.paths.length === 0
      ) {
        onError('The native dialog returned an invalid source authorization. Choose again.')
        return null
      }
      invalidateAsyncInputs()
      const next = new Set(selection.paths)
      selectedPathsRef.current = next
      setSelectedPaths(next)
      sourceGrantRef.current = selection
      invalidatePreview('Source selection changed. The previous preview is no longer valid.')
      return selection
    } catch (error) {
      if (operationIsCurrent(generation, fingerprint, operation)) {
        onError(friendlyDesktopError(error))
      }
      return null
    } finally {
      if (operation === operationGenerationRef.current) setBusyKey('')
    }
  }

  async function previewImport() {
    let grant = sourceGrantRef.current
    const selected = [...selectedPathsRef.current]
    if (
      !grant ||
      grant.paths.length !== selected.length ||
      grant.paths.some((path, index) => path !== selected[index])
    ) {
      grant = await addSourceFiles()
      if (!grant) return null
    }
    const currentRegistration = registrationRef.current
    const currentPaths = [...grant.paths]
    if (!currentRegistration || currentPaths.length === 0) return null
    const generation = inputGenerationRef.current
    const fingerprint = currentInputFingerprint()
    if (!operationIsCurrent(generation, fingerprint)) return null
    const operation = ++operationGenerationRef.current
    sourceGrantRef.current = null
    try {
      setBusyKey('preview')
      const result = await api.previewSourceImport({
        paths: currentPaths,
        grantToken: grant.grantToken,
        destinationScopeId: currentRegistration.scopeId,
        packName: 'Imported instructions',
        sourceKind: 'auto',
        actor: 'desktop-onboarding',
      })
      if (!operationIsCurrent(generation, fingerprint, operation)) return null
      if (!result.previewFingerprint.trim()) {
        setImportPreview(null)
        onError(
          'The backend did not return an authoritative import fingerprint. Nothing was imported; refresh the preview before continuing.',
        )
        return null
      }
      if (!result.applyGrantToken.trim()) {
        setImportPreview(null)
        onError(
          'The backend did not return a one-time apply authorization. Nothing was imported; choose and preview the files again.',
        )
        return null
      }
      setImportPreview(result)
      setApplyResult(null)
      onAnnounce(
        `Import preview ready: ${result.candidates.length} candidate${result.candidates.length === 1 ? '' : 's'}, ${result.conflicts} conflict${result.conflicts === 1 ? '' : 's'}.`,
      )
      return result
    } catch (error) {
      if (operationIsCurrent(generation, fingerprint, operation)) {
        onError(friendlyDesktopError(error))
      }
      return null
    } finally {
      if (operation === operationGenerationRef.current) setBusyKey('')
    }
  }

  async function savePolicy() {
    const generation = inputGenerationRef.current
    const fingerprint = currentInputFingerprint()
    const mode = policyModeRef.current
    const modeAtStart = sourceModeRef.current
    const operation = ++operationGenerationRef.current
    try {
      setBusyKey('policy')
      const policy = await api.setReviewPolicy({
        mode,
        actor: 'desktop-onboarding',
        note: 'Selected during first-run onboarding.',
      })
      if (!operationIsCurrent(generation, fingerprint, operation)) return
      setPolicySaved(true)
      if (modeAtStart === 'import') {
        setPolicySaved(false)
        const refreshed = await previewImport()
        if (!refreshed) {
          return
        }
        setPolicySaved(true)
      }
      onAnnounce(`Review policy saved as ${policy.mode}.`)
    } catch (error) {
      if (operationIsCurrent(generation, fingerprint, operation)) {
        onError(friendlyDesktopError(error))
      }
    } finally {
      if (operation === operationGenerationRef.current) setBusyKey('')
    }
  }

  async function saveManualEntry() {
    const currentRegistration = registrationRef.current
    if (!currentRegistration || !manualDraft.body.trim()) return false
    const generation = inputGenerationRef.current
    const fingerprint = currentInputFingerprint()
    const operation = ++operationGenerationRef.current
    try {
      setBusyKey('manual-save')
      const packs = await api.listPacks(currentRegistration.scopeId)
      if (!operationIsCurrent(generation, fingerprint, operation)) return false
      let identity =
        manualEntryRef.current?.scopeId === currentRegistration.scopeId
          ? manualEntryRef.current
          : null
      if (!identity) {
        const scopedEntries = await api.listEntries(currentRegistration.scopeId)
        if (!operationIsCurrent(generation, fingerprint, operation)) return false
        const existing = scopedEntries.find(
          (entry) => entry.key === 'manual-context' && entry.status === 'active',
        )
        if (existing) {
          identity = {
            id: existing.id,
            scopeId: existing.scopeId,
            packId: existing.packId,
            packName: existing.packName,
            key: existing.key,
          }
        }
      }
      const firstPack = identity
        ? packs.find((pack) => pack.id === identity.packId)
        : packs.find((pack) => pack.status !== 'draft')
      const saved: ContextEntry = await api.saveEntry({
        id: identity?.id,
        scopeId: currentRegistration.scopeId,
        packId: identity?.packId ?? firstPack?.id,
        packName:
          identity?.packName ??
          firstPack?.name ??
          (manualDraft.title.trim() || 'Project operating notes'),
        key: identity?.key ?? 'manual-context',
        title: manualDraft.title.trim() || 'Project operating notes',
        kind: 'instruction',
        format: 'markdown',
        body: manualDraft.body,
        tags: manualDraft.tags
          .split(',')
          .map((tag) => tag.trim())
          .filter(Boolean),
        locked: false,
        actor: 'desktop-onboarding',
        note: identity
          ? 'Updated the manual onboarding entry.'
          : 'Created the manual onboarding entry.',
      })
      if (!operationIsCurrent(generation, fingerprint, operation)) return true
      const nextIdentity = {
        id: saved.id,
        scopeId: saved.scopeId,
        packId: saved.packId,
        packName: saved.packName,
        key: saved.key,
      }
      manualEntryRef.current = nextIdentity
      setManualEntry(nextIdentity)
      setManualBaseline(manualFingerprint(manualDraft))
      invalidateAsyncInputs()
      setImportPreview(null)
      setApplyResult(null)
      setComposition(null)
      onAnnounce(identity ? 'Manual first entry updated locally.' : 'Manual first entry saved locally.')
      return true
    } catch (error) {
      onError(friendlyDesktopError(error))
      return false
    } finally {
      if (operation === operationGenerationRef.current) setBusyKey('')
    }
  }

  function requestApplyImport() {
    const currentRegistration = registrationRef.current
    if (!currentRegistration || !importPreview) return
    if (!importPreview.applyAllowed) {
      onError(
        'This backend preview is blocked and cannot be applied. Resolve its warnings and preview again.',
      )
      return
    }
    const generation = inputGenerationRef.current
    const fingerprint = currentInputFingerprint()
    const reviewedPreview = importPreview
    const reviewedPaths = [...selectedPathsRef.current]
    setConfirm({
      title: 'Apply this source import?',
      description:
        'The preview will be applied under the saved review policy. New entries may be durable immediately or may wait in Inbox.',
      label: 'Apply import',
      action: async () => {
        const operation = ++operationGenerationRef.current
        if (!operationIsCurrent(generation, fingerprint, operation)) {
          setImportPreview(null)
          setApplyResult(null)
          setPolicySaved(false)
          setStep(2)
          onError(
            'Project, source, destination, or policy selection changed. Nothing was imported; preview again.',
          )
          return
        }
        try {
          const result = await api.applySourceImport({
            paths: reviewedPaths,
            grantToken: reviewedPreview.applyGrantToken,
            destinationScopeId: currentRegistration.scopeId,
            packName: reviewedPreview.packName,
            sourceKind: 'auto',
            actor: 'desktop-onboarding',
            previewId: reviewedPreview.previewId,
            expectedPreviewFingerprint: reviewedPreview.previewFingerprint,
            confirmation: true,
          })
          if (!operationIsCurrent(generation, fingerprint, operation)) return
          invalidateAsyncInputs()
          setImportPreview(null)
          setApplyResult(result)
          const outcome = importOutcome(result)
          setCompletionIssue(
            outcome === 'rejected'
              ? 'The backend rejected every candidate. No import was applied, so onboarding cannot complete from this result.'
              : '',
          )
          onAnnounce(
            `Import result: ${result.appliedCount} applied, ${result.pendingCount} waiting for review, ${result.skippedCount} skipped, ${result.rejectedCount} rejected.`,
          )
        } catch (error) {
          if (!operationIsCurrent(generation, fingerprint, operation)) return
          const code = normalizeDesktopError(error).code
          if (
            code === 'conflict' ||
            code === 'path_grant_required' ||
            code === 'path_grant_invalid' ||
            code === 'path_grant_expired'
          ) {
            invalidateAsyncInputs()
            setImportPreview(null)
            setApplyResult(null)
            setPolicySaved(false)
            setStep(2)
            onError(
              'The source files, review policy, destination state, or one-time path authorization changed after preview. Nothing was imported; authorize and preview the selected sources again.',
            )
            return
          }
          throw error
        }
      },
    })
  }

  function requestApprovePending() {
    if (!applyResult?.affectedReviewIds.length) return
    setConfirm({
      title: 'Approve the pending imported entries?',
      description:
        'This bulk approval applies only to the imported review items listed by the backend. Results may be partial.',
      label: `Approve ${applyResult.affectedReviewIds.length}`,
      action: async () => {
        const result = await api.bulkReviewDecision({
          itemIds: applyResult.affectedReviewIds,
          decision: 'approve',
          confirmation: true,
          actor: 'desktop-onboarding',
          note: 'Approved during onboarding.',
        })
        if (result.stopped || result.results.some((item) => !item.success)) {
          onError(
            `${result.completed} of ${result.attempted} attempted imports were approved. Remaining items stay in Inbox.`,
          )
        } else {
          onAnnounce(`${result.completed} imported review items approved.`)
        }
        invalidateAsyncInputs()
        setImportPreview(null)
        setApplyResult({
          ...applyResult,
          pendingCount: Math.max(0, applyResult.pendingCount - result.completed),
          affectedReviewIds: applyResult.affectedReviewIds.filter(
            (id) => !result.results.some((item) => item.itemId === id && item.success),
          ),
        })
      },
    })
  }

  async function composeAndComplete() {
    const currentRegistration = registrationRef.current
    if (!currentRegistration) return
    if (completionCommitted) {
      try {
        setBusyKey('complete')
        await onComplete()
      } catch (error) {
        setCompletionIssue(
          `Onboarding is complete, but the workspace could not refresh. ${friendlyDesktopError(error)}`,
        )
      } finally {
        setBusyKey('')
      }
      return
    }
    if (sourceModeRef.current === 'import' && currentImportOutcome === 'rejected') {
      setCompletionIssue(
        'The fully rejected import did not create durable context. Preview different sources or create a manual entry.',
      )
      return
    }
    try {
      setBusyKey('complete')
      setCompletionIssue('')
      const entries = await api.listEntries(currentRegistration.scopeId)
      if (!entries.some((entry) => entry.status === 'active')) {
        setCompletionIssue(
          'Create, restore, or approve at least one durable active entry before composing again. Nothing was marked complete.',
        )
        return
      }
      const result = await api.composeEffectiveContext({
        scopeId: currentRegistration.scopeId,
        destinationAdapter:
          snapshot.adapters.find((adapter) => adapter.enabled)?.id ?? 'generic',
      })
      setComposition(result)
      if (result.metrics.includedEntries === 0) {
        setCompletionIssue(
          'The selected scope composed no active durable entries. Nothing was marked complete; restore, approve, or create context and compose again.',
        )
        return
      }
      try {
        await api.completeOnboarding()
      } catch (error) {
        const typed = normalizeDesktopError(error)
        setCompletionIssue(
          typed.code === 'invalid_input'
            ? 'The backend kept onboarding incomplete because the selected scope did not compose active durable context. Nothing was marked complete. Review, restore, or approve an active entry, then compose again.'
            : `The backend could not complete final verification. ${friendlyDesktopError(error)}`,
        )
        return
      }
      setCompletionCommitted(true)
      onAnnounce('Onboarding complete. Effective Context composed successfully.')
      try {
        await onComplete()
      } catch (error) {
        setCompletionIssue(
          `Onboarding is complete, but the workspace could not refresh. ${friendlyDesktopError(error)}`,
        )
      }
    } catch (error) {
      setCompletionIssue(
        `Final onboarding verification could not complete. ${friendlyDesktopError(error)}`,
      )
    } finally {
      setBusyKey('')
    }
  }

  const sourceSummary = useMemo(() => {
    if (importPreview) {
      return `${importPreview.candidates.length} candidates · ${importPreview.conflicts} conflicts · ${importPreview.duplicates} duplicates`
    }
    if (applyResult) {
      return `${applyResult.candidateCount} candidates · ${applyResult.appliedCount} applied · ${applyResult.pendingCount} pending · ${applyResult.skippedCount} skipped · ${applyResult.rejectedCount} rejected`
    }
    return ''
  }, [applyResult, importPreview])

  return (
    <main className="onboarding-shell">
      <aside className="onboarding-rail">
        <div className="onboarding-brand">
          <span className="brand-mark" aria-hidden="true">
            UC
          </span>
          <div>
            <strong>Universal Context Manager</strong>
            <small>Local editorial control room</small>
          </div>
        </div>
        <ol className="onboarding-steps" aria-label="Onboarding progress">
          {steps.map((label, index) => (
            <li
              key={label}
              className={
                index === step ? 'is-current' : index < step ? 'is-complete' : undefined
              }
            >
              <button
                type="button"
                disabled={index > step}
                aria-current={index === step ? 'step' : undefined}
                onClick={() => requestStep(index)}
              >
                <span>{index < step ? '✓' : index + 1}</span>
                <strong>{label}</strong>
              </button>
            </li>
          ))}
        </ol>
        <div className="onboarding-local-note">
          <span aria-hidden="true">⌂</span>
          <p>
            {snapshot.privacy.localOnlyStatement}{' '}
            {snapshot.privacy.downstreamAdapterDisclosure}
          </p>
        </div>
      </aside>

      <section className="onboarding-stage" data-dialog-fallback tabIndex={-1}>
        <header className="onboarding-stage__header">
          <p className="eyebrow">
            Step {step + 1} of {steps.length}
          </p>
          <StatusPill label={snapshot.diagnostics.overallState} />
        </header>

        {step === 0 ? (
          <div className="onboarding-panel onboarding-intro">
            <div className="onboarding-kicker">A local memory desk for agent work</div>
            <h1>Know what is stored.<br />Choose what is composed.</h1>
            <p className="onboarding-lede">
              {snapshot.privacy.localOnlyStatement}{' '}
              {snapshot.privacy.downstreamAdapterDisclosure}
            </p>
            <div className="boundary-diagram" aria-label="Local storage and downstream boundary">
              <article>
                <span>01</span>
                <strong>Local store</strong>
                <p>{snapshot.privacy.localOnlyStatement}</p>
              </article>
              <div aria-hidden="true">→</div>
              <article>
                <span>02</span>
                <strong>Exact composition</strong>
                <p>Backend-owned ordering, Markdown, exclusions, and metrics.</p>
              </article>
              <div aria-hidden="true">→</div>
              <article>
                <span>03</span>
                <strong>Downstream harness</strong>
                <p>{snapshot.privacy.downstreamAdapterDisclosure}</p>
              </article>
            </div>
            <div className="onboarding-privacy-disclosures">
              <p>{snapshot.privacy.secretScanningStatement}</p>
              <p>{snapshot.privacy.applicationEncryptionBoundary}</p>
            </div>
            <div className="local-path-card">
              <span>Local data path</span>
              <code>{snapshot.privacy.dataPath}</code>
            </div>
            <footer className="onboarding-actions">
              <span className="onboarding-privacy-flags">
                <StatusPill
                  label={`app telemetry ${
                    snapshot.privacy.telemetryEnabled ? 'enabled' : 'disabled'
                  }`}
                />
                <StatusPill
                  label={`app network egress ${
                    snapshot.privacy.networkEgressEnabled ? 'enabled' : 'disabled'
                  }`}
                />
              </span>
              <button type="button" className="primary-button" onClick={() => setStep(1)}>
                Begin setup
              </button>
            </footer>
          </div>
        ) : null}

        {step === 1 ? (
          <div className="onboarding-panel">
            <p className="onboarding-kicker">Register a durable repository scope</p>
            <h1>Choose the project you want to remember.</h1>
            <p className="onboarding-lede">
              The native folder chooser supplies a path to the backend. The backend normalizes it,
              creates the project scope, and detects supported instruction sources.
            </p>
            {registration ? (
              <div className="project-registration-card">
                <div>
                  <StatusPill label={registration.selected ? 'registered' : 'not selected'} />
                  <h2>{registration.label}</h2>
                  <code>{registration.normalizedPath}</code>
                </div>
                <dl>
                  <div>
                    <dt>Scope</dt>
                    <dd>This repository</dd>
                  </div>
                  <div>
                    <dt>Detected sources</dt>
                    <dd>{registration.instructionSources.length}</dd>
                  </div>
                  <div>
                    <dt>Existing durable context</dt>
                    <dd>{registration.durable ? 'yes' : 'no'}</dd>
                  </div>
                </dl>
              </div>
            ) : resumePath ? (
              <div className="resume-card">
                <p>Resume the previously selected project.</p>
                <code>{resumePath}</code>
                <button
                  type="button"
                  className="secondary-button"
                  disabled={busyKey === 'project-dialog'}
                  onClick={chooseProject}
                >
                  {busyKey === 'project-dialog'
                    ? 'Opening…'
                    : 'Reauthorize this project folder…'}
                </button>
              </div>
            ) : (
              <EmptyState
                title="No project registered yet"
                body="Choose a local project folder. UCM does not scan outside the selected folder."
              />
            )}
            <footer className="onboarding-actions">
              <button type="button" className="secondary-button" onClick={() => requestStep(0)}>
                Back
              </button>
              <div className="button-row">
                <button
                  type="button"
                  className="secondary-button"
                  disabled={busyKey === 'project-dialog'}
                  onClick={chooseProject}
                >
                  {busyKey === 'project-dialog' ? 'Opening…' : 'Choose project folder…'}
                </button>
                {registration ? (
                  <button type="button" className="primary-button" onClick={() => setStep(2)}>
                    Continue
                  </button>
                ) : null}
              </div>
            </footer>
          </div>
        ) : null}

        {step === 2 && registration ? (
          <div className="onboarding-panel">
            <p className="onboarding-kicker">Establish the first durable context</p>
            <h1>Import what already exists—or write one entry.</h1>
            <p className="onboarding-lede">
              A preview never writes data. File, destination scope, and policy changes invalidate
              it.
            </p>
            <div className="source-mode-tabs" role="tablist" aria-label="First entry method">
              <button
                type="button"
                role="tab"
                aria-selected={sourceMode === 'import'}
                className={sourceMode === 'import' ? 'is-selected' : ''}
                disabled={readableSources.length === 0 && selectedPaths.size === 0}
                onClick={() => updateSourceMode('import')}
              >
                Import sources
              </button>
              <button
                type="button"
                role="tab"
                aria-selected={sourceMode === 'manual'}
                className={sourceMode === 'manual' ? 'is-selected' : ''}
                onClick={() => updateSourceMode('manual')}
              >
                Manual entry
              </button>
              {registration.durable ? (
                <button
                  type="button"
                  role="tab"
                  aria-selected={sourceMode === 'existing'}
                  className={sourceMode === 'existing' ? 'is-selected' : ''}
                  onClick={() => updateSourceMode('existing')}
                >
                  Use existing context
                </button>
              ) : null}
            </div>

            {sourceMode === 'import' ? (
              <div className="source-import-layout">
                <section aria-labelledby="detected-source-heading">
                  <header className="subsection-heading">
                    <div>
                      <p className="eyebrow">Backend detection</p>
                      <h2 id="detected-source-heading">Supported instruction sources</h2>
                    </div>
                    <button
                      type="button"
                      className="secondary-button"
                      disabled={busyKey === 'source-dialog'}
                      onClick={addSourceFiles}
                    >
                      Add files…
                    </button>
                  </header>
                  {registration.instructionSources.length === 0 &&
                  selectedPaths.size === 0 ? (
                    <EmptyState
                      title="No supported files detected"
                      body="Choose Manual entry to create the first durable context."
                    />
                  ) : (
                    <ul className="source-checklist">
                      {[
                        ...registration.instructionSources,
                        ...[...selectedPaths]
                          .filter(
                            (path) =>
                              !registration.instructionSources.some(
                                (source) => source.path === path,
                              ),
                          )
                          .map((path) => ({
                            path,
                            relativePath: path,
                            sourceKind: 'auto' as const,
                            readable: true,
                          })),
                      ].map((source) => (
                        <li key={source.path}>
                          <label>
                            <input
                              type="checkbox"
                              checked={selectedPaths.has(source.path)}
                              disabled={!source.readable}
                              onChange={(event) => {
                                invalidateAsyncInputs()
                                sourceGrantRef.current = null
                                const next = new Set(selectedPathsRef.current)
                                if (event.target.checked) next.add(source.path)
                                else next.delete(source.path)
                                selectedPathsRef.current = next
                                setSelectedPaths(next)
                                invalidatePreview(
                                  'Source selection changed. The previous preview is no longer valid.',
                                )
                              }}
                            />
                            <span>
                              <strong>{source.relativePath}</strong>
                              <small>{source.sourceKind}</small>
                            </span>
                            <StatusPill label={source.readable ? 'readable' : 'unreadable'} />
                          </label>
                        </li>
                      ))}
                    </ul>
                  )}
                </section>
                <section className="onboarding-preview" aria-labelledby="source-preview-heading">
                  <header className="subsection-heading">
                    <div>
                      <p className="eyebrow">Read-only preview</p>
                      <h2 id="source-preview-heading">Import candidates</h2>
                    </div>
                    {importPreview ? <StatusPill label={importPreview.reviewMode} /> : null}
                  </header>
                  {importPreview ? (
                    <>
                      <p className="preview-summary">{sourceSummary}</p>
                      {importPreview.warnings.length > 0 ? (
                        <ul className="detail-list" aria-label="Import preview warnings">
                          {importPreview.warnings.map((warning) => (
                            <li key={warning}>{warning}</li>
                          ))}
                        </ul>
                      ) : null}
                      <ul>
                        {importPreview.candidates.map((candidate) => (
                          <li key={candidate.candidateIndex}>
                            <header>
                              <strong>{candidate.title ?? candidate.key}</strong>
                              <StatusPill label={candidate.disposition} />
                            </header>
                            <small>
                              {candidate.key} · {candidate.detectedSourceKind} · {candidate.format}
                            </small>
                            <pre>{candidate.renderedBody}</pre>
                          </li>
                        ))}
                      </ul>
                    </>
                  ) : (
                    <EmptyState
                      title="Preview required"
                      body="Select one or more readable sources, then ask the backend for a preview."
                    />
                  )}
                  <button
                    type="button"
                    className="primary-button full-width-button"
                    disabled={!canPreview || busyKey === 'preview'}
                    onClick={() => void previewImport()}
                  >
                    {busyKey === 'preview' ? 'Previewing…' : 'Preview selected sources'}
                  </button>
                </section>
              </div>
            ) : sourceMode === 'manual' ? (
              <div className="manual-onboarding-entry">
                <label>
                  <span>Entry title</span>
                  <input
                    value={manualDraft.title}
                    onChange={(event) =>
                      setManualDraft((current) => ({ ...current, title: event.target.value }))
                    }
                  />
                </label>
                <label>
                  <span>Markdown</span>
                  <textarea
                    aria-label="Manual first entry"
                    value={manualDraft.body}
                    onChange={(event) =>
                      setManualDraft((current) => ({ ...current, body: event.target.value }))
                    }
                    rows={12}
                    placeholder="Write the first durable instruction for this repository."
                  />
                </label>
                <label>
                  <span>Tags</span>
                  <input
                    value={manualDraft.tags}
                    onChange={(event) =>
                      setManualDraft((current) => ({ ...current, tags: event.target.value }))
                    }
                  />
                </label>
                {manualEntry ? (
                  <p className="saved-inline">
                    <StatusPill label="saved" /> Manual entry is durable.
                  </p>
                ) : null}
              </div>
            ) : (
              <section className="finish-card">
                <div>
                  <p className="eyebrow">Resumed setup</p>
                  <h2>Use existing durable context</h2>
                  <p>
                    The backend found active entries in this repository. You can keep them,
                    confirm policy, and compose before completing onboarding.
                  </p>
                </div>
                <StatusPill label="durable" />
              </section>
            )}

            <footer className="onboarding-actions">
              <button type="button" className="secondary-button" onClick={() => requestStep(1)}>
                Back
              </button>
              <button
                type="button"
                className="primary-button"
                disabled={
                  sourceMode === 'import'
                    ? !importPreview
                    : sourceMode === 'manual'
                      ? !manualDraft.body.trim()
                      : false
                }
                onClick={() => requestStep(3)}
              >
                Continue to policy
              </button>
            </footer>
          </div>
        ) : null}

        {step === 3 ? (
          <div className="onboarding-panel">
            <p className="onboarding-kicker">Choose the review gate</p>
            <h1>Decide how agent proposals become durable.</h1>
            <p className="onboarding-lede">
              Global and locked proposals always wait for review. Recognized secrets are always
              rejected before durable storage.
            </p>
            <div className="onboarding-policy-options">
              {(Object.keys(policyCopy) as ReviewMode[]).map((mode) => (
                <label key={mode} className={policyMode === mode ? 'is-selected' : ''}>
                  <input
                    type="radio"
                    name="onboarding-policy"
                    value={mode}
                    checked={policyMode === mode}
                    onChange={() => {
                      invalidateAsyncInputs()
                      policyModeRef.current = mode
                      setPolicyMode(mode)
                      setPolicySaved(false)
                      invalidatePreview(
                        'Policy changed. The import preview must be generated again.',
                      )
                    }}
                  />
                  <span className="policy-letter" aria-hidden="true">
                    {mode[0].toLocaleUpperCase()}
                  </span>
                  <span>
                    <strong>{mode}</strong>
                    <small>{policyCopy[mode]}</small>
                  </span>
                </label>
              ))}
            </div>
            {sourceMode === 'import' && !importPreview ? (
              <p className="policy-preview-note">
                Saving this policy will ask the backend for a fresh import preview under{' '}
                <strong>{policyMode}</strong>.
              </p>
            ) : null}
            <footer className="onboarding-actions">
              <button type="button" className="secondary-button" onClick={() => requestStep(2)}>
                Back
              </button>
              <div className="button-row">
                <button
                  type="button"
                  className="secondary-button"
                  disabled={busyKey === 'policy'}
                  onClick={savePolicy}
                >
                  {busyKey === 'policy' ? 'Saving…' : 'Save policy'}
                </button>
                <button
                  type="button"
                  className="primary-button"
                  disabled={!policySaved || (sourceMode === 'import' && !importPreview)}
                  onClick={() => setStep(4)}
                >
                  Continue
                </button>
              </div>
            </footer>
          </div>
        ) : null}

        {step === 4 && registration ? (
          <div className="onboarding-panel">
            <p className="onboarding-kicker">Create, compose, verify</p>
            <h1>Finish with durable context and exact output.</h1>
            <p className="onboarding-lede">
              Adapter health can warn, but it does not block completion. Onboarding completes only
              after an active entry exists and backend composition succeeds.
            </p>
            {adapterWarning ? (
              <div className="warning-callout">
                <h2>Connection warning</h2>
                <p>
                  Diagnostics report <strong>{snapshot.diagnostics.overallState}</strong>. You can
                  finish local onboarding now and repair adapters later in Connections.
                </p>
              </div>
            ) : null}

            {sourceMode === 'manual' ? (
              <section className="finish-card">
                <div>
                  <p className="eyebrow">Manual path</p>
                  <h2>{manualDraft.title}</h2>
                  <p>{manualEntry ? 'The first entry is durable.' : 'Create the first durable entry.'}</p>
                </div>
                {!manualEntry ? (
                  <button
                    type="button"
                    className="primary-button"
                    disabled={busyKey === 'manual-save' || !manualDraft.body.trim()}
                    onClick={saveManualEntry}
                  >
                    {busyKey === 'manual-save' ? 'Saving…' : 'Create manual entry'}
                  </button>
                ) : (
                  <StatusPill label="durable" />
                )}
              </section>
            ) : sourceMode === 'import' ? (
              <section className="finish-card finish-card--stacked">
                <header>
                  <div>
                    <p className="eyebrow">Import path</p>
                    <h2>{importPreview?.packName ?? applyResult?.packName}</h2>
                    <p>{sourceSummary}</p>
                  </div>
                  {!applyResult ? (
                    <button
                      type="button"
                      className="primary-button"
                      disabled={!importPreview?.applyAllowed}
                      onClick={requestApplyImport}
                    >
                      {importPreview?.applyAllowed
                        ? 'Apply selected import…'
                        : 'Import blocked by preview'}
                    </button>
                  ) : (
                    <StatusPill
                      label={
                        currentImportOutcome === 'review_required'
                          ? 'review required'
                          : currentImportOutcome === 'rejected'
                            ? 'rejected'
                            : currentImportOutcome === 'no_changes'
                              ? 'no changes'
                              : currentImportOutcome === 'applied'
                                ? 'applied'
                                : 'not applied'
                      }
                    />
                  )}
                </header>
                {applyResult ? (
                  <>
                    <dl className="import-result-ledger">
                      <div>
                        <dt>Applied</dt>
                        <dd>{applyResult.appliedCount}</dd>
                      </div>
                      <div>
                        <dt>Pending</dt>
                        <dd>{applyResult.pendingCount}</dd>
                      </div>
                      <div>
                        <dt>Skipped</dt>
                        <dd>{applyResult.skippedCount}</dd>
                      </div>
                      <div>
                        <dt>Rejected</dt>
                        <dd>{applyResult.rejectedCount}</dd>
                      </div>
                    </dl>
                    {applyResult.affectedReviewIds.length > 0 ? (
                      <button
                        type="button"
                        className="secondary-button"
                        onClick={requestApprovePending}
                      >
                        Approve pending imports…
                      </button>
                    ) : null}
                  </>
                ) : null}
              </section>
            ) : (
              <section className="finish-card">
                <div>
                  <p className="eyebrow">Existing durable context</p>
                  <h2>{registration.label}</h2>
                  <p>Ready for a fresh backend composition check.</p>
                </div>
                <StatusPill label="durable" />
              </section>
            )}

            {composition ? (
              <section className="composition-success">
                <span aria-hidden="true">✓</span>
                <div>
                  <strong>Effective Context composed</strong>
                  <p>
                    {composition.metrics.includedEntries} included entries ·{' '}
                    {composition.metrics.renderedBytes.toLocaleString()} rendered bytes ·{' '}
                    {composition.destinationAdapter}
                  </p>
                </div>
              </section>
            ) : null}

            {completionIssue ? (
              <section className="onboarding-blocked" role="alert">
                <span aria-hidden="true">!</span>
                <div>
                  <strong>Onboarding remains incomplete</strong>
                  <p>{completionIssue}</p>
                </div>
              </section>
            ) : null}

            <footer className="onboarding-actions">
              <button type="button" className="secondary-button" onClick={() => requestStep(3)}>
                Back
              </button>
              <button
                type="button"
                className="primary-button"
                disabled={
                  busyKey === 'complete' ||
                  (sourceMode === 'manual' && !manualEntry) ||
                  (sourceMode === 'import' &&
                    (!applyResult ||
                      currentImportOutcome === 'rejected' ||
                      currentImportOutcome === 'not_applied'))
                }
                onClick={composeAndComplete}
              >
                {busyKey === 'complete'
                  ? completionCommitted
                    ? 'Refreshing…'
                    : 'Composing…'
                  : completionCommitted
                    ? 'Refresh workspace'
                    : 'Compose and finish'}
              </button>
            </footer>
          </div>
        ) : null}
      </section>

      {confirm ? (
        <ConfirmationDialog
          title={confirm.title}
          description={confirm.description}
          confirmLabel={confirm.label}
          busy={confirmBusy}
          onCancel={() => setConfirm(null)}
          onConfirm={async () => {
            try {
              setConfirmBusy(true)
              await confirm.action()
              setConfirm(null)
            } catch (error) {
              onError(friendlyDesktopError(error))
            } finally {
              setConfirmBusy(false)
            }
          }}
        />
      ) : null}

      {pendingStep !== null ? (
        <DirtyDecisionDialog
          itemLabel="The manual first entry"
          busy={busyKey === 'manual-save'}
          onStay={() => setPendingStep(null)}
          onDiscard={() => {
            setManualDraft(emptyManual)
            setManualBaseline(manualFingerprint(emptyManual))
            setPendingStep(null)
            setStep(pendingStep)
          }}
          onSave={async () => {
            const saved = await saveManualEntry()
            if (saved) {
              setPendingStep(null)
              setStep(pendingStep)
            }
          }}
        />
      ) : null}
    </main>
  )
}
