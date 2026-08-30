import { useEffect, useMemo, useState } from 'react'
import type { DesktopApi } from '../api/desktopApi'
import { friendlyDesktopError, normalizeDesktopError } from '../api/desktopApi'
import type {
  BundleImportPreview,
  DashboardSnapshot,
  DiagnosticAction,
  DiagnosticsReport,
  ForgetScopeResult,
  ReviewMode,
  WorkspaceNode,
} from '../types'
import {
  EmptyState,
  SectionHeader,
  StatusPill,
} from './Common'
import {
  type ConfirmationRequest,
  formatBytes,
  formatTimestamp,
  scopeLayerLabel,
} from '../lib/ui'

function flattenWorkspace(nodes: WorkspaceNode[]): WorkspaceNode[] {
  return nodes.flatMap((node) => [node, ...flattenWorkspace(node.children)])
}

function apiCompatibilityLabel(report: DiagnosticsReport) {
  if (report.apiVersion === null) return 'legacy · degraded'
  if (report.apiVersion !== report.expectedApiVersion) return 'incompatible'
  return 'compatible'
}

const policyCopy: Record<ReviewMode, { title: string; body: string }> = {
  strict: {
    title: 'Strict',
    body: 'Every non-duplicate agent proposal waits for review. Global and locked changes still wait; recognized secrets are rejected.',
  },
  balanced: {
    title: 'Balanced',
    body: 'New project and task entries can apply directly. Conflicts, global changes, and locked changes wait for review; recognized secrets are rejected.',
  },
  fast: {
    title: 'Fast',
    body: 'Project and task conflicts can apply directly. Global and locked changes still wait for review; recognized secrets are rejected.',
  },
}

const supportedRepairKinds = new Set([
  'refresh',
  'start_daemon',
  'restart_daemon',
  'retry_spool',
])

