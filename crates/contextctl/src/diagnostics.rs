use anyhow::{Context, Result, anyhow};
use clap::ValueEnum;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeSet;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum AdapterKind {
    Codex,
    #[value(name = "claude-code")]
    ClaudeCode,
}

impl AdapterKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::ClaudeCode => "claude-code",
        }
    }
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub(crate) struct AdapterStatus {
    pub adapter: AdapterKind,
    pub installed: bool,
    pub configured: bool,
    pub marker_paths: Vec<String>,
    pub issues: Vec<String>,
    pub next_commands: Vec<String>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub(crate) struct BinaryStatus {
    pub name: String,
    pub available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub(crate) struct McpHandshake {
    pub server_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server_version: Option<String>,
    pub tools: Vec<String>,
}

#[derive(Debug)]
struct PluginRuntimeInspection {
    valid: bool,
    issues: Vec<String>,
    repair_commands: Vec<String>,
}

pub(crate) fn inspect_adapter(
    adapter: AdapterKind,
    home: Option<&Path>,
    source_root: Option<&Path>,
) -> AdapterStatus {
    let Some(home) = home else {
        return AdapterStatus {
            adapter,
            installed: false,
            configured: false,
            marker_paths: Vec::new(),
            issues: vec![
                "Home directory is unavailable, so plugin markers cannot be checked.".to_string(),
            ],
            next_commands: adapter_install_commands(adapter, source_root),
        };
    };

    let mut marker_paths = match adapter {
        AdapterKind::Codex => {
            let plugins_root = home.join(".codex/plugins");
            valid_plugin_markers(&plugins_root, ".codex-plugin", "context-manager", 8)
                .into_iter()
                .filter(|marker| active_codex_marker(marker, &plugins_root))
                .collect()
        }
        AdapterKind::ClaudeCode => valid_plugin_markers(
            &home.join(".claude/plugins/cache"),
            ".claude-plugin",
            "context-manager",
            8,
        ),
    };

    let mut issues = Vec::new();
    let mut registry_paths = Vec::new();
    if adapter == AdapterKind::ClaudeCode {
        let registry = home.join(".claude/plugins/installed_plugins.json");
        match claude_registry_install_paths(&registry) {
            Ok(paths) => {
                registry_paths = paths;
                for install_path in &registry_paths {
                    let marker = install_path.join(".claude-plugin/plugin.json");
                    if valid_plugin_marker(&marker, "context-manager")
                        && !marker_paths.contains(&marker)
                    {
                        marker_paths.push(marker);
                    }
                }
            }
            Err(error) if registry.exists() => {
                issues.push(format!(
                    "Claude plugin registry could not be read: {}",
                    safe_message(&error)
                ));
            }
            Err(_) => {}
        }
    }

    marker_paths.sort();
    marker_paths.dedup();
    let installed = !marker_paths.is_empty();
    let runtime_inspections = marker_paths
        .iter()
        .filter_map(|marker| marker.parent().and_then(Path::parent))
        .map(|root| (root, inspect_plugin_runtime(root)))
        .collect::<Vec<_>>();
    let configured_roots = runtime_inspections
        .iter()
        .filter_map(|(root, inspection)| inspection.valid.then_some(*root))
        .collect::<Vec<_>>();
    let registry_configured = match adapter {
        AdapterKind::Codex => true,
        AdapterKind::ClaudeCode => configured_roots.iter().any(|root| {
            registry_paths
                .iter()
                .any(|registered| same_path(root, registered))
        }),
    };
    let configured = installed && !configured_roots.is_empty() && registry_configured;

    if !installed {
        issues.push(format!(
            "{} plugin marker was not found.",
            adapter_display_name(adapter)
        ));
    } else if configured_roots.is_empty() {
        for (_, inspection) in &runtime_inspections {
            issues.extend(inspection.issues.clone());
        }
        if issues.is_empty() {
            issues.push(
                "Plugin manifest exists, but bundled runtime markers are incomplete.".to_string(),
            );
        }
    } else if !registry_configured {
        issues.push(
            "Claude plugin files exist, but installed_plugins.json does not register that install path."
                .to_string(),
        );
    }

    let mut next_commands = Vec::new();
    if !configured {
        for (_, inspection) in &runtime_inspections {
            next_commands.extend(inspection.repair_commands.clone());
        }
        next_commands.extend(adapter_install_commands(adapter, source_root));
        let mut seen = BTreeSet::new();
        next_commands.retain(|command| seen.insert(command.clone()));
    }

    AdapterStatus {
        adapter,
        installed,
        configured,
        marker_paths: marker_paths
            .iter()
            .map(|path| path.display().to_string())
            .collect(),
        issues,
        next_commands,
    }
}

pub(crate) fn discover_source_root() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("CONTEXT_MANAGER_SOURCE_ROOT") {
        let path = PathBuf::from(path);
        if is_source_root(&path) {
            return Some(path);
        }
    }

    let manifest_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    if is_source_root(&manifest_root) {
        return manifest_root.canonicalize().ok().or(Some(manifest_root));
    }

    let mut candidates = Vec::new();
    if let Ok(executable) = std::env::current_exe() {
        candidates.extend(executable.ancestors().map(Path::to_path_buf));
    }
    if let Ok(cwd) = std::env::current_dir() {
        candidates.extend(cwd.ancestors().map(Path::to_path_buf));
    }
    candidates.into_iter().find(|path| is_source_root(path))
}

pub(crate) fn inspect_binary(name: &str, env_key: Option<&str>) -> BinaryStatus {
    let path = if name == "contextctl" {
        std::env::current_exe().ok()
    } else {
        discover_binary(name, env_key)
    };
    let Some(path) = path else {
        return BinaryStatus {
            name: name.to_string(),
            available: false,
            path: None,
            version: None,
            note: Some(format!(
                "{name} was not found via its override, CONTEXT_MANAGER_BIN_DIR, a contextctl sibling, ~/.local/bin, PATH, or the app bundle; install the matching binary or set {}.",
                env_key.unwrap_or("PATH")
            )),
        };
    };

    if name == "contextctl" {
        return BinaryStatus {
            name: name.to_string(),
            available: true,
            path: Some(path.display().to_string()),
            version: Some(env!("CARGO_PKG_VERSION").to_string()),
            note: None,
        };
    }

    let (version, note) = binary_version(&path);
    BinaryStatus {
        name: name.to_string(),
        available: true,
        path: Some(path.display().to_string()),
        version,
        note,
    }
}

pub(crate) fn discover_binary(name: &str, env_key: Option<&str>) -> Option<PathBuf> {
    if let Some(env_key) = env_key {
        if let Some(path) = non_empty_env_path(env_key) {
            return is_executable_file(&path).then_some(path);
        }
    }
    if name == "context-mcp" && env_key == Some("CONTEXT_MCP_BIN") {
        if let Some(path) = non_empty_env_path("CONTEXT_MANAGER_CONTEXT_MCP") {
            return is_executable_file(&path).then_some(path);
        }
    }

    if let Some(bin_dir) = non_empty_env_path("CONTEXT_MANAGER_BIN_DIR") {
        let candidate = bin_dir.join(name);
        if is_executable_file(&candidate) {
            return Some(candidate);
        }
    }

    for candidate in current_executable_candidates(name) {
        if is_executable_file(&candidate) {
            return Some(candidate);
        }
    }

    if let Some(home) = non_empty_env_path("HOME") {
        let candidate = home.join(".local/bin").join(name);
        if is_executable_file(&candidate) {
            return Some(candidate);
        }
    }

    if let Some(path) = find_on_path(name) {
        return Some(path);
    }

    let app_candidate = PathBuf::from(format!(
        "/Applications/Universal Context Manager.app/Contents/MacOS/{name}"
    ));
    if is_executable_file(&app_candidate) {
        return Some(app_candidate);
    }

    None
}

fn current_executable_candidates(name: &str) -> Vec<PathBuf> {
    let Ok(current) = std::env::current_exe() else {
        return Vec::new();
    };
    let Some(parent) = current.parent() else {
        return Vec::new();
    };
    let mut candidates = vec![parent.join(name)];
    if let Some(nested_build_dir) = parent.parent() {
        candidates.push(nested_build_dir.join(name));
    }
    candidates
}

pub(crate) fn run_mcp_handshake(binary: &Path) -> Result<McpHandshake> {
    let mut child = Command::new(binary)
        .args(["serve", "--adapter", "doctor", "--stdio"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("spawn {}", binary.display()))?;

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow!("context-mcp stdout was not available"))?;
    let (sender, receiver) = mpsc::channel();
    let _reader = thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            if sender.send(line).is_err() {
                break;
            }
        }
    });

    let result = (|| -> Result<McpHandshake> {
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow!("context-mcp stdin was not available"))?;
        for request in [
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"contextctl-doctor","version":"0.1.0"}}}"#,
            r#"{"jsonrpc":"2.0","method":"notifications/initialized","params":{}}"#,
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}"#,
        ] {
            writeln!(stdin, "{request}")?;
        }
        stdin.flush()?;
        drop(stdin);

        let initialize = receive_mcp_line(&receiver, "initialize")?;
        let tools = receive_mcp_line(&receiver, "tools/list")?;
        parse_mcp_handshake(&initialize, &tools)
    })();

    let _ = child.kill();
    let _ = child.wait();
    result
}

