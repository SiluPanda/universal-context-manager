use std::{
    collections::{BTreeMap, BTreeSet},
    env, fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant, SystemTime},
};

use context_core::{
    ContextPaths, HealthReport, StoreStats, CONTEXT_API_VERSION, LATEST_SCHEMA_VERSION,
};
use serde_json::{json, Value};

use crate::models::{
    AdapterHealth, AdapterKind, AdapterStatus, DiagnosticAction, DiagnosticActionKind,
    DiagnosticCheck, DiagnosticState, DiagnosticsReport,
};

pub(crate) const ADAPTER_DAEMON: &str = "adapter-daemon";
pub(crate) const ADAPTER_CODEX: &str = "adapter-codex";
pub(crate) const ADAPTER_CLAUDE: &str = "adapter-claude-code";

const EXPECTED_VERSION: &str = env!("CARGO_PKG_VERSION");
const SUPPORTED_MCP_PROTOCOL_VERSION: &str = "2025-03-26";
const MAX_PLUGIN_SCAN_ENTRIES: usize = 4_000;
const MAX_PLUGIN_SCAN_DEPTH: usize = 9;

pub(crate) fn collect_diagnostics(
    paths: &ContextPaths,
    settings_path: &Path,
    connected: bool,
    health: Option<&HealthReport>,
    stats: Option<&StoreStats>,
    adapter_enabled: &BTreeMap<String, bool>,
    checked_at: &str,
) -> DiagnosticsReport {
    let mut checks = vec![daemon_check(paths, connected, health, stats, checked_at)];
    checks.extend(permission_checks(paths, settings_path, checked_at));

    let contextd = binary_check("contextd", "CONTEXTD_BIN", checked_at);
    let context_mcp = binary_check("context-mcp", "CONTEXT_MCP_BIN", checked_at);
    let contextctl = binary_check("contextctl", "CONTEXTCTL_BIN", checked_at);
    let mcp_binary = context_mcp
        .path
        .as_deref()
        .map(PathBuf::from)
        .filter(|_| context_mcp.state == DiagnosticState::Healthy);
    checks.push(contextd);
    checks.push(context_mcp);
    checks.push(contextctl);

    let (spool_check, spool_backlog) = spool_check(paths, checked_at);
    checks.push(spool_check);
    checks.push(mcp_handshake_check(mcp_binary.as_deref(), checked_at));
    checks.push(adapter_check(
        AdapterHarness::Codex,
        *adapter_enabled.get(ADAPTER_CODEX).unwrap_or(&true),
        checked_at,
    ));
    checks.push(adapter_check(
        AdapterHarness::Claude,
        *adapter_enabled.get(ADAPTER_CLAUDE).unwrap_or(&true),
        checked_at,
    ));

    let overall_state = checks
        .iter()
        .filter(|check| check.state != DiagnosticState::Ignored)
        .max_by_key(|check| diagnostic_rank(&check.state))
        .map(|check| check.state.clone())
        .unwrap_or(DiagnosticState::Healthy);

    DiagnosticsReport {
        generated_at: checked_at.to_string(),
        overall_state,
        daemon_reachable: connected,
        component_version: health.and_then(|report| report.component_version.clone()),
        api_version: health.and_then(|report| report.api_version),
        expected_api_version: CONTEXT_API_VERSION,
        schema_version: health
            .map(|report| report.schema_version)
            .or_else(|| stats.map(|stats| stats.schema_version)),
        expected_schema_version: LATEST_SCHEMA_VERSION,
        spool_backlog,
        checks,
    }
}

pub(crate) fn adapters_from_diagnostics(
    report: &DiagnosticsReport,
    paths: &ContextPaths,
    stats: Option<&StoreStats>,
    adapter_enabled: &BTreeMap<String, bool>,
) -> Vec<AdapterStatus> {
    vec![
        adapter_status(
            report,
            AdapterViewSpec {
                id: ADAPTER_CODEX,
                name: "Codex harness",
                kind: AdapterKind::Terminal,
                check_id: "adapter-codex-install",
                enabled: *adapter_enabled.get(ADAPTER_CODEX).unwrap_or(&true),
                queue_depth: 0,
                fallback_path: user_home().join(".codex/plugins"),
            },
        ),
        adapter_status(
            report,
            AdapterViewSpec {
                id: ADAPTER_CLAUDE,
                name: "Claude Code harness",
                kind: AdapterKind::Terminal,
                check_id: "adapter-claude-install",
                enabled: *adapter_enabled.get(ADAPTER_CLAUDE).unwrap_or(&true),
                queue_depth: 0,
                fallback_path: user_home().join(".claude/plugins"),
            },
        ),
        adapter_status(
            report,
            AdapterViewSpec {
                id: ADAPTER_DAEMON,
                name: "Context daemon",
                kind: AdapterKind::Api,
                check_id: "daemon-health",
                enabled: *adapter_enabled.get(ADAPTER_DAEMON).unwrap_or(&true),
                queue_depth: stats.map(|value| value.reviews as u32).unwrap_or(0),
                fallback_path: paths.socket_path.clone(),
            },
        ),
    ]
}

struct AdapterViewSpec<'a> {
    id: &'a str,
    name: &'a str,
    kind: AdapterKind,
    check_id: &'a str,
    enabled: bool,
    queue_depth: u32,
    fallback_path: PathBuf,
}

fn adapter_status(report: &DiagnosticsReport, spec: AdapterViewSpec<'_>) -> AdapterStatus {
    let check = report.checks.iter().find(|check| check.id == spec.check_id);
    let state = if spec.enabled {
        check
            .map(|check| check.state.clone())
            .unwrap_or(DiagnosticState::Degraded)
    } else {
        DiagnosticState::Ignored
    };
    let health = match state {
        DiagnosticState::Healthy => AdapterHealth::Healthy,
        DiagnosticState::Ignored
        | DiagnosticState::NotInstalled
        | DiagnosticState::Stopped
        | DiagnosticState::Starting => AdapterHealth::Offline,
        DiagnosticState::Degraded
        | DiagnosticState::Incompatible
        | DiagnosticState::MigrationRequired
        | DiagnosticState::Failed => AdapterHealth::Degraded,
    };
    AdapterStatus {
        id: spec.id.to_string(),
        name: spec.name.to_string(),
        kind: spec.kind,
        enabled: spec.enabled,
        health,
        last_checked_at: report.generated_at.clone(),
        queue_depth: spec.queue_depth,
        path: check
            .and_then(|check| check.path.clone())
            .unwrap_or_else(|| spec.fallback_path.display().to_string()),
        note: check
            .map(|check| check.summary.clone())
            .unwrap_or_else(|| "Diagnostic check unavailable.".to_string()),
        state,
        detected_version: check.and_then(|check| check.detected_version.clone()),
        remediation: check
            .map(|check| check.remediation.clone())
            .unwrap_or_default(),
    }
}