export function ConnectionsView({
  api,
  snapshot,
  focusedConnectionId,
  focusedRunId,
  onConfirm,
  onAnnounce,
  onError,
  onDataChanged,
  onOpenHistory,
  onResetOnboarding,
}: {
  api: DesktopApi
  snapshot: DashboardSnapshot
  focusedConnectionId?: string
  focusedRunId?: string
  onConfirm: (request: ConfirmationRequest) => void
  onAnnounce: (message: string) => void
  onError: (message: string) => void
  onDataChanged: () => Promise<void>
  onOpenHistory: () => void
  onResetOnboarding: () => void
}) {
  const [tab, setTab] = useState<'connections' | 'privacy'>('connections')
  const [diagnostics, setDiagnostics] = useState(snapshot.diagnostics)
  const [privacy, setPrivacy] = useState(snapshot.privacy)
  const [busyKey, setBusyKey] = useState('')
  const [policyMode, setPolicyMode] = useState<ReviewMode>(
    snapshot.reviewPolicy?.mode ?? snapshot.settings.reviewMode,
  )
  const [bundlePreview, setBundlePreview] = useState<BundleImportPreview | null>(null)
  const [scopeActionId, setScopeActionId] = useState(snapshot.selectedScopeId)
  const [scopeResult, setScopeResult] = useState<ForgetScopeResult | null>(null)
  const [historyAvailable, setHistoryAvailable] = useState(false)
  const scopes = useMemo(() => flattenWorkspace(snapshot.workspace), [snapshot.workspace])
  const diagnosticActionIds = useMemo(
    () =>
      new Set(
        diagnostics.checks.flatMap((check) =>
          check.remediation.map((action) => action.id),
        ),
      ),
    [diagnostics.checks],
  )

  useEffect(() => {
    setDiagnostics(snapshot.diagnostics)
    setPrivacy(snapshot.privacy)
    setPolicyMode(snapshot.reviewPolicy?.mode ?? snapshot.settings.reviewMode)
  }, [snapshot])

  useEffect(() => {
    let cancelled = false
    void api
      .loadDiagnostics()
      .then((report) => {
        if (!cancelled) setDiagnostics(report)
      })
      .catch((error) => {
        if (!cancelled) onError(friendlyDesktopError(error))
      })
    return () => {
      cancelled = true
    }
  }, [api, onError])

  useEffect(() => {
    if (focusedConnectionId) {
      if (focusedConnectionId === 'privacy-data') {
        setTab('privacy')
        window.setTimeout(() => {
          document.getElementById('privacy-heading')?.focus()
        }, 0)
        return
      }
      setTab('connections')
      window.setTimeout(() => {
        document.getElementById(`connection-${focusedConnectionId}`)?.focus()
      }, 0)
    }
  }, [focusedConnectionId])

  useEffect(() => {
    if (focusedRunId) {
      setTab('connections')
      window.setTimeout(() => {
        document.getElementById(`run-${focusedRunId}`)?.focus()
      }, 0)
    }
  }, [focusedRunId])

  async function refreshDiagnostics() {
    try {
      setBusyKey('refresh')
      const report = await api.refreshDiagnostics()
      setDiagnostics(report)
      await onDataChanged()
      onAnnounce('Diagnostics refreshed.')
    } catch (error) {
      onError(friendlyDesktopError(error))
    } finally {
      setBusyKey('')
    }
  }

  async function refreshAfterMutation(
    successMessage: string,
    options: { refreshPrivacy?: boolean } = {},
  ) {
    onAnnounce(successMessage)
    try {
      await onDataChanged()
      if (options.refreshPrivacy) {
        setPrivacy(await api.loadPrivacySummary())
      }
    } catch {
      onError(
        `${successMessage} The displayed local state could not refresh and may be stale. Use Refresh diagnostics or Refresh counts; do not repeat the mutation.`,
      )
    }
  }

  async function runRepair(action: DiagnosticAction) {
    try {
      setBusyKey(action.id)
      let outcome = action.label
      if (action.kind === 'start_daemon' || action.kind === 'restart_daemon') {
        const result =
          action.kind === 'start_daemon'
            ? await api.startDaemon()
            : await api.restartDaemon()
        setDiagnostics(result.diagnostics)
        outcome = `${result.message} Performed: ${result.performed ? 'yes' : 'no'}.`
      } else if (action.kind === 'retry_spool') {
        const result = await api.retrySpool()
        setDiagnostics(result.diagnostics)
        outcome = `Spool retry attempted ${result.attempted}; delivered ${result.delivered}; retained ${result.retained}; errors ${result.errors.length}.`
      }
      onAnnounce(outcome)
      try {
        const report = await api.refreshDiagnostics()
        setDiagnostics(report)
        await onDataChanged()
        onAnnounce(`${outcome} Diagnostics refreshed.`)
      } catch {
        onError(
          `${outcome} The repair completed, but diagnostics could not refresh and may be stale. Do not repeat the repair solely to refresh this view.`,
        )
      }
    } catch (error) {
      onError(friendlyDesktopError(error))
    } finally {
      setBusyKey('')
    }
  }

  async function refreshPrivacy() {
    try {
      setBusyKey('privacy')
      const result = await api.loadPrivacySummary()
      setPrivacy(result)
      onAnnounce('Privacy and local data counts refreshed.')
    } catch (error) {
      onError(friendlyDesktopError(error))
    } finally {
      setBusyKey('')
    }
  }

  async function chooseBundle() {
    try {
      setBusyKey('bundle-choose')
      const selection = await api.selectBundleImportFile()
      if (!selection) {
        onAnnounce('Backup selection cancelled.')
        return
      }
      const path = selection.paths[0]
      if (!path || selection.purpose !== 'bundle_import_preview') {
        onError('The native dialog returned an invalid bundle authorization. Choose again.')
        return
      }
      const preview = await api.previewBundleImport(path, selection.grantToken)
      setBundlePreview(preview)
      onAnnounce(
        preview.valid
          ? 'Backup preview is ready. Nothing has been imported.'
          : 'Backup preview is blocked. Nothing has been imported.',
      )
    } catch (error) {
      setBundlePreview(null)
      onError(friendlyDesktopError(error))
    } finally {
      setBusyKey('')
    }
  }

  function requestBundleImport() {
    if (!bundlePreview) return
    if (!bundlePreview.valid || !bundlePreview.applyGrantToken.trim()) {
      onError(
        'This bundle preview is invalid or missing its one-time apply authorization. Nothing was imported; choose another backup and preview it.',
      )
      return
    }
    onConfirm({
      title: 'Import this local backup?',
      description:
        'The validated UCM bundle can add or update local records. Existing keys may enter review according to the current policy.',
      confirmLabel: 'Import backup',
      detail: (
        <dl className="confirmation-facts">
          <div>
            <dt>File</dt>
            <dd className="path-value">{bundlePreview.path}</dd>
          </div>
          <div>
            <dt>Entries</dt>
            <dd>{bundlePreview.entryCount}</dd>
          </div>
          <div>
            <dt>Checksum</dt>
            <dd className="path-value">{bundlePreview.checksumSha256}</dd>
          </div>
        </dl>
      ),
      action: async () => {
        try {
          const summary = await api.applyBundleImport({
            path: bundlePreview.path,
            grantToken: bundlePreview.applyGrantToken,
            checksumSha256: bundlePreview.checksumSha256,
            confirmation: true,
          })
          setBundlePreview(null)
          setHistoryAvailable(true)
          await refreshAfterMutation(
            `Backup import completed with ${summary.packsImported} newly created pack${
              summary.packsImported === 1 ? '' : 's'
            }. Open history to inspect resulting revisions.`,
            { refreshPrivacy: true },
          )
        } catch (error) {
          const code = normalizeDesktopError(error).code
          if (
            code === 'conflict' ||
            code === 'path_grant_required' ||
            code === 'path_grant_invalid' ||
            code === 'path_grant_expired'
          ) {
            setBundlePreview(null)
            onError(
              'The bundle or its one-time path authorization is no longer valid. Nothing was imported; choose and preview the backup again.',
            )
            return
          }
          onError(friendlyDesktopError(error))
          throw error
        }
      },
    })
  }

  async function exportBackup() {
    try {
      setBusyKey('export')
      const selection = await api.selectExportDestination()
      if (!selection) {
        onAnnounce('Backup export cancelled.')
        return
      }
      const path = selection.paths[0]
      if (!path || selection.purpose !== 'export_archive') {
        onError('The native dialog returned an invalid export authorization. Choose again.')
        return
      }
      const summary = await api.exportArchive(path, selection.grantToken)
      onAnnounce(`Backup exported to ${summary.path}.`)
    } catch (error) {
      onError(friendlyDesktopError(error))
    } finally {
      setBusyKey('')
    }
  }

  function requestScopeAction(kind: 'archive' | 'forget') {
    const scope = scopes.find((candidate) => candidate.id === scopeActionId)
    if (!scope) return
    const description =
      kind === 'forget'
        ? 'The current backend archives matching packs and reports this operation as reversible. It does not erase revision history or claim secure deletion.'
        : 'Matching packs in this scope are archived. Project actions also include derived task scopes, and the backend reports whether the result is reversible.'
    onConfirm({
      title: `${kind === 'forget' ? 'Forget' : 'Archive'} ${scopeLayerLabel(scope.kind)} context?`,
      description,
      confirmLabel: kind === 'forget' ? 'Run forget workflow' : 'Archive scope',
      tone: 'danger',
      detail: (
        <div className="scope-confirmation">
          <strong>{scope.label}</strong>
          <span title={scope.id}>{scopeLayerLabel(scope.kind)}</span>
        </div>
      ),
      action: async () => {
        try {
          const input = {
            scopeId: scope.id,
            confirmation: true,
            actor: 'desktop-operator',
          }
          const result =
            kind === 'forget' ? await api.forgetScope(input) : await api.archiveScope(input)
          setScopeResult(result)
          setHistoryAvailable(true)
          await refreshAfterMutation(
            `${result.packsArchived} packs archived. The backend reports this operation as ${
              result.reversible ? 'reversible' : 'not reversible'
            }. ${result.packsAlreadyArchived} were already archived. Open history for the audit trail.`,
            { refreshPrivacy: true },
          )
        } catch (error) {
          onError(friendlyDesktopError(error))
          throw error
        }
      },
    })
  }

  async function savePolicy() {
    try {
      setBusyKey('policy')
      const policy = await api.setReviewPolicy({
        mode: policyMode,
        actor: 'desktop-operator',
        note: 'Selected in Connections.',
      })
      await refreshAfterMutation(`Review policy saved as ${policy.mode}.`)
    } catch (error) {
      onError(friendlyDesktopError(error))
    } finally {
      setBusyKey('')
    }
  }

  return (
    <div className="view-stack connections-view">
      <SectionHeader
        title="Connections"
        actions={
          <div className="segmented-control" role="tablist" aria-label="Connections sections">
            <button
              type="button"
              role="tab"
              aria-selected={tab === 'connections'}
              className={tab === 'connections' ? 'is-selected' : ''}
              onClick={() => setTab('connections')}
            >
              Connections
            </button>
            <button
              type="button"
              role="tab"
              aria-selected={tab === 'privacy'}
              className={tab === 'privacy' ? 'is-selected' : ''}
              onClick={() => {
                setTab('privacy')
                void refreshPrivacy()
              }}
            >
              Privacy &amp; Data
            </button>
          </div>
        }
      />

      {tab === 'connections' ? (
        <div className="settings-grid">
          <section className="settings-card settings-card--wide" aria-labelledby="diagnostics-heading">
            <header className="settings-card__header">
              <div>
                <h3 id="diagnostics-heading">Diagnostics</h3>
                <p>Last checked/refreshed {formatTimestamp(diagnostics.generatedAt)}</p>
              </div>
              <div className="button-row">
                <StatusPill label={diagnostics.overallState} />
                <button
                  type="button"
                  className="secondary-button"
                  disabled={busyKey === 'refresh'}
                  onClick={refreshDiagnostics}
                >
                  {busyKey === 'refresh' ? 'Refreshing…' : 'Refresh diagnostics'}
                </button>
              </div>
            </header>
            <dl className="diagnostic-summary">
              <div>
                <dt>Component version</dt>
                <dd>{diagnostics.componentVersion ?? 'unknown'}</dd>
              </div>
              <div>
                <dt>Context API</dt>
                <dd>
                  <span>
                    {diagnostics.apiVersion ?? 'legacy / unknown'} / expected{' '}
                    {diagnostics.expectedApiVersion}
                  </span>
                  <StatusPill label={apiCompatibilityLabel(diagnostics)} />
                </dd>
              </div>
              <div>
                <dt>Daemon reachable</dt>
                <dd>{diagnostics.daemonReachable ? 'yes' : 'no'}</dd>
              </div>
              <div>
                <dt>Schema</dt>
                <dd>
                  {diagnostics.schemaVersion ?? 'unknown'} / expected{' '}
                  {diagnostics.expectedSchemaVersion}
                </dd>
              </div>
              <div>
                <dt>Spool backlog</dt>
                <dd>{diagnostics.spoolBacklog}</dd>
              </div>
            </dl>
            <ul className="diagnostic-list">
              {diagnostics.checks.map((check) => (
                <li key={check.id}>
                  <header>
                    <div>
                      <strong>{check.label}</strong>
                      <small>{check.component}</small>
                    </div>
                    <StatusPill label={check.state} />
                  </header>
                  <p>{check.summary}</p>
                  {check.details.length > 0 ? (
                    <ul className="detail-list">
                      {check.details.map((detail) => (
                        <li key={detail}>{detail}</li>
                      ))}
                    </ul>
                  ) : null}
                  <dl className="inline-properties">
                    {check.path ? (
                      <div>
                        <dt>Path</dt>
                        <dd className="path-value">{check.path}</dd>
                      </div>
                    ) : null}
                    {check.detectedVersion ? (
                      <div>
                        <dt>Detected</dt>
                        <dd>{check.detectedVersion}</dd>
                      </div>
                    ) : null}
                    {check.expectedVersion ? (
                      <div>
                        <dt>Expected</dt>
                        <dd>{check.expectedVersion}</dd>
                      </div>
                    ) : null}
                  </dl>
                  {check.remediation.length > 0 ? (
                    <div className="remediation-row">
                      {check.remediation.map((action) =>
                        supportedRepairKinds.has(action.kind) ? (
                          <button
                            type="button"
                            className="secondary-button"
                            key={action.id}
                            disabled={busyKey === action.id}
                            onClick={() => void runRepair(action)}
                          >
                            {busyKey === action.id ? 'Working…' : action.label}
                          </button>
                        ) : (
                          <span className="manual-remediation" key={action.id}>
                            Manual: {action.label}
                          </span>
                        ),
                      )}
                    </div>
                  ) : null}
                </li>
              ))}
            </ul>
          </section>

          <section className="settings-card" aria-labelledby="adapters-heading">
            <header className="settings-card__header">
              <div>
                <h3 id="adapters-heading">Adapters</h3>
              </div>
            </header>
            <ul className="adapter-list">
              {snapshot.adapters.map((adapter) => (
                <li
                  id={`connection-${adapter.id}`}
                  key={adapter.id}
                  tabIndex={-1}
                  className={focusedConnectionId === adapter.id ? 'is-focused' : ''}
                >
                  <header>
                    <strong>{adapter.name}</strong>
                    <StatusPill label={adapter.state} />
                  </header>
                  <p>{adapter.note}</p>
                  <dl className="inline-properties">
                    <div>
                      <dt>Path / marker</dt>
                      <dd className="path-value">{adapter.path}</dd>
                    </div>
                    {adapter.detectedVersion ? (
                      <div>
                        <dt>Version</dt>
                        <dd>{adapter.detectedVersion}</dd>
                      </div>
                    ) : null}
                    <div>
                      <dt>Queue</dt>
                      <dd>{adapter.queueDepth}</dd>
                    </div>
                    <div>
                      <dt>Last checked</dt>
                      <dd>{formatTimestamp(adapter.lastCheckedAt)}</dd>
                    </div>
                  </dl>
                  <label className="compact-check">
                    <input
                      type="checkbox"
                      checked={adapter.enabled}
                      onChange={async (event) => {
                        try {
                          await api.toggleAdapter(adapter.id, event.target.checked)
                          await refreshAfterMutation(
                            `${adapter.name} is ${
                              event.target.checked ? 'monitored' : 'ignored'
                            } locally.`,
                          )
                        } catch (error) {
                          onError(friendlyDesktopError(error))
                        }
                      }}
                    />
                    <span>{adapter.enabled ? 'Monitor locally' : 'Ignored locally'}</span>
                  </label>
                  {adapter.remediation.length > 0 ? (
                    <div className="remediation-row">
                      {adapter.remediation.map((action) =>
                        diagnosticActionIds.has(action.id) ? (
                          <span className="manual-remediation" key={action.id}>
                            See Diagnostics: {action.label}
                          </span>
                        ) : supportedRepairKinds.has(action.kind) ? (
                          <button
                            type="button"
                            className="secondary-button"
                            key={action.id}
                            disabled={busyKey === action.id}
                            onClick={() => void runRepair(action)}
                          >
                            {busyKey === action.id ? 'Working…' : action.label}
                          </button>
                        ) : (
                          <span className="manual-remediation" key={action.id}>
                            Manual: {action.label}
                          </span>
                        ),
                      )}
                    </div>
                  ) : null}
                </li>
              ))}
            </ul>
          </section>

          <section
            id="connection-review-policy"
            className={`settings-card ${
              focusedConnectionId === 'review-policy' ? 'is-focused' : ''
            }`}
            aria-labelledby="policy-heading"
            tabIndex={-1}
          >
            <header className="settings-card__header">
              <div>
                <h3 id="policy-heading">Review policy</h3>
              </div>
              <StatusPill label={snapshot.reviewPolicy?.mode ?? snapshot.settings.reviewMode} />
            </header>
            <div className="policy-options">
              {(Object.keys(policyCopy) as ReviewMode[]).map((mode) => (
                <label key={mode} className={policyMode === mode ? 'is-selected' : ''}>
                  <input
                    type="radio"
                    name="review-policy"
                    value={mode}
                    checked={policyMode === mode}
                    onChange={() => setPolicyMode(mode)}
                  />
                  <span>
                    <strong>{policyCopy[mode].title}</strong>
                    <small>{policyCopy[mode].body}</small>
                  </span>
                </label>
              ))}
            </div>
            <footer className="card-actions">
              <small>
                Policy revision {snapshot.reviewPolicy?.revision ?? 0} · updated by{' '}
                {snapshot.reviewPolicy?.updatedBy ?? 'unknown'}
              </small>
              <button
                type="button"
                className="primary-button"
                disabled={
                  busyKey === 'policy' ||
                  policyMode ===
                    (snapshot.reviewPolicy?.mode ?? snapshot.settings.reviewMode)
                }
                onClick={savePolicy}
              >
                {busyKey === 'policy' ? 'Saving…' : 'Save policy'}
              </button>
            </footer>
          </section>

          <section className="settings-card settings-card--wide" aria-labelledby="runs-heading">
            <header className="settings-card__header">
              <div>
                <h3 id="runs-heading">Recent runs</h3>
              </div>
              <span>{snapshot.activity.length}</span>
            </header>
            {snapshot.activity.length === 0 ? (
              <EmptyState title="No recorded runs" body="Local run references will appear here." />
            ) : (
              <ul className="run-list">
                {snapshot.activity.map((run) => (
                  <li
                    id={`run-${run.id}`}
                    key={run.id}
                    tabIndex={-1}
                    className={focusedRunId === run.id ? 'is-focused' : ''}
                  >
                    <div>
                      <strong>{run.summary}</strong>
                      <small>
                        {run.actor} · {formatTimestamp(run.startedAt)} · {run.stepCount} steps
                      </small>
                    </div>
                    <StatusPill label={run.status} />
                  </li>
                ))}
              </ul>
            )}
          </section>
        </div>
      ) : (
        <div className="settings-grid privacy-grid">
          <section className="settings-card settings-card--wide" aria-labelledby="privacy-heading">
            <header className="settings-card__header">
              <div>
                <h3 id="privacy-heading" tabIndex={-1}>Privacy boundary</h3>
              </div>
              <button
                type="button"
                className="secondary-button"
                disabled={busyKey === 'privacy'}
                onClick={refreshPrivacy}
              >
                {busyKey === 'privacy' ? 'Refreshing…' : 'Refresh counts'}
              </button>
            </header>
            <div className="privacy-statements">
              <article>
                <span aria-hidden="true">⌂</span>
                <div>
                  <h4>Local durable storage</h4>
                  <p>{privacy.localOnlyStatement}</p>
                </div>
              </article>
              <article>
                <span aria-hidden="true">→</span>
                <div>
                  <h4>Downstream disclosure</h4>
                  <p>{privacy.downstreamAdapterDisclosure}</p>
                </div>
              </article>
              <article>
                <span aria-hidden="true">◇</span>
                <div>
                  <h4>Secret scanning</h4>
                  <p>{privacy.secretScanningStatement}</p>
                </div>
              </article>
              <article>
                <span aria-hidden="true">○</span>
                <div>
                  <h4>Encryption boundary</h4>
                  <p>{privacy.applicationEncryptionBoundary}</p>
                </div>
              </article>
            </div>
            <dl className="privacy-flags">
              <div>
                <dt>App telemetry</dt>
                <dd>
                  <StatusPill label={privacy.telemetryEnabled ? 'enabled' : 'disabled'} />
                </dd>
              </div>
              <div>
                <dt>App network egress</dt>
                <dd>
                  <StatusPill label={privacy.networkEgressEnabled ? 'enabled' : 'disabled'} />
                </dd>
              </div>
            </dl>
          </section>

          <section className="settings-card" aria-labelledby="paths-heading">
            <header className="settings-card__header">
              <div>
                <h3 id="paths-heading">Local paths</h3>
              </div>
            </header>
            <dl className="path-list">
              <div>
                <dt>Data</dt>
                <dd>{privacy.dataPath}</dd>
              </div>
              <div>
                <dt>Database</dt>
                <dd>{privacy.databasePath}</dd>
              </div>
              <div>
                <dt>Socket</dt>
                <dd>{privacy.socketPath}</dd>
              </div>
              <div>
                <dt>Spool</dt>
                <dd>{privacy.spoolPath}</dd>
              </div>
              <div>
                <dt>Settings</dt>
                <dd>{privacy.settingsPath}</dd>
              </div>
            </dl>
          </section>

          <section className="settings-card" aria-labelledby="counts-heading">
            <header className="settings-card__header">
              <div>
                <h3 id="counts-heading">Local counts</h3>
                <p>
                  {privacy.countsAvailable
                    ? `Available from ${privacy.countsSource ?? 'backend'}`
                    : 'Record counts are currently unavailable'}
                </p>
              </div>
              <StatusPill
                label={privacy.countsAvailable ? 'counts available' : 'counts unavailable'}
              />
            </header>
            <dl className="count-ledger">
              <div>
                <dt>Packs</dt>
                <dd>{privacy.countsAvailable ? privacy.counts.packs : 'Unavailable'}</dd>
              </div>
              <div>
                <dt>Entries</dt>
                <dd>{privacy.countsAvailable ? privacy.counts.entries : 'Unavailable'}</dd>
              </div>
              <div>
                <dt>Reviews</dt>
                <dd>{privacy.countsAvailable ? privacy.counts.reviews : 'Unavailable'}</dd>
              </div>
              <div>
                <dt>Runs</dt>
                <dd>{privacy.countsAvailable ? privacy.counts.runs : 'Unavailable'}</dd>
              </div>
              <div>
                <dt>Spool backlog</dt>
                <dd>{privacy.counts.spoolBacklog}</dd>
              </div>
            </dl>
          </section>

          <section className="settings-card settings-card--wide" aria-labelledby="backup-heading">
            <header className="settings-card__header">
              <div>
                <h3 id="backup-heading">Backup &amp; import preview</h3>
                <p>Paths are selected through macOS dialogs, not editable text fields.</p>
              </div>
              <div className="button-row">
                <button
                  type="button"
                  className="secondary-button"
                  disabled={busyKey === 'export'}
                  onClick={exportBackup}
                >
                  {busyKey === 'export' ? 'Exporting…' : 'Export backup…'}
                </button>
                <button
                  type="button"
                  className="primary-button"
                  disabled={busyKey === 'bundle-choose'}
                  onClick={chooseBundle}
                >
                  {busyKey === 'bundle-choose' ? 'Opening…' : 'Choose backup…'}
                </button>
              </div>
            </header>
            {bundlePreview ? (
              <div className="bundle-preview">
                <div className="bundle-preview__path">
                  <StatusPill label={bundlePreview.valid ? 'valid preview' : 'invalid'} />
                  <code>{bundlePreview.path}</code>
                </div>
                <dl>
                  <div>
                    <dt>Format</dt>
                    <dd>{bundlePreview.format}</dd>
                  </div>
                  <div>
                    <dt>Size</dt>
                    <dd>{formatBytes(bundlePreview.fileSizeBytes)}</dd>
                  </div>
                  <div>
                    <dt>Packs</dt>
                    <dd>{bundlePreview.packCount}</dd>
                  </div>
                  <div>
                    <dt>Entries</dt>
                    <dd>{bundlePreview.entryCount}</dd>
                  </div>
                  <div>
                    <dt>Reviews</dt>
                    <dd>{bundlePreview.reviewCount}</dd>
                  </div>
                  <div>
                    <dt>Runs</dt>
                    <dd>{bundlePreview.runCount}</dd>
                  </div>
                </dl>
                <dl className="bundle-metadata">
                  <div>
                    <dt>Exported</dt>
                    <dd>{formatTimestamp(bundlePreview.exportedAt)}</dd>
                  </div>
                  <div>
                    <dt>Checksum</dt>
                    <dd className="path-value">{bundlePreview.checksumSha256}</dd>
                  </div>
                  <div>
                    <dt>Scopes</dt>
                    <dd className="path-value">
                      {bundlePreview.scopeIds.length > 0
                        ? bundlePreview.scopeIds.join(', ')
                        : 'None reported'}
                    </dd>
                  </div>
                </dl>
                {bundlePreview.warnings.length > 0 ? (
                  <ul className="detail-list">
                    {bundlePreview.warnings.map((warning) => (
                      <li key={warning}>{warning}</li>
                    ))}
                  </ul>
                ) : null}
                <footer className="card-actions">
                  <small>Preview only. No records have been imported.</small>
                  <button
                    type="button"
                    className="primary-button"
                    disabled={!bundlePreview.valid || !bundlePreview.applyGrantToken.trim()}
                    onClick={requestBundleImport}
                  >
                    {bundlePreview.valid && bundlePreview.applyGrantToken.trim()
                      ? 'Import this backup…'
                      : 'Import blocked'}
                  </button>
                </footer>
              </div>
            ) : (
              <p className="subtle-copy">
                Choose a UCM JSON or Markdown bundle to inspect counts, scopes, checksum, and
                warnings before confirmation.
              </p>
            )}
          </section>

          <section className="settings-card settings-card--wide" aria-labelledby="scope-data-heading">
            <header className="settings-card__header">
              <div>
                <h3 id="scope-data-heading">Archive or forget a scope</h3>
                <p>
                  The current backend archives matching packs and reports reversibility. It does
                  not claim secure erasure.
                </p>
              </div>
            </header>
            <div className="scope-data-actions">
              <label>
                <span>Scope</span>
                <select
                  value={scopeActionId}
                  onChange={(event) => setScopeActionId(event.target.value)}
                >
                  {scopes.map((scope) => (
                    <option key={scope.id} value={scope.id}>
                      {scopeLayerLabel(scope.kind)}
                      {scope.kind === 'task' ? ' (derived)' : ''} — {scope.label}
                    </option>
                  ))}
                </select>
              </label>
              <div className="button-row">
                <button
                  type="button"
                  className="secondary-button"
                  onClick={() => requestScopeAction('archive')}
                >
                  Archive scope…
                </button>
                <button
                  type="button"
                  className="danger-quiet-button"
                  onClick={() => requestScopeAction('forget')}
                >
                  Forget scope…
                </button>
              </div>
            </div>
            {scopeResult ? (
              <div className="scope-result">
                <StatusPill label={scopeResult.stopped ? 'partial failure' : 'completed'} />
                <span>
                  {scopeResult.packsArchived} archived · {scopeResult.entriesAffected} entries
                  affected · backend says {scopeResult.reversible ? 'reversible' : 'not reversible'}
                </span>
                {scopeResult.failures.length > 0 ? (
                  <ul>
                    {scopeResult.failures.map((failure) => (
                      <li key={failure.packId}>
                        {failure.packName}: {failure.error.code}
                      </li>
                    ))}
                  </ul>
                ) : null}
              </div>
            ) : null}
            {historyAvailable ? (
              <button type="button" className="text-button" onClick={onOpenHistory}>
                Open history
              </button>
            ) : null}
          </section>

          <section className="settings-card settings-card--wide" aria-labelledby="onboarding-reset-heading">
            <header className="settings-card__header">
              <div>
                <h3 id="onboarding-reset-heading">Onboarding</h3>
                <p>
                  Resetting shows the wizard again; it does not remove existing local context.
                </p>
              </div>
              <button type="button" className="secondary-button" onClick={onResetOnboarding}>
                Run onboarding again…
              </button>
            </header>
          </section>
        </div>
      )}
    </div>
  )
}