pub(crate) fn parse_mcp_handshake(initialize_line: &str, tools_line: &str) -> Result<McpHandshake> {
    let initialize: Value =
        serde_json::from_str(initialize_line).context("parse initialize response")?;
    if initialize.get("error").is_some() {
        return Err(anyhow!("context-mcp initialize returned an error"));
    }
    let result = initialize
        .get("result")
        .ok_or_else(|| anyhow!("initialize response is missing result"))?;
    let server_info = result
        .get("serverInfo")
        .ok_or_else(|| anyhow!("initialize response is missing serverInfo"))?;
    let server_name = server_info
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("initialize response is missing server name"))?
        .to_string();
    if server_name != "context-mcp" {
        return Err(anyhow!(
            "initialize response came from an unexpected server"
        ));
    }
    if !result
        .get("capabilities")
        .and_then(|value| value.get("tools"))
        .is_some_and(Value::is_object)
    {
        return Err(anyhow!(
            "initialize response does not advertise tools capability"
        ));
    }

    let tools_response: Value =
        serde_json::from_str(tools_line).context("parse tools/list response")?;
    if tools_response.get("error").is_some() {
        return Err(anyhow!("context-mcp tools/list returned an error"));
    }
    let tools = tools_response
        .get("result")
        .and_then(|value| value.get("tools"))
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("tools/list response is missing tools"))?
        .iter()
        .filter_map(|tool| tool.get("name").and_then(Value::as_str))
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    for required in ["compose_context", "search_context", "commit_work"] {
        if !tools.iter().any(|name| name == required) {
            return Err(anyhow!("tools/list is missing required UCM tools"));
        }
    }

    Ok(McpHandshake {
        server_name,
        server_version: server_info
            .get("version")
            .and_then(Value::as_str)
            .map(ToString::to_string),
        tools,
    })
}

