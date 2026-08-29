import { useEffect, useMemo, useState } from 'react'
import type {
  BulkReviewDecisionResult,
  DashboardSnapshot,
  ReviewDecision,
  ReviewItem,
  ReviewReason,
  WorkspaceNode,
} from '../types'
import {
  EmptyState,
  SectionHeader,
  StatusPill,
} from './Common'
import {
  formatQueueAge,
  formatTimestamp,
  isTextEditingTarget,
  scopeLayerLabel,
} from '../lib/ui'

type AgeFilter = 'all' | 'hour' | 'day' | 'week' | 'older'

function flattenWorkspace(nodes: WorkspaceNode[]): WorkspaceNode[] {
  return nodes.flatMap((node) => [node, ...flattenWorkspace(node.children)])
}

function matchesAge(review: ReviewItem, filter: AgeFilter) {
  if (filter === 'all') return true
  if (filter === 'hour') return review.ageSeconds < 3_600
  if (filter === 'day') return review.ageSeconds < 86_400
  if (filter === 'week') return review.ageSeconds < 604_800
  return review.ageSeconds >= 604_800
}

export function InboxView({
  snapshot,
  selectedReviewId,
  bulkResult,
  dialogOpen,
  reviewBusy,
  reviewEdit,
  onSelectReview,
  onStartEdit,
  onEditDraftChange,
  onCancelEdit,
  onRequestTransition,
  onDecision,
  onBulk,
  onOpenHistory,
}: {
  snapshot: DashboardSnapshot
  selectedReviewId: string
  bulkResult: BulkReviewDecisionResult | null
  dialogOpen: boolean
  reviewBusy: boolean
  reviewEdit: { reviewId: string; draft: string; baseline: string } | null
  onSelectReview: (reviewId: string) => void
  onStartEdit: (review: ReviewItem) => void
  onEditDraftChange: (value: string) => void
  onCancelEdit: () => void
  onRequestTransition: (action: () => void) => void
  onDecision: (
    decision: ReviewDecision,
    review: ReviewItem,
    editedContent?: string,
  ) => Promise<boolean>
  onBulk: (decision: 'approve' | 'reject', itemIds: string[]) => void
  onOpenHistory: (review: ReviewItem) => void
}) {
  const [scopeFilter, setScopeFilter] = useState('all')
  const [reasonFilter, setReasonFilter] = useState('all')
  const [sourceFilter, setSourceFilter] = useState('all')
  const [riskFilter, setRiskFilter] = useState('all')
  const [ageFilter, setAgeFilter] = useState<AgeFilter>('all')
  const [selectedIds, setSelectedIds] = useState<Set<string>>(new Set())
  const selectedReview =
    snapshot.reviewQueue.find((review) => review.id === selectedReviewId) ?? null
  const editing = Boolean(selectedReview && reviewEdit?.reviewId === selectedReview.id)
  const editDraft = editing ? reviewEdit?.draft ?? '' : selectedReview?.suggestedEdit ?? ''
  const sources = useMemo(
    () => [...new Set(snapshot.reviewQueue.map((review) => review.source))].sort(),
    [snapshot.reviewQueue],
  )
  const reasons = useMemo(
    () =>
      [
        ...new Set(
          snapshot.reviewQueue
            .map((review) => review.reason)
            .filter((reason): reason is ReviewReason => Boolean(reason)),
        ),
      ].sort(),
    [snapshot.reviewQueue],
  )
  const filtered = useMemo(
    () =>
      snapshot.reviewQueue.filter(
        (review) =>
          (scopeFilter === 'all' || review.scopeId === scopeFilter) &&
          (reasonFilter === 'all' || review.reason === reasonFilter) &&
          (sourceFilter === 'all' || review.source === sourceFilter) &&
          (riskFilter === 'all' || review.risk === riskFilter) &&
          matchesAge(review, ageFilter),
      ),
    [ageFilter, reasonFilter, riskFilter, scopeFilter, snapshot.reviewQueue, sourceFilter],
  )

  useEffect(() => {
    setSelectedIds((current) => {
      const available = new Set(snapshot.reviewQueue.map((review) => review.id))
      return new Set([...current].filter((id) => available.has(id)))
    })
  }, [snapshot.reviewQueue])

  useEffect(() => {
    if (filtered.length === 0) {
      if (selectedReviewId) onSelectReview('')
      return
    }
    if (!filtered.some((review) => review.id === selectedReviewId)) {
      onSelectReview(filtered[0].id)
    }
  }, [filtered, onSelectReview, selectedReviewId])

  useEffect(() => {
    function handleKeyDown(event: KeyboardEvent) {
      if (
        dialogOpen ||
        reviewBusy ||
        event.isComposing ||
        isTextEditingTarget(event.target)
      ) {
        return
      }
      if (event.metaKey || event.ctrlKey || event.altKey) return
      const index = filtered.findIndex((review) => review.id === selectedReviewId)
      if (event.key === 'ArrowDown' && filtered.length > 0) {
        event.preventDefault()
        onSelectReview(filtered[Math.min(filtered.length - 1, Math.max(0, index + 1))].id)
      } else if (event.key === 'ArrowUp' && filtered.length > 0) {
        event.preventDefault()
        onSelectReview(filtered[Math.max(0, index <= 0 ? 0 : index - 1)].id)
      } else if (selectedReview && event.key.toLocaleLowerCase() === 'a') {
        event.preventDefault()
        void onDecision('approve', selectedReview)
      } else if (selectedReview && event.key.toLocaleLowerCase() === 'r') {
        event.preventDefault()
        void onDecision('reject', selectedReview)
      } else if (selectedReview && event.key.toLocaleLowerCase() === 'e') {
        event.preventDefault()
        onStartEdit(selectedReview)
      }
    }
    window.addEventListener('keydown', handleKeyDown)
    return () => window.removeEventListener('keydown', handleKeyDown)
  }, [
    dialogOpen,
    filtered,
    onDecision,
    onStartEdit,
    reviewBusy,
    onSelectReview,
    selectedReview,
    selectedReviewId,
  ])

  const allVisibleSelected =
    filtered.length > 0 && filtered.every((review) => selectedIds.has(review.id))

  return (
    <div className="view-stack inbox-view">
      <SectionHeader
        eyebrow="Human review gate"
        title="Inbox"
        detail="Approve or reject queued changes. Editing remains a single-item action."
        actions={
          selectedIds.size > 0 ? (
            <div className="button-row">
              <button
                type="button"
                className="secondary-button"
                onClick={() => onBulk('reject', [...selectedIds])}
              >
                Reject {selectedIds.size}…
              </button>
              <button
                type="button"
                className="primary-button"
                onClick={() => onBulk('approve', [...selectedIds])}
              >
                Approve {selectedIds.size}…
              </button>
            </div>
          ) : (
            <span className="keyboard-hint">
              <kbd>A</kbd> approve <kbd>R</kbd> reject <kbd>E</kbd> edit
            </span>
          )
        }
      />

      <section className="review-filters" aria-label="Review filters">
        <label>
          <span>Scope</span>
          <select
            value={scopeFilter}
            onChange={(event) => {
              const value = event.target.value
              onRequestTransition(() => setScopeFilter(value))
            }}
          >
            <option value="all">All scopes</option>
            {flattenWorkspace(snapshot.workspace).map((scope) => (
                <option key={scope.id} value={scope.id}>
                  {scopeLayerLabel(scope.kind)}
                  {scope.kind === 'task' ? ' (derived)' : ''} — {scope.label}
                </option>
              ))}
          </select>
        </label>
        <label>
          <span>Reason</span>
          <select
            value={reasonFilter}
            onChange={(event) => {
              const value = event.target.value
              onRequestTransition(() => setReasonFilter(value))
            }}
          >
            <option value="all">All reasons</option>
            {reasons.map((reason) => (
              <option key={reason} value={reason}>
                {reason}
              </option>
            ))}
          </select>
        </label>
        <label>
          <span>Source</span>
          <select
            value={sourceFilter}
            onChange={(event) => {
              const value = event.target.value
              onRequestTransition(() => setSourceFilter(value))
            }}
          >
            <option value="all">All sources</option>
            {sources.map((source) => (
              <option key={source} value={source}>
                {source}
              </option>
            ))}
          </select>
        </label>
        <label>
          <span>Risk</span>
          <select
            value={riskFilter}
            onChange={(event) => {
              const value = event.target.value
              onRequestTransition(() => setRiskFilter(value))
            }}
          >
            <option value="all">All risk</option>
            <option value="low">low</option>
            <option value="medium">medium</option>
            <option value="high">high</option>
          </select>
        </label>
        <label>
          <span>Queue age</span>
          <select
            value={ageFilter}
            onChange={(event) => {
              const value = event.target.value as AgeFilter
              onRequestTransition(() => setAgeFilter(value))
            }}
          >
            <option value="all">Any age</option>
            <option value="hour">Under 1 hour</option>
            <option value="day">Under 24 hours</option>
            <option value="week">Under 7 days</option>
            <option value="older">7 days or older</option>
          </select>
        </label>
      </section>

      {bulkResult ? (
        <section
          className={`bulk-result ${bulkResult.stopped ? 'bulk-result--partial' : ''}`}
          aria-labelledby="bulk-result-heading"
        >
          <header>
            <div>
              <p className="eyebrow">Bulk {bulkResult.decision}</p>
              <h3 id="bulk-result-heading">
                {bulkResult.completed} of {bulkResult.attempted} attempted items completed
              </h3>
            </div>
            <StatusPill label={bulkResult.stopped ? 'partial failure' : 'completed'} />
          </header>
          <ul>
            {bulkResult.results.map((result) => (
              <li key={result.itemId}>
                <StatusPill
                  label={
                    result.requiresFollowUp
                      ? 'follow-up required'
                      : result.success
                        ? result.state ?? 'completed'
                        : 'failed'
                  }
                />
                <code>{result.itemId}</code>
                <span>
                  {result.requiresFollowUp
                    ? 'The decision completed, but the backend requires a follow-up review.'
                    : result.success
                    ? 'Completed'
                    : result.error?.code === 'unavailable'
                      ? 'Local service unavailable; the item remains queued.'
                      : 'Not completed; refresh before retrying.'}
                </span>
              </li>
            ))}
          </ul>
        </section>
      ) : null}

      <div className="review-workbench">
        <section className="review-queue" aria-labelledby="review-queue-heading">
          <header className="pane-heading">
            <label className="select-all-row">
              <input
                type="checkbox"
                aria-label="Select all visible reviews"
                checked={allVisibleSelected}
                onChange={(event) => {
                  setSelectedIds((current) => {
                    const next = new Set(current)
                    for (const review of filtered) {
                      if (event.target.checked) next.add(review.id)
                      else next.delete(review.id)
                    }
                    return next
                  })
                }}
              />
              <span id="review-queue-heading">
                {filtered.length} of {snapshot.reviewQueue.length} pending
              </span>
            </label>
          </header>
          {filtered.length === 0 ? (
            <EmptyState
              title={snapshot.reviewQueue.length === 0 ? 'Inbox clear' : 'No filtered reviews'}
              body={
                snapshot.reviewQueue.length === 0
                  ? 'New gated changes will appear here.'
                  : 'Adjust the scope, source, reason, risk, or age filters.'
              }
            />
          ) : (
            <ul className="review-list">
              {filtered.map((review) => (
                <li key={review.id}>
                  <label className="review-select">
                    <input
                      type="checkbox"
                      aria-label={`Select ${review.title}`}
                      checked={selectedIds.has(review.id)}
                      onChange={(event) => {
                        setSelectedIds((current) => {
                          const next = new Set(current)
                          if (event.target.checked) next.add(review.id)
                          else next.delete(review.id)
                          return next
                        })
                      }}
                    />
                  </label>
                  <button
                    type="button"
                    className={`review-row ${
                      selectedReviewId === review.id ? 'review-row--selected' : ''
                    }`}
                    onClick={() => onSelectReview(review.id)}
                  >
                    <span className="review-row__top">
                      <strong>{review.title}</strong>
                      <StatusPill label={review.risk} />
                    </span>
                    <span className="review-row__summary">{review.summary}</span>
                    <span className="review-row__meta">
                      <span>{review.reason ?? 'review'}</span>
                      <span>{review.source}</span>
                      <span>{formatQueueAge(review.ageSeconds)} queued</span>
                    </span>
                  </button>
                </li>
              ))}
            </ul>
          )}
        </section>

        <section
          id="review-detail"
          className="review-detail"
          aria-labelledby="review-detail-heading"
          aria-busy={reviewBusy}
          tabIndex={-1}
        >
          {selectedReview ? (
            <>
              <header className="pane-heading pane-heading--detail">
                <div>
                  <p className="eyebrow">
                    {scopeLayerLabel(selectedReview.scopeKind)}
                    {selectedReview.scopeKind === 'task' ? ' · derived' : ''} ·{' '}
                    {selectedReview.scopeLabel}
                  </p>
                  <h3 id="review-detail-heading">{selectedReview.title}</h3>
                  <p>
                    Queued {formatQueueAge(selectedReview.ageSeconds)} ·{' '}
                    {formatTimestamp(selectedReview.requestedAt)}
                  </p>
                </div>
                <div className="pane-heading__actions">
                  <StatusPill label={selectedReview.reason ?? 'review'} />
                  <StatusPill label={`${selectedReview.risk} risk`} />
                </div>
              </header>

              <div className="review-provenance">
                <dl>
                  <div>
                    <dt>Requested by</dt>
                    <dd>{selectedReview.requestedBy}</dd>
                  </div>
                  {selectedReview.provenance?.actor ? (
                    <div>
                      <dt>Provenance actor</dt>
                      <dd>{selectedReview.provenance.actor}</dd>
                    </div>
                  ) : null}
                  <div>
                    <dt>Source</dt>
                    <dd>{selectedReview.source}</dd>
                  </div>
                  <div>
                    <dt>Entry</dt>
                    <dd>
                      {selectedReview.packName} / {selectedReview.entryKey}
                    </dd>
                  </div>
                  {selectedReview.provenance?.sourceRef ? (
                    <div>
                      <dt>Source reference</dt>
                      <dd className="path-value">{selectedReview.provenance.sourceRef}</dd>
                    </div>
                  ) : null}
                  {selectedReview.provenance?.runId ? (
                    <div>
                      <dt>Run</dt>
                      <dd>{selectedReview.provenance.runId}</dd>
                    </div>
                  ) : null}
                  {selectedReview.requestId ? (
                    <div>
                      <dt>Request</dt>
                      <dd>{selectedReview.requestId}</dd>
                    </div>
                  ) : null}
                  {selectedReview.provenance?.note ? (
                    <div>
                      <dt>Provenance note</dt>
                      <dd>{selectedReview.provenance.note}</dd>
                    </div>
                  ) : null}
                </dl>
              </div>

              {editing ? (
                <label className="review-edit-field">
                  <span>Edited proposed content</span>
                  <textarea
                    autoFocus
                    value={editDraft}
                    onChange={(event) => onEditDraftChange(event.target.value)}
                    rows={13}
                  />
                </label>
              ) : (
                <div className="side-by-side-diff" aria-label="Existing and proposed content">
                  <section>
                    <header>
                      <span>Existing</span>
                      <StatusPill
                        label={selectedReview.diffSides.before === undefined ? 'new entry' : 'stored'}
                      />
                    </header>
                    <pre>
                      {selectedReview.diffSides.before ??
                        'No existing entry. This proposal would create one.'}
                    </pre>
                  </section>
                  <section>
                    <header>
                      <span>Proposed</span>
                      <StatusPill
                        label={selectedReview.diffSides.changed ? 'changed' : 'unchanged'}
                      />
                    </header>
                    <pre>{selectedReview.diffSides.after}</pre>
                  </section>
                </div>
              )}

              <footer className="review-actions">
                <button
                  type="button"
                  className="text-button"
                  onClick={() => onOpenHistory(selectedReview)}
                >
                  Open history
                </button>
                <div className="button-row">
                  <button
                    type="button"
                    className="danger-quiet-button"
                    disabled={reviewBusy}
                    onClick={() => void onDecision('reject', selectedReview)}
                  >
                    Reject
                  </button>
                  {editing ? (
                    <>
                      <button
                        type="button"
                        className="secondary-button"
                        disabled={reviewBusy}
                        onClick={onCancelEdit}
                      >
                        Cancel edit
                      </button>
                      <button
                        type="button"
                        className="primary-button"
                        disabled={reviewBusy}
                        onClick={() => void onDecision('edit', selectedReview, editDraft)}
                      >
                        Apply edited approval
                      </button>
                    </>
                  ) : (
                    <>
                      <button
                        type="button"
                        className="secondary-button"
                        disabled={reviewBusy}
                        onClick={() => onStartEdit(selectedReview)}
                      >
                        Edit one
                      </button>
                      <button
                        type="button"
                        className="primary-button"
                        disabled={reviewBusy}
                        onClick={() => void onDecision('approve', selectedReview)}
                      >
                        Approve
                      </button>
                    </>
                  )}
                </div>
              </footer>
            </>
          ) : (
            <EmptyState
              title="Select a review"
              body="Choose a queued item to compare its stored and proposed content."
            />
          )}
        </section>
      </div>
    </div>
  )
}
