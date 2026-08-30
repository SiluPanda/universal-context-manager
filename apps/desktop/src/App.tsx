import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import { createDesktopApi, desktopApi, friendlyDesktopError, type DesktopApi } from './api/desktopApi'
import { ConnectionsView } from './components/ConnectionsView'
import {
  ConfirmationDialog,
  DirtyDecisionDialog,
} from './components/Common'
import { EffectiveContextView } from './components/EffectiveContextView'
import { InboxView } from './components/InboxView'
import { LibraryView } from './components/LibraryView'
import { Onboarding } from './components/Onboarding'
import {
  type PrimaryView,
  QuickOpen,
  type QuickOpenItem,
  type QuickOpenTarget,
} from './components/QuickOpen'
import { SearchView } from './components/SearchView'
import {
  draftFromEntry,
  emptyEntryDraft,
  entryDraftToInput,
  isEntryDraftDirty,
  type EntryDraft,
} from './lib/entryDraft'
import {
  type ConfirmationRequest,
  formatTimestamp,
  scopeLayerLabel,
} from './lib/ui'
import { flattenWorkspace } from './lib/contextUtils'
import type {
  BulkReviewDecisionResult,
  ContextEntry,
  ContextPack,
  DashboardSnapshot,
  ReviewDecision,
  ReviewItem,
  RevisionEntry,
  SearchResult,
} from './types'
import './App.css'

interface AppProps {
  api?: DesktopApi
}

interface ReviewEditDraft {
  reviewId: string
  draft: string
  baseline: string
}

const navigation: Array<{
  id: PrimaryView
  label: string
  icon: PrimaryView
  description: string
}> = [
  { id: 'inbox', label: 'Inbox', icon: 'inbox', description: 'Review queued changes' },
  { id: 'library', label: 'Library', icon: 'library', description: 'Edit durable entries' },
  {
    id: 'effective',
    label: 'Effective Context',
    icon: 'effective',
    description: 'Inspect exact output',
  },
  { id: 'search', label: 'Search', icon: 'search', description: 'Find local records' },
  {
    id: 'connections',
    label: 'Connections',
    icon: 'connections',
    description: 'Health, policy, and privacy',
  },
]

function NavigationIcon({ name }: { name: PrimaryView }) {
  return (
    <svg viewBox="0 0 16 16" aria-hidden="true">
      {name === 'inbox' ? (
        <>
          <path d="M2.5 3.5h11v9h-11z" />
          <path d="M2.5 9h3l1.1 1.5h2.8L10.5 9h3" />
        </>
      ) : null}
      {name === 'library' ? (
        <>
          <path d="M3 3.25h10v9.5H3z" />
          <path d="M5.25 5.5h5.5M5.25 8h5.5M5.25 10.5h3.5" />
        </>
      ) : null}
      {name === 'effective' ? (
        <>
          <path d="M2.5 4h11M2.5 8h8M2.5 12h5" />
          <path d="m11 10.5 2.5 1.5-2.5 1.5z" />
        </>
      ) : null}
      {name === 'search' ? (
        <>
          <circle cx="7" cy="7" r="4.25" />
          <path d="m10.25 10.25 3 3" />
        </>
      ) : null}
      {name === 'connections' ? (
        <>
          <path d="M2.5 4h11M2.5 8h11M2.5 12h11" />
          <circle cx="5.25" cy="4" r="1.25" />
          <circle cx="10.75" cy="8" r="1.25" />
          <circle cx="7" cy="12" r="1.25" />
        </>
      ) : null}
    </svg>
  )
}

function firstEntryForScope(snapshot: DashboardSnapshot, scopeId: string) {
  return (
    snapshot.entries.find((entry) => entry.scopeId === scopeId && entry.status === 'active') ??
    snapshot.entries.find((entry) => entry.scopeId === scopeId)
  )
}

function selectedPackForNewEntry(snapshot: DashboardSnapshot, scopeId: string) {
  return snapshot.packs.find((pack) => pack.scopeId === scopeId)
}

function entryForReview(snapshot: DashboardSnapshot, review: ReviewItem) {
  return snapshot.entries.find(
    (entry) =>
      entry.scopeId === review.scopeId &&
      entry.packId === review.packId &&
      entry.key === review.entryKey,
  )
}