pub(crate) fn safe_message(error: &dyn std::fmt::Display) -> String {
    let line = error
        .to_string()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if line.chars().count() <= 240 {
        line
    } else {
        let mut value = line.chars().take(239).collect::<String>();
        value.push('…');
        value
    }
}

pub(crate) fn normalized_component_version(raw: &str) -> Option<String> {
    raw.split_whitespace().rev().find_map(|token| {
        let token = token
            .trim_matches(|character: char| matches!(character, ',' | ';' | '(' | ')' | '[' | ']'));
        let token = token.strip_prefix('v').unwrap_or(token);
        let core = token
            .split_once('+')
            .map(|(value, _)| value)
            .unwrap_or(token)
            .split_once('-')
            .map(|(value, _)| value)
            .unwrap_or(token);
        let numeric_parts = core.split('.').collect::<Vec<_>>();
        if numeric_parts.len() == 3
            && numeric_parts.iter().all(|part| {
                !part.is_empty() && part.chars().all(|character| character.is_ascii_digit())
            })
            && token.chars().all(|character| {
                character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '+')
            })
        {
            Some(token.to_string())
        } else {
            None
        }
    })
}

pub(crate) fn health_report_component_version(value: &Value) -> Option<String> {
    [
        value.get("component_version"),
        value.get("daemon_version"),
        value.get("version"),
        value.pointer("/component/version"),
        value.pointer("/daemon/version"),
        value.pointer("/components/contextd"),
        value.pointer("/components/contextd/version"),
        value.pointer("/component_versions/contextd"),
    ]
    .into_iter()
    .flatten()
    .find_map(version_from_json)
}

fn receive_mcp_line(
    receiver: &mpsc::Receiver<std::io::Result<String>>,
    method: &str,
) -> Result<String> {
    receiver
        .recv_timeout(Duration::from_secs(3))
        .map_err(|_| anyhow!("timed out waiting for context-mcp {method} response"))?
        .with_context(|| format!("read context-mcp {method} response"))
}

fn valid_plugin_markers(
    root: &Path,
    marker_directory: &str,
    plugin_name: &str,
    max_depth: usize,
) -> Vec<PathBuf> {
    let mut found = Vec::new();
    walk_files(root, max_depth, &mut |path| {
        let is_marker = path.file_name().and_then(|name| name.to_str()) == Some("plugin.json")
            && path
                .parent()
                .and_then(Path::file_name)
                .and_then(|name| name.to_str())
                == Some(marker_directory);
        if is_marker && valid_plugin_marker(path, plugin_name) {
            found.push(path.to_path_buf());
        }
    });
    found
}

fn valid_plugin_marker(path: &Path, expected_name: &str) -> bool {
    let Ok(payload) = fs::read_to_string(path) else {
        return false;
    };
    serde_json::from_str::<Value>(&payload)
        .ok()
        .and_then(|value| {
            value
                .get("name")
                .and_then(Value::as_str)
                .map(ToString::to_string)
        })
        .is_some_and(|name| name == expected_name)
}

