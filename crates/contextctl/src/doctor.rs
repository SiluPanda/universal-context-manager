use crate::diagnostics::{
    AdapterKind, AdapterStatus, BinaryStatus, discover_binary, discover_source_root,
    health_report_component_version, inspect_adapter, inspect_binary, normalized_component_version,
    run_mcp_handshake, safe_message,
};
use crate::output::table;
use context_client::{ClientError, ContextClient};
use context_core::{CONTEXT_API_VERSION, IpcRequest, IpcResponse, ReviewPolicy};
use serde::Serialize;
use serde_json::json;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DoctorState {
    Healthy,
    Degraded,
    Failed,
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CheckStatus {
    Pass,
    Warn,
    Fail,
    Incompatible,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct DoctorCheck {
    pub id: String,
    pub status: CheckStatus,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remediation: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct DoctorPaths {
    pub data_dir: String,
    pub db_path: String,
    pub socket_path: String,
    pub spool_dir: String,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct DoctorRepair {
    pub action: String,
    pub succeeded: bool,
    pub message: String,
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum VersionState {
    Compatible,
    Unknown,
    Incompatible,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct DoctorVersions {
    pub state: VersionState,
    pub contextctl: String,
    pub contextd_binary: Option<String>,
    pub context_mcp_binary: Option<String>,
    pub running_daemon: Option<String>,
    pub mcp_initialize_server: Option<String>,
    pub api: DoctorApiCompatibility,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct DoctorApiCompatibility {
    pub state: VersionState,
    pub expected: u32,
    pub running_daemon: Option<u32>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct DoctorReport {
    pub overall: DoctorState,
    pub versions: DoctorVersions,
    pub paths: DoctorPaths,
    pub binaries: Vec<BinaryStatus>,
    pub adapters: Vec<AdapterStatus>,
    pub checks: Vec<DoctorCheck>,
    pub repairs: Vec<DoctorRepair>,
}

#[derive(Clone, Debug)]
struct DaemonHealth {
    schema_version: i64,
    component_version: Option<String>,
    api_version: Option<u32>,
}

pub(crate) fn run_doctor(client: &ContextClient, repair: bool) -> DoctorReport {
    let paths = &client.config().paths;
    let mut repairs = Vec::new();

    if repair {
        match paths.ensure_parent_dirs() {
            Ok(()) => repairs.push(DoctorRepair {
                action: "ensure_private_directories".to_string(),
                succeeded: true,
                message: "Ensured data, database parent, socket parent, and spool directories."
                    .to_string(),
            }),
            Err(error) => repairs.push(DoctorRepair {
                action: "ensure_private_directories".to_string(),
                succeeded: false,
                message: format!("Could not ensure directories: {}", safe_message(&error)),
            }),
        }
        match client.ensure_daemon() {
            Ok(()) => repairs.push(DoctorRepair {
                action: "ensure_daemon".to_string(),
                succeeded: true,
                message: "Verified or started contextd. Existing daemon behavior handles only an unreachable stale socket before binding.".to_string(),
            }),
            Err(error) => repairs.push(DoctorRepair {
                action: "ensure_daemon".to_string(),
                succeeded: false,
                message: format!("Could not start contextd: {}", safe_message(&error)),
            }),
        }
        match client.retry_spool() {
            Ok(report) => repairs.push(DoctorRepair {
                action: "retry_spool".to_string(),
                succeeded: report.retained == 0,
                message: format!(
                    "Retried {} queued request(s): {} delivered, {} retained.",
                    report.attempted, report.delivered, report.retained
                ),
            }),
            Err(error) => repairs.push(DoctorRepair {
                action: "retry_spool".to_string(),
                succeeded: false,
                message: format!("Spool retry failed: {}", safe_message(&error)),
            }),
        }
    }

    let binaries = vec![
        inspect_binary("contextctl", None),
        inspect_binary("contextd", Some("CONTEXTD_BIN")),
        inspect_binary("context-mcp", Some("CONTEXT_MCP_BIN")),
    ];
    let contextctl_version = env!("CARGO_PKG_VERSION");
    let mut checks = Vec::new();
    for binary in &binaries {
        if !binary.available {
            checks.push(check(
                format!("binary_{}", binary.name),
                CheckStatus::Fail,
                format!("{} is unavailable.", binary.name),
                binary.note.clone(),
            ));
        } else {
            checks.push(check(
                format!("binary_{}", binary.name),
                CheckStatus::Pass,
                format!(
                    "{} is available at {}{}.",
                    binary.name,
                    binary.path.as_deref().unwrap_or("unknown path"),
                    binary
                        .version
                        .as_deref()
                        .map(|version| format!(" (version {version})"))
                        .unwrap_or_default()
                ),
                None,
            ));
        }
    }
    checks.push(check(
        "version_contextctl",
        CheckStatus::Pass,
        format!("contextctl establishes component version {contextctl_version}."),
        None,
    ));
    for (name, id) in [
        ("contextd", "version_contextd_binary"),
        ("context-mcp", "version_context_mcp_binary"),
    ] {
        let binary = binaries
            .iter()
            .find(|binary| binary.name == name)
            .expect("known binary status");
        checks.push(component_version_check(
            id,
            name,
            contextctl_version,
            binary.version.as_deref(),
            binary.available,
        ));
    }

    let daemon_health = match ping_without_autostart(&paths.socket_path) {
        Ok(health) => {
            checks.push(check(
                "daemon_reachable",
                CheckStatus::Pass,
                format!(
                    "contextd is reachable at {} (schema v{}{}{}).",
                    paths.socket_path.display(),
                    health.schema_version,
                    health
                        .component_version
                        .as_deref()
                        .map(|version| format!(", component {version}"))
                        .unwrap_or_default(),
                    health
                        .api_version
                        .map(|version| format!(", API v{version}"))
                        .unwrap_or_default()
                ),
                None,
            ));
            Some(health)
        }
        Err(error) => {
            checks.push(check(
                "daemon_reachable",
                CheckStatus::Fail,
                format!("contextd is unreachable: {}", safe_message(&error)),
                Some(
                    "Run `contextctl doctor --repair`; if contextd is not on PATH, set CONTEXTD_BIN."
                        .to_string(),
                ),
            ));
            None
        }
    };
    checks.push(component_version_check(
        "version_contextd_daemon",
        "running contextd",
        contextctl_version,
        daemon_health
            .as_ref()
            .and_then(|health| health.component_version.as_deref()),
        daemon_health.is_some(),
    ));
    checks.push(api_version_check(
        CONTEXT_API_VERSION,
        daemon_health.as_ref().and_then(|health| health.api_version),
        daemon_health.is_some(),
    ));

    checks.push(database_check(&paths.db_path, daemon_health.as_ref()));
    checks.extend(permission_checks(
        &paths.data_dir,
        &paths.db_path,
        &paths.socket_path,
        &paths.spool_dir,
    ));

    if let Some(health) = daemon_health.as_ref() {
        checks.push(review_policy_check(health, client.get_review_policy()));
    } else {
        checks.push(check(
            "review_policy",
            CheckStatus::Fail,
            "Review policy could not be checked while contextd is unreachable.".to_string(),
            Some("Restore daemon reachability, then rerun `contextctl doctor`.".to_string()),
        ));
    }

    let backlog = spool_backlog(&paths.spool_dir);
    match backlog {
        Ok(0) => checks.push(check(
            "spool_backlog",
            CheckStatus::Pass,
            "No queued commit requests are waiting in the spool.".to_string(),
            None,
        )),
        Ok(count) => checks.push(check(
            "spool_backlog",
            CheckStatus::Warn,
            format!("{count} queued commit request(s) remain in the spool."),
            Some("Run `contextctl retry-spool` or `contextctl doctor --repair`.".to_string()),
        )),
        Err(error) => checks.push(check(
            "spool_backlog",
            CheckStatus::Fail,
            format!(
                "Spool backlog could not be inspected: {}",
                safe_message(&error)
            ),
            Some("Check spool directory ownership and permissions.".to_string()),
        )),
    }

    let mcp_handshake = match discover_binary("context-mcp", Some("CONTEXT_MCP_BIN")) {
        Some(binary) => match run_mcp_handshake(&binary) {
            Ok(handshake) => {
                checks.push(check(
                    "mcp_handshake",
                    CheckStatus::Pass,
                    format!(
                        "MCP initialize and tools/list succeeded for {} with {} tools.",
                        handshake.server_name,
                        handshake.tools.len()
                    ),
                    None,
                ));
                Some(handshake)
            }
            Err(error) => {
                checks.push(check(
                    "mcp_handshake",
                    CheckStatus::Fail,
                    format!("MCP handshake failed: {}", safe_message(&error)),
                    Some(
                        "Install a matching context-mcp binary or set CONTEXT_MCP_BIN, then rerun doctor."
                            .to_string(),
                    ),
                ));
                None
            }
        },
        None => {
            checks.push(check(
                "mcp_handshake",
                CheckStatus::Fail,
                "MCP handshake was skipped because context-mcp is unavailable.".to_string(),
                Some("Install context-mcp or set CONTEXT_MCP_BIN.".to_string()),
            ));
            None
        }
    };
    checks.push(component_version_check(
        "version_context_mcp_server",
        "MCP initialize server",
        contextctl_version,
        mcp_handshake
            .as_ref()
            .and_then(|handshake| handshake.server_version.as_deref()),
        mcp_handshake.is_some(),
    ));

    let source_root = discover_source_root();
    let home = home_directory();
    let adapters = [AdapterKind::Codex, AdapterKind::ClaudeCode]
        .into_iter()
        .map(|adapter| inspect_adapter(adapter, home.as_deref(), source_root.as_deref()))
        .collect::<Vec<_>>();
    for adapter in &adapters {
        checks.push(if adapter.configured {
            check(
                format!("adapter_{}", adapter.adapter.as_str()),
                CheckStatus::Pass,
                format!(
                    "{} plugin installation and runtime markers are valid.",
                    adapter.adapter.as_str()
                ),
                None,
            )
        } else {
            check(
                format!("adapter_{}", adapter.adapter.as_str()),
                CheckStatus::Warn,
                format!(
                    "{} plugin is not fully installed/configured.",
                    adapter.adapter.as_str()
                ),
                Some(if adapter.next_commands.is_empty() {
                    "Reinstall the adapter from a verified Universal Context Manager source checkout."
                        .to_string()
                } else {
                    adapter.next_commands.join(" && ")
                }),
            )
        });
    }

    let contextd_binary_version = binary_version(&binaries, "contextd");
    let context_mcp_binary_version = binary_version(&binaries, "context-mcp");
    let daemon_version = daemon_health
        .as_ref()
        .and_then(|health| health.component_version.clone());
    let mcp_server_version = mcp_handshake
        .as_ref()
        .and_then(|handshake| handshake.server_version.clone())
        .and_then(|version| normalized_component_version(&version));
    let component_state = version_state(
        contextctl_version,
        [
            contextd_binary_version.as_deref(),
            context_mcp_binary_version.as_deref(),
            daemon_version.as_deref(),
            mcp_server_version.as_deref(),
        ],
    );
    let daemon_api_version = daemon_health.as_ref().and_then(|health| health.api_version);
    let api_state = api_version_state(CONTEXT_API_VERSION, daemon_api_version);
    let versions = DoctorVersions {
        state: combine_version_states(component_state, api_state),
        contextctl: contextctl_version.to_string(),
        contextd_binary: contextd_binary_version,
        context_mcp_binary: context_mcp_binary_version,
        running_daemon: daemon_version,
        mcp_initialize_server: mcp_server_version,
        api: DoctorApiCompatibility {
            state: api_state,
            expected: CONTEXT_API_VERSION,
            running_daemon: daemon_api_version,
        },
    };
    let overall = overall_state(&checks);

    DoctorReport {
        overall,
        versions,
        paths: DoctorPaths {
            data_dir: paths.data_dir.display().to_string(),
            db_path: paths.db_path.display().to_string(),
            socket_path: paths.socket_path.display().to_string(),
            spool_dir: paths.spool_dir.display().to_string(),
        },
        binaries,
        adapters,
        checks,
        repairs,
    }
}

pub(crate) fn format_doctor_report(report: &DoctorReport) -> String {
    let mut lines = vec![
        format!("Doctor: {:?}", report.overall).to_lowercase(),
        format!("Overall compatibility: {:?}", report.versions.state).to_lowercase(),
        format!(
            "API contract: {} (expected {}, daemon {})",
            format!("{:?}", report.versions.api.state).to_lowercase(),
            report.versions.api.expected,
            report
                .versions
                .api
                .running_daemon
                .map(|version| version.to_string())
                .unwrap_or_else(|| "unknown".to_string())
        ),
    ];
    lines.push(table(
        &["CHECK", "STATUS", "DETAIL"],
        report
            .checks
            .iter()
            .map(|check| {
                vec![
                    check.id.clone(),
                    format!("{:?}", check.status).to_lowercase(),
                    check.message.clone(),
                ]
            })
            .collect(),
    ));
    if !report.repairs.is_empty() {
        lines.push("Repairs:".to_string());
        for repair in &report.repairs {
            lines.push(format!(
                "  {}: {} — {}",
                repair.action,
                if repair.succeeded { "ok" } else { "failed" },
                repair.message
            ));
        }
    }
    let remediations = report
        .checks
        .iter()
        .filter_map(|check| check.remediation.as_ref())
        .collect::<Vec<_>>();
    if !remediations.is_empty() {
        lines.push("Remediation:".to_string());
        for remediation in remediations {
            lines.push(format!("  {remediation}"));
        }
    }
    lines.join("\n")
}

fn ping_without_autostart(socket_path: &Path) -> anyhow::Result<DaemonHealth> {
    let mut stream = UnixStream::connect(socket_path)
        .map_err(|error| anyhow::anyhow!("connect {}: {error}", socket_path.display()))?;
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    let request = IpcRequest {
        id: "contextctl-doctor-ping".to_string(),
        method: "ping".to_string(),
        params: json!({}),
    };
    writeln!(stream, "{}", serde_json::to_string(&request)?)?;
    stream.flush()?;
    let mut response = String::new();
    BufReader::new(stream).read_line(&mut response)?;
    if response.len() > 1024 * 1024 {
        return Err(anyhow::anyhow!(
            "daemon ping response exceeded the diagnostic limit"
        ));
    }
    let response: IpcResponse = serde_json::from_str(response.trim())?;
    if !response.ok {
        return Err(anyhow::anyhow!("daemon rejected the diagnostic ping"));
    }
    let result = response
        .result
        .ok_or_else(|| anyhow::anyhow!("daemon ping returned no result"))?;
    daemon_health_from_value(result)
}

fn daemon_health_from_value(result: serde_json::Value) -> anyhow::Result<DaemonHealth> {
    let schema_version = result
        .get("schema_version")
        .and_then(serde_json::Value::as_i64)
        .ok_or_else(|| anyhow::anyhow!("daemon ping omitted schema_version"))?;
    Ok(DaemonHealth {
        schema_version,
        component_version: health_report_component_version(&result),
        api_version: result
            .get("api_version")
            .and_then(serde_json::Value::as_u64)
            .and_then(|version| u32::try_from(version).ok()),
    })
}

fn database_check(path: &Path, health: Option<&DaemonHealth>) -> DoctorCheck {
    let Ok(metadata) = fs::metadata(path) else {
        return check(
            "database_schema",
            CheckStatus::Fail,
            format!("Database file does not exist at {}.", path.display()),
            Some(
                "Run `contextctl doctor --repair` to start contextd and initialize storage."
                    .to_string(),
            ),
        );
    };
    if !metadata.is_file() {
        return check(
            "database_schema",
            CheckStatus::Fail,
            format!("Database path is not a regular file: {}.", path.display()),
            Some("Set CONTEXT_DB_PATH to a private regular-file location.".to_string()),
        );
    }
    match health {
        Some(health) if health.schema_version > 0 => check(
            "database_schema",
            CheckStatus::Pass,
            format!(
                "Database {} is open at schema v{}.",
                path.display(),
                health.schema_version
            ),
            None,
        ),
        _ => check(
            "database_schema",
            CheckStatus::Fail,
            format!(
                "Database exists at {}, but its schema could not be verified.",
                path.display()
            ),
            Some("Restore daemon reachability and rerun `contextctl doctor`.".to_string()),
        ),
    }
}

fn review_policy_check(
    health: &DaemonHealth,
    result: Result<ReviewPolicy, ClientError>,
) -> DoctorCheck {
    match result {
        Ok(policy) => check(
            "review_policy",
            CheckStatus::Pass,
            format!(
                "Review policy is {} at revision {}.",
                policy.mode, policy.revision_no
            ),
            None,
        ),
        Err(error) if legacy_or_unknown_daemon(health) && unsupported_review_policy_rpc(&error) => {
            check(
                "review_policy",
                CheckStatus::Warn,
                "Legacy contextd does not expose get_review_policy; review policy is unknown."
                    .to_string(),
                Some(
                    "Upgrade contextd to the same version as contextctl to inspect review policy."
                        .to_string(),
                ),
            )
        }
        Err(error) => check(
            "review_policy",
            CheckStatus::Fail,
            format!("Review policy could not be read: {}", safe_message(&error)),
            Some("Restart or upgrade contextd and rerun `contextctl doctor`.".to_string()),
        ),
    }
}

fn legacy_or_unknown_daemon(health: &DaemonHealth) -> bool {
    health.api_version != Some(CONTEXT_API_VERSION)
        || health
            .component_version
            .as_deref()
            .and_then(normalized_component_version)
            .is_none_or(|version| version != env!("CARGO_PKG_VERSION"))
}

fn unsupported_review_policy_rpc(error: &ClientError) -> bool {
    match error {
        ClientError::Remote(message) => {
            let message = message.to_ascii_lowercase();
            message.contains("unknown method")
                || message.contains("method not found")
                || message.contains("-32601")
        }
        _ => false,
    }
}

fn component_version_check(
    id: &str,
    component: &str,
    expected: &str,
    observed: Option<&str>,
    available: bool,
) -> DoctorCheck {
    if !available {
        return check(
            id,
            CheckStatus::Fail,
            format!("{component} version cannot be compared because the component is unavailable."),
            Some(format!(
                "Install {component} version {expected} from the same Universal Context Manager release."
            )),
        );
    }

    let Some(observed) = observed.and_then(normalized_component_version) else {
        return check(
            id,
            CheckStatus::Warn,
            format!(
                "{component} did not report a component version; compatibility with contextctl {expected} is unknown."
            ),
            Some(
                "Older components remain usable when their protocol is compatible, but install matching versioned binaries to clear this degraded check."
                    .to_string(),
            ),
        );
    };
    if observed == expected {
        check(
            id,
            CheckStatus::Pass,
            format!("{component} {observed} matches contextctl {expected}."),
            None,
        )
    } else {
        check(
            id,
            CheckStatus::Incompatible,
            format!(
                "Incompatible component version: {component} is {observed}, but contextctl is {expected}."
            ),
            Some(format!(
                "Install {component} {expected} from the same release and restart it before retrying."
            )),
        )
    }
}

fn api_version_check(expected: u32, observed: Option<u32>, available: bool) -> DoctorCheck {
    if !available {
        return check(
            "api_version_contextd_daemon",
            CheckStatus::Fail,
            "Context API version cannot be compared because contextd is unavailable.".to_string(),
            Some("Restore daemon reachability and rerun `contextctl doctor`.".to_string()),
        );
    }
    match observed {
        None => check(
            "api_version_contextd_daemon",
            CheckStatus::Warn,
            format!(
                "Running contextd did not report an API version; compatibility with API v{expected} is unknown."
            ),
            Some(
                "Legacy daemons remain usable when compatible, but upgrade contextd to verify the API contract."
                    .to_string(),
            ),
        ),
        Some(observed) if observed == expected => check(
            "api_version_contextd_daemon",
            CheckStatus::Pass,
            format!("Running contextd API v{observed} matches contextctl API v{expected}."),
            None,
        ),
        Some(observed) => check(
            "api_version_contextd_daemon",
            CheckStatus::Incompatible,
            format!(
                "Incompatible Context API version: running contextd reports v{observed}, but contextctl requires v{expected}."
            ),
            Some(
                "Install and restart contextd from the same release as contextctl before retrying."
                    .to_string(),
            ),
        ),
    }
}

fn permission_checks(
    data_dir: &Path,
    db_path: &Path,
    socket_path: &Path,
    spool_dir: &Path,
) -> Vec<DoctorCheck> {
    vec![
        permission_check("permissions_data", data_dir, true),
        permission_check("permissions_database", db_path, false),
        permission_check("permissions_socket", socket_path, false),
        permission_check("permissions_spool", spool_dir, true),
    ]
}

fn permission_check(id: &str, path: &Path, directory: bool) -> DoctorCheck {
    let Ok(metadata) = fs::metadata(path) else {
        return check(
            id,
            CheckStatus::Warn,
            format!("{} does not exist yet.", path.display()),
            Some("Run `contextctl doctor --repair` to ensure runtime paths.".to_string()),
        );
    };
    if metadata.is_dir() != directory {
        return check(
            id,
            CheckStatus::Fail,
            format!("{} has the wrong file type.", path.display()),
            Some("Choose a valid private runtime path and rerun doctor.".to_string()),
        );
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = metadata.permissions().mode() & 0o777;
        if mode & 0o077 != 0 {
            return check(
                id,
                CheckStatus::Warn,
                format!(
                    "{} has mode {mode:03o}, which is not private.",
                    path.display()
                ),
                Some(format!(
                    "Review ownership, then restrict access (for example: chmod {} '{}').",
                    if directory { "700" } else { "600" },
                    path.display()
                )),
            );
        }
        check(
            id,
            CheckStatus::Pass,
            format!("{} has private mode {mode:03o}.", path.display()),
            None,
        )
    }
    #[cfg(not(unix))]
    {
        check(
            id,
            CheckStatus::Pass,
            format!("{} exists with the expected file type.", path.display()),
            None,
        )
    }
}

fn spool_backlog(path: &Path) -> anyhow::Result<usize> {
    if !path.exists() {
        return Ok(0);
    }
    let mut count = 0;
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let metadata = fs::symlink_metadata(entry.path())?;
        if metadata.file_type().is_file()
            && entry.path().extension().and_then(|value| value.to_str()) == Some("json")
        {
            count += 1;
        }
    }
    Ok(count)
}

fn check(
    id: impl Into<String>,
    status: CheckStatus,
    message: String,
    remediation: Option<String>,
) -> DoctorCheck {
    DoctorCheck {
        id: id.into(),
        status,
        message,
        remediation,
    }
}

fn overall_state(checks: &[DoctorCheck]) -> DoctorState {
    if checks
        .iter()
        .any(|check| matches!(check.status, CheckStatus::Fail | CheckStatus::Incompatible))
    {
        DoctorState::Failed
    } else if checks.iter().any(|check| check.status == CheckStatus::Warn) {
        DoctorState::Degraded
    } else {
        DoctorState::Healthy
    }
}

fn binary_version(binaries: &[BinaryStatus], name: &str) -> Option<String> {
    binaries
        .iter()
        .find(|binary| binary.name == name)
        .and_then(|binary| binary.version.clone())
}

fn version_state<'a>(
    expected: &str,
    observed: impl IntoIterator<Item = Option<&'a str>>,
) -> VersionState {
    let mut unknown = false;
    for version in observed {
        match version.and_then(normalized_component_version) {
            Some(version) if version != expected => return VersionState::Incompatible,
            Some(_) => {}
            None => unknown = true,
        }
    }
    if unknown {
        VersionState::Unknown
    } else {
        VersionState::Compatible
    }
}

fn api_version_state(expected: u32, observed: Option<u32>) -> VersionState {
    match observed {
        Some(observed) if observed == expected => VersionState::Compatible,
        Some(_) => VersionState::Incompatible,
        None => VersionState::Unknown,
    }
}

fn combine_version_states(component: VersionState, api: VersionState) -> VersionState {
    if component == VersionState::Incompatible || api == VersionState::Incompatible {
        VersionState::Incompatible
    } else if component == VersionState::Unknown || api == VersionState::Unknown {
        VersionState::Unknown
    } else {
        VersionState::Compatible
    }
}

fn home_directory() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn doctor_report_has_stable_structured_serialization() {
        let report = DoctorReport {
            overall: DoctorState::Degraded,
            versions: DoctorVersions {
                state: VersionState::Unknown,
                contextctl: "0.1.0".to_string(),
                contextd_binary: None,
                context_mcp_binary: None,
                running_daemon: None,
                mcp_initialize_server: None,
                api: DoctorApiCompatibility {
                    state: VersionState::Unknown,
                    expected: CONTEXT_API_VERSION,
                    running_daemon: None,
                },
            },
            paths: DoctorPaths {
                data_dir: "/data".to_string(),
                db_path: "/data/context.db".to_string(),
                socket_path: "/data/contextd.sock".to_string(),
                spool_dir: "/data/spool".to_string(),
            },
            binaries: Vec::new(),
            adapters: Vec::new(),
            checks: vec![DoctorCheck {
                id: "daemon_reachable".to_string(),
                status: CheckStatus::Warn,
                message: "not running".to_string(),
                remediation: Some("start it".to_string()),
            }],
            repairs: Vec::new(),
        };
        let value = serde_json::to_value(report).expect("serialize");
        assert_eq!(value["overall"], "degraded");
        assert_eq!(value["versions"]["state"], "unknown");
        assert!(value["versions"]["running_daemon"].is_null());
        assert_eq!(value["versions"]["api"]["expected"], CONTEXT_API_VERSION);
        assert_eq!(value["versions"]["api"]["state"], "unknown");
        assert_eq!(value["checks"][0]["status"], "warn");
        assert_eq!(value["checks"][0]["remediation"], "start it");
    }

    #[test]
    fn component_version_checks_reject_skew_and_degrade_unknown_versions() {
        let compatible =
            component_version_check("version_contextd", "contextd", "1.2.3", Some("1.2.3"), true);
        assert_eq!(compatible.status, CheckStatus::Pass);

        let incompatible =
            component_version_check("version_contextd", "contextd", "1.2.3", Some("1.3.0"), true);
        assert_eq!(incompatible.status, CheckStatus::Incompatible);
        assert!(
            incompatible
                .message
                .contains("Incompatible component version")
        );

        let legacy = component_version_check("version_contextd", "contextd", "1.2.3", None, true);
        assert_eq!(legacy.status, CheckStatus::Warn);
        assert!(legacy.message.contains("unknown"));
    }

    #[test]
    fn incompatible_check_serializes_explicitly() {
        let check =
            component_version_check("version_mcp", "context-mcp", "1.2.3", Some("2.0.0"), true);
        let value = serde_json::to_value(check).expect("serialize");
        assert_eq!(value["status"], "incompatible");
    }

    #[test]
    fn incompatible_version_forces_failed_overall_state() {
        let check =
            component_version_check("version_mcp", "context-mcp", "1.2.3", Some("2.0.0"), true);
        assert_eq!(overall_state(&[check]), DoctorState::Failed);
    }

    #[test]
    fn aggregate_version_state_distinguishes_unknown_and_skew() {
        assert_eq!(
            version_state("1.2.3", [Some("1.2.3"), Some("1.2.3")]),
            VersionState::Compatible
        );
        assert_eq!(
            version_state("1.2.3", [Some("1.2.3"), None]),
            VersionState::Unknown
        );
        assert_eq!(
            version_state("1.2.3", [Some("1.2.3"), Some("2.0.0")]),
            VersionState::Incompatible
        );
    }

    #[test]
    fn old_daemon_health_remains_reachable_with_unknown_version() {
        let health = daemon_health_from_value(json!({
            "schema_version": 5,
            "packs": 0,
            "entries": 0,
            "reviews": 0,
            "runs": 0
        }))
        .expect("legacy health");
        assert_eq!(health.schema_version, 5);
        assert_eq!(health.component_version, None);
        assert_eq!(health.api_version, None);
    }

    #[test]
    fn legacy_daemon_without_policy_rpc_is_degraded_not_failed() {
        let health = DaemonHealth {
            schema_version: 4,
            component_version: None,
            api_version: None,
        };
        let check = review_policy_check(
            &health,
            Err(ClientError::Remote(
                "unknown method: get_review_policy (-32000)".to_string(),
            )),
        );
        assert_eq!(check.status, CheckStatus::Warn);
        assert!(check.message.contains("unknown"));

        let versioned_legacy = DaemonHealth {
            schema_version: 4,
            component_version: Some("0.0.9".to_string()),
            api_version: None,
        };
        let check = review_policy_check(
            &versioned_legacy,
            Err(ClientError::Remote(
                "method not found: get_review_policy (-32601)".to_string(),
            )),
        );
        assert_eq!(check.status, CheckStatus::Warn);
    }

    #[test]
    fn current_daemon_policy_failure_still_fails() {
        let health = DaemonHealth {
            schema_version: 5,
            component_version: Some(env!("CARGO_PKG_VERSION").to_string()),
            api_version: Some(CONTEXT_API_VERSION),
        };
        let check = review_policy_check(
            &health,
            Err(ClientError::Remote(
                "unknown method: get_review_policy (-32000)".to_string(),
            )),
        );
        assert_eq!(check.status, CheckStatus::Fail);
    }

    #[test]
    fn api_version_match_missing_and_mismatch_have_explicit_states() {
        let health = daemon_health_from_value(json!({
            "component_version": env!("CARGO_PKG_VERSION"),
            "api_version": CONTEXT_API_VERSION,
            "schema_version": 5
        }))
        .expect("current health");
        assert_eq!(health.api_version, Some(CONTEXT_API_VERSION));

        let matching = api_version_check(CONTEXT_API_VERSION, Some(CONTEXT_API_VERSION), true);
        assert_eq!(matching.status, CheckStatus::Pass);

        let legacy = api_version_check(CONTEXT_API_VERSION, None, true);
        assert_eq!(legacy.status, CheckStatus::Warn);
        assert_eq!(
            api_version_state(CONTEXT_API_VERSION, None),
            VersionState::Unknown
        );

        let incompatible =
            api_version_check(CONTEXT_API_VERSION, Some(CONTEXT_API_VERSION + 1), true);
        assert_eq!(incompatible.status, CheckStatus::Incompatible);
        assert_eq!(
            api_version_state(CONTEXT_API_VERSION, Some(CONTEXT_API_VERSION + 1)),
            VersionState::Incompatible
        );
        assert_eq!(overall_state(&[incompatible]), DoctorState::Failed);
    }

    #[test]
    fn api_mismatch_overrides_matching_package_and_schema_versions() {
        let health = DaemonHealth {
            schema_version: 5,
            component_version: Some(env!("CARGO_PKG_VERSION").to_string()),
            api_version: Some(CONTEXT_API_VERSION + 1),
        };
        assert_eq!(health.schema_version, 5);
        assert_eq!(
            health.component_version.as_deref(),
            Some(env!("CARGO_PKG_VERSION"))
        );
        let api_check = api_version_check(CONTEXT_API_VERSION, health.api_version, true);
        assert_eq!(api_check.status, CheckStatus::Incompatible);
        assert_eq!(
            combine_version_states(VersionState::Compatible, VersionState::Incompatible),
            VersionState::Incompatible
        );
    }
}