fn daemon_check(
    paths: &ContextPaths,
    connected: bool,
    health: Option<&HealthReport>,
    stats: Option<&StoreStats>,
    checked_at: &str,
) -> DiagnosticCheck {
    let (state, summary, details, remediation) = if let Some(report) = health {
        let mut details = vec![
            match report.api_version {
                Some(version) => {
                    format!("Context API version {version}; desktop expects {CONTEXT_API_VERSION}.")
                }
                None => "Context API version was not reported by this legacy health response."
                    .to_string(),
            },
            match report.component_version.as_deref() {
                Some(version) => format!(
                    "Daemon component version {version}; desktop expects {EXPECTED_VERSION}."
                ),
                None => "Daemon component version was not reported by this legacy health response."
                    .to_string(),
            },
            format!(
                "Database schema {}; expected {}.",
                report.schema_version, LATEST_SCHEMA_VERSION
            ),
            format!(
                "{} packs, {} entries, {} pending reviews.",
                report.packs, report.entries, report.reviews
            ),
        ];
        if report
            .api_version
            .is_some_and(|version| version != CONTEXT_API_VERSION)
        {
            (
                DiagnosticState::Incompatible,
                "The daemon Context API version is incompatible with this desktop build."
                    .to_string(),
                details,
                vec![action(
                    "install-local-binaries",
                    "Install matching local components",
                    DiagnosticActionKind::Manual,
                )],
            )
        } else if report.schema_version > LATEST_SCHEMA_VERSION {
            (
                DiagnosticState::Incompatible,
                "The daemon schema is newer than this desktop build supports.".to_string(),
                details,
                vec![action(
                    "update-desktop",
                    "Update the desktop application",
                    DiagnosticActionKind::Manual,
                )],
            )
        } else if report
            .component_version
            .as_deref()
            .is_some_and(|version| version != EXPECTED_VERSION)
        {
            (
                DiagnosticState::Incompatible,
                "The daemon version does not match this desktop build.".to_string(),
                details,
                vec![action(
                    "install-local-binaries",
                    "Install matching local components",
                    DiagnosticActionKind::Manual,
                )],
            )
        } else if report.api_version.is_some()
            && report.component_version.is_some()
            && report.schema_version < LATEST_SCHEMA_VERSION
        {
            (
                DiagnosticState::MigrationRequired,
                "The daemon is reachable but its data schema requires migration.".to_string(),
                details,
                vec![action(
                    "migrate-daemon-manually",
                    "Stop the daemon and start matching contextd to run migrations",
                    DiagnosticActionKind::Manual,
                )],
            )
        } else if report.api_version.is_none() {
            details.push(
                "API compatibility cannot be confirmed until contextd reports its Context API version."
                    .to_string(),
            );
            (
                DiagnosticState::Degraded,
                "The daemon is reachable, but its Context API version is unknown.".to_string(),
                details,
                vec![
                    action(
                        "refresh-diagnostics",
                        "Recheck daemon health",
                        DiagnosticActionKind::Refresh,
                    ),
                    action(
                        "install-local-binaries",
                        "Install matching local components",
                        DiagnosticActionKind::Manual,
                    ),
                ],
            )
        } else if report.component_version.is_none() {
            details.push(
                "Version compatibility cannot be confirmed until contextd reports its component version."
                    .to_string(),
            );
            (
                DiagnosticState::Degraded,
                "The daemon is reachable, but its component version is unknown.".to_string(),
                details,
                vec![
                    action(
                        "refresh-diagnostics",
                        "Recheck daemon health",
                        DiagnosticActionKind::Refresh,
                    ),
                    action(
                        "install-local-binaries",
                        "Install matching local components",
                        DiagnosticActionKind::Manual,
                    ),
                ],
            )
        } else {
            (
                DiagnosticState::Healthy,
                "The local context daemon is version- and schema-compatible.".to_string(),
                details,
                vec![action(
                    "refresh-diagnostics",
                    "Recheck daemon health",
                    DiagnosticActionKind::Refresh,
                )],
            )
        }
    } else if connected {
        (
            DiagnosticState::Degraded,
            "The daemon connection was reported without a health payload.".to_string(),
            Vec::new(),
            vec![action(
                "refresh-diagnostics",
                "Refresh diagnostics",
                DiagnosticActionKind::Refresh,
            )],
        )
    } else if stats.is_some_and(|stats| stats.schema_version < LATEST_SCHEMA_VERSION) {
        (
            DiagnosticState::MigrationRequired,
            "The daemon is stopped and the local data schema requires migration.".to_string(),
            vec![format!(
                "Detected schema {}; expected {}.",
                stats.map(|stats| stats.schema_version).unwrap_or_default(),
                LATEST_SCHEMA_VERSION
            )],
            vec![action(
                "start-daemon",
                "Start daemon to run supported migrations",
                DiagnosticActionKind::StartDaemon,
            )],
        )
    } else if stats.is_some_and(|stats| stats.schema_version > LATEST_SCHEMA_VERSION) {
        (
            DiagnosticState::Incompatible,
            "The local data schema is newer than this desktop build supports.".to_string(),
            vec![format!(
                "Detected schema {}; expected {}.",
                stats.map(|stats| stats.schema_version).unwrap_or_default(),
                LATEST_SCHEMA_VERSION
            )],
            vec![action(
                "update-desktop",
                "Update the desktop application",
                DiagnosticActionKind::Manual,
            )],
        )
    } else if recent_socket(&paths.socket_path) {
        (
            DiagnosticState::Starting,
            "A daemon socket appeared recently, but the health check is not ready.".to_string(),
            Vec::new(),
            vec![action(
                "refresh-diagnostics",
                "Retry health check",
                DiagnosticActionKind::Refresh,
            )],
        )
    } else {
        (
            DiagnosticState::Stopped,
            "The local context daemon is not reachable.".to_string(),
            Vec::new(),
            vec![action(
                "start-daemon",
                "Start daemon",
                DiagnosticActionKind::StartDaemon,
            )],
        )
    };
    DiagnosticCheck {
        id: "daemon-health".to_string(),
        label: "Daemon health, version, and schema".to_string(),
        component: "daemon".to_string(),
        state,
        summary,
        details,
        path: Some(paths.socket_path.display().to_string()),
        detected_version: health.and_then(|report| report.component_version.clone()),
        expected_version: Some(EXPECTED_VERSION.to_string()),
        remediation,
        checked_at: checked_at.to_string(),
    }
}

fn recent_socket(path: &Path) -> bool {
    fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|modified| SystemTime::now().duration_since(modified).ok())
        .is_some_and(|age| age < Duration::from_secs(15))
}