fn inspect_plugin_runtime(root: &Path) -> PluginRuntimeInspection {
    let mut issues = Vec::new();
    let mut repair_commands = Vec::new();
    let mcp_path = root.join(".mcp.json");
    let hooks_path = root.join("hooks/hooks.json");
    let mcp = match read_json(&mcp_path) {
        Ok(mcp) => Some(mcp),
        Err(error) => {
            issues.push(format!(
                "MCP configuration is missing or invalid at {}: {}",
                mcp_path.display(),
                safe_message(&error)
            ));
            None
        }
    };
    let hooks = match read_json(&hooks_path) {
        Ok(hooks) => Some(hooks),
        Err(error) => {
            issues.push(format!(
                "Hook configuration is missing or invalid at {}: {}",
                hooks_path.display(),
                safe_message(&error)
            ));
            None
        }
    };

    if let Some(mcp) = mcp.as_ref() {
        match mcp
            .get("mcpServers")
            .and_then(|value| value.get("context-mcp"))
        {
            Some(server) => validate_launcher_references(
                root,
                "MCP",
                "run-context-mcp.sh",
                server,
                &mut issues,
                &mut repair_commands,
            ),
            None => issues.push(format!(
                "{} does not define mcpServers.context-mcp.",
                mcp_path.display()
            )),
        }
    }

    if let Some(hooks) = hooks.as_ref() {
        match hooks.get("hooks") {
            Some(hook_events) => match hook_events.get("SessionStart") {
                Some(session_start) => {
                    validate_launcher_references(
                        root,
                        "hook",
                        "run-context-hook.sh",
                        session_start,
                        &mut issues,
                        &mut repair_commands,
                    );
                }
                None => issues.push(format!(
                    "{} does not define hooks.SessionStart.",
                    hooks_path.display()
                )),
            },
            None => {
                issues.push(format!(
                    "{} does not define hooks.SessionStart.",
                    hooks_path.display()
                ));
            }
        }
    }

    PluginRuntimeInspection {
        valid: issues.is_empty(),
        issues,
        repair_commands,
    }
}

fn validate_launcher_references(
    root: &Path,
    label: &str,
    launcher_name: &str,
    value: &Value,
    issues: &mut Vec<String>,
    repair_commands: &mut Vec<String>,
) {
    let references = launcher_references(value, launcher_name);
    if references.is_empty() {
        issues.push(format!(
            "The {label} configuration does not reference a bundled run-context launcher."
        ));
        return;
    }
    for reference in references {
        let Some(path) = resolve_plugin_launcher(root, &reference) else {
            issues.push(format!(
                "The {label} launcher reference cannot be resolved safely: {reference}"
            ));
            continue;
        };
        let metadata = fs::metadata(&path);
        match metadata {
            Err(_) => issues.push(format!(
                "Referenced {label} launcher is missing: {}",
                path.display()
            )),
            Ok(metadata) if !metadata.is_file() => issues.push(format!(
                "Referenced {label} launcher is not a regular file: {}",
                path.display()
            )),
            Ok(_) if !is_executable_file(&path) => {
                issues.push(format!(
                    "Referenced {label} launcher is not executable: {}",
                    path.display()
                ));
                repair_commands.push(format!("chmod u+x {}", shell_quote_path(&path)));
            }
            Ok(_) => {}
        }
    }
}

fn launcher_references(value: &Value, launcher_name: &str) -> BTreeSet<String> {
    let mut references = BTreeSet::new();
    collect_launcher_references(value, launcher_name, &mut references);
    references
}

fn collect_launcher_references(
    value: &Value,
    launcher_name: &str,
    references: &mut BTreeSet<String>,
) {
    match value {
        Value::String(value) => {
            let mut remaining = value.as_str();
            while let Some(index) = remaining.find(launcher_name) {
                let end = index + launcher_name.len();
                let prefix = &remaining[..index];
                let start = prefix
                    .rfind(|character: char| {
                        character.is_whitespace() || matches!(character, '"' | '\'' | '=' | ',')
                    })
                    .map(|index| index + 1)
                    .unwrap_or(0);
                references.insert(remaining[start..end].to_string());
                remaining = &remaining[end..];
            }
        }
        Value::Array(values) => {
            for value in values {
                collect_launcher_references(value, launcher_name, references);
            }
        }
        Value::Object(values) => {
            for value in values.values() {
                collect_launcher_references(value, launcher_name, references);
            }
        }
        _ => {}
    }
}

fn resolve_plugin_launcher(root: &Path, reference: &str) -> Option<PathBuf> {
    let relative = [
        "${PLUGIN_ROOT}/",
        "$PLUGIN_ROOT/",
        "${CLAUDE_PLUGIN_ROOT}/",
        "$CLAUDE_PLUGIN_ROOT/",
        "./",
    ]
    .iter()
    .find_map(|prefix| reference.strip_prefix(prefix))
    .unwrap_or(reference);
    if relative.contains('$') {
        return None;
    }
    let path = PathBuf::from(relative);
    if path.is_absolute() {
        Some(path)
    } else {
        Some(root.join(path))
    }
}

