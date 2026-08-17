import { useEffect, useMemo, useState, type ReactNode } from 'react'
import type {
  AdapterStatus,
  ContextPack,
  ContextPreview,
  DashboardSnapshot,
  ReviewDecision,
  RevisionEntry,
  SavePackInput,
  SearchResult,
  Settings,
} from './types'
import { createDesktopApi, desktopApi, type DesktopApi } from './api/desktopApi'
import { flattenWorkspace, type FlatScopeNode } from './lib/contextUtils'
import './App.css'

type MainView = 'overview' | 'review' | 'operations'
type LoadState = 'loading' | 'ready' | 'error'
type BannerTone = 'success' | 'error' | 'info'

interface PackEditorDraft {
  id?: string
  scopeId: string
  name: string
  status: ContextPack['status']
  summary: string
  tags: string
  body: string
}

interface AppProps {
  api?: DesktopApi
}

const NEW_PACK_ID = '__new_pack__'
const DEFAULT_EXPORT_PATH = '~/Desktop/context-manager-export.json'
const views: Array<{ id: MainView; label: string; description: string }> = [
  { id: 'overview', label: 'Overview', description: 'Packs, previews, and run activity.' },
  { id: 'review', label: 'Search & review', description: 'FTS search, queue triage, and restore.' },
  { id: 'operations', label: 'Operations', description: 'Adapters, import/export, and local settings.' },
]

function formatTimestamp(value: string) {
  return new Intl.DateTimeFormat('en-US', {
    month: 'short',
    day: 'numeric',
    hour: 'numeric',
    minute: '2-digit',
  }).format(new Date(value))
}

function formatDuration(value: number) {
  const minutes = Math.round(value / 60000)
  if (minutes < 60) {
    return `${minutes}m`
  }

  const hours = Math.floor(minutes / 60)
  const remainder = minutes % 60
  return remainder === 0 ? `${hours}h` : `${hours}h ${remainder}m`
}

function statusTone(value: string) {
  const normalized = value.toLowerCase()
  if (normalized === 'active' || normalized === 'healthy' || normalized === 'completed' || normalized === 'synced') {
    return 'positive'
  }
  if (normalized === 'draft' || normalized === 'review' || normalized === 'degraded' || normalized === 'running' || normalized === 'in progress' || normalized === 'queued' || normalized === 'monitoring' || normalized === 'needs review') {
    return 'warning'
  }
  if (normalized === 'offline' || normalized === 'failed' || normalized === 'blocked') {
    return 'negative'
  }
  return 'neutral'
}

function emptyDraft(scopeId: string): PackEditorDraft {
  return {
    scopeId,
    name: '',
    status: 'draft',
    summary: '',
    tags: 'draft, review',
    body: '',
  }
}

function draftFromPack(pack: ContextPack): PackEditorDraft {
  return {
    id: pack.id,
    scopeId: pack.scopeId,
    name: pack.name,
    status: pack.status,
    summary: pack.summary,
    tags: pack.tags.join(', '),
    body: pack.body,
  }
}

function packsForScope(snapshot: DashboardSnapshot | null, scopeId: string) {
  if (!snapshot) {
    return []
  }

  return snapshot.packs.filter((pack) => pack.scopeId === scopeId)
}

function findFirstPackId(snapshot: DashboardSnapshot, scopeId: string) {
  return packsForScope(snapshot, scopeId)[0]?.id ?? NEW_PACK_ID
}

function bannerMessage(tone: BannerTone, message: string) {
  return { tone, message }
}

function Card({
  title,
  subtitle,
  actions,
  children,
}: {
  title: string
  subtitle?: string
  actions?: ReactNode
  children: ReactNode
}) {
  return (
    <section className="card">
      <header className="card-header">
        <div>
          <h2>{title}</h2>
          {subtitle ? <p className="card-subtitle">{subtitle}</p> : null}
        </div>
        {actions ? <div className="card-actions">{actions}</div> : null}
      </header>
      {children}
    </section>
  )
}

function EmptyState({ title, body }: { title: string; body: string }) {
  return (
    <div className="empty-state" role="status">
      <strong>{title}</strong>
      <p>{body}</p>
    </div>
  )
}

function StatusPill({ label, tone }: { label: string; tone: string }) {
  return <span className={`status-pill status-pill--${tone}`}>{label}</span>
}

function SummaryMetric({ value, label, detail }: { value: string; label: string; detail: string }) {
  return (
    <article className="metric-card">
      <p className="metric-value">{value}</p>
      <p className="metric-label">{label}</p>
      <p className="metric-detail">{detail}</p>
    </article>
  )
}