fn permission_checks(
    paths: &ContextPaths,
    settings_path: &Path,
    checked_at: &str,
) -> Vec<DiagnosticCheck> {
    vec![
        permission_check(
            "data-permissions",
            "Data directory permissions",
            &paths.data_dir,
            PermissionTarget::Directory,
            checked_at,
        ),
        permission_check(
            "database-permissions",
            "Database file permissions",
            &paths.db_path,
            PermissionTarget::PrivateFile,
            checked_at,
        ),
        permission_check(
            "socket-permissions",
            "Socket path permissions",
            &paths.socket_path,
            PermissionTarget::Socket,
            checked_at,
        ),
        permission_check(
            "spool-permissions",
            "Spool directory permissions",
            &paths.spool_dir,
            PermissionTarget::Directory,
            checked_at,
        ),
        permission_check(
            "settings-permissions",
            "Desktop settings permissions",
            settings_path,
            PermissionTarget::OptionalPrivateFile,
            checked_at,
        ),
    ]
}

#[derive(Clone, Copy)]
enum PermissionTarget {
    Directory,
    PrivateFile,
    OptionalPrivateFile,
    Socket,
}

fn permission_check(
    id: &str,
    label: &str,
    path: &Path,
    target: PermissionTarget,
    checked_at: &str,
) -> DiagnosticCheck {
    let metadata = fs::symlink_metadata(path);
    let (state, summary, details) = match metadata {
        Ok(metadata) => {
            let correct_type = match target {
                PermissionTarget::Directory => metadata.is_dir(),
                PermissionTarget::PrivateFile | PermissionTarget::OptionalPrivateFile => {
                    metadata.is_file()
                }
                PermissionTarget::Socket => is_socket(&metadata),
            };
            if !correct_type {
                (
                    DiagnosticState::Failed,
                    "The path exists but has an unexpected filesystem type.".to_string(),
                    Vec::new(),
                )
            } else {
                match assess_path_security(path, &metadata, target) {
                    PathSecurityAssessment::Healthy => (
                        DiagnosticState::Healthy,
                        "The path is owned by the current user, accessible, and private."
                            .to_string(),
                        Vec::new(),
                    ),
                    PathSecurityAssessment::ExcessPermissions => (
                        DiagnosticState::Degraded,
                        "The path is accessible beyond the current user.".to_string(),
                        vec![
                            "Restrict group/other access before storing sensitive context."
                                .to_string(),
                        ],
                    ),
                    PathSecurityAssessment::WrongOwner => (
                        DiagnosticState::Failed,
                        "The path is not owned by the effective desktop user.".to_string(),
                        vec!["Move the data to a user-owned path or correct ownership.".to_string()],
                    ),
                    PathSecurityAssessment::Inaccessible => (
                        DiagnosticState::Failed,
                        "The effective desktop user does not have the required path access."
                            .to_string(),
                        Vec::new(),
                    ),
                    PathSecurityAssessment::Unknown => (
                        DiagnosticState::Degraded,
                        "Ownership and effective access could not be verified on this platform."
                            .to_string(),
                        Vec::new(),
                    ),
                }
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let state = match target {
                PermissionTarget::Directory => DiagnosticState::Failed,
                PermissionTarget::PrivateFile | PermissionTarget::Socket => {
                    DiagnosticState::Stopped
                }
                PermissionTarget::OptionalPrivateFile => DiagnosticState::Healthy,
            };
            let summary = if matches!(target, PermissionTarget::OptionalPrivateFile) {
                "The optional file has not been created; private defaults are active.".to_string()
            } else {
                "The path has not been created yet.".to_string()
            };
            (state, summary, Vec::new())
        }
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => (
            DiagnosticState::Failed,
            "The current user cannot inspect this path.".to_string(),
            Vec::new(),
        ),
        Err(_) => (
            DiagnosticState::Failed,
            "The path could not be inspected.".to_string(),
            Vec::new(),
        ),
    };
    DiagnosticCheck {
        id: id.to_string(),
        label: label.to_string(),
        component: "storage".to_string(),
        state,
        summary,
        details,
        path: Some(path.display().to_string()),
        detected_version: None,
        expected_version: None,
        remediation: vec![action(
            "open-storage-path",
            "Inspect path permissions",
            DiagnosticActionKind::OpenPath,
        )],
        checked_at: checked_at.to_string(),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PathSecurityAssessment {
    Healthy,
    ExcessPermissions,
    WrongOwner,
    Inaccessible,
    Unknown,
}

#[cfg(unix)]
fn assess_path_security(
    path: &Path,
    metadata: &fs::Metadata,
    target: PermissionTarget,
) -> PathSecurityAssessment {
    use std::{
        ffi::CString,
        os::unix::{
            ffi::OsStrExt,
            fs::{MetadataExt, PermissionsExt},
        },
    };
    let current_uid = unsafe { libc::geteuid() };
    let Ok(path) = CString::new(path.as_os_str().as_bytes()) else {
        return PathSecurityAssessment::Unknown;
    };
    let access_mode = match target {
        PermissionTarget::Directory => libc::R_OK | libc::W_OK | libc::X_OK,
        PermissionTarget::PrivateFile
        | PermissionTarget::OptionalPrivateFile
        | PermissionTarget::Socket => libc::R_OK | libc::W_OK,
    };
    let accessible = unsafe { libc::access(path.as_ptr(), access_mode) } == 0;
    let mode = metadata.permissions().mode() & 0o777;
    let excessive = match target {
        PermissionTarget::Directory => mode & 0o077 != 0,
        PermissionTarget::PrivateFile | PermissionTarget::OptionalPrivateFile => mode & 0o077 != 0,
        PermissionTarget::Socket => mode & 0o077 != 0,
    };
    classify_unix_security(metadata.uid(), current_uid, accessible, excessive)
}

#[cfg(unix)]
fn classify_unix_security(
    owner_uid: u32,
    current_uid: u32,
    accessible: bool,
    excessive: bool,
) -> PathSecurityAssessment {
    if owner_uid != current_uid {
        PathSecurityAssessment::WrongOwner
    } else if !accessible {
        PathSecurityAssessment::Inaccessible
    } else if excessive {
        PathSecurityAssessment::ExcessPermissions
    } else {
        PathSecurityAssessment::Healthy
    }
}

#[cfg(not(unix))]
fn assess_path_security(
    _path: &Path,
    _metadata: &fs::Metadata,
    _target: PermissionTarget,
) -> PathSecurityAssessment {
    PathSecurityAssessment::Unknown
}

#[cfg(unix)]
fn is_socket(metadata: &fs::Metadata) -> bool {
    use std::os::unix::fs::FileTypeExt;
    metadata.file_type().is_socket()
}

#[cfg(not(unix))]
fn is_socket(metadata: &fs::Metadata) -> bool {
    metadata.is_file()
}

fn binary_check(name: &str, env_key: &str, checked_at: &str) -> DiagnosticCheck {
    let Some(path) = discover_binary(name, env_key) else {
        return DiagnosticCheck {
            id: format!("binary-{name}"),
            label: format!("{name} binary"),
            component: "binary".to_string(),
            state: DiagnosticState::NotInstalled,
            summary: format!("{name} was not found in the application bundle or PATH."),
            details: Vec::new(),
            path: None,
            detected_version: None,
            expected_version: Some(EXPECTED_VERSION.to_string()),
            remediation: vec![action(
                "install-local-binaries",
                "Install or rebuild the matching local binaries",
                DiagnosticActionKind::Manual,
            )],
            checked_at: checked_at.to_string(),
        };
    };
    if !is_executable(&path) {
        return DiagnosticCheck {
            id: format!("binary-{name}"),
            label: format!("{name} binary"),
            component: "binary".to_string(),
            state: DiagnosticState::Failed,
            summary: format!("{name} exists but is not executable."),
            details: Vec::new(),
            path: Some(path.display().to_string()),
            detected_version: None,
            expected_version: Some(EXPECTED_VERSION.to_string()),
            remediation: vec![action(
                "repair-binary-permissions",
                "Repair binary permissions",
                DiagnosticActionKind::Manual,
            )],
            checked_at: checked_at.to_string(),
        };
    }
    let version = command_version(&path);
    let (state, summary) = match version.as_deref() {
        Some(version) if version == EXPECTED_VERSION => (
            DiagnosticState::Healthy,
            format!("{name} is available and version-compatible."),
        ),
        Some(_) => (
            DiagnosticState::Incompatible,
            format!("{name} does not match the desktop application version."),
        ),
        None => (
            DiagnosticState::Degraded,
            format!("{name} is executable but did not report a recognizable version."),
        ),
    };
    DiagnosticCheck {
        id: format!("binary-{name}"),
        label: format!("{name} binary"),
        component: "binary".to_string(),
        state,
        summary,
        details: Vec::new(),
        path: Some(path.display().to_string()),
        detected_version: version,
        expected_version: Some(EXPECTED_VERSION.to_string()),
        remediation: vec![action(
            "install-local-binaries",
            "Install or rebuild the matching local binaries",
            DiagnosticActionKind::Manual,
        )],
        checked_at: checked_at.to_string(),
    }
}

pub(crate) fn discover_binary(name: &str, env_key: &str) -> Option<PathBuf> {
    let binary_name = platform_binary_name(name);
    let explicit = env::var_os(env_key)
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty());
    let candidates = ordered_binary_candidates(
        &binary_name,
        explicit.clone(),
        env::var_os("CONTEXT_MANAGER_BIN_DIR").map(PathBuf::from),
        env::current_exe().ok(),
        prepared_sidecar_candidates(name, &binary_name),
        env::var_os("HOME").map(PathBuf::from),
        env::var_os("PATH")
            .map(|value| env::split_paths(&value).collect())
            .unwrap_or_default(),
    );
    if explicit.is_some() {
        return candidates.into_iter().next();
    }
    candidates.into_iter().find(|path| path.is_file())
}

fn ordered_binary_candidates(
    binary_name: &str,
    explicit: Option<PathBuf>,
    manager_bin_dir: Option<PathBuf>,
    current_exe: Option<PathBuf>,
    prepared_sidecars: Vec<PathBuf>,
    home: Option<PathBuf>,
    path_directories: Vec<PathBuf>,
) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(path) = explicit.filter(|path| !path.as_os_str().is_empty()) {
        candidates.push(path);
    }
    if let Some(directory) = manager_bin_dir.filter(|path| !path.as_os_str().is_empty()) {
        candidates.push(directory.join(binary_name));
    }
    if let Some(current) = current_exe {
        if let Some(parent) = current.parent() {
            candidates.push(parent.join(binary_name));
            if let Some(directory) = parent.parent() {
                candidates.push(directory.join(binary_name));
            }
        }
    }
    candidates.extend(prepared_sidecars);
    if let Some(home) = home.filter(|path| !path.as_os_str().is_empty()) {
        candidates.push(home.join(".local/bin").join(binary_name));
    }
    candidates.extend(
        path_directories
            .into_iter()
            .filter(|path| !path.as_os_str().is_empty())
            .map(|directory| directory.join(binary_name)),
    );
    candidates
}

fn prepared_sidecar_candidates(name: &str, binary_name: &str) -> Vec<PathBuf> {
    let manifest_binaries = Path::new(env!("CARGO_MANIFEST_DIR")).join("binaries");
    if let Ok(entries) = fs::read_dir(manifest_binaries) {
        let mut matches = entries
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name()
                    .and_then(|value| value.to_str())
                    .is_some_and(|value| {
                        value == binary_name || value.starts_with(&format!("{name}-"))
                    })
            })
            .collect::<Vec<_>>();
        matches.sort();
        matches
    } else {
        Vec::new()
    }
}