fn claude_registry_install_paths(path: &Path) -> Result<Vec<PathBuf>> {
    let registry = read_json(path)?;
    let mut paths = Vec::new();
    if let Some(plugins) = registry.get("plugins").and_then(Value::as_object) {
        for (name, installs) in plugins {
            if !name.starts_with("context-manager@") {
                continue;
            }
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
    Ok(paths)
}

fn read_json(path: &Path) -> Result<Value> {
    let payload = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    serde_json::from_str(&payload).with_context(|| format!("parse {}", path.display()))
}

fn walk_files(root: &Path, max_depth: usize, visitor: &mut impl FnMut(&Path)) {
    if max_depth == 0 {
        return;
    }
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(metadata) = fs::symlink_metadata(&path) else {
            continue;
        };
        if metadata.file_type().is_symlink() {
            continue;
        }
        if metadata.is_file() {
            visitor(&path);
        } else if metadata.is_dir() {
            walk_files(&path, max_depth - 1, visitor);
        }
    }
}

fn adapter_install_commands(adapter: AdapterKind, source_root: Option<&Path>) -> Vec<String> {
    let root = source_root
        .map(shell_quote_path)
        .unwrap_or_else(|| "<UCM_SOURCE_CHECKOUT>".to_string());
    match adapter {
        AdapterKind::Codex => vec![
            format!("codex plugin marketplace add {root}"),
            "codex plugin add context-manager@universal-context-manager-local".to_string(),
        ],
        AdapterKind::ClaudeCode => vec![
            format!("claude plugin marketplace add {root}"),
            "claude plugin install context-manager@universal-context-manager-local".to_string(),
        ],
    }
}

fn adapter_display_name(adapter: AdapterKind) -> &'static str {
    match adapter {
        AdapterKind::Codex => "Codex",
        AdapterKind::ClaudeCode => "Claude Code",
    }
}

fn active_codex_marker(marker: &Path, plugins_root: &Path) -> bool {
    let Some(plugin_root) = marker.parent().and_then(Path::parent) else {
        return false;
    };
    let Ok(relative) = plugin_root.strip_prefix(plugins_root) else {
        return false;
    };
    let components = relative
        .components()
        .filter_map(|component| component.as_os_str().to_str())
        .collect::<Vec<_>>();
    if components.is_empty()
        || components
            .iter()
            .any(|component| component.starts_with('.') || component.contains("backup"))
    {
        return false;
    }
    components[0] == "context-manager"
        || (components[0] == "cache" && components.contains(&"context-manager"))
}

fn is_source_root(path: &Path) -> bool {
    path.join(".agents/plugins/marketplace.json").is_file()
        && path.join(".claude-plugin/marketplace.json").is_file()
}

fn shell_quote_path(path: &Path) -> String {
    let value = path.display().to_string().replace('\'', "'\"'\"'");
    format!("'{value}'")
}

fn is_executable_file(path: &Path) -> bool {
    let Ok(metadata) = fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

fn find_on_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|directory| directory.join(name))
        .find(|candidate| is_executable_file(candidate))
}

fn non_empty_env_path(key: &str) -> Option<PathBuf> {
    std::env::var_os(key)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

fn binary_version(path: &Path) -> (Option<String>, Option<String>) {
    let mut child = match Command::new(path)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(error) => {
            return (
                None,
                Some(format!("version probe failed: {}", safe_message(&error))),
            );
        }
    };
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(20)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return (
                    None,
                    Some(
                        "version probe timed out; binary is available but version is unknown."
                            .to_string(),
                    ),
                );
            }
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return (
                    None,
                    Some(format!("version probe failed: {}", safe_message(&error))),
                );
            }
        }
    }
    let output = match child.wait_with_output() {
        Ok(output) => output,
        Err(error) => {
            return (
                None,
                Some(format!("version probe failed: {}", safe_message(&error))),
            );
        }
    };
    if output.status.success() {
        let raw_version = String::from_utf8_lossy(&output.stdout)
            .lines()
            .find(|line| !line.trim().is_empty())
            .map(str::trim)
            .map(ToString::to_string);
        match raw_version
            .as_deref()
            .and_then(normalized_component_version)
        {
            Some(version) => (Some(version), None),
            None => (
                None,
                Some(
                    "binary returned a successful --version response, but no semantic version could be parsed."
                        .to_string(),
                ),
            ),
        }
    } else {
        (
            None,
            Some(
                "binary is available but does not expose a successful --version probe.".to_string(),
            ),
        )
    }
}

fn version_from_json(value: &Value) -> Option<String> {
    match value {
        Value::String(value) => normalized_component_version(value),
        Value::Object(object) => object
            .get("version")
            .or_else(|| object.get("component_version"))
            .and_then(version_from_json),
        _ => None,
    }
}