function App({ api = desktopApi }: AppProps) {
  const [loadState, setLoadState] = useState<'loading' | 'ready' | 'error'>('loading')
  const [snapshot, setSnapshot] = useState<DashboardSnapshot | null>(null)
  const [activeView, setActiveView] = useState<PrimaryView>('library')
  const [selectedScopeId, setSelectedScopeId] = useState('')
  const [selectedEntryId, setSelectedEntryId] = useState('')
  const [selectedReviewId, setSelectedReviewId] = useState('')
  const [reviewEdit, setReviewEdit] = useState<ReviewEditDraft | null>(null)
  const [entryDraft, setEntryDraft] = useState<EntryDraft>(emptyEntryDraft(''))
  const [revisions, setRevisions] = useState<RevisionEntry[]>([])
  const [focusRevisionId, setFocusRevisionId] = useState<string>()
  const [focusedConnectionId, setFocusedConnectionId] = useState<string>()
  const [focusedRunId, setFocusedRunId] = useState<string>()
  const [quickOpen, setQuickOpen] = useState(false)
  const [confirmation, setConfirmation] = useState<ConfirmationRequest | null>(null)
  const [confirmationBusy, setConfirmationBusy] = useState(false)
  const [dirtyDialog, setDirtyDialog] = useState(false)
  const [dirtyBusy, setDirtyBusy] = useState(false)
  const [busyKey, setBusyKey] = useState('')
  const [bulkResult, setBulkResult] = useState<BulkReviewDecisionResult | null>(null)
  const [notice, setNotice] = useState<{
    message: string
    actionLabel?: string
    action?: () => void
  } | null>(null)
  const [errorMessage, setErrorMessage] = useState('')
  const [liveMessage, setLiveMessage] = useState('')
  const pendingProtectedAction = useRef<(() => void | Promise<void>) | null>(null)
  const reviewDecisionInFlightRef = useRef(false)
  const snapshotRef = useRef<DashboardSnapshot | null>(null)
  snapshotRef.current = snapshot

  const scopes = useMemo(
    () => flattenWorkspace(snapshot?.workspace ?? []),
    [snapshot?.workspace],
  )
  const currentScope = scopes.find((scope) => scope.id === selectedScopeId)
  const currentNavigation = navigation.find((item) => item.id === activeView)
  const selectedEntry = snapshot?.entries.find(
    (entry) => entry.id === selectedEntryId && entry.scopeId === selectedScopeId,
  )
  const selectedReview =
    snapshot?.reviewQueue.find((review) => review.id === selectedReviewId) ?? null
  const editorDirty = Boolean(
    snapshot &&
      entryDraft.scopeId === selectedScopeId &&
      isEntryDraftDirty(entryDraft, selectedEntry),
  )
  const reviewEditDirty = Boolean(
    reviewEdit &&
      reviewEdit.reviewId === selectedReviewId &&
      reviewEdit.draft !== reviewEdit.baseline,
  )
  const activeDirtyKind =
    activeView === 'inbox' && reviewEditDirty
      ? 'review'
      : activeView === 'library' && editorDirty
        ? 'entry'
        : null
  const anyDialogOpen = quickOpen || Boolean(confirmation) || dirtyDialog
  const reviewDecisionBusy = busyKey.startsWith('review-')

  const announce = useCallback((message: string) => {
    setErrorMessage('')
    setNotice({ message })
    setLiveMessage('')
    window.setTimeout(() => setLiveMessage(message), 0)
  }, [])

  const showError = useCallback((message: string) => {
    setErrorMessage(message)
    setLiveMessage('')
  }, [])

  const handleApiError = useCallback(
    (error: unknown) => showError(friendlyDesktopError(error)),
    [showError],
  )

  const refreshDashboard = useCallback(
    async (preferred?: { scopeId?: string; entryId?: string; reviewId?: string }) => {
      const next = await api.loadDashboard()
      const flattened = flattenWorkspace(next.workspace)
      const nextScopeId =
        preferred?.scopeId && flattened.some((scope) => scope.id === preferred.scopeId)
          ? preferred.scopeId
          : flattened.some((scope) => scope.id === next.selectedScopeId)
            ? next.selectedScopeId
            : selectedScopeId && flattened.some((scope) => scope.id === selectedScopeId)
              ? selectedScopeId
              : flattened[0]?.id ?? ''
      const requestedEntryId = preferred?.entryId ?? selectedEntryId
      const requestedEntry = next.entries.find(
        (entry) => entry.id === requestedEntryId && entry.scopeId === nextScopeId,
      )
      const nextEntry = requestedEntry ?? firstEntryForScope(next, nextScopeId)
      const requestedReviewId = preferred?.reviewId ?? selectedReviewId

      setSnapshot(next)
      setSelectedScopeId(nextScopeId)
      setSelectedEntryId(nextEntry?.id ?? '')
      setEntryDraft(
        nextEntry
          ? draftFromEntry(nextEntry)
          : emptyEntryDraft(nextScopeId, selectedPackForNewEntry(next, nextScopeId)),
      )
      setSelectedReviewId(
        next.reviewQueue.some((review) => review.id === requestedReviewId)
          ? requestedReviewId
          : next.reviewQueue[0]?.id ?? '',
      )
      if (!next.reviewQueue.some((review) => review.id === reviewEdit?.reviewId)) {
        setReviewEdit(null)
      }
      return next
    },
    [api, reviewEdit?.reviewId, selectedEntryId, selectedReviewId, selectedScopeId],
  )

  function commitEntryContext(
    scopeId: string,
    entry: ContextEntry | undefined,
    options: {
      view?: PrimaryView
      revisionId?: string
      focus?: 'editor' | 'history'
      pack?: ContextPack
    } = {},
  ) {
    if (entry && entry.scopeId !== scopeId) {
      showError('The selected entry does not belong to the current scope. Refresh and try again.')
      return false
    }
    setSelectedScopeId(scopeId)
    setSelectedEntryId(entry?.id ?? '')
    setEntryDraft(
      entry
        ? draftFromEntry(entry)
        : emptyEntryDraft(
            scopeId,
            options.pack ??
              (snapshotRef.current
              ? selectedPackForNewEntry(snapshotRef.current, scopeId)
              : undefined),
          ),
    )
    setFocusRevisionId(options.revisionId)
    if (options.view) setActiveView(options.view)
    setErrorMessage('')
    window.setTimeout(() => {
      if (options.focus === 'history' && options.revisionId) {
        document
          .getElementById(`history-${options.revisionId}`)
          ?.querySelector<HTMLElement>('button')
          ?.focus()
      } else if (options.focus === 'editor') {
        document.getElementById('entry-editor')?.focus()
      }
    }, 0)
    return true
  }

  async function persistAndCommitEntry(
    entry: ContextEntry,
    options: { revisionId?: string; focus?: 'editor' | 'history' } = {},
  ) {
    await api.setSelectedScope(entry.scopeId)
    commitEntryContext(entry.scopeId, entry, {
      view: 'library',
      revisionId: options.revisionId,
      focus: options.focus ?? (options.revisionId ? 'history' : 'editor'),
    })
  }

  async function persistAndCommitScope(scopeId: string, view?: PrimaryView) {
    const current = snapshotRef.current
    if (!current) return
    await api.setSelectedScope(scopeId)
    const entry = firstEntryForScope(current, scopeId)
    commitEntryContext(scopeId, entry, { view })
  }

  function mergeEntryLocally(entry: ContextEntry) {
    setSnapshot((current) => {
      if (!current) return current
      const exists = current.entries.some((candidate) => candidate.id === entry.id)
      return {
        ...current,
        entries: exists
          ? current.entries.map((candidate) => (candidate.id === entry.id ? entry : candidate))
          : [entry, ...current.entries],
      }
    })
    commitEntryContext(entry.scopeId, entry)
  }

  function showStaleMutationNotice(
    successMessage: string,
    preferred?: { scopeId?: string; entryId?: string; reviewId?: string },
  ) {
    setNotice({
      message: `${successMessage} The displayed state could not refresh and may be stale. Do not repeat the mutation.`,
      actionLabel: 'Refresh view',
      action: () => {
        void refreshDashboard(preferred).catch(handleApiError)
      },
    })
    setLiveMessage(successMessage)
  }

  useEffect(() => {
    let cancelled = false
    async function load() {
      try {
        const next = await api.loadDashboard()
        if (cancelled) return
        const scopeId = next.selectedScopeId
        const entry = firstEntryForScope(next, scopeId)
        setSnapshot(next)
        setSelectedScopeId(scopeId)
        setSelectedEntryId(entry?.id ?? '')
        setEntryDraft(
          entry
            ? draftFromEntry(entry)
            : emptyEntryDraft(scopeId, selectedPackForNewEntry(next, scopeId)),
        )
        setSelectedReviewId(next.reviewQueue[0]?.id ?? '')
        setLoadState('ready')
      } catch (error) {
        if (!cancelled) {
          setLoadState('error')
          handleApiError(error)
        }
      }
    }
    void load()
    return () => {
      cancelled = true
    }
  }, [api, handleApiError])

  useEffect(() => {
    if (!snapshot || !selectedEntry) {
      setRevisions([])
      return
    }
    let cancelled = false
    void api
      .listRevisions(selectedEntry.packId)
      .then((result) => {
        if (!cancelled) {
          setRevisions(result.filter((revision) => revision.entityId === selectedEntry.id))
        }
      })
      .catch((error) => {
        if (!cancelled) handleApiError(error)
      })
    return () => {
      cancelled = true
    }
  }, [api, handleApiError, selectedEntry, snapshot])

  useEffect(() => {
    function handleBeforeUnload(event: BeforeUnloadEvent) {
      if (!editorDirty && !reviewEditDirty) return
      event.preventDefault()
      event.returnValue = ''
    }
    window.addEventListener('beforeunload', handleBeforeUnload)
    return () => window.removeEventListener('beforeunload', handleBeforeUnload)
  }, [editorDirty, reviewEditDirty])

  useEffect(() => {
    function handleGlobalKeyDown(event: KeyboardEvent) {
      if (event.isComposing) return
      if ((event.metaKey || event.ctrlKey) && event.key.toLocaleLowerCase() === 'k') {
        if (confirmation || dirtyDialog) return
        event.preventDefault()
        setQuickOpen(true)
      }
    }
    window.addEventListener('keydown', handleGlobalKeyDown)
    return () => window.removeEventListener('keydown', handleGlobalKeyDown)
  }, [confirmation, dirtyDialog])

  async function saveEntry() {
    if (!snapshot) return false
    if (
      entryDraft.scopeId !== selectedScopeId ||
      (selectedEntry && selectedEntry.scopeId !== selectedScopeId)
    ) {
      showError('The draft no longer matches the selected scope. Refresh before saving.')
      return false
    }
    if (entryDraft.format === 'json') {
      try {
        JSON.parse(entryDraft.body)
      } catch {
        showError('Enter valid JSON before saving. Nothing was stored.')
        return false
      }
    }
    setBusyKey('save-entry')
    let saved: ContextEntry
    try {
      saved = await api.saveEntry(entryDraftToInput(entryDraft))
    } catch (error) {
      handleApiError(error)
      setBusyKey('')
      return false
    }

    mergeEntryLocally(saved)
    try {
      const next = await refreshDashboard({ scopeId: saved.scopeId, entryId: saved.id })
      const refreshed = next.entries.find(
        (entry) => entry.id === saved.id && entry.scopeId === saved.scopeId,
      )
      if (refreshed) commitEntryContext(refreshed.scopeId, refreshed)
      setNotice({
        message: `Saved ${saved.title ?? saved.key}. Only this entry was updated.`,
        actionLabel: 'Open history',
        action: () => requestEntryHistory(saved.id, saved.scopeId),
      })
      setLiveMessage(`Saved ${saved.title ?? saved.key}.`)
    } catch {
      showStaleMutationNotice(
        `Saved ${saved.title ?? saved.key}.`,
        { scopeId: saved.scopeId, entryId: saved.id },
      )
    } finally {
      setBusyKey('')
    }
    return true
  }

  function runProtected(action: () => void | Promise<void>) {
    if (
      (activeView === 'library' && editorDirty) ||
      (activeView === 'inbox' && reviewEditDirty)
    ) {
      pendingProtectedAction.current = action
      setDirtyDialog(true)
      return
    }
    void action()
  }

  function selectEntry(entryId: string) {
    if (!snapshot) return
    const entry = snapshot.entries.find(
      (candidate) => candidate.id === entryId && candidate.scopeId === selectedScopeId,
    )
    if (!entry) return
    runProtected(() => {
      commitEntryContext(selectedScopeId, entry)
    })
  }

  function startNewEntry() {
    if (!snapshot) return
    runProtected(() => {
      commitEntryContext(selectedScopeId, undefined)
    })
  }

  function changeScope(scopeId: string, nextView?: PrimaryView) {
    if (!snapshot) return
    if (scopeId === selectedScopeId) {
      if (nextView) changeView(nextView)
      return
    }
    runProtected(async () => {
      try {
        await persistAndCommitScope(scopeId, nextView)
      } catch (error) {
        handleApiError(error)
      }
    })
  }

  function changeView(view: PrimaryView) {
    if (view === activeView) return
    runProtected(() => {
      setActiveView(view)
      if (activeView === 'inbox') setReviewEdit(null)
      setFocusedConnectionId(undefined)
      setFocusedRunId(undefined)
      setErrorMessage('')
    })
  }

  function requestEntryHistory(entryId: string, scopeId: string, revisionId?: string) {
    const current = snapshotRef.current
    if (!current) return
    const entry = current.entries.find(
      (candidate) => candidate.id === entryId && candidate.scopeId === scopeId,
    )
    if (!entry) {
      showError('That entry is no longer available. Refresh before opening history.')
      return
    }
    runProtected(async () => {
      try {
        await persistAndCommitEntry(entry, { revisionId, focus: 'history' })
      } catch (error) {
        handleApiError(error)
      }
    })
  }

  async function finishEntryMutation(entry: ContextEntry, successMessage: string) {
    mergeEntryLocally(entry)
    try {
      await refreshDashboard({ scopeId: entry.scopeId, entryId: entry.id })
      setNotice({
        message: successMessage,
        actionLabel: 'Open history',
        action: () => requestEntryHistory(entry.id, entry.scopeId),
      })
      setLiveMessage(successMessage)
    } catch {
      showStaleMutationNotice(successMessage, {
        scopeId: entry.scopeId,
        entryId: entry.id,
      })
    }
  }

  function requestArchive(entry: ContextEntry) {
    runProtected(() => {
      setConfirmation({
        title: 'Archive this entry?',
        description:
          'Only this entry will be archived. Sibling entries in the same pack remain unchanged.',
        confirmLabel: 'Archive entry',
        tone: 'danger',
        action: async () => {
          const archived = await api.archiveEntry(entry.id)
          await finishEntryMutation(
            archived,
            `${archived.title ?? archived.key} was archived.`,
          )
        },
      })
    })
  }

  function requestRestoreEntry(entry: ContextEntry) {
    runProtected(() => {
      setConfirmation({
        title: 'Restore this entry?',
        description:
          'The backend will create a new active revision. Existing sibling entries are not changed.',
        confirmLabel: 'Restore entry',
        action: async () => {
          const restored = await api.restoreEntry(entry.id)
          await finishEntryMutation(
            restored,
            `${restored.title ?? restored.key} was restored as a new revision.`,
          )
        },
      })
    })
  }

  function requestRevertPrevious(entry: ContextEntry) {
    const revision = entry.revision - 1
    runProtected(() => {
      setConfirmation({
        title: `Revert to revision ${revision}?`,
        description:
          'The selected historical value becomes a new active revision. The current revision remains in history.',
        confirmLabel: 'Revert entry',
        action: async () => {
          const restored = await api.revertEntryRevision({
            entryId: entry.id,
            revision,
            actor: 'desktop-operator',
          })
          await finishEntryMutation(
            restored,
            `${restored.title ?? restored.key} was reverted into a new revision.`,
          )
        },
      })
    })
  }

  function requestRestoreRevision(revision: RevisionEntry) {
    if (!selectedEntry) return
    const scopeId = selectedEntry.scopeId
    runProtected(() => {
      setConfirmation({
        title: 'Restore this historical revision?',
        description:
          'The backend restores the selected snapshot as a new revision. Later history remains available.',
        confirmLabel: 'Restore revision',
        action: async () => {
          const result = await api.restoreRevision(revision.id)
          try {
            const next = await refreshDashboard({
              scopeId,
              entryId: result.entityId,
            })
            const restored = next.entries.find(
              (entry) => entry.id === result.entityId && entry.scopeId === scopeId,
            )
            setNotice({
              message: `${revision.entityLabel} was restored from history.`,
              actionLabel: restored ? 'Open history' : 'Refresh view',
              action: () =>
                restored
                  ? requestEntryHistory(restored.id, restored.scopeId)
                  : void refreshDashboard({ scopeId, entryId: result.entityId }).catch(
                      handleApiError,
                    ),
            })
            setLiveMessage(`${revision.entityLabel} was restored.`)
          } catch {
            showStaleMutationNotice(
              `${revision.entityLabel} was restored from history.`,
              { scopeId, entryId: result.entityId },
            )
          }
        },
      })
    })
  }

  function selectReview(reviewId: string, openInbox = false) {
    if (!snapshot) return
    const review = snapshot.reviewQueue.find((candidate) => candidate.id === reviewId)
    if (!review && reviewId) return
    runProtected(async () => {
      try {
        if (review && review.scopeId !== selectedScopeId) {
          await persistAndCommitScope(review.scopeId)
        }
        setSelectedReviewId(review?.id ?? '')
        setReviewEdit(null)
        if (openInbox) setActiveView('inbox')
        window.setTimeout(() => document.getElementById('review-detail')?.focus(), 0)
      } catch (error) {
        handleApiError(error)
      }
    })
  }

  function startReviewEdit(review: ReviewItem) {
    setReviewEdit({
      reviewId: review.id,
      draft: review.suggestedEdit,
      baseline: review.suggestedEdit,
    })
  }

  async function reviewDecision(
    decision: ReviewDecision,
    review: ReviewItem,
    editedContent?: string,
  ): Promise<boolean> {
    if (reviewDecisionInFlightRef.current) return false
    reviewDecisionInFlightRef.current = true
    setBusyKey(`review-${decision}`)
    try {
      await api.reviewDecision({ itemId: review.id, decision, editedContent })
    } catch (error) {
      handleApiError(error)
      reviewDecisionInFlightRef.current = false
      setBusyKey('')
      return false
    }

    setReviewEdit(null)
    setSnapshot((current) =>
      current
        ? {
            ...current,
            reviewQueue: current.reviewQueue.filter((item) => item.id !== review.id),
          }
        : current,
    )
    try {
      const next = await refreshDashboard({ scopeId: review.scopeId })
      const historyEntry = entryForReview(next, review)
      const message =
        decision === 'reject'
          ? `${review.title} was rejected.`
          : `${review.title} was ${decision === 'edit' ? 'edited and approved' : 'approved'}.`
      setNotice(
        historyEntry
          ? {
              message,
              actionLabel: 'Open history',
              action: () => requestEntryHistory(historyEntry.id, historyEntry.scopeId),
            }
          : { message },
      )
      setLiveMessage(`${review.title} review completed.`)
    } catch {
      showStaleMutationNotice(
        `${review.title} was ${
          decision === 'reject'
            ? 'rejected'
            : decision === 'edit'
              ? 'edited and approved'
              : 'approved'
        }.`,
        { scopeId: review.scopeId },
      )
    } finally {
      reviewDecisionInFlightRef.current = false
      setBusyKey('')
    }
    return true
  }

  function requestBulkDecision(
    decision: 'approve' | 'reject',
    itemIds: string[],
  ) {
    if (!snapshot) return
    const reviews = itemIds
      .map((id) => snapshot.reviewQueue.find((review) => review.id === id))
      .filter((review): review is ReviewItem => Boolean(review))
    runProtected(() => setConfirmation({
      title: `${decision === 'approve' ? 'Approve' : 'Reject'} ${reviews.length} reviews?`,
      description:
        'Bulk actions can complete partially. The result will list every attempted item, then the Inbox will refresh.',
      confirmLabel: `${decision === 'approve' ? 'Approve' : 'Reject'} selected`,
      tone: decision === 'reject' ? 'danger' : 'primary',
      detail: (
        <ul className="confirmation-list">
          {reviews.map((review) => (
            <li key={review.id}>{review.title}</li>
          ))}
        </ul>
      ),
      action: async () => {
        const result = await api.bulkReviewDecision({
          itemIds,
          decision,
          confirmation: true,
          actor: 'desktop-operator',
          note: `Bulk ${decision} from Inbox.`,
        })
        const followUpCount = result.results.filter(
          (item) => item.success && item.requiresFollowUp,
        ).length
        setBulkResult(result)
        const completedIds = new Set(
          result.results.filter((item) => item.success).map((item) => item.itemId),
        )
        setSnapshot((current) =>
          current
            ? {
                ...current,
                reviewQueue: current.reviewQueue.filter(
                  (review) => !completedIds.has(review.id),
                ),
              }
            : current,
        )
        setReviewEdit(null)
        const successMessage = `${result.completed} of ${result.attempted} attempted reviews completed${
          result.stopped ? '; processing stopped after a failure' : ''
        }${followUpCount > 0 ? `; ${followUpCount} require follow-up` : ''}.`
        try {
          const next = await refreshDashboard()
          const historyEntry = reviews
            .filter((review) => completedIds.has(review.id))
            .map((review) => entryForReview(next, review))
            .find((entry): entry is ContextEntry => Boolean(entry))
          setNotice(
            historyEntry
              ? {
                  message: successMessage,
                  actionLabel: 'Open history',
                  action: () =>
                    requestEntryHistory(historyEntry.id, historyEntry.scopeId),
                }
              : { message: successMessage },
          )
          setLiveMessage(
            `${result.completed} of ${result.attempted} attempted reviews completed.`,
          )
        } catch {
          showStaleMutationNotice(successMessage)
        }
      },
    }))
  }

  function navigateToEntry(entryId: string, scopeId: string) {
    if (!snapshot) return
    const entry = snapshot.entries.find(
      (candidate) => candidate.id === entryId && candidate.scopeId === scopeId,
    )
    if (!entry) {
      showError('That local entry is no longer available. Refresh and try again.')
      return
    }
    runProtected(async () => {
      try {
        await persistAndCommitEntry(entry, { focus: 'editor' })
      } catch (error) {
        handleApiError(error)
      }
    })
  }

  function activateSearchResult(result: SearchResult) {
    if (!snapshot) return
    if (result.kind === 'entry') {
      const entry = snapshot.entries.find(
        (candidate) =>
          candidate.id === result.target.entryId &&
          (!result.target.scopeId || candidate.scopeId === result.target.scopeId),
      )
      if (entry) {
        navigateToEntry(entry.id, entry.scopeId)
        return
      }
    }
    if (result.kind === 'review') {
      const review = snapshot.reviewQueue.find(
        (candidate) => candidate.id === (result.target.reviewId ?? result.id),
      )
      if (review) {
        selectReview(review.id, true)
        return
      }
    }
    if (result.kind === 'revision') {
      const revision = snapshot.revisions.find(
        (candidate) => candidate.id === (result.target.revisionId ?? result.id),
      )
      const entry = revision
        ? snapshot.entries.find((candidate) => candidate.id === revision.entityId)
        : undefined
      if (revision && entry) {
        requestEntryHistory(entry.id, entry.scopeId, revision.id)
        return
      }
      if (revision?.entityId === 'review') {
        setActiveView('connections')
        setFocusedConnectionId('review-policy')
        setFocusedRunId(undefined)
        return
      }
    }
    if (result.kind === 'run') {
      const run = snapshot.activity.find((candidate) => candidate.id === result.id)
      if (run) {
        runProtected(async () => {
          try {
            if (
              result.target.scopeId &&
              scopes.some((scope) => scope.id === result.target.scopeId)
            ) {
              await persistAndCommitScope(result.target.scopeId)
            }
            setActiveView('connections')
            setFocusedRunId(run.id)
            setFocusedConnectionId(undefined)
          } catch (error) {
            handleApiError(error)
          }
        })
        return
      }
    }
    if (result.kind === 'adapter') {
      const adapter = snapshot.adapters.find(
        (candidate) => candidate.id === (result.target.adapterId ?? result.id),
      )
      if (adapter) {
        setActiveView('connections')
        setFocusedConnectionId(adapter.id)
        setFocusedRunId(undefined)
        return
      }
    }
    if (result.kind === 'pack') {
      const pack = snapshot.packs.find(
        (candidate) =>
          candidate.id === (result.target.packId ?? result.id) &&
          (!result.target.scopeId || candidate.scopeId === result.target.scopeId),
      )
      const entryFromPack = pack
        ? snapshot.entries.find((candidate) => candidate.packId === pack.id)
        : undefined
      if (entryFromPack) {
        navigateToEntry(entryFromPack.id, entryFromPack.scopeId)
        return
      }
      if (pack) {
        runProtected(async () => {
          try {
            await api.setSelectedScope(pack.scopeId)
            commitEntryContext(pack.scopeId, undefined, {
              view: 'library',
              focus: 'editor',
              pack,
            })
          } catch (error) {
            handleApiError(error)
          }
        })
        return
      }
    }
    showError('That search result no longer maps to an available local record.')
  }

  function activateQuickTarget(target: QuickOpenTarget) {
    if (!snapshot) return
    if (target.type === 'view') {
      changeView(target.view)
    } else if (target.type === 'scope') {
      changeScope(target.scopeId, 'library')
    } else if (target.type === 'entry') {
      navigateToEntry(target.entryId, target.scopeId)
    } else if (target.type === 'review') {
      selectReview(target.reviewId, true)
    } else if (target.type === 'revision') {
      const revision = snapshot.revisions.find(
        (candidate) => candidate.id === target.revisionId,
      )
      const entry = snapshot.entries.find(
        (candidate) => candidate.id === target.entityId,
      )
      if (revision && entry) requestEntryHistory(entry.id, entry.scopeId, revision.id)
      else if (revision?.entityId === 'review') {
        runProtected(() => {
          setActiveView('connections')
          setFocusedConnectionId('review-policy')
          setFocusedRunId(undefined)
        })
      }
    } else if (target.type === 'run') {
      runProtected(() => {
        setActiveView('connections')
        setFocusedRunId(target.runId)
        setFocusedConnectionId(undefined)
      })
    } else if (target.type === 'connection') {
      runProtected(() => {
        setActiveView('connections')
        setFocusedConnectionId(target.connectionId)
        setFocusedRunId(undefined)
      })
    } else if (target.type === 'new-entry') {
      runProtected(async () => {
        try {
          await api.setSelectedScope(target.scopeId)
          commitEntryContext(target.scopeId, undefined, {
            view: 'library',
            focus: 'editor',
          })
        } catch (error) {
          handleApiError(error)
        }
      })
    }
  }

  const quickOpenItems = useMemo<QuickOpenItem[]>(() => {
    if (!snapshot) return []
    const commands: QuickOpenItem[] = [
      ...navigation.map((item, index) => ({
        id: `view-${item.id}`,
        kind: 'command' as const,
        title: `Open ${item.label}`,
        detail: item.description,
        searchText: `${item.label} ${item.description}`,
        target: { type: 'view' as const, view: item.id },
        rank: index,
      })),
      {
        id: 'command-new-entry',
        kind: 'command',
        title: 'New entry',
        detail: `Create an entry in ${currentScope?.label ?? 'the selected scope'}`,
        searchText: 'new create entry context',
        target: { type: 'new-entry', scopeId: selectedScopeId },
        rank: 6,
      },
      {
        id: 'command-privacy',
        kind: 'command',
        title: 'Open Privacy & Data',
        detail: 'Local paths, disclosures, backup preview, and scoped archive',
        searchText: 'privacy data backup import forget archive paths',
        target: { type: 'connection', connectionId: 'privacy-data' },
        rank: 7,
      },
    ]
    return [
      ...commands,
      ...snapshot.entries.map((entry, index) => ({
        id: `entry-${entry.id}`,
        kind: 'entry' as const,
        title: entry.title ?? entry.key,
        detail: `${entry.packName} · ${scopeLayerLabel(entry.scopeKind)}${
          entry.scopeKind === 'task' ? ' (derived)' : ''
        } · ${entry.format}`,
        searchText: `${entry.key} ${entry.kind} ${entry.body} ${entry.tags.join(' ')} ${entry.provenance.source}`,
        target: { type: 'entry' as const, entryId: entry.id, scopeId: entry.scopeId },
        rank: 20 + index,
      })),
      ...scopes.map((scope, index) => ({
        id: `scope-${scope.id}`,
        kind: 'scope' as const,
        title: scopeLayerLabel(scope.kind),
        detail: `${scope.kind === 'task' ? 'Derived · ' : ''}${scope.label} · ${
          scope.description
        }`,
        searchText: `${scope.label} ${scope.kind} ${scope.description} ${scope.status}`,
        target: { type: 'scope' as const, scopeId: scope.id },
        rank: 50 + index,
      })),
      ...snapshot.reviewQueue.map((review, index) => ({
        id: `review-${review.id}`,
        kind: 'review' as const,
        title: review.title,
        detail: `${review.packName} · ${review.reason ?? 'review'}`,
        searchText: `${review.summary} ${review.source} ${review.requestedBy} ${review.entryKey}`,
        target: {
          type: 'review' as const,
          reviewId: review.id,
          scopeId: review.scopeId,
        },
        rank: 80 + index,
      })),
      ...snapshot.revisions.map((revision, index) => ({
        id: `revision-${revision.id}`,
        kind: 'revision' as const,
        title: revision.entityLabel,
        detail: `${revision.note} · ${formatTimestamp(revision.createdAt)}`,
        searchText: `${revision.changeSummary} ${revision.author}`,
        target: {
          type: 'revision' as const,
          revisionId: revision.id,
          entityId: revision.entityId,
        },
        rank: 110 + index,
      })),
      ...snapshot.activity.map((run, index) => ({
        id: `run-${run.id}`,
        kind: 'run' as const,
        title: run.summary,
        detail: `${run.actor} · ${run.status}`,
        searchText: `${run.actor} ${run.status}`,
        target: { type: 'run' as const, runId: run.id },
        rank: 140 + index,
      })),
      ...snapshot.adapters.map((adapter, index) => ({
        id: `adapter-${adapter.id}`,
        kind: 'connection' as const,
        title: adapter.name,
        detail: `${adapter.state} · ${adapter.note}`,
        searchText: `${adapter.kind} ${adapter.path} ${adapter.detectedVersion ?? ''}`,
        target: { type: 'connection' as const, connectionId: adapter.id },
        rank: 170 + index,
      })),
    ]
  }, [currentScope?.label, scopes, selectedScopeId, snapshot])

  if (loadState === 'loading') {
    return (
      <main className="loading-shell">
        <div className="titlebar-drag-strip" data-tauri-drag-region aria-hidden="true" />
        <span className="spinner loading-spinner" aria-hidden="true" />
        <div role="status">
          <h1>Opening Context</h1>
          <p>Reading the local dashboard and review policy.</p>
        </div>
      </main>
    )
  }

  if (loadState === 'error' || !snapshot) {
    return (
      <main className="loading-shell">
        <div className="titlebar-drag-strip" data-tauri-drag-region aria-hidden="true" />
        <div className="load-error" role="alert">
          <h1>Couldn’t open Context</h1>
          <p>{errorMessage || 'The local dashboard could not be loaded.'}</p>
          <button type="button" className="primary-button" onClick={() => window.location.reload()}>
            Retry
          </button>
        </div>
      </main>
    )
  }

  if (!snapshot.onboarding.complete) {
    return (
      <>
        <div className="sr-only" aria-live="polite" aria-atomic="true">
          {liveMessage}
        </div>
        {errorMessage ? (
          <div className="global-alert" role="alert">
            {errorMessage}
          </div>
        ) : null}
        <Onboarding
          api={api}
          snapshot={snapshot}
          onAnnounce={announce}
          onError={showError}
          onComplete={async () => {
            await refreshDashboard()
            setActiveView('library')
          }}
        />
      </>
    )
  }

  return (
    <main className="app-shell">
      <div className="sr-only" aria-live="polite" aria-atomic="true">
        {liveMessage}
      </div>

      <aside className="sidebar">
        <div
          className="sidebar-drag-region"
          data-tauri-drag-region
          aria-hidden="true"
        />
        <nav className="primary-nav" aria-label="Primary navigation">
          {navigation.map((item) => (
            <button
              key={item.id}
              type="button"
              className={activeView === item.id ? 'is-selected' : ''}
              aria-current={activeView === item.id ? 'page' : undefined}
              onClick={() => changeView(item.id)}
              title={item.description}
            >
              <span className="nav-icon">
                <NavigationIcon name={item.icon} />
              </span>
              <span>{item.label}</span>
              {item.id === 'inbox' && snapshot.reviewQueue.length > 0 ? (
                <span className="nav-count">{snapshot.reviewQueue.length}</span>
              ) : null}
            </button>
          ))}
        </nav>

        <section className="scope-rail" aria-labelledby="scope-rail-heading">
          <header>
            <h2 id="scope-rail-heading">Scope</h2>
            <span>{scopes.length}</span>
          </header>
          <div className="scope-buttons">
            {scopes.map((scope) => (
              <button
                key={scope.id}
                type="button"
                className={selectedScopeId === scope.id ? 'is-selected' : ''}
                aria-pressed={selectedScopeId === scope.id}
                title={`${scope.id}\n${scope.description}`}
                onClick={() => changeScope(scope.id)}
              >
                <span className="scope-button__line">
                  <strong>{scope.label}</strong>
                </span>
                <small>
                  {scopeLayerLabel(scope.kind)}
                  {scope.kind === 'task' ? ' · Derived' : ''}
                </small>
              </button>
            ))}
          </div>
        </section>

        <footer className="sidebar-footer">
          <span
            className={`sidebar-connection ${
              !snapshot.connected
                ? 'is-offline'
                : snapshot.diagnostics.overallState === 'healthy'
                  ? 'is-healthy'
                  : snapshot.diagnostics.overallState === 'degraded' ||
                      snapshot.diagnostics.overallState === 'starting'
                    ? 'is-warning'
                    : 'is-error'
            }`}
            title={`Last checked ${formatTimestamp(snapshot.diagnostics.generatedAt)}`}
          >
            <span className="sidebar-connection__dot" aria-hidden="true" />
            <span>
              {snapshot.connected ? 'Connected' : 'Offline'} · Diagnostics{' '}
              {snapshot.diagnostics.overallState}
            </span>
          </span>
        </footer>
      </aside>

      <section className="workspace" data-dialog-fallback tabIndex={-1}>
        <header className="workspace-bar" data-tauri-drag-region>
          <div className="workspace-bar__title" data-tauri-drag-region>
            <h1 data-tauri-drag-region>{currentNavigation?.label ?? 'Context'}</h1>
            <small data-tauri-drag-region>
              {currentScope?.label ?? 'Local context'} ·{' '}
              {scopeLayerLabel(currentScope?.kind ?? 'project')}
              {currentScope?.kind === 'task' ? ' · Derived' : ''}
            </small>
          </div>
          <button
            type="button"
            className="quick-open-header-button"
            aria-label="Open Quick Open"
            aria-keyshortcuts="Meta+K Control+K"
            data-quick-open-trigger
            onClick={() => setQuickOpen(true)}
          >
            <NavigationIcon name="search" />
            Quick Open
            <kbd>⌘K</kbd>
          </button>
        </header>

        {snapshot.notices.length > 0 ? (
          <details className="system-notices">
            <summary>
              <span>Local notes</span>
              <small>{snapshot.notices.length}</small>
            </summary>
            <ul>
              {snapshot.notices.map((item) => (
                <li key={item}>{item}</li>
              ))}
            </ul>
          </details>
        ) : null}

        {errorMessage ? (
          <div className="banner banner--error" role="alert">
            <span aria-hidden="true">!</span>
            <p>{errorMessage}</p>
            <button type="button" className="text-button" onClick={() => setErrorMessage('')}>
              Dismiss
            </button>
          </div>
        ) : null}

        {notice ? (
          <div className="banner banner--notice" role="status">
            <span aria-hidden="true">✓</span>
            <p>{notice.message}</p>
            {notice.actionLabel && notice.action ? (
              <button type="button" className="text-button" onClick={notice.action}>
                {notice.actionLabel}
              </button>
            ) : (
              <button type="button" className="text-button" onClick={() => setNotice(null)}>
                Dismiss
              </button>
            )}
          </div>
        ) : null}

        {activeView === 'library' ? (
          <LibraryView
            snapshot={snapshot}
            scopeId={selectedScopeId}
            selectedEntryId={selectedEntryId}
            draft={entryDraft}
            revisions={revisions}
            busyKey={busyKey}
            dirty={editorDirty}
            focusRevisionId={focusRevisionId}
            onDraftChange={setEntryDraft}
            onSelectEntry={selectEntry}
            onNewEntry={startNewEntry}
            onSave={() => void saveEntry()}
            onDiscard={() =>
              setEntryDraft(
                selectedEntry
                  ? draftFromEntry(selectedEntry)
                  : emptyEntryDraft(
                      selectedScopeId,
                      selectedPackForNewEntry(snapshot, selectedScopeId),
                    ),
              )
            }
            onArchive={requestArchive}
            onRestore={requestRestoreEntry}
            onRevertPrevious={requestRevertPrevious}
            onRestoreRevision={requestRestoreRevision}
          />
        ) : null}

        {activeView === 'effective' ? (
          <EffectiveContextView
            api={api}
            snapshot={snapshot}
            initialScopeId={selectedScopeId}
            onOpenEntry={navigateToEntry}
            onAnnounce={announce}
            onError={showError}
          />
        ) : null}

        {activeView === 'inbox' ? (
          <InboxView
            snapshot={snapshot}
            selectedReviewId={selectedReviewId}
            bulkResult={bulkResult}
            dialogOpen={anyDialogOpen}
            reviewBusy={reviewDecisionBusy}
            reviewEdit={reviewEdit}
            onSelectReview={selectReview}
            onStartEdit={startReviewEdit}
            onEditDraftChange={(draft) =>
              setReviewEdit((current) => (current ? { ...current, draft } : current))
            }
            onCancelEdit={() => setReviewEdit(null)}
            onRequestTransition={runProtected}
            onDecision={reviewDecision}
            onBulk={requestBulkDecision}
            onOpenHistory={(review) => {
              const entry = entryForReview(snapshot, review)
              if (entry) requestEntryHistory(entry.id, entry.scopeId)
              else showError('This proposed entry has no durable history yet.')
            }}
          />
        ) : null}

        {activeView === 'search' ? (
          <SearchView api={api} onActivate={activateSearchResult} onError={showError} />
        ) : null}

        {activeView === 'connections' ? (
          <ConnectionsView
            api={api}
            snapshot={snapshot}
            focusedConnectionId={focusedConnectionId}
            focusedRunId={focusedRunId}
            onConfirm={setConfirmation}
            onAnnounce={announce}
            onError={showError}
            onDataChanged={async () => {
              await refreshDashboard()
            }}
            onOpenHistory={() => {
              const revision = snapshot.revisions[0]
              const entry = revision
                ? snapshot.entries.find((candidate) => candidate.id === revision.entityId)
                : undefined
              if (revision && entry) {
                requestEntryHistory(entry.id, entry.scopeId, revision.id)
              }
              else {
                changeView('library')
                announce('Opened Library. Select an entry to inspect its history.')
              }
            }}
            onResetOnboarding={() =>
              setConfirmation({
                title: 'Run onboarding again?',
                description:
                  'This resets only onboarding completion state. Existing local context remains stored.',
                confirmLabel: 'Reset onboarding',
                action: async () => {
                  await api.resetOnboarding()
                  await refreshDashboard()
                  announce('Onboarding reset. Existing context was not removed.')
                },
              })
            }
          />
        ) : null}
      </section>

      {quickOpen ? (
        <QuickOpen
          items={quickOpenItems}
          onClose={() => setQuickOpen(false)}
          onActivate={(item) => {
            setQuickOpen(false)
            activateQuickTarget(item.target)
          }}
        />
      ) : null}

      {confirmation ? (
        <ConfirmationDialog
          title={confirmation.title}
          description={confirmation.description}
          confirmLabel={confirmation.confirmLabel}
          tone={confirmation.tone}
          detail={confirmation.detail}
          busy={confirmationBusy}
          onCancel={() => setConfirmation(null)}
          onConfirm={async () => {
            try {
              setConfirmationBusy(true)
              await confirmation.action()
              setConfirmation(null)
            } catch (error) {
              handleApiError(error)
            } finally {
              setConfirmationBusy(false)
            }
          }}
        />
      ) : null}

      {dirtyDialog ? (
        <DirtyDecisionDialog
          itemLabel={
            activeDirtyKind === 'review'
              ? selectedReview?.title ?? 'The edited review'
              : selectedEntry?.title ?? selectedEntry?.key ?? 'The new entry'
          }
          busy={dirtyBusy}
          onStay={() => {
            pendingProtectedAction.current = null
            setDirtyDialog(false)
          }}
          onDiscard={() => {
            const action = pendingProtectedAction.current
            pendingProtectedAction.current = null
            if (activeDirtyKind === 'review') {
              setReviewEdit(null)
            } else {
              setEntryDraft(
                selectedEntry
                  ? draftFromEntry(selectedEntry)
                  : emptyEntryDraft(
                      selectedScopeId,
                      selectedPackForNewEntry(snapshot, selectedScopeId),
                    ),
              )
            }
            setDirtyDialog(false)
            if (action) void action()
          }}
          onSave={async () => {
            try {
              setDirtyBusy(true)
              const saved =
                activeDirtyKind === 'review' && selectedReview && reviewEdit
                  ? await reviewDecision('edit', selectedReview, reviewEdit.draft)
                  : await saveEntry()
              if (!saved) return
              const action = pendingProtectedAction.current
              pendingProtectedAction.current = null
              setDirtyDialog(false)
              if (action) await action()
            } finally {
              setDirtyBusy(false)
            }
          }}
        />
      ) : null}
    </main>
  )
}

export function MockedApp() {
  return <App api={createDesktopApi({ forceMock: true })} />
}

export default App