fn platform_binary_name(name: &str) -> String {
    if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    }
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(path)
        .map(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}

fn command_version(path: &Path) -> Option<String> {
    let output = run_command_with_input(path, &["--version"], None, Duration::from_secs(2)).ok()?;
    let text = format!(
        "{} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    text.split_whitespace()
        .find_map(normalize_version_token)
        .map(ToString::to_string)
}

fn normalize_version_token(token: &str) -> Option<&str> {
    let trimmed = token.trim_matches(|character: char| {
        !character.is_ascii_alphanumeric() && character != '.' && character != '-'
    });
    let starts_with_digit = trimmed
        .chars()
        .next()
        .is_some_and(|character| character.is_ascii_digit());
    if starts_with_digit && trimmed.chars().filter(|value| *value == '.').count() >= 2 {
        Some(trimmed)
    } else {
        None
    }
}

fn spool_check(paths: &ContextPaths, checked_at: &str) -> (DiagnosticCheck, usize) {
    let mut backlog = 0;
    let mut invalid = 0;
    match fs::read_dir(&paths.spool_dir) {
        Ok(entries) => {
            for entry in entries.filter_map(Result::ok) {
                let path = entry.path();
                if path.extension().and_then(|value| value.to_str()) != Some("json") {
                    continue;
                }
                if entry
                    .file_type()
                    .map(|file_type| file_type.is_file())
                    .unwrap_or(false)
                {
                    backlog += 1;
                } else {
                    invalid += 1;
                }
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => invalid += 1,
    }
    let (state, summary) = if invalid > 0 {
        (
            DiagnosticState::Failed,
            "The spool contains unreadable or unsafe entries.".to_string(),
        )
    } else if backlog > 0 {
        (
            DiagnosticState::Degraded,
            format!("{backlog} queued write request(s) are waiting for retry."),
        )
    } else {
        (
            DiagnosticState::Healthy,
            "The durable write spool has no pending requests.".to_string(),
        )
    };
    (
        DiagnosticCheck {
            id: "spool-backlog".to_string(),
            label: "Spool backlog".to_string(),
            component: "storage".to_string(),
            state,
            summary,
            details: if invalid > 0 {
                vec![format!(
                    "{invalid} unsafe or unreadable spool entry(s) detected."
                )]
            } else {
                Vec::new()
            },
            path: Some(paths.spool_dir.display().to_string()),
            detected_version: None,
            expected_version: None,
            remediation: if backlog > 0 {
                vec![action(
                    "retry-spool",
                    "Retry queued writes",
                    DiagnosticActionKind::RetrySpool,
                )]
            } else {
                Vec::new()
            },
            checked_at: checked_at.to_string(),
        },
        backlog,
    )
}

fn mcp_handshake_check(binary: Option<&Path>, checked_at: &str) -> DiagnosticCheck {
    let Some(binary) = binary else {
        return DiagnosticCheck {
            id: "mcp-handshake".to_string(),
            label: "MCP initialize and tools/list".to_string(),
            component: "mcp".to_string(),
            state: DiagnosticState::NotInstalled,
            summary: "The MCP handshake cannot run because context-mcp is unavailable.".to_string(),
            details: Vec::new(),
            path: None,
            detected_version: None,
            expected_version: Some(EXPECTED_VERSION.to_string()),
            remediation: vec![action(
                "install-local-binaries",
                "Install the matching context-mcp binary",
                DiagnosticActionKind::Manual,
            )],
            checked_at: checked_at.to_string(),
        };
    };
    let request = [
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": SUPPORTED_MCP_PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": {"name": "ucm-desktop-diagnostics", "version": EXPECTED_VERSION}
            }
        })
        .to_string(),
        json!({"jsonrpc": "2.0", "method": "notifications/initialized", "params": {}}).to_string(),
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}}).to_string(),
    ]
    .join("\n")
        + "\n";
    let output = run_command_with_input(
        binary,
        &["--adapter", "desktop-diagnostics", "--stdio", "serve"],
        Some(request.as_bytes()),
        Duration::from_secs(3),
    );
    let (state, summary, details) = match output {
        Ok(output) if output.status.success() => match validate_mcp_output(&output.stdout) {
            Ok(tool_count) => (
                DiagnosticState::Healthy,
                "MCP initialize and tools/list completed successfully.".to_string(),
                vec![format!("{tool_count} required tools are available.")],
            ),
            Err(message) => (DiagnosticState::Failed, message, Vec::new()),
        },
        Ok(_) => (
            DiagnosticState::Failed,
            "context-mcp exited before completing the MCP handshake.".to_string(),
            Vec::new(),
        ),
        Err(_) => (
            DiagnosticState::Failed,
            "context-mcp did not complete the MCP handshake within the safety timeout.".to_string(),
            Vec::new(),
        ),
    };
    DiagnosticCheck {
        id: "mcp-handshake".to_string(),
        label: "MCP initialize and tools/list".to_string(),
        component: "mcp".to_string(),
        state,
        summary,
        details,
        path: Some(binary.display().to_string()),
        detected_version: command_version(binary),
        expected_version: Some(EXPECTED_VERSION.to_string()),
        remediation: vec![action(
            "refresh-diagnostics",
            "Retry MCP handshake",
            DiagnosticActionKind::Refresh,
        )],
        checked_at: checked_at.to_string(),
    }
}

