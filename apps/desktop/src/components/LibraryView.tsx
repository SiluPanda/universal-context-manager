import { useEffect, useMemo, useRef, useState } from 'react'
import type {
  ContextEntry,
  ContextPack,
  DashboardSnapshot,
  RevisionEntry,
  WorkspaceNode,
} from '../types'
import type { EntryDraft } from '../lib/entryDraft'
import {
  EmptyState,
  SectionHeader,
  StatusPill,
} from './Common'
import { formatTimestamp, scopeLayerDetail, scopeLayerLabel } from '../lib/ui'

function entryLabel(entry: ContextEntry) {
  return entry.title?.trim() || entry.key
}

function entriesByPack(entries: ContextEntry[], packs: ContextPack[]) {
  return packs
    .map((pack) => ({
      pack,
      entries: entries
        .filter((entry) => entry.packId === pack.id)
        .sort((left, right) => left.key.localeCompare(right.key)),
    }))
    .filter((group) => group.entries.length > 0)
}

function flattenWorkspace(nodes: WorkspaceNode[]): WorkspaceNode[] {
  return nodes.flatMap((node) => [node, ...flattenWorkspace(node.children)])
}

export function LibraryView({
  snapshot,
  scopeId,
  selectedEntryId,
  draft,
  revisions,
  busyKey,
  dirty,
  focusRevisionId,
  onDraftChange,
  onSelectEntry,
  onNewEntry,
  onSave,
  onDiscard,
  onArchive,
  onRestore,
  onRevertPrevious,
  onRestoreRevision,
}: {
  snapshot: DashboardSnapshot
  scopeId: string
  selectedEntryId: string
  draft: EntryDraft
  revisions: RevisionEntry[]
  busyKey: string
  dirty: boolean
  focusRevisionId?: string
  onDraftChange: (draft: EntryDraft) => void
  onSelectEntry: (entryId: string) => void
  onNewEntry: () => void
  onSave: () => void
  onDiscard: () => void
  onArchive: (entry: ContextEntry) => void
  onRestore: (entry: ContextEntry) => void
  onRevertPrevious: (entry: ContextEntry) => void
  onRestoreRevision: (revision: RevisionEntry) => void
}) {
  const [showArchived, setShowArchived] = useState(true)
  const historyRef = useRef<HTMLDivElement>(null)
  const scope = useMemo(
    () => flattenWorkspace(snapshot.workspace).find((candidate) => candidate.id === scopeId),
    [scopeId, snapshot.workspace],
  )
  const scopeEntries = useMemo(
    () =>
      snapshot.entries.filter(
        (entry) =>
          entry.scopeId === scopeId && (showArchived || entry.status !== 'deleted'),
      ),
    [scopeId, showArchived, snapshot.entries],
  )
  const scopePacks = useMemo(
    () => snapshot.packs.filter((pack) => pack.scopeId === scopeId),
    [scopeId, snapshot.packs],
  )
  const grouped = useMemo(
    () => entriesByPack(scopeEntries, scopePacks),
    [scopeEntries, scopePacks],
  )
  const selectedEntry = snapshot.entries.find(
    (entry) => entry.id === selectedEntryId && entry.scopeId === scopeId,
  )
  const referencedRuns = selectedEntry
    ? snapshot.activity.filter((run) => run.contextPackIds.includes(selectedEntry.packId))
    : []
  const jsonError = useMemo(() => {
    if (draft.format !== 'json' || !draft.body.trim()) return ''
    try {
      JSON.parse(draft.body)
      return ''
    } catch {
      return 'Enter valid JSON before saving. The current entry has not been changed.'
    }
  }, [draft.body, draft.format])

  useEffect(() => {
    if (!focusRevisionId) return
    const target = document.getElementById(`history-${focusRevisionId}`)
    target?.scrollIntoView?.({ block: 'nearest' })
    target?.querySelector<HTMLElement>('button')?.focus()
  }, [focusRevisionId, revisions])

  return (
    <div className="view-stack library-view">
      <SectionHeader
        title="Library"
        detail="Entries are saved independently; packs organize them."
        actions={
          <>
            <label className="compact-check">
              <input
                type="checkbox"
                checked={showArchived}
                onChange={(event) => setShowArchived(event.target.checked)}
              />
              <span>Show archived</span>
            </label>
            <button type="button" className="primary-button" onClick={onNewEntry}>
              New entry
            </button>
          </>
        }
      />

      <div className="library-workbench">
        <section className="entry-browser" aria-labelledby="entry-browser-heading">
          <header className="pane-heading">
            <div>
              <h3 id="entry-browser-heading">Entries</h3>
              <p>{scopeEntries.length} in this scope</p>
            </div>
            <StatusPill label={scope?.kind === 'task' ? 'derived' : 'durable'} />
          </header>

          {grouped.length === 0 ? (
            <EmptyState
              title="No entries in this scope"
              body={
                scopePacks.length === 0
                  ? 'Import instructions or complete onboarding to create the first pack and entry.'
                  : 'Create an entry inside one of this scope’s existing packs.'
              }
            />
          ) : (
            <div className="entry-groups">
              {grouped.map(({ pack, entries }) => (
                <section className="entry-group" key={pack.id} aria-label={`${pack.name} pack`}>
                  <header className="entry-group__header">
                    <div>
                      <span className="entry-group__rule" aria-hidden="true" />
                      <strong>{pack.name}</strong>
                    </div>
                    <span>{entries.length}</span>
                  </header>
                  <ul className="entry-list">
                    {entries.map((entry) => (
                      <li key={entry.id}>
                        <button
                          type="button"
                          className={`entry-row ${
                            selectedEntryId === entry.id ? 'entry-row--selected' : ''
                          }`}
                          aria-pressed={selectedEntryId === entry.id}
                          onClick={() => onSelectEntry(entry.id)}
                        >
                          <span className="entry-row__top">
                            <strong>{entryLabel(entry)}</strong>
                            <StatusPill label={entry.status} />
                          </span>
                          <span className="entry-row__key">{entry.key}</span>
                          <span className="entry-row__meta">
                            <span>{entry.kind}</span>
                            <span>{entry.format}</span>
                            <span>r{entry.revision}</span>
                            {entry.locked ? <span>locked</span> : null}
                          </span>
                        </button>
                      </li>
                    ))}
                  </ul>
                </section>
              ))}
            </div>
          )}
        </section>

        <section
          id="entry-editor"
          className="entry-editor"
          aria-labelledby="entry-editor-heading"
          tabIndex={-1}
        >
          <header className="pane-heading pane-heading--editor">
            <div>
              <h3 id="entry-editor-heading">
                {selectedEntry ? entryLabel(selectedEntry) : 'Untitled context'}
              </h3>
            </div>
            <div className="pane-heading__actions">
              <StatusPill label={selectedEntry?.status ?? 'draft'} />
              {selectedEntry?.locked ? <StatusPill label="locked" tone="warning" /> : null}
            </div>
          </header>

          {scopePacks.length === 0 ? (
            <EmptyState
              title="A pack is required"
              body="Packs are created by onboarding or import. Choose another scope or import a source first."
            />
          ) : (
            <form
              className="entry-form"
              onSubmit={(event) => {
                event.preventDefault()
                if (!jsonError && selectedEntry?.status !== 'deleted') onSave()
              }}
            >
              <div className="field-grid">
                <label>
                  <span>Pack group</span>
                  <select
                    aria-label="Pack group"
                    value={draft.packId}
                    disabled={Boolean(selectedEntry)}
                    onChange={(event) => {
                      const pack = scopePacks.find((candidate) => candidate.id === event.target.value)
                      onDraftChange({
                        ...draft,
                        packId: event.target.value,
                        packName: pack?.name ?? '',
                      })
                    }}
                  >
                    <option value="">Choose a pack</option>
                    {scopePacks.map((pack) => (
                      <option key={pack.id} value={pack.id}>
                        {pack.name}
                      </option>
                    ))}
                  </select>
                </label>
                <label>
                  <span>Format</span>
                  <select
                    aria-label="Entry format"
                    value={draft.format}
                    disabled={selectedEntry?.status === 'deleted'}
                    onChange={(event) =>
                      onDraftChange({
                        ...draft,
                        format: event.target.value as EntryDraft['format'],
                      })
                    }
                  >
                    <option value="markdown">Markdown</option>
                    <option value="json">JSON</option>
                  </select>
                </label>
                <label>
                  <span>Title</span>
                  <input
                    aria-label="Entry title"
                    value={draft.title}
                    disabled={selectedEntry?.status === 'deleted'}
                    onChange={(event) => onDraftChange({ ...draft, title: event.target.value })}
                    placeholder="Human-readable title"
                  />
                </label>
                <label>
                  <span>Key</span>
                  <input
                    aria-label="Entry key"
                    value={draft.key}
                    disabled={Boolean(selectedEntry)}
                    onChange={(event) => onDraftChange({ ...draft, key: event.target.value })}
                    placeholder="stable-entry-key"
                    spellCheck={false}
                  />
                  {selectedEntry ? (
                    <small>Keys are stable. Create a new entry to use another key.</small>
                  ) : null}
                </label>
                <label>
                  <span>Kind</span>
                  <input
                    aria-label="Entry kind"
                    value={draft.kind}
                    disabled={selectedEntry?.status === 'deleted'}
                    onChange={(event) => onDraftChange({ ...draft, kind: event.target.value })}
                    placeholder="instruction"
                  />
                </label>
                <label>
                  <span>Tags</span>
                  <input
                    aria-label="Entry tags"
                    value={draft.tags}
                    disabled={selectedEntry?.status === 'deleted'}
                    onChange={(event) => onDraftChange({ ...draft, tags: event.target.value })}
                    placeholder="testing, workflow"
                  />
                </label>
              </div>

              <label className="content-field">
                <span>{draft.format === 'json' ? 'JSON value' : 'Markdown'}</span>
                <textarea
                  aria-label="Entry content"
                  value={draft.body}
                  disabled={selectedEntry?.status === 'deleted'}
                  onChange={(event) => onDraftChange({ ...draft, body: event.target.value })}
                  spellCheck={draft.format !== 'json'}
                  rows={16}
                  className={draft.format === 'json' ? 'code-input' : ''}
                  placeholder={
                    draft.format === 'json'
                      ? '{\n  "preference": true\n}'
                      : 'Write the durable context this entry should contribute.'
                  }
                />
              </label>
              {jsonError ? (
                <p className="field-error" role="alert">
                  {jsonError}
                </p>
              ) : null}

              <div className="editor-options">
                <label className="compact-check">
                  <input
                    type="checkbox"
                    checked={draft.locked}
                    disabled={selectedEntry?.status === 'deleted'}
                    onChange={(event) =>
                      onDraftChange({ ...draft, locked: event.target.checked })
                    }
                  />
                  <span>Lock entry against agent replacement</span>
                </label>
                {draft.format === 'json' && !jsonError && draft.body.trim() ? (
                  <button
                    type="button"
                    className="text-button"
                    onClick={() =>
                      onDraftChange({
                        ...draft,
                        body: JSON.stringify(JSON.parse(draft.body), null, 2),
                      })
                    }
                  >
                    Format JSON
                  </button>
                ) : null}
              </div>

              <footer className="editor-footer">
                <div className="editor-footer__meta">
                  <span>{scopeLayerDetail(scope?.kind ?? 'project')}</span>
                  {selectedEntry ? <span>Revision {selectedEntry.revision}</span> : null}
                </div>
                <div className="button-row">
                  {selectedEntry?.status === 'deleted' ? (
                    <button
                      type="button"
                      className="primary-button"
                      disabled={busyKey === 'restore-entry'}
                      onClick={() => onRestore(selectedEntry)}
                    >
                      {busyKey === 'restore-entry' ? 'Restoring…' : 'Restore entry'}
                    </button>
                  ) : (
                    <>
                      <button type="button" className="secondary-button" onClick={onDiscard}>
                        Discard draft
                      </button>
                      {selectedEntry ? (
                        <button
                          type="button"
                          className="danger-quiet-button"
                          onClick={() => onArchive(selectedEntry)}
                        >
                          Archive…
                        </button>
                      ) : null}
                      <button
                        type="submit"
                        className="primary-button"
                        disabled={
                          busyKey === 'save-entry' ||
                          !dirty ||
                          Boolean(jsonError) ||
                          !draft.packId ||
                          !draft.key.trim() ||
                          !draft.kind.trim()
                        }
                      >
                        {busyKey === 'save-entry' ? 'Saving…' : 'Save entry'}
                      </button>
                    </>
                  )}
                </div>
              </footer>
            </form>
          )}
        </section>

        <aside className="entry-inspector" aria-labelledby="entry-inspector-heading">
          <header className="pane-heading">
            <div>
              <h3 id="entry-inspector-heading">
                {selectedEntry ? entryLabel(selectedEntry) : 'Draft'}
              </h3>
            </div>
          </header>

          {selectedEntry ? (
            <div className="inspector-scroll">
              <section className="inspector-section">
                <h4>Record</h4>
                <dl className="property-list">
                  <div>
                    <dt>Status</dt>
                    <dd>
                      <StatusPill label={selectedEntry.status} />
                    </dd>
                  </div>
                  <div>
                    <dt>Scope</dt>
                    <dd title={selectedEntry.scopeId}>
                      {scopeLayerLabel(selectedEntry.scopeKind)}
                      <small>
                        {selectedEntry.scopeKind === 'task' ? 'Derived · ' : ''}
                        {selectedEntry.scopeLabel}
                      </small>
                    </dd>
                  </div>
                  <div>
                    <dt>Pack</dt>
                    <dd>{selectedEntry.packName}</dd>
                  </div>
                  <div>
                    <dt>Pack key</dt>
                    <dd className="path-value">{selectedEntry.packKey}</dd>
                  </div>
                  <div>
                    <dt>Kind / format</dt>
                    <dd>
                      {selectedEntry.kind} · {selectedEntry.format}
                    </dd>
                  </div>
                  <div>
                    <dt>Revision</dt>
                    <dd>{selectedEntry.revision}</dd>
                  </div>
                  <div>
                    <dt>Created</dt>
                    <dd>{formatTimestamp(selectedEntry.createdAt)}</dd>
                  </div>
                  <div>
                    <dt>Updated</dt>
                    <dd>{formatTimestamp(selectedEntry.updatedAt)}</dd>
                  </div>
                  <div>
                    <dt>Lock</dt>
                    <dd>{selectedEntry.locked ? 'Locked' : 'Unlocked'}</dd>
                  </div>
                </dl>
                <div className="tag-list" aria-label="Entry tags">
                  {selectedEntry.tags.length > 0 ? (
                    selectedEntry.tags.map((tag) => <span key={tag}>{tag}</span>)
                  ) : (
                    <small>No tags</small>
                  )}
                </div>
              </section>

              <section className="inspector-section">
                <h4>Provenance</h4>
                <dl className="property-list property-list--stacked">
                  <div>
                    <dt>Actor</dt>
                    <dd>{selectedEntry.provenance.actor}</dd>
                  </div>
                  <div>
                    <dt>Source</dt>
                    <dd>{selectedEntry.provenance.source}</dd>
                  </div>
                  {selectedEntry.provenance.sourceRef ? (
                    <div>
                      <dt>Source reference</dt>
                      <dd className="path-value">{selectedEntry.provenance.sourceRef}</dd>
                    </div>
                  ) : null}
                  {selectedEntry.provenance.runId ? (
                    <div>
                      <dt>Run</dt>
                      <dd>{selectedEntry.provenance.runId}</dd>
                    </div>
                  ) : null}
                  {selectedEntry.provenance.requestId ? (
                    <div>
                      <dt>Request</dt>
                      <dd>{selectedEntry.provenance.requestId}</dd>
                    </div>
                  ) : null}
                  {selectedEntry.provenance.note ? (
                    <div>
                      <dt>Note</dt>
                      <dd>{selectedEntry.provenance.note}</dd>
                    </div>
                  ) : null}
                </dl>
              </section>

              <section className="inspector-section">
                <div className="inspector-section__heading">
                  <h4>Referenced by</h4>
                  <span>{referencedRuns.length}</span>
                </div>
                {referencedRuns.length === 0 ? (
                  <p className="inspector-empty">No recorded run references this pack.</p>
                ) : (
                  <ul className="reference-list">
                    {referencedRuns.map((run) => (
                      <li key={run.id}>
                        <strong>{run.actor}</strong>
                        <p>{run.summary}</p>
                        <small>
                          {run.status} · {formatTimestamp(run.startedAt)}
                        </small>
                      </li>
                    ))}
                  </ul>
                )}
              </section>

              <section className="inspector-section" ref={historyRef}>
                <div className="inspector-section__heading">
                  <h4>History</h4>
                  <span>{revisions.length}</span>
                </div>
                {selectedEntry.revision > 1 && selectedEntry.status === 'active' ? (
                  <button
                    type="button"
                    className="secondary-button full-width-button"
                    onClick={() => onRevertPrevious(selectedEntry)}
                  >
                    Revert to revision {selectedEntry.revision - 1}…
                  </button>
                ) : null}
                {revisions.length === 0 ? (
                  <p className="inspector-empty">No recorded revisions for this entry.</p>
                ) : (
                  <ol className="history-list">
                    {revisions.map((revision) => (
                      <li
                        id={`history-${revision.id}`}
                        key={revision.id}
                        className={focusRevisionId === revision.id ? 'history-item--focused' : ''}
                      >
                        <strong>{revision.note}</strong>
                        <p>{revision.changeSummary}</p>
                        <small>
                          {revision.author} · {formatTimestamp(revision.createdAt)}
                        </small>
                        {revision.restorable ? (
                          <button
                            type="button"
                            className="text-button"
                            onClick={() => onRestoreRevision(revision)}
                          >
                            Restore this revision…
                          </button>
                        ) : null}
                      </li>
                    ))}
                  </ol>
                )}
              </section>
            </div>
          ) : (
            <EmptyState
              title="New entry draft"
              body="Choose a pack, then define a stable key and durable content."
            />
          )}
        </aside>
      </div>
    </div>
  )
}