function SearchResultRow({ result }: { result: SearchResult }) {
  return (
    <li className="result-row">
      <div>
        <div className="row-heading">
          <strong>{result.title}</strong>
          <StatusPill label={result.kind} tone="neutral" />
        </div>
        <p>{result.excerpt}</p>
        <div className="row-meta">
          <span>{result.scopeLabel}</span>
          <span>{formatTimestamp(result.updatedAt)}</span>
          <span>score {result.score}</span>
        </div>
      </div>
    </li>
  )
}

function ScopeButton({
  node,
  selected,
  onSelect,
}: {
  node: FlatScopeNode
  selected: boolean
  onSelect: (scopeId: string) => void
}) {
  return (
    <button
      type="button"
      className={`scope-button ${selected ? 'scope-button--selected' : ''}`}
      onClick={() => onSelect(node.id)}
      aria-pressed={selected}
    >
      <div className="scope-button__meta">
        <span className="scope-button__kind">{node.kind}</span>
        <StatusPill label={node.status} tone={statusTone(node.status)} />
      </div>
      <strong style={{ paddingLeft: `${node.depth * 12}px` }}>{node.label}</strong>
      <span>{node.description}</span>
    </button>
  )
}

function AdapterRow({
  adapter,
  onToggle,
  disabled,
}: {
  adapter: AdapterStatus
  onToggle: (adapter: AdapterStatus) => void
  disabled: boolean
}) {
  return (
    <li className="adapter-row">
      <div>
        <div className="row-heading">
          <strong>{adapter.name}</strong>
          <StatusPill label={adapter.health} tone={statusTone(adapter.health)} />
        </div>
        <p>{adapter.note}</p>
        <div className="row-meta">
          <span>{adapter.kind}</span>
          <span>{adapter.path}</span>
          <span>{formatTimestamp(adapter.lastCheckedAt)}</span>
        </div>
      </div>
      <label className="toggle">
        <input
          type="checkbox"
          checked={adapter.enabled}
          disabled={disabled}
          onChange={() => onToggle(adapter)}
        />
        <span>{adapter.enabled ? 'Monitored' : 'Ignored'}</span>
      </label>
    </li>
  )
}