fn validate_mcp_output(bytes: &[u8]) -> Result<usize, String> {
    let mut initialized = false;
    let mut protocol_matches = false;
    let mut tools = BTreeSet::new();
    for line in String::from_utf8_lossy(bytes).lines() {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        match value.get("id").and_then(Value::as_i64) {
            Some(1) => {
                initialized = value
                    .pointer("/result/serverInfo/name")
                    .and_then(Value::as_str)
                    == Some("context-mcp");
                protocol_matches = value
                    .pointer("/result/protocolVersion")
                    .and_then(Value::as_str)
                    == Some(SUPPORTED_MCP_PROTOCOL_VERSION);
            }
            Some(2) => {
                if let Some(items) = value.pointer("/result/tools").and_then(Value::as_array) {
                    tools.extend(
                        items
                            .iter()
                            .filter_map(|item| item.get("name").and_then(Value::as_str))
                            .map(ToString::to_string),
                    );
                }
            }
            _ => {}
        }
    }
    let required = ["compose_context", "search_context", "commit_work"];
    if !initialized {
        return Err("context-mcp returned an invalid initialize response.".to_string());
    }
    if !protocol_matches {
        return Err("context-mcp returned an unsupported protocolVersion.".to_string());
    }
    if required.iter().any(|name| !tools.contains(*name)) {
        return Err("context-mcp tools/list is missing one or more required tools.".to_string());
    }
    Ok(required.len())
}

fn run_command_with_input(
    path: &Path,
    args: &[&str],
    input: Option<&[u8]>,
    timeout: Duration,
) -> std::io::Result<std::process::Output> {
    let mut child = Command::new(path)
        .args(args)
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    if let (Some(input), Some(mut stdin)) = (input, child.stdin.take()) {
        stdin.write_all(input)?;
    }
    let started = Instant::now();
    loop {
        if child.try_wait()?.is_some() {
            return child.wait_with_output();
        }
        if started.elapsed() >= timeout {
            child.kill()?;
            let _ = child.wait();
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "command timed out",
            ));
        }
        thread::sleep(Duration::from_millis(25));
    }
}

#[derive(Clone, Copy)]
enum AdapterHarness {
    Codex,
    Claude,
}

impl AdapterHarness {
    fn id(self) -> &'static str {
        match self {
            Self::Codex => "adapter-codex-install",
            Self::Claude => "adapter-claude-install",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Codex => "Codex plugin installation",
            Self::Claude => "Claude Code plugin installation",
        }
    }

    fn component(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::Claude => "claude-code",
        }
    }

    fn manifest_dir(self) -> &'static str {
        match self {
            Self::Codex => ".codex-plugin",
            Self::Claude => ".claude-plugin",
        }
    }
}

fn adapter_check(harness: AdapterHarness, enabled: bool, checked_at: &str) -> DiagnosticCheck {
    let marker = adapter_marker(harness);
    let scanned_roots = scan_adapter_roots(harness);
    let active_roots = active_adapter_roots(harness, &marker, &scanned_roots);
    evaluate_adapter_installations(
        harness,
        enabled,
        checked_at,
        marker.present,
        active_roots,
        scanned_roots,
    )
}