fn same_path(left: &Path, right: &Path) -> bool {
    match (left.canonicalize(), right.canonicalize()) {
        (Ok(left), Ok(right)) => left == right,
        _ => left == right,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::{Mutex, OnceLock};
    use tempfile::tempdir;

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    #[test]
    fn parses_real_mcp_initialize_and_tools_list_shape() {
        let initialize = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {
                "protocolVersion": "2025-03-26",
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "context-mcp", "version": "0.1.0" }
            }
        })
        .to_string();
        let tools = json!({
            "jsonrpc": "2.0",
            "id": 2,
            "result": {
                "tools": [
                    { "name": "compose_context" },
                    { "name": "search_context" },
                    { "name": "commit_work" }
                ]
            }
        })
        .to_string();

        let handshake = parse_mcp_handshake(&initialize, &tools).expect("handshake");
        assert_eq!(handshake.server_name, "context-mcp");
        assert_eq!(handshake.server_version.as_deref(), Some("0.1.0"));
        assert_eq!(handshake.tools.len(), 3);
    }

    #[test]
    fn normalizes_component_version_outputs() {
        assert_eq!(
            normalized_component_version("contextd 0.1.0"),
            Some("0.1.0".to_string())
        );
        assert_eq!(
            normalized_component_version("context-mcp v1.2.3-beta.1+build"),
            Some("1.2.3-beta.1+build".to_string())
        );
        assert_eq!(normalized_component_version("contextd unknown"), None);
    }

    #[test]
    fn extracts_new_and_legacy_health_version_shapes() {
        assert_eq!(
            health_report_component_version(&json!({
                "schema_version": 5,
                "component": { "name": "contextd", "version": "0.1.0" }
            })),
            Some("0.1.0".to_string())
        );
        assert_eq!(
            health_report_component_version(&json!({
                "schema_version": 5,
                "component_version": "contextd 0.2.0"
            })),
            Some("0.2.0".to_string())
        );
        assert_eq!(
            health_report_component_version(&json!({ "schema_version": 5 })),
            None
        );
    }

    #[test]
    fn binary_discovery_matches_shared_launcher_locations() {
        let _guard = env_lock().lock().expect("env lock");
        let root = tempdir().expect("root");
        let manager_bin = root.path().join("manager-bin");
        let home = root.path().join("home");
        fs::create_dir_all(&manager_bin).expect("manager bin");
        fs::create_dir_all(home.join(".local/bin")).expect("local bin");
        let name = format!("context-version-test-{}", std::process::id());
        let manager_binary = manager_bin.join(&name);
        let local_binary = home.join(".local/bin").join(&name);
        let mcp_alias_binary = root.path().join("context-mcp-alias");
        write_test_executable(&manager_binary);
        write_test_executable(&local_binary);
        write_test_executable(&mcp_alias_binary);

        let old_bin_dir = std::env::var_os("CONTEXT_MANAGER_BIN_DIR");
        let old_home = std::env::var_os("HOME");
        let old_mcp_bin = std::env::var_os("CONTEXT_MCP_BIN");
        let old_mcp_alias = std::env::var_os("CONTEXT_MANAGER_CONTEXT_MCP");
        unsafe {
            std::env::set_var("CONTEXT_MANAGER_BIN_DIR", &manager_bin);
            std::env::set_var("HOME", &home);
        }
        assert_eq!(
            discover_binary(&name, None).expect("manager binary"),
            manager_binary
        );
        unsafe {
            std::env::remove_var("CONTEXT_MANAGER_BIN_DIR");
        }
        assert_eq!(
            discover_binary(&name, None).expect("local binary"),
            local_binary
        );
        unsafe {
            std::env::remove_var("CONTEXT_MCP_BIN");
            std::env::set_var("CONTEXT_MANAGER_CONTEXT_MCP", &mcp_alias_binary);
        }
        assert_eq!(
            discover_binary("context-mcp", Some("CONTEXT_MCP_BIN")).expect("MCP alias"),
            mcp_alias_binary
        );
        unsafe {
            match old_bin_dir {
                Some(value) => std::env::set_var("CONTEXT_MANAGER_BIN_DIR", value),
                None => std::env::remove_var("CONTEXT_MANAGER_BIN_DIR"),
            }
            match old_home {
                Some(value) => std::env::set_var("HOME", value),
                None => std::env::remove_var("HOME"),
            }
            match old_mcp_bin {
                Some(value) => std::env::set_var("CONTEXT_MCP_BIN", value),
                None => std::env::remove_var("CONTEXT_MCP_BIN"),
            }
            match old_mcp_alias {
                Some(value) => std::env::set_var("CONTEXT_MANAGER_CONTEXT_MCP", value),
                None => std::env::remove_var("CONTEXT_MANAGER_CONTEXT_MCP"),
            }
        }
    }

    #[test]
    fn binary_discovery_prefers_contextctl_siblings_before_user_and_path_fallbacks() {
        let _guard = env_lock().lock().expect("env lock");
        let root = tempdir().expect("root");
        let home = root.path().join("home");
        let path_dir = root.path().join("path");
        fs::create_dir_all(home.join(".local/bin")).expect("local bin");
        fs::create_dir_all(&path_dir).expect("path bin");

        let name = format!("context-precedence-test-{}", std::process::id());
        let candidates = current_executable_candidates(&name);
        assert!(candidates.len() >= 2);
        let sibling = candidates[0].clone();
        let nested_build = candidates[1].clone();
        let local = home.join(".local/bin").join(&name);
        let path_binary = path_dir.join(&name);
        for path in [&sibling, &nested_build, &local, &path_binary] {
            write_test_executable(path);
        }

        let old_bin_dir = std::env::var_os("CONTEXT_MANAGER_BIN_DIR");
        let old_home = std::env::var_os("HOME");
        let old_path = std::env::var_os("PATH");
        let mut test_path_entries = vec![path_dir.clone()];
        if let Some(path) = old_path.as_ref() {
            test_path_entries.extend(std::env::split_paths(path));
        }
        let test_path = std::env::join_paths(test_path_entries).expect("test PATH");
        unsafe {
            std::env::remove_var("CONTEXT_MANAGER_BIN_DIR");
            std::env::set_var("HOME", &home);
            std::env::set_var("PATH", test_path);
        }

        assert_eq!(discover_binary(&name, None), Some(sibling.clone()));
        fs::remove_file(&sibling).expect("remove sibling");
        assert_eq!(discover_binary(&name, None), Some(nested_build.clone()));
        fs::remove_file(&nested_build).expect("remove nested");
        assert_eq!(discover_binary(&name, None), Some(local.clone()));
        fs::remove_file(&local).expect("remove local");
        assert_eq!(discover_binary(&name, None), Some(path_binary));

        unsafe {
            match old_bin_dir {
                Some(value) => std::env::set_var("CONTEXT_MANAGER_BIN_DIR", value),
                None => std::env::remove_var("CONTEXT_MANAGER_BIN_DIR"),
            }
            match old_home {
                Some(value) => std::env::set_var("HOME", value),
                None => std::env::remove_var("HOME"),
            }
            match old_path {
                Some(value) => std::env::set_var("PATH", value),
                None => std::env::remove_var("PATH"),
            }
        }
    }

    #[test]
    fn adapter_check_requires_real_runtime_markers() {
        let home = tempdir().expect("home");
        let plugin_root = home.path().join(".codex/plugins/context-manager");
        fs::create_dir_all(plugin_root.join(".codex-plugin")).expect("plugin directory");
        fs::write(
            plugin_root.join(".codex-plugin/plugin.json"),
            r#"{"name":"context-manager"}"#,
        )
        .expect("plugin marker");

        let incomplete = inspect_adapter(AdapterKind::Codex, Some(home.path()), None);
        assert!(incomplete.installed);
        assert!(!incomplete.configured);

        fs::create_dir_all(plugin_root.join("hooks")).expect("hooks");
        fs::create_dir_all(plugin_root.join("scripts")).expect("scripts");
        fs::write(
            plugin_root.join(".mcp.json"),
            r#"{"mcpServers":{"context-mcp":{"command":"./scripts/run-context-mcp.sh"}}}"#,
        )
        .expect("mcp marker");
        fs::write(
            plugin_root.join("hooks/hooks.json"),
            r#"{"hooks":{"SessionStart":[{"hooks":[{"type":"command","command":"sh \"${PLUGIN_ROOT}/scripts/run-context-hook.sh\" codex session-start"}]}]}}"#,
        )
        .expect("hooks marker");
        write_test_executable(&plugin_root.join("scripts/run-context-mcp.sh"));
        write_test_executable(&plugin_root.join("scripts/run-context-hook.sh"));

        let complete = inspect_adapter(AdapterKind::Codex, Some(home.path()), None);
        assert!(complete.installed);
        assert!(complete.configured);
        assert_eq!(complete.marker_paths.len(), 1);
    }

    #[test]
    fn adapter_check_reports_missing_and_non_executable_launchers() {
        let home = tempdir().expect("home");
        let plugin_root = home.path().join(".codex/plugins/context-manager");
        fs::create_dir_all(plugin_root.join(".codex-plugin")).expect("plugin directory");
        fs::create_dir_all(plugin_root.join("hooks")).expect("hooks");
        fs::create_dir_all(plugin_root.join("scripts")).expect("scripts");
        fs::write(
            plugin_root.join(".codex-plugin/plugin.json"),
            r#"{"name":"context-manager"}"#,
        )
        .expect("plugin marker");
        fs::write(
            plugin_root.join(".mcp.json"),
            r#"{"mcpServers":{"context-mcp":{"command":"./scripts/run-context-mcp.sh"}}}"#,
        )
        .expect("mcp marker");
        fs::write(
            plugin_root.join("hooks/hooks.json"),
            r#"{"hooks":{"SessionStart":[{"hooks":[{"type":"command","command":"sh","args":["${PLUGIN_ROOT}/scripts/run-context-hook.sh","codex","session-start"]}]}]}}"#,
        )
        .expect("hooks marker");
        write_test_executable(&plugin_root.join("scripts/run-context-hook.sh"));

        let missing = inspect_adapter(AdapterKind::Codex, Some(home.path()), None);
        assert!(!missing.configured);
        assert!(missing.issues.iter().any(|issue| {
            issue.contains("MCP launcher is missing")
                && issue.contains("scripts/run-context-mcp.sh")
        }));
        assert!(
            missing
                .next_commands
                .iter()
                .any(|command| command.contains("codex plugin add"))
        );

        fs::write(
            plugin_root.join("scripts/run-context-mcp.sh"),
            "#!/bin/sh\nexit 0\n",
        )
        .expect("non-executable launcher");
        let non_executable = inspect_adapter(AdapterKind::Codex, Some(home.path()), None);
        assert!(!non_executable.configured);
        assert!(non_executable.issues.iter().any(|issue| {
            issue.contains("MCP launcher is not executable")
                && issue.contains("scripts/run-context-mcp.sh")
        }));
        assert!(
            non_executable
                .next_commands
                .iter()
                .any(|command| command.starts_with("chmod u+x "))
        );

        write_test_executable(&plugin_root.join("scripts/run-context-mcp.sh"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(
                plugin_root.join("scripts/run-context-hook.sh"),
                fs::Permissions::from_mode(0o600),
            )
            .expect("remove hook execute bit");
        }
        let hook_non_executable = inspect_adapter(AdapterKind::Codex, Some(home.path()), None);
        assert!(!hook_non_executable.configured);
        assert!(hook_non_executable.issues.iter().any(|issue| {
            issue.contains("hook launcher is not executable")
                && issue.contains("scripts/run-context-hook.sh")
        }));
    }

    #[test]
    fn adapter_check_requires_session_start_launcher() {
        let home = tempdir().expect("home");
        let plugin_root = home.path().join(".codex/plugins/context-manager");
        fs::create_dir_all(plugin_root.join(".codex-plugin")).expect("plugin directory");
        fs::create_dir_all(plugin_root.join("hooks")).expect("hooks directory");
        fs::create_dir_all(plugin_root.join("scripts")).expect("scripts directory");
        fs::write(
            plugin_root.join(".codex-plugin/plugin.json"),
            r#"{"name":"context-manager"}"#,
        )
        .expect("plugin marker");
        fs::write(
            plugin_root.join(".mcp.json"),
            r#"{"mcpServers":{"context-mcp":{"command":"./scripts/run-context-mcp.sh"}}}"#,
        )
        .expect("mcp marker");
        fs::write(
            plugin_root.join("hooks/hooks.json"),
            r#"{"hooks":{"SessionStart":[],"SessionEnd":[{"hooks":[{"type":"command","command":"sh \"${PLUGIN_ROOT}/scripts/run-context-hook.sh\" codex session-end"}]}]}}"#,
        )
        .expect("hooks marker");
        write_test_executable(&plugin_root.join("scripts/run-context-mcp.sh"));
        write_test_executable(&plugin_root.join("scripts/run-context-hook.sh"));

        let status = inspect_adapter(AdapterKind::Codex, Some(home.path()), None);
        assert!(!status.configured);
        assert!(status.issues.iter().any(|issue| {
            issue.contains("hook configuration")
                && issue.contains("does not reference a bundled run-context launcher")
        }));
    }

    #[test]
    fn codex_backup_marker_does_not_count_as_installed() {
        let home = tempdir().expect("home");
        let plugin_root = home.path().join(".codex/plugins/context-manager.backup");
        fs::create_dir_all(plugin_root.join(".codex-plugin")).expect("plugin directory");
        fs::write(
            plugin_root.join(".codex-plugin/plugin.json"),
            r#"{"name":"context-manager"}"#,
        )
        .expect("plugin marker");
        fs::create_dir_all(plugin_root.join("hooks")).expect("hooks");
        fs::write(
            plugin_root.join(".mcp.json"),
            r#"{"mcpServers":{"context-mcp":{}}}"#,
        )
        .expect("mcp marker");
        fs::write(
            plugin_root.join("hooks/hooks.json"),
            r#"{"hooks":{"SessionStart":[]}}"#,
        )
        .expect("hooks marker");

        let status = inspect_adapter(AdapterKind::Codex, Some(home.path()), None);
        assert!(!status.installed);
        assert!(!status.configured);
    }

    fn write_test_executable(path: &Path) {
        fs::write(path, "#!/bin/sh\nexit 0\n").expect("write executable");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(path, fs::Permissions::from_mode(0o700)).expect("chmod");
        }
    }
}