function App({ api = desktopApi }: AppProps) {
  const [loadState, setLoadState] = useState<LoadState>('loading')
  const [snapshot, setSnapshot] = useState<DashboardSnapshot | null>(null)
  const [preview, setPreview] = useState<ContextPreview | null>(null)
  const [revisions, setRevisions] = useState<RevisionEntry[]>([])
  const [selectedScopeId, setSelectedScopeId] = useState('')
  const [selectedPackId, setSelectedPackId] = useState(NEW_PACK_ID)
  const [selectedReviewId, setSelectedReviewId] = useState('')
  const [reviewDraft, setReviewDraft] = useState('')
  const [editorDraft, setEditorDraft] = useState<PackEditorDraft>(emptyDraft(''))
  const [activeView, setActiveView] = useState<MainView>('overview')
  const [searchQuery, setSearchQuery] = useState('')
  const [searchResults, setSearchResults] = useState<SearchResult[]>([])
  const [searchLoading, setSearchLoading] = useState(false)
  const [settingsDraft, setSettingsDraft] = useState<Settings | null>(null)
  const [banner, setBanner] = useState<{ tone: BannerTone; message: string } | null>(null)
  const [busyKey, setBusyKey] = useState('')
  const [ioPath, setIoPath] = useState(DEFAULT_EXPORT_PATH)

  const scopes = useMemo(() => flattenWorkspace(snapshot?.workspace ?? []), [snapshot])
  const currentScope = scopes.find((scope) => scope.id === selectedScopeId)
  const currentPacks = useMemo(
    () => packsForScope(snapshot, selectedScopeId),
    [snapshot, selectedScopeId],
  )
  const selectedPack = currentPacks.find((pack) => pack.id === selectedPackId)
  const selectedReview = snapshot?.reviewQueue.find((item) => item.id === selectedReviewId) ?? null

  async function refreshDashboard(preferredScopeId?: string, preferredPackId?: string) {
    const nextSnapshot = await api.loadDashboard()
    const nextScopeId =
      preferredScopeId && nextSnapshot.workspace.length > 0
        ? flattenWorkspace(nextSnapshot.workspace).some((scope) => scope.id === preferredScopeId)
          ? preferredScopeId
          : nextSnapshot.selectedScopeId
        : nextSnapshot.selectedScopeId
    const nextPackId =
      preferredPackId && nextSnapshot.packs.some((pack) => pack.id === preferredPackId)
        ? preferredPackId
        : findFirstPackId(nextSnapshot, nextScopeId)

    setSnapshot(nextSnapshot)
    setSelectedScopeId(nextScopeId)
    setSelectedPackId(nextPackId)
    setSelectedReviewId(nextSnapshot.reviewQueue[0]?.id ?? '')
    setSettingsDraft(nextSnapshot.settings)
  }

  useEffect(() => {
    let cancelled = false

    async function load() {
      try {
        setLoadState('loading')
        const nextSnapshot = await api.loadDashboard()
        if (cancelled) {
          return
        }

        const firstScopeId = nextSnapshot.selectedScopeId
        setSnapshot(nextSnapshot)
        setSelectedScopeId(firstScopeId)
        setSelectedPackId(findFirstPackId(nextSnapshot, firstScopeId))
        setSelectedReviewId(nextSnapshot.reviewQueue[0]?.id ?? '')
        setSettingsDraft(nextSnapshot.settings)
        setLoadState('ready')
      } catch (error) {
        if (!cancelled) {
          setLoadState('error')
          setBanner(bannerMessage('error', error instanceof Error ? error.message : 'Failed to load the desktop control plane.'))
        }
      }
    }

    void load()

    return () => {
      cancelled = true
    }
  }, [api])

  useEffect(() => {
    if (!snapshot || !selectedScopeId) {
      return
    }

    let cancelled = false

    async function loadPreview() {
      try {
        const nextPreview = await api.composePreview(selectedScopeId)
        if (!cancelled) {
          setPreview(nextPreview)
        }
      } catch (error) {
        if (!cancelled) {
          setBanner(bannerMessage('error', error instanceof Error ? error.message : 'Unable to compose the preview.'))
          setPreview(null)
        }
      }
    }

    void loadPreview()

    return () => {
      cancelled = true
    }
  }, [api, snapshot, selectedScopeId])

  useEffect(() => {
    if (!snapshot) {
      return
    }

    if (selectedPackId === NEW_PACK_ID) {
      setEditorDraft(emptyDraft(selectedScopeId))
      setRevisions([])
      return
    }

    const pack = snapshot.packs.find((candidate) => candidate.id === selectedPackId)
    if (!pack) {
      return
    }

    setEditorDraft(draftFromPack(pack))
    const packId = pack.id

    let cancelled = false

    async function loadRevisions() {
      try {
        const nextRevisions = await api.listRevisions(packId)
        if (!cancelled) {
          setRevisions(nextRevisions)
        }
      } catch (error) {
        if (!cancelled) {
          setBanner(bannerMessage('error', error instanceof Error ? error.message : 'Unable to load revision history.'))
          setRevisions([])
        }
      }
    }

    void loadRevisions()

    return () => {
      cancelled = true
    }
  }, [api, selectedPackId, selectedScopeId, snapshot])

  useEffect(() => {
    if (!selectedReview) {
      setReviewDraft('')
      return
    }

    setReviewDraft(selectedReview.suggestedEdit)
  }, [selectedReview])

  useEffect(() => {
    if (!searchQuery.trim()) {
      setSearchResults([])
      setSearchLoading(false)
      return
    }

    let cancelled = false
    const timeout = window.setTimeout(async () => {
      try {
        setSearchLoading(true)
        const results = await api.searchIndex(searchQuery)
        if (!cancelled) {
          setSearchResults(results)
        }
      } catch (error) {
        if (!cancelled) {
          setBanner(bannerMessage('error', error instanceof Error ? error.message : 'Search failed.'))
          setSearchResults([])
        }
      } finally {
        if (!cancelled) {
          setSearchLoading(false)
        }
      }
    }, 180)

    return () => {
      cancelled = true
      window.clearTimeout(timeout)
    }
  }, [api, searchQuery])

  function handleScopeChange(scopeId: string) {
    if (!snapshot) {
      return
    }

    setSelectedScopeId(scopeId)
    setSelectedPackId(findFirstPackId(snapshot, scopeId))
    setBanner(null)
  }

  async function handleSavePack() {
    if (!selectedScopeId) {
      return
    }

    const payload: SavePackInput = {
      id: editorDraft.id,
      scopeId: selectedScopeId,
      name: editorDraft.name,
      status: editorDraft.status,
      summary: editorDraft.summary,
      tags: editorDraft.tags
        .split(',')
        .map((tag) => tag.trim())
        .filter(Boolean),
      body: editorDraft.body,
    }

    try {
      setBusyKey('save-pack')
      const saved = await api.savePack(payload)
      await refreshDashboard(selectedScopeId, saved.id)
      setLoadState('ready')
      setBanner(bannerMessage('success', `Saved ${saved.name} to ${saved.scopeLabel}.`))
    } catch (error) {
      setBanner(bannerMessage('error', error instanceof Error ? error.message : 'Unable to save the pack.'))
    } finally {
      setBusyKey('')
    }
  }

  async function handleReviewAction(decision: ReviewDecision) {
    if (!selectedReview) {
      return
    }

    try {
      setBusyKey(`review-${decision}`)
      await api.reviewDecision({
        itemId: selectedReview.id,
        decision,
        editedContent: decision === 'edit' ? reviewDraft : undefined,
      })
      await refreshDashboard(selectedReview.scopeId, selectedReview.packId)
      setBanner(
        bannerMessage(
          'success',
          decision === 'reject'
            ? `Rejected ${selectedReview.title}.`
            : `Applied ${decision === 'approve' ? 'approved' : 'edited'} review update for ${selectedReview.packName}.`,
        ),
      )
    } catch (error) {
      setBanner(bannerMessage('error', error instanceof Error ? error.message : 'Unable to process the review item.'))
    } finally {
      setBusyKey('')
    }
  }

  async function handleRestore(revision: RevisionEntry) {
    try {
      setBusyKey(`restore-${revision.id}`)
      const result = await api.restoreRevision(revision.id)
      await refreshDashboard(selectedScopeId, result.entityId)
      setBanner(bannerMessage('success', `Restored ${revision.entityLabel} from ${revision.id}.`))
    } catch (error) {
      setBanner(bannerMessage('error', error instanceof Error ? error.message : 'Unable to restore the revision.'))
    } finally {
      setBusyKey('')
    }
  }

  async function handleToggleAdapter(adapter: AdapterStatus) {
    try {
      setBusyKey(`adapter-${adapter.id}`)
      await api.toggleAdapter(adapter.id, !adapter.enabled)
      await refreshDashboard(selectedScopeId, selectedPackId === NEW_PACK_ID ? undefined : selectedPackId)
      setBanner(
        bannerMessage(
          'success',
          `${adapter.name} ${adapter.enabled ? 'disabled' : 'enabled'} locally.`,
        ),
      )
    } catch (error) {
      setBanner(bannerMessage('error', error instanceof Error ? error.message : 'Unable to update the adapter.'))
    } finally {
      setBusyKey('')
    }
  }

  async function handleSaveSettings() {
    if (!settingsDraft) {
      return
    }

    try {
      setBusyKey('save-settings')
      await api.saveSettings(settingsDraft)
      await refreshDashboard(selectedScopeId, selectedPackId === NEW_PACK_ID ? undefined : selectedPackId)
      setBanner(bannerMessage('success', 'Saved desktop settings.'))
    } catch (error) {
      setBanner(bannerMessage('error', error instanceof Error ? error.message : 'Unable to save settings.'))
    } finally {
      setBusyKey('')
    }
  }

  async function handleArchive(direction: 'import' | 'export') {
    try {
      setBusyKey(direction)
      const summary =
        direction === 'export' ? await api.exportArchive(ioPath) : await api.importArchive(ioPath)
      await refreshDashboard(selectedScopeId, selectedPackId === NEW_PACK_ID ? undefined : selectedPackId)
      setBanner(
        bannerMessage(
          'success',
          `${direction === 'export' ? 'Exported' : 'Imported'} ${summary.packsImported} packs at ${summary.path}.`,
        ),
      )
    } catch (error) {
      setBanner(
        bannerMessage(
          'error',
          error instanceof Error ? error.message : `Unable to ${direction} the local archive.`,
        ),
      )
    } finally {
      setBusyKey('')
    }
  }

  if (loadState === 'loading' && !snapshot) {
    return (
      <main className="app-shell app-shell--centered">
        <div className="loading-state" role="status">
          <div className="spinner" aria-hidden="true"></div>
          <h1>Loading the local context control plane</h1>
          <p>Preparing packs, review queues, adapter health, and revision history.</p>
        </div>
      </main>
    )
  }

  if (loadState === 'error' || !snapshot || !settingsDraft) {
    return (
      <main className="app-shell app-shell--centered">
        <div className="loading-state loading-state--error">
          <h1>Couldn’t load the desktop control plane</h1>
          <p>{banner?.message ?? 'The local snapshot is unavailable right now.'}</p>
          <button type="button" className="primary-button" onClick={() => window.location.reload()}>
            Retry
          </button>
        </div>
      </main>
    )
  }

  return (
    <main className="app-shell">
      <aside className="sidebar">
        <div className="brand-block">
          <div className="brand-mark" aria-hidden="true">
            <span></span>
          </div>
          <div>
            <p className="eyebrow">Universal Context Manager</p>
            <h1>Desktop control plane</h1>
          </div>
        </div>

        <div className="sidebar-status">
          <StatusPill label={snapshot.connected ? 'Local daemon connected' : 'Offline'} tone={snapshot.connected ? 'positive' : 'negative'} />
          <span>Last sync {formatTimestamp(snapshot.lastSyncAt)}</span>
        </div>

        <nav className="view-nav" aria-label="Primary views">
          {views.map((view) => (
            <button
              key={view.id}
              type="button"
              className={`view-button ${activeView === view.id ? 'view-button--selected' : ''}`}
              onClick={() => setActiveView(view.id)}
            >
              <strong>{view.label}</strong>
              <span>{view.description}</span>
            </button>
          ))}
        </nav>

        <div className="scope-list" aria-label="Global, project, and task scopes">
          <div className="section-heading">
            <strong>Scopes</strong>
            <button
              type="button"
              className="ghost-button"
              onClick={() => {
                setSelectedPackId(NEW_PACK_ID)
                setEditorDraft(emptyDraft(selectedScopeId))
              }}
            >
              New pack
            </button>
          </div>
          {scopes.map((scope) => (
            <ScopeButton
              key={scope.id}
              node={scope}
              selected={scope.id === selectedScopeId}
              onSelect={handleScopeChange}
            />
          ))}
        </div>
      </aside>

      <section className="workspace">
        <header className="workspace-header">
          <div>
            <p className="eyebrow">Local-first workspace</p>
            <h2>{currentScope?.label ?? 'Select a scope'}</h2>
            <p className="workspace-copy">
              {currentScope?.description ?? 'Choose a global, project, or task scope from the sidebar.'}
            </p>
          </div>
          <div className="workspace-header__meta">
            <StatusPill label={currentScope?.status ?? 'Unknown'} tone={statusTone(currentScope?.status ?? 'unknown')} />
            <span>{snapshot.notices.length} notice{snapshot.notices.length === 1 ? '' : 's'}</span>
          </div>
        </header>

        {banner ? <div className={`banner banner--${banner.tone}`}>{banner.message}</div> : null}

        <section className="metrics-grid">
          <SummaryMetric
            value={snapshot.stats.activePacks.toString()}
            label="Active packs"
            detail="Approved and included in composed previews."
          />
          <SummaryMetric
            value={snapshot.stats.pendingReviews.toString()}
            label="Pending reviews"
            detail="Human-governed changes waiting for a decision."
          />
          <SummaryMetric
            value={snapshot.stats.healthyAdapters.toString()}
            label="Healthy adapters"
            detail="Local bridges reporting current status."
          />
          <SummaryMetric
            value={snapshot.stats.runningAgents.toString()}
            label="Running agents"
            detail="Background context runs currently executing."
          />
        </section>

        {snapshot.notices.length > 0 ? (
          <Card title="Operator notices" subtitle="Recent local warnings and guidance.">
            <ul className="notice-list">
              {snapshot.notices.map((notice) => (
                <li key={notice}>{notice}</li>
              ))}
            </ul>
          </Card>
        ) : null}

        {activeView === 'overview' ? (
          <div className="content-grid content-grid--overview">
            <Card
              title="Context packs"
              subtitle="Select a pack to edit, review metadata, and manage status."
              actions={
                <button type="button" className="ghost-button" onClick={() => setSelectedPackId(NEW_PACK_ID)}>
                  New draft
                </button>
              }
            >
              <div className="split-panel">
                {currentPacks.length === 0 ? (
                  <EmptyState
                    title="No packs in this scope"
                    body="Use the editor to create the first draft for this scope."
                  />
                ) : (
                  <ul className="list-panel" aria-label="Context packs in this scope">
                    {currentPacks.map((pack) => (
                      <li key={pack.id}>
                        <button
                          type="button"
                          className={`list-button ${selectedPackId === pack.id ? 'list-button--selected' : ''}`}
                          onClick={() => setSelectedPackId(pack.id)}
                        >
                          <div className="row-heading">
                            <strong>{pack.name}</strong>
                            <StatusPill label={pack.status} tone={statusTone(pack.status)} />
                          </div>
                          <p>{pack.summary}</p>
                          <div className="row-meta">
                            <span>{pack.tokenEstimate.toLocaleString()} tokens</span>
                            <span>rev {pack.revision}</span>
                            <span>{formatTimestamp(pack.updatedAt)}</span>
                          </div>
                        </button>
                      </li>
                    ))}
                  </ul>
                )}

                <div className="editor-panel">
                    <div className="form-grid">
                      <label>
                        <span>Pack name</span>
                        <input
                          value={editorDraft.name}
                          onChange={(event) =>
                            setEditorDraft((draft) => ({ ...draft, name: event.target.value }))
                          }
                          placeholder="Reviewer notes"
                        />
                      </label>
                      <label>
                        <span>Status</span>
                        <select
                          value={editorDraft.status}
                          disabled={editorDraft.status === 'review'}
                          onChange={(event) =>
                            setEditorDraft((draft) => ({
                              ...draft,
                              status: event.target.value as ContextPack['status'],
                            }))
                          }
                        >
                          <option value="draft">Draft</option>
                          <option value="review" disabled>
                            Pending review
                          </option>
                          <option value="active">Active</option>
                        </select>
                      </label>
                      <label className="form-grid__full">
                        <span>Summary</span>
                        <input
                          value={editorDraft.summary}
                          onChange={(event) =>
                            setEditorDraft((draft) => ({ ...draft, summary: event.target.value }))
                          }
                          placeholder="One sentence operators will scan first."
                        />
                      </label>
                      <label className="form-grid__full">
                        <span>Tags</span>
                        <input
                          value={editorDraft.tags}
                          onChange={(event) =>
                            setEditorDraft((draft) => ({ ...draft, tags: event.target.value }))
                          }
                          placeholder="review, migration, rollout"
                        />
                      </label>
                      <label className="form-grid__full">
                        <span>Body</span>
                        <textarea
                          value={editorDraft.body}
                          onChange={(event) =>
                            setEditorDraft((draft) => ({ ...draft, body: event.target.value }))
                          }
                          rows={10}
                          placeholder="Capture the scoped context operators and agents should receive."
                        />
                      </label>
                    </div>

                    <div className="inline-meta">
                      <span>Scope: {currentScope?.label ?? 'Unknown'}</span>
                      <span>{selectedPack?.provenance.length ?? 1} provenance source{selectedPack?.provenance.length === 1 ? '' : 's'}</span>
                    </div>

                    {selectedPack?.provenance.length ? (
                      <div className="tag-cluster">
                        {selectedPack.provenance.map((source) => (
                          <span key={source} className="tag tag--subtle">
                            {source}
                          </span>
                        ))}
                      </div>
                    ) : null}

                    <div className="button-row">
                      <button
                        type="button"
                        className="primary-button"
                        disabled={busyKey === 'save-pack'}
                        onClick={handleSavePack}
                      >
                        {busyKey === 'save-pack' ? 'Saving…' : 'Save pack'}
                      </button>
                      <button
                        type="button"
                        className="ghost-button"
                        onClick={() => setEditorDraft(selectedPack ? draftFromPack(selectedPack) : emptyDraft(selectedScopeId))}
                      >
                        Reset
                      </button>
                    </div>
                </div>
              </div>
            </Card>

            <Card title="Composed preview" subtitle="The stack sent to local agents after draft filtering.">
              {!preview || preview.sections.length === 0 ? (
                <EmptyState
                  title="Nothing to compose yet"
                  body="Approve or create packs in the selected scope to build a preview."
                />
              ) : (
                <div className="preview-panel">
                  <div className="preview-summary">
                    <div>
                      <strong>{preview.headline}</strong>
                      <p>{preview.totalTokens.toLocaleString()} tokens across {preview.sections.length} sections</p>
                    </div>
                    {preview.warnings.length > 0 ? (
                      <div className="warning-box" role="status">
                        {preview.warnings.map((warning) => (
                          <p key={warning}>{warning}</p>
                        ))}
                      </div>
                    ) : null}
                  </div>
                  <ul className="section-stack">
                    {preview.sections.map((section) => (
                      <li key={section.id} className="preview-section">
                        <div className="row-heading">
                          <strong>{section.packName}</strong>
                          <span>{section.tokens.toLocaleString()} tokens</span>
                        </div>
                        <div className="row-meta">
                          <span>{section.title}</span>
                          <span>{section.scopeLabel}</span>
                        </div>
                        <p>{section.body}</p>
                      </li>
                    ))}
                  </ul>
                </div>
              )}
            </Card>

            <Card title="Activity & runs" subtitle="Recent local executions and review operations.">
              {snapshot.activity.length === 0 ? (
                <EmptyState
                  title="No local runs yet"
                  body="Triggered runs, imports, and reviews will appear here with status and context coverage."
                />
              ) : (
                <ul className="timeline-list">
                  {snapshot.activity.map((run) => (
                    <li key={run.id} className="timeline-item">
                      <div className="row-heading">
                        <strong>{run.summary}</strong>
                        <StatusPill label={run.status} tone={statusTone(run.status)} />
                      </div>
                      <p>{run.actor}</p>
                      <div className="row-meta">
                        <span>{formatTimestamp(run.startedAt)}</span>
                        <span>{formatDuration(run.durationMs)}</span>
                        <span>{run.stepCount} steps</span>
                        <span>{run.contextPackIds.length} packs</span>
                      </div>
                    </li>
                  ))}
                </ul>
              )}
            </Card>
          </div>
        ) : null}

        {activeView === 'review' ? (
          <div className="content-grid content-grid--review">
            <Card title="FTS search" subtitle="Ranked local matches across packs, reviews, runs, revisions, and adapters.">
              <label className="search-box">
                <span className="sr-only">Search local context</span>
                <input
                  value={searchQuery}
                  onChange={(event) => setSearchQuery(event.target.value)}
                  placeholder="Search migration, restore, adapter, provenance…"
                />
              </label>
              {searchLoading ? <p className="helper-text">Searching local index…</p> : null}
              {searchQuery.trim() && searchResults.length === 0 && !searchLoading ? (
                <EmptyState
                  title="No local matches"
                  body="Try a pack title, tag, adapter name, or revision note."
                />
              ) : null}
              {searchResults.length > 0 ? (
                <ul className="results-list">
                  {searchResults.map((result) => (
                    <SearchResultRow key={result.id} result={result} />
                  ))}
                </ul>
              ) : null}
            </Card>

            <Card title="Review queue" subtitle="Approve, reject, or edit changes before they become active context.">
              {snapshot.reviewQueue.length === 0 ? (
                <EmptyState
                  title="Queue is clear"
                  body="New review requests will surface here with suggested edits and risk labels."
                />
              ) : (
                <div className="split-panel">
                  <ul className="list-panel" aria-label="Review queue items">
                    {snapshot.reviewQueue.map((item) => (
                      <li key={item.id}>
                        <button
                          type="button"
                          className={`list-button ${selectedReviewId === item.id ? 'list-button--selected' : ''}`}
                          onClick={() => setSelectedReviewId(item.id)}
                        >
                          <div className="row-heading">
                            <strong>{item.title}</strong>
                            <StatusPill label={item.risk} tone={statusTone(item.risk)} />
                          </div>
                          <p>{item.summary}</p>
                          <div className="row-meta">
                            <span>{item.scopeLabel}</span>
                            <span>{item.requestedBy}</span>
                            <span>{formatTimestamp(item.requestedAt)}</span>
                          </div>
                        </button>
                      </li>
                    ))}
                  </ul>

                  {selectedReview ? (
                    <div className="editor-panel">
                      <div className="detail-card">
                        <div className="row-heading">
                          <strong>{selectedReview.packName}</strong>
                          <StatusPill label={selectedReview.risk} tone={statusTone(selectedReview.risk)} />
                        </div>
                        <p>{selectedReview.diff}</p>
                        <div className="row-meta">
                          <span>{selectedReview.scopeLabel}</span>
                          <span>{selectedReview.requestedBy}</span>
                        </div>
                      </div>
                      <label className="form-grid__full">
                        <span>Editable review draft</span>
                        <textarea
                          value={reviewDraft}
                          onChange={(event) => setReviewDraft(event.target.value)}
                          rows={10}
                        />
                      </label>
                      <div className="button-row">
                        <button
                          type="button"
                          className="primary-button"
                          disabled={busyKey === 'review-approve'}
                          onClick={() => handleReviewAction('approve')}
                        >
                          {busyKey === 'review-approve' ? 'Approving…' : 'Approve'}
                        </button>
                        <button
                          type="button"
                          className="secondary-button"
                          disabled={busyKey === 'review-edit'}
                          onClick={() => handleReviewAction('edit')}
                        >
                          {busyKey === 'review-edit' ? 'Applying…' : 'Apply edited draft'}
                        </button>
                        <button
                          type="button"
                          className="ghost-button"
                          disabled={busyKey === 'review-reject'}
                          onClick={() => handleReviewAction('reject')}
                        >
                          {busyKey === 'review-reject' ? 'Rejecting…' : 'Reject'}
                        </button>
                      </div>
                    </div>
                  ) : null}
                </div>
              )}
            </Card>

            <Card title="Provenance & revision history" subtitle="Inspect recent snapshots and restore a known-good version.">
              {selectedPack ? (
                <>
                  <div className="tag-cluster">
                    {selectedPack.provenance.map((source) => (
                      <span key={source} className="tag">
                        {source}
                      </span>
                    ))}
                  </div>
                  {revisions.length === 0 ? (
                    <EmptyState
                      title="No revisions for this pack"
                      body="Every save, import, or approved edit will create a restorable snapshot here."
                    />
                  ) : (
                    <ul className="timeline-list">
                      {revisions.map((revision) => (
                        <li key={revision.id} className="timeline-item">
                          <div className="row-heading">
                            <strong>{revision.note}</strong>
                            {revision.restorable ? (
                              <button
                                type="button"
                                className="ghost-button"
                                disabled={busyKey === `restore-${revision.id}`}
                                onClick={() => handleRestore(revision)}
                              >
                                {busyKey === `restore-${revision.id}` ? 'Restoring…' : 'Restore'}
                              </button>
                            ) : null}
                          </div>
                          <p>{revision.changeSummary}</p>
                          <div className="row-meta">
                            <span>{revision.author}</span>
                            <span>{formatTimestamp(revision.createdAt)}</span>
                            <span>{revision.id}</span>
                          </div>
                        </li>
                      ))}
                    </ul>
                  )}
                </>
              ) : (
                <EmptyState
                  title="Select a pack first"
                  body="Revision history and provenance badges follow the pack selected in the overview."
                />
              )}
            </Card>
          </div>
        ) : null}

        {activeView === 'operations' ? (
          <div className="content-grid content-grid--operations">
            <Card title="Harness & daemon health" subtitle="Monitor local harness discovery and the single-writer daemon.">
              {snapshot.adapters.length === 0 ? (
                <EmptyState
                  title="No adapters configured"
                  body="Install a Codex or Claude Code adapter, then start the local daemon."
                />
              ) : (
                <ul className="timeline-list">
                  {snapshot.adapters.map((adapter) => (
                    <AdapterRow
                      key={adapter.id}
                      adapter={adapter}
                      disabled={busyKey === `adapter-${adapter.id}`}
                      onToggle={handleToggleAdapter}
                    />
                  ))}
                </ul>
              )}
            </Card>

            <Card title="Import / export" subtitle="Move local archives in and out without leaving the control plane.">
              <div className="form-grid">
                <label className="form-grid__full">
                  <span>Archive path</span>
                  <input value={ioPath} onChange={(event) => setIoPath(event.target.value)} />
                </label>
              </div>
              <div className="button-row">
                <button
                  type="button"
                  className="primary-button"
                  disabled={busyKey === 'export'}
                  onClick={() => handleArchive('export')}
                >
                  {busyKey === 'export' ? 'Exporting…' : 'Export snapshot'}
                </button>
                <button
                  type="button"
                  className="secondary-button"
                  disabled={busyKey === 'import'}
                  onClick={() => handleArchive('import')}
                >
                  {busyKey === 'import' ? 'Importing…' : 'Import snapshot'}
                </button>
              </div>
              <p className="helper-text">
                Exports include persisted context packs, entries, reviews, and runs from the local daemon-backed store. Desktop preferences stay local.
              </p>
            </Card>

            <Card title="Settings" subtitle="Local-only runtime paths and preview guardrails.">
              <div className="form-grid">
                <label>
                  <span>Preview warning threshold</span>
                  <input
                    type="number"
                    min={256}
                    step={32}
                    value={settingsDraft.maxPreviewTokens}
                    onChange={(event) =>
                      setSettingsDraft((current) =>
                        current
                          ? { ...current, maxPreviewTokens: Number(event.target.value) || 0 }
                          : current,
                      )
                    }
                  />
                </label>
                <label>
                  <span>Socket path</span>
                  <input
                    value={settingsDraft.socketPath}
                    onChange={(event) =>
                      setSettingsDraft((current) =>
                        current ? { ...current, socketPath: event.target.value } : current,
                      )
                    }
                  />
                </label>
              </div>
              <p className="helper-text">
                Governance is fixed to the hybrid policy: safe project/task writes auto-apply;
                global, conflicting, and locked writes require review. No telemetry or cloud sync is enabled.
              </p>
              <div className="button-row">
                <button
                  type="button"
                  className="primary-button"
                  disabled={busyKey === 'save-settings'}
                  onClick={handleSaveSettings}
                >
                  {busyKey === 'save-settings' ? 'Saving…' : 'Save settings'}
                </button>
              </div>
            </Card>
          </div>
        ) : null}
      </section>
    </main>
  )
}

export function MockedApp() {
  return <App api={createDesktopApi({ forceMock: true })} />
}

export default App