fn evaluate_adapter_installations(
    harness: AdapterHarness,
    enabled: bool,
    checked_at: &str,
    registered: bool,
    active_roots: Vec<PathBuf>,
    scanned_roots: Vec<PathBuf>,
) -> DiagnosticCheck {
    let active_set = active_roots.iter().collect::<BTreeSet<_>>();
    let stale_roots = scanned_roots
        .into_iter()
        .filter(|root| !active_set.contains(root))
        .collect::<Vec<_>>();
    let installations = active_roots
        .into_iter()
        .map(|root| {
            validate_adapter_root(harness, root.clone()).unwrap_or(AdapterInstallation {
                root,
                version: None,
                issues: vec![
                    "The registered plugin root has no valid plugin manifest.".to_string(),
                ],
            })
        })
        .collect::<Vec<_>>();
    let mut details = Vec::new();
    if !stale_roots.is_empty() {
        details.push(format!(
            "Found {} unregistered cached plugin root(s); they are not used for health.",
            stale_roots.len()
        ));
    }
    details.extend(
        installations
            .iter()
            .flat_map(|installation| installation.issues.iter().cloned()),
    );
    let path = installations.first().map(|value| value.root.clone());
    let version = installations
        .first()
        .and_then(|value| value.version.clone());
    let (state, summary) = if !enabled {
        (
            DiagnosticState::Ignored,
            "This adapter is disabled in desktop settings.".to_string(),
        )
    } else if !registered {
        (
            DiagnosticState::NotInstalled,
            if stale_roots.is_empty() {
                "No active context-manager plugin registration was found.".to_string()
            } else {
                "Only stale or unregistered plugin caches were found.".to_string()
            },
        )
    } else if installations.is_empty() {
        (
            DiagnosticState::Degraded,
            "The harness reports the plugin, but no authoritative active root was found."
                .to_string(),
        )
    } else if installations.iter().any(|installation| {
        installation
            .version
            .as_deref()
            .is_some_and(|version| version != EXPECTED_VERSION)
    }) {
        (
            DiagnosticState::Incompatible,
            "An active registered plugin version does not match this desktop build.".to_string(),
        )
    } else if installations
        .iter()
        .any(|installation| !installation.issues.is_empty())
    {
        (
            DiagnosticState::Degraded,
            "An active registered plugin root is incomplete or unavailable.".to_string(),
        )
    } else {
        (
            DiagnosticState::Healthy,
            "All active registered plugin roots have valid manifests and launchers.".to_string(),
        )
    };
    DiagnosticCheck {
        id: harness.id().to_string(),
        label: harness.label().to_string(),
        component: harness.component().to_string(),
        state,
        summary,
        details,
        path: path.map(|value| value.display().to_string()),
        detected_version: version,
        expected_version: Some(EXPECTED_VERSION.to_string()),
        remediation: vec![action(
            "install-adapter",
            "Install or repair the plugin through the harness",
            DiagnosticActionKind::Manual,
        )],
        checked_at: checked_at.to_string(),
    }
}

struct AdapterMarker {
    present: bool,
    paths: Vec<PathBuf>,
    identifiers: Vec<String>,
}

