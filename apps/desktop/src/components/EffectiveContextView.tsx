import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import type { DesktopApi } from '../api/desktopApi'
import { friendlyDesktopError } from '../api/desktopApi'
import type { ContextPreview, DashboardSnapshot, WorkspaceNode } from '../types'
import {
  EmptyState,
  SectionHeader,
  StatusPill,
} from './Common'
import { formatBytes, formatTimestamp, scopeLayerLabel } from '../lib/ui'

function flattenWorkspace(nodes: WorkspaceNode[]): WorkspaceNode[] {
  return nodes.flatMap((node) => [node, ...flattenWorkspace(node.children)])
}

async function copyExactText(value: string) {
  if (navigator.clipboard?.writeText) {
    await navigator.clipboard.writeText(value)
    return
  }
  const textarea = document.createElement('textarea')
  textarea.value = value
  textarea.setAttribute('readonly', '')
  textarea.style.position = 'fixed'
  textarea.style.opacity = '0'
  document.body.append(textarea)
  textarea.select()
  const copied = document.execCommand?.('copy')
  textarea.remove()
  if (!copied) throw new Error('Clipboard is unavailable')
}

export function EffectiveContextView({
  api,
  snapshot,
  initialScopeId,
  onOpenEntry,
  onAnnounce,
  onError,
}: {
  api: DesktopApi
  snapshot: DashboardSnapshot
  initialScopeId: string
  onOpenEntry: (entryId: string, scopeId: string) => void
  onAnnounce: (message: string) => void
  onError: (message: string) => void
}) {
  const scopes = useMemo(() => flattenWorkspace(snapshot.workspace), [snapshot.workspace])
  const availableAdapters = useMemo(
    () => snapshot.adapters.filter((adapter) => adapter.enabled),
    [snapshot.adapters],
  )
  const [scopeId, setScopeId] = useState(initialScopeId)
  const [adapterId, setAdapterId] = useState(
    availableAdapters[0]?.id ?? 'generic',
  )
  const [preview, setPreview] = useState<ContextPreview | null>(null)
  const [loading, setLoading] = useState(true)
  const [failed, setFailed] = useState(false)
  const [panel, setPanel] = useState<'exact' | 'trace'>('exact')
  const pairRef = useRef({ scopeId: initialScopeId, adapterId })
  const requestGenerationRef = useRef(0)

  const updatePair = useCallback((nextScopeId: string, nextAdapterId: string) => {
    requestGenerationRef.current += 1
    pairRef.current = { scopeId: nextScopeId, adapterId: nextAdapterId }
    setPreview(null)
    setFailed(false)
    setScopeId(nextScopeId)
    setAdapterId(nextAdapterId)
  }, [])

  useEffect(() => {
    if (
      scopes.some((scope) => scope.id === initialScopeId) &&
      initialScopeId !== pairRef.current.scopeId
    ) {
      updatePair(initialScopeId, pairRef.current.adapterId)
    }
  }, [initialScopeId, scopes, updatePair])

  useEffect(() => {
    if (
      adapterId !== 'generic' &&
      !availableAdapters.some((adapter) => adapter.id === adapterId)
    ) {
      updatePair(scopeId, availableAdapters[0]?.id ?? 'generic')
    }
  }, [adapterId, availableAdapters, scopeId, updatePair])

  const compose = useCallback(async () => {
    const pair = { scopeId, adapterId }
    pairRef.current = pair
    const generation = ++requestGenerationRef.current
    try {
      setLoading(true)
      setFailed(false)
      setPreview(null)
      const result = await api.composeEffectiveContext({
        scopeId: pair.scopeId,
        destinationAdapter: pair.adapterId,
      })
      if (
        generation !== requestGenerationRef.current ||
        pair.scopeId !== pairRef.current.scopeId ||
        pair.adapterId !== pairRef.current.adapterId
      ) {
        return
      }
      setPreview(result)
    } catch (error) {
      if (generation !== requestGenerationRef.current) return
      setPreview(null)
      setFailed(true)
      onError(friendlyDesktopError(error))
    } finally {
      if (generation === requestGenerationRef.current) setLoading(false)
    }
  }, [adapterId, api, onError, scopeId])

  useEffect(() => {
    void compose()
  }, [compose])

  const selectedScope = scopes.find((scope) => scope.id === scopeId)

  return (
    <div className="view-stack effective-view">
      <SectionHeader
        eyebrow="Backend-composed output"
        title="Effective Context"
        detail="The exact Markdown, ordering, exclusions, and metrics below come from compose_effective_context."
        actions={
          <button type="button" className="secondary-button" disabled={loading} onClick={compose}>
            {loading ? 'Composing…' : 'Compose again'}
          </button>
        }
      />

      <section className="composition-controls" aria-label="Composition controls">
        <label>
          <span>Scope</span>
          <select
            aria-label="Effective Context scope"
            value={scopeId}
            onChange={(event) => updatePair(event.target.value, adapterId)}
          >
            {scopes.map((scope) => (
              <option key={scope.id} value={scope.id}>
                {scopeLayerLabel(scope.kind)}
                {scope.kind === 'task' ? ' (derived)' : ''} — {scope.label}
              </option>
            ))}
          </select>
          <small title={selectedScope?.id}>
            {selectedScope?.kind === 'task' ? 'Derived scope · ' : ''}
            {selectedScope?.description}
          </small>
        </label>
        <label>
          <span>Destination adapter</span>
          <select
            aria-label="Destination adapter"
            value={adapterId}
            onChange={(event) => updatePair(scopeId, event.target.value)}
          >
            {availableAdapters.length === 0 ? <option value="generic">generic</option> : null}
            {availableAdapters.map((adapter) => (
              <option key={adapter.id} value={adapter.id}>
                {adapter.name}
              </option>
            ))}
          </select>
          <small>One target is composed at a time.</small>
        </label>
      </section>

      {loading && !preview ? (
        <div className="loading-panel" role="status">
          <span className="spinner" aria-hidden="true" />
          <div>
            <strong>Composing exact output</strong>
            <p>Waiting for the local backend.</p>
          </div>
        </div>
      ) : null}

      {failed ? (
        <EmptyState
          title="Effective Context is unavailable"
          body="Refresh Connections, then retry composition. No client-side reconstruction is shown."
        >
          <button type="button" className="primary-button" onClick={compose}>
            Retry composition
          </button>
        </EmptyState>
      ) : null}

      {preview ? (
        <>
          <section className="composition-ledger" aria-label="Composition summary">
            <div>
              <p className="eyebrow">Destination</p>
              <strong>{preview.destinationAdapter}</strong>
              <small>Generated {formatTimestamp(preview.generatedAt)}</small>
            </div>
            <dl>
              <div>
                <dt>Rendered</dt>
                <dd>{formatBytes(preview.metrics.renderedBytes)}</dd>
              </div>
              <div>
                <dt>Estimated tokens</dt>
                <dd>{preview.metrics.estimatedTokens.toLocaleString()}</dd>
              </div>
              <div>
                <dt>Included</dt>
                <dd>{preview.metrics.includedEntries}</dd>
              </div>
              <div>
                <dt>Excluded</dt>
                <dd>{preview.metrics.excludedEntries}</dd>
              </div>
            </dl>
          </section>

          {preview.warnings.length > 0 ? (
            <section className="warning-callout" aria-labelledby="composition-warning-heading">
              <h3 id="composition-warning-heading">Composition warnings</h3>
              <ul>
                {preview.warnings.map((warning) => (
                  <li key={warning}>{warning}</li>
                ))}
              </ul>
            </section>
          ) : null}

          <div className="composition-tabs" role="tablist" aria-label="Effective Context panels">
            <button
              type="button"
              role="tab"
              aria-selected={panel === 'exact'}
              className={panel === 'exact' ? 'is-selected' : ''}
              onClick={() => setPanel('exact')}
            >
              Exact output
            </button>
            <button
              type="button"
              role="tab"
              aria-selected={panel === 'trace'}
              className={panel === 'trace' ? 'is-selected' : ''}
              onClick={() => setPanel('trace')}
            >
              Inclusion trace
            </button>
          </div>

          {panel === 'exact' ? (
            <section className="exact-output-panel" aria-labelledby="exact-output-heading">
              <header>
                <div>
                  <p className="eyebrow">Byte-for-byte backend Markdown</p>
                  <h3 id="exact-output-heading">Rendered Markdown</h3>
                </div>
                <button
                  type="button"
                  className="secondary-button"
                  disabled={!preview.renderedMarkdown}
                  onClick={async () => {
                    try {
                      await copyExactText(preview.renderedMarkdown)
                      onAnnounce('Copied the exact backend Markdown.')
                    } catch {
                      onError(
                        'Clipboard access is unavailable. Select the exact output and copy it manually.',
                      )
                    }
                  }}
                >
                  Copy exact Markdown
                </button>
              </header>
              {preview.renderedMarkdown ? (
                <pre data-testid="exact-rendered-markdown">{preview.renderedMarkdown}</pre>
              ) : (
                <EmptyState
                  title="No rendered Markdown"
                  body="The backend returned an empty composition for this scope."
                />
              )}
            </section>
          ) : (
            <div className="trace-grid">
              <section className="trace-sections" aria-labelledby="ordered-sections-heading">
                <header className="subsection-heading">
                  <div>
                    <p className="eyebrow">Backend order</p>
                    <h3 id="ordered-sections-heading">Global → Project → Task</h3>
                  </div>
                  <span>{preview.sections.length} sections</span>
                </header>
                {preview.sections.length === 0 ? (
                  <EmptyState
                    title="No included sections"
                    body="No active durable entry contributed to this composition."
                  />
                ) : (
                  <ol className="section-ledger">
                    {preview.sections.map((section) => (
                      <li key={section.id}>
                        <span className="section-ledger__order">{section.order + 1}</span>
                        <div>
                          <div className="row-heading">
                            <strong>
                              {scopeLayerLabel(section.scopeKind)}
                              {section.scopeKind === 'task' ? ' · derived' : ''}
                            </strong>
                            <StatusPill label={section.layer || section.scopeKind} />
                          </div>
                          <h4>{section.packName}</h4>
                          <p>{section.scopeLabel}</p>
                          <small>
                            {section.entryIds.length} entries · {section.tokens.toLocaleString()}{' '}
                            estimated tokens
                          </small>
                        </div>
                      </li>
                    ))}
                  </ol>
                )}
              </section>

              <section className="trace-entries" aria-labelledby="included-entries-heading">
                <header className="subsection-heading">
                  <div>
                    <p className="eyebrow">Ordered provenance</p>
                    <h3 id="included-entries-heading">Included entries</h3>
                  </div>
                  <span>{preview.includedEntries.length}</span>
                </header>
                <ol className="trace-list">
                  {preview.includedEntries.map((entry) => (
                    <li key={`${entry.entryId}-${entry.order}`}>
                      <button
                        type="button"
                        onClick={() => onOpenEntry(entry.entryId, entry.scopeId)}
                      >
                        <span className="trace-list__order">{entry.order + 1}</span>
                        <span>
                          <strong>{entry.title ?? entry.key}</strong>
                          <small>
                            {entry.packName} · r{entry.revision} ·{' '}
                            {entry.tokenEstimate.toLocaleString()} estimated tokens
                          </small>
                          <small>
                            {entry.provenance.actor} via {entry.provenance.source}
                          </small>
                        </span>
                      </button>
                    </li>
                  ))}
                </ol>
              </section>

              <section className="trace-exclusions" aria-labelledby="excluded-entries-heading">
                <header className="subsection-heading">
                  <div>
                    <p className="eyebrow">Backend exclusions</p>
                    <h3 id="excluded-entries-heading">Excluded entries</h3>
                  </div>
                  <span>{preview.exclusions.length}</span>
                </header>
                {preview.exclusions.length === 0 ? (
                  <p className="subtle-copy">No exclusions were reported.</p>
                ) : (
                  <ul className="trace-list">
                    {preview.exclusions.map((entry) => (
                      <li key={`${entry.entryId}-${entry.revision}`}>
                        <button
                          type="button"
                          onClick={() => onOpenEntry(entry.entryId, entry.scopeId)}
                        >
                          <span>
                            <strong>{entry.entryKey}</strong>
                            <small>
                              {entry.packName} · r{entry.revision}
                            </small>
                          </span>
                          <StatusPill label={entry.reason} />
                        </button>
                      </li>
                    ))}
                  </ul>
                )}
              </section>
            </div>
          )}
        </>
      ) : null}
    </div>
  )
}