fn adapter_marker(harness: AdapterHarness) -> AdapterMarker {
    match harness {
        AdapterHarness::Codex => {
            let config = user_home().join(".codex/config.toml");
            let mut identifiers = fs::read_to_string(&config)
                .map(|contents| {
                    contents
                        .lines()
                        .filter_map(codex_plugin_identifier)
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            identifiers.sort();
            identifiers.dedup();
            AdapterMarker {
                present: !identifiers.is_empty(),
                paths: Vec::new(),
                identifiers,
            }
        }
        AdapterHarness::Claude => {
            let registry = user_home().join(".claude/plugins/installed_plugins.json");
            let mut paths = Vec::new();
            let present = fs::read_to_string(&registry)
                .ok()
                .and_then(|contents| serde_json::from_str::<Value>(&contents).ok())
                .and_then(|value| value.get("plugins").cloned())
                .and_then(|plugins| plugins.as_object().cloned())
                .map(|plugins| {
                    let mut found = false;
                    for (key, installs) in plugins {
                        if key == "context-manager" || key.starts_with("context-manager@") {
                            found = true;
                            if let Some(installs) = installs.as_array() {
                                paths.extend(installs.iter().filter_map(|install| {
                                    install
                                        .get("installPath")
                                        .and_then(Value::as_str)
                                        .map(PathBuf::from)
                                }));
                            }
                        }
                    }
                    found
                })
                .unwrap_or(false);
            AdapterMarker {
                present,
                paths,
                identifiers: Vec::new(),
            }
        }
    }
}

fn codex_plugin_identifier(line: &str) -> Option<String> {
    let line = line.trim();
    let identifier = if let Some(rest) = line.strip_prefix("[plugins.\"") {
        rest.split('"').next()
    } else if let Some(rest) = line.strip_prefix("[plugins.") {
        rest.split([']', '.']).next()
    } else {
        None
    }?;
    (identifier == "context-manager" || identifier.starts_with("context-manager@"))
        .then(|| identifier.to_string())
}

fn codex_root_matches_identifier(root: &Path, identifier: &str) -> bool {
    if let Some((plugin, marketplace)) = identifier.split_once('@') {
        let components = root
            .components()
            .filter_map(|component| component.as_os_str().to_str())
            .collect::<Vec<_>>();
        components
            .windows(2)
            .any(|parts| parts == [marketplace, plugin])
    } else {
        root.file_name().and_then(|name| name.to_str()) == Some(identifier)
    }
}

fn scan_adapter_roots(harness: AdapterHarness) -> Vec<PathBuf> {
    let scan_root = match harness {
        AdapterHarness::Codex => user_home().join(".codex/plugins"),
        AdapterHarness::Claude => user_home().join(".claude/plugins"),
    };
    let mut manifests = Vec::new();
    let mut visited = 0;
    scan_for_manifests(
        &scan_root,
        harness.manifest_dir(),
        0,
        &mut manifests,
        &mut visited,
    );
    let mut roots = manifests
        .into_iter()
        .filter_map(|manifest| {
            manifest
                .parent()
                .and_then(Path::parent)
                .map(Path::to_path_buf)
        })
        .collect::<Vec<_>>();
    roots.sort();
    roots.dedup();
    roots
}

fn active_adapter_roots(
    harness: AdapterHarness,
    marker: &AdapterMarker,
    scanned_roots: &[PathBuf],
) -> Vec<PathBuf> {
    let mut roots = match harness {
        AdapterHarness::Claude => marker.paths.clone(),
        AdapterHarness::Codex => scanned_roots
            .iter()
            .filter(|root| {
                marker
                    .identifiers
                    .iter()
                    .any(|identifier| codex_root_matches_identifier(root, identifier))
            })
            .cloned()
            .collect(),
    };
    roots.sort();
    roots.dedup();
    roots
}

fn scan_for_manifests(
    directory: &Path,
    manifest_dir: &str,
    depth: usize,
    results: &mut Vec<PathBuf>,
    visited: &mut usize,
) {
    if depth > MAX_PLUGIN_SCAN_DEPTH || *visited >= MAX_PLUGIN_SCAN_ENTRIES {
        return;
    }
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    for entry in entries.filter_map(Result::ok) {
        if *visited >= MAX_PLUGIN_SCAN_ENTRIES {
            return;
        }
        *visited += 1;
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
        if name.contains("backup")
            || name == ".tmp"
            || name == ".remote-plugin-install-staging"
            || name == "marketplaces"
        {
            continue;
        }
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_symlink() {
            continue;
        }
        if file_type.is_dir() {
            scan_for_manifests(&path, manifest_dir, depth + 1, results, visited);
        } else if name == "plugin.json"
            && path
                .parent()
                .and_then(Path::file_name)
                .and_then(|value| value.to_str())
                == Some(manifest_dir)
        {
            results.push(path);
        }
    }
}

struct AdapterInstallation {
    root: PathBuf,
    version: Option<String>,
    issues: Vec<String>,
}

fn validate_adapter_root(harness: AdapterHarness, root: PathBuf) -> Option<AdapterInstallation> {
    let manifest_path = root.join(harness.manifest_dir()).join("plugin.json");
    let manifest = fs::read_to_string(&manifest_path)
        .ok()
        .and_then(|contents| serde_json::from_str::<Value>(&contents).ok())?;
    if manifest.get("name").and_then(Value::as_str) != Some("context-manager") {
        return None;
    }
    let version = manifest
        .get("version")
        .and_then(Value::as_str)
        .map(ToString::to_string);
    let mut issues = Vec::new();
    if version.is_none() {
        issues.push("The plugin manifest does not declare a version.".to_string());
    }
    let mcp_path = root.join(".mcp.json");
    let mcp_valid = fs::read_to_string(&mcp_path)
        .ok()
        .and_then(|contents| serde_json::from_str::<Value>(&contents).ok())
        .and_then(|value| {
            value
                .pointer("/mcpServers/context-mcp/command")
                .and_then(Value::as_str)
                .map(|command| command.contains("run-context-mcp"))
        })
        .unwrap_or(false);
    if !mcp_valid {
        issues.push("The plugin-scoped .mcp.json is missing or invalid.".to_string());
    }
    let launcher = root.join("scripts/run-context-mcp.sh");
    if !is_executable(&launcher) {
        issues.push("The context-mcp launcher is missing or not executable.".to_string());
    }
    Some(AdapterInstallation {
        root,
        version,
        issues,
    })
}

fn action(id: &str, label: &str, kind: DiagnosticActionKind) -> DiagnosticAction {
    DiagnosticAction {
        id: id.to_string(),
        label: label.to_string(),
        kind,
    }
}

fn diagnostic_rank(state: &DiagnosticState) -> u8 {
    match state {
        DiagnosticState::Healthy | DiagnosticState::Ignored => 0,
        DiagnosticState::NotInstalled | DiagnosticState::Stopped => 1,
        DiagnosticState::Starting => 2,
        DiagnosticState::Degraded => 3,
        DiagnosticState::MigrationRequired => 4,
        DiagnosticState::Incompatible => 5,
        DiagnosticState::Failed => 6,
    }
}

fn user_home() -> PathBuf {
    env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("~"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn write_valid_adapter_root(harness: AdapterHarness, root: &Path) {
        fs::create_dir_all(root.join(harness.manifest_dir())).expect("manifest directory");
        fs::create_dir_all(root.join("scripts")).expect("scripts directory");
        fs::write(
            root.join(harness.manifest_dir()).join("plugin.json"),
            serde_json::to_vec(&json!({
                "name": "context-manager",
                "version": EXPECTED_VERSION
            }))
            .expect("manifest"),
        )
        .expect("write manifest");
        fs::write(
            root.join(".mcp.json"),
            r#"{"mcpServers":{"context-mcp":{"command":"./scripts/run-context-mcp.sh"}}}"#,
        )
        .expect("write mcp manifest");
        let launcher = root.join("scripts/run-context-mcp.sh");
        fs::write(&launcher, "#!/bin/sh\nexit 0\n").expect("write launcher");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&launcher, fs::Permissions::from_mode(0o700))
                .expect("launcher permissions");
        }
    }

    #[test]
    fn mcp_handshake_validation_requires_initialize_and_all_tools() {
        let valid = [
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "result": {
                    "protocolVersion": SUPPORTED_MCP_PROTOCOL_VERSION,
                    "serverInfo": {"name": "context-mcp"}
                }
            })
            .to_string(),
            json!({
                "jsonrpc": "2.0",
                "id": 2,
                "result": {"tools": [
                    {"name": "compose_context"},
                    {"name": "search_context"},
                    {"name": "commit_work"}
                ]}
            })
            .to_string(),
        ]
        .join("\n");
        assert_eq!(
            validate_mcp_output(valid.as_bytes()).expect("valid handshake"),
            3
        );

        let incomplete = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {
                "protocolVersion": SUPPORTED_MCP_PROTOCOL_VERSION,
                "serverInfo": {"name": "context-mcp"}
            }
        })
        .to_string();
        assert!(validate_mcp_output(incomplete.as_bytes()).is_err());

        let bad_protocol = [
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "result": {
                    "protocolVersion": "1900-01-01",
                    "serverInfo": {"name": "context-mcp"}
                }
            })
            .to_string(),
            json!({
                "jsonrpc": "2.0",
                "id": 2,
                "result": {"tools": [
                    {"name": "compose_context"},
                    {"name": "search_context"},
                    {"name": "commit_work"}
                ]}
            })
            .to_string(),
        ]
        .join("\n");
        assert!(validate_mcp_output(bad_protocol.as_bytes())
            .expect_err("bad protocol")
            .contains("protocolVersion"));
    }

    #[test]
    fn failed_diagnostics_rank_above_degraded_and_ignored() {
        assert!(
            diagnostic_rank(&DiagnosticState::Failed) > diagnostic_rank(&DiagnosticState::Degraded)
        );
        assert_eq!(
            diagnostic_rank(&DiagnosticState::Ignored),
            diagnostic_rank(&DiagnosticState::Healthy)
        );
    }

    #[cfg(unix)]
    #[test]
    fn unix_permission_assessment_rejects_wrong_owner_and_missing_access() {
        assert_eq!(
            classify_unix_security(501, 502, true, false),
            PathSecurityAssessment::WrongOwner
        );
        assert_eq!(
            classify_unix_security(501, 501, false, false),
            PathSecurityAssessment::Inaccessible
        );
        assert_eq!(
            classify_unix_security(501, 501, true, true),
            PathSecurityAssessment::ExcessPermissions
        );
    }

    #[test]
    fn daemon_health_compares_component_version_separately_from_schema() {
        let paths = ContextPaths {
            data_dir: PathBuf::from("/local/data"),
            db_path: PathBuf::from("/local/data/context.db"),
            socket_path: PathBuf::from("/local/data/contextd.sock"),
            spool_dir: PathBuf::from("/local/data/spool"),
        };
        let mismatched = HealthReport {
            component_version: Some("9.9.9".to_string()),
            api_version: Some(CONTEXT_API_VERSION),
            schema_version: LATEST_SCHEMA_VERSION,
            packs: 1,
            entries: 2,
            reviews: 0,
            runs: 0,
        };
        let check = daemon_check(&paths, true, Some(&mismatched), None, "checked");
        assert_eq!(check.state, DiagnosticState::Incompatible);
        assert_eq!(check.detected_version.as_deref(), Some("9.9.9"));
        assert_eq!(check.expected_version.as_deref(), Some(EXPECTED_VERSION));
        assert!(check
            .details
            .iter()
            .any(|detail| detail.contains("Database schema")));

        let legacy = HealthReport {
            component_version: None,
            ..mismatched
        };
        let legacy_check = daemon_check(&paths, true, Some(&legacy), None, "checked");
        assert_eq!(legacy_check.state, DiagnosticState::Degraded);
        assert!(legacy_check.detected_version.is_none());
        assert!(legacy_check
            .remediation
            .iter()
            .any(|action| action.kind == DiagnosticActionKind::Refresh));
        assert!(legacy_check
            .remediation
            .iter()
            .all(|action| action.kind != DiagnosticActionKind::RestartDaemon));

        let healthy = HealthReport {
            component_version: Some(EXPECTED_VERSION.to_string()),
            api_version: Some(CONTEXT_API_VERSION),
            schema_version: LATEST_SCHEMA_VERSION,
            packs: 1,
            entries: 2,
            reviews: 0,
            runs: 0,
        };
        let healthy_check = daemon_check(&paths, true, Some(&healthy), None, "checked");
        assert_eq!(healthy_check.state, DiagnosticState::Healthy);
        assert!(healthy_check
            .remediation
            .iter()
            .all(|action| action.kind != DiagnosticActionKind::RestartDaemon));

        let legacy_api = HealthReport {
            api_version: None,
            ..healthy.clone()
        };
        let legacy_api_check = daemon_check(&paths, true, Some(&legacy_api), None, "checked");
        assert_eq!(legacy_api_check.state, DiagnosticState::Degraded);
        assert!(legacy_api_check
            .summary
            .contains("Context API version is unknown"));

        let mismatched_api = HealthReport {
            api_version: Some(CONTEXT_API_VERSION + 1),
            ..healthy.clone()
        };
        let mismatched_api_check =
            daemon_check(&paths, true, Some(&mismatched_api), None, "checked");
        assert_eq!(mismatched_api_check.state, DiagnosticState::Incompatible);
        assert!(mismatched_api_check.summary.contains("API version"));

        let newer_schema_and_old_component = HealthReport {
            component_version: Some("0.0.1".to_string()),
            schema_version: LATEST_SCHEMA_VERSION + 1,
            ..healthy.clone()
        };
        let newer_schema_check = daemon_check(
            &paths,
            true,
            Some(&newer_schema_and_old_component),
            None,
            "checked",
        );
        assert_eq!(newer_schema_check.state, DiagnosticState::Incompatible);
        assert!(newer_schema_check.summary.contains("schema is newer"));
        assert!(newer_schema_check
            .remediation
            .iter()
            .any(|action| action.label == "Update the desktop application"));

        let migration = HealthReport {
            schema_version: LATEST_SCHEMA_VERSION - 1,
            ..healthy
        };
        let migration_check = daemon_check(&paths, true, Some(&migration), None, "checked");
        assert_eq!(migration_check.state, DiagnosticState::MigrationRequired);
        assert!(migration_check
            .remediation
            .iter()
            .all(|action| action.kind == DiagnosticActionKind::Manual));
    }

    #[test]
    fn binary_discovery_precedence_matches_runtime_sidecar_resolution() {
        let candidates = ordered_binary_candidates(
            "contextd",
            Some(PathBuf::from("/explicit/contextd")),
            Some(PathBuf::from("/manager/bin")),
            Some(PathBuf::from("/workspace/target/debug/app")),
            vec![PathBuf::from(
                "/workspace/apps/desktop/src-tauri/binaries/contextd-target",
            )],
            Some(PathBuf::from("/home/operator")),
            vec![PathBuf::from("/path/first"), PathBuf::from("/path/second")],
        );
        assert_eq!(
            candidates,
            vec![
                PathBuf::from("/explicit/contextd"),
                PathBuf::from("/manager/bin/contextd"),
                PathBuf::from("/workspace/target/debug/contextd"),
                PathBuf::from("/workspace/target/contextd"),
                PathBuf::from("/workspace/apps/desktop/src-tauri/binaries/contextd-target"),
                PathBuf::from("/home/operator/.local/bin/contextd"),
                PathBuf::from("/path/first/contextd"),
                PathBuf::from("/path/second/contextd"),
            ]
        );
    }

    #[test]
    fn claude_stale_cache_cannot_mask_broken_registered_root() {
        let temp = tempdir().expect("tempdir");
        let active = temp.path().join("registered-broken");
        let stale = temp.path().join("cache/stale/context-manager/0.1.0");
        fs::create_dir_all(&active).expect("active root");
        write_valid_adapter_root(AdapterHarness::Claude, &stale);
        let check = evaluate_adapter_installations(
            AdapterHarness::Claude,
            true,
            "checked",
            true,
            vec![active.clone()],
            vec![active.clone(), stale],
        );
        assert_eq!(check.state, DiagnosticState::Degraded);
        assert_eq!(
            check.path.as_deref(),
            Some(active.to_string_lossy().as_ref())
        );
        assert!(check
            .details
            .iter()
            .any(|detail| detail.contains("unregistered cached")));
    }

    #[test]
    fn codex_active_marketplace_root_outranks_stale_cache() {
        let temp = tempdir().expect("tempdir");
        let active = temp
            .path()
            .join("cache/active-market/context-manager/0.1.0");
        let stale = temp.path().join("cache/stale-market/context-manager/0.1.0");
        fs::create_dir_all(&active).expect("active root");
        write_valid_adapter_root(AdapterHarness::Codex, &stale);
        let marker = AdapterMarker {
            present: true,
            paths: Vec::new(),
            identifiers: vec!["context-manager@active-market".to_string()],
        };
        let scanned = vec![active.clone(), stale];
        let active_roots = active_adapter_roots(AdapterHarness::Codex, &marker, &scanned);
        assert_eq!(active_roots, vec![active.clone()]);
        let check = evaluate_adapter_installations(
            AdapterHarness::Codex,
            true,
            "checked",
            marker.present,
            active_roots,
            scanned,
        );
        assert_eq!(check.state, DiagnosticState::Degraded);
        assert_eq!(
            check.path.as_deref(),
            Some(active.to_string_lossy().as_ref())
        );
        assert!(check
            .details
            .iter()
            .any(|detail| detail.contains("unregistered cached")));
    }
}
