use clap::{Parser, Subcommand};
use context_client::{ClientConfig, ContextClient};
use context_core::{
    CommitWorkRequest, ComposeRequest, ScopeKind, SearchRequest, normalize_project_scope_id,
};
use serde_json::{Value, json};
use std::io::{self, BufRead, Write};
use tracing_subscriber::EnvFilter;

#[derive(Debug, Parser)]
#[command(
    name = "context-mcp",
    version,
    about = "Serve Universal Context Manager tools over MCP stdio"
)]
struct Args {
    /// Harness identity used for diagnostics and provenance defaults.
    #[arg(long, global = true, default_value = "generic")]
    adapter: String,

    /// Use stdin/stdout as the MCP transport. This is the only v1 transport.
    #[arg(long, global = true, default_value_t = false)]
    stdio: bool,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Start the MCP server.
    Serve,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let _transport_is_stdio =
        args.stdio || args.command.is_none() || matches!(args.command, Some(Command::Serve));
    let _adapter = args.adapter;
    init_tracing();
    let client = ContextClient::new(ClientConfig::discover()?);
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut writer = stdout.lock();
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let message: Value = match serde_json::from_str(&line) {
            Ok(value) => value,
            Err(err) => {
                write_json(
                    &mut writer,
                    &jsonrpc_error(Value::Null, -32700, format!("parse error: {err}")),
                )?;
                continue;
            }
        };
        let method = message
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or_default();
        // JSON-RPC notifications have no `id` and must never receive a
        // response. MCP currently uses this for `notifications/initialized`
        // and may add other lifecycle notifications over time.
        if message.get("id").is_none() {
            continue;
        }
        let id = message.get("id").cloned().unwrap_or(Value::Null);
        let response = match method {
            "initialize" => {
                let protocol = message
                    .get("params")
                    .and_then(|params| params.get("protocolVersion"))
                    .cloned()
                    .unwrap_or_else(|| json!("2025-03-26"));
                jsonrpc_result(
                    id,
                    json!({
                        "protocolVersion": protocol,
                        "capabilities": { "tools": {} },
                        "serverInfo": {
                            "name": "context-mcp",
                            "version": env!("CARGO_PKG_VERSION")
                        }
                    }),
                )
            }
            "tools/list" => jsonrpc_result(id, json!({ "tools": tool_definitions() })),
            "tools/call" => match tool_call(
                &client,
                message.get("params").cloned().unwrap_or(Value::Null),
            ) {
                Ok(result) => jsonrpc_result(id, result),
                Err(err) => jsonrpc_error(id, -32000, err.to_string()),
            },
            "ping" => jsonrpc_result(id, json!({})),
            other => jsonrpc_error(id, -32601, format!("unknown method: {other}")),
        };
        write_json(&mut writer, &response)?;
    }
    Ok(())
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("error"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .try_init();
}

fn tool_definitions() -> Vec<Value> {
    vec![
        json!({
            "name": "compose_context",
            "description": "Load the active layered global/project/task context at the start of work. Returns both rendered Markdown and provenance-preserving structured sections.",
            "inputSchema": {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "project_scope_id": { "type": ["string", "null"] },
                    "task_scope_id": { "type": ["string", "null"] },
                    "include_archived": { "type": "boolean", "default": false }
                }
            }
        }),
        json!({
            "name": "search_context",
            "description": "Search active durable context on demand with FTS5 across the applicable global/project/task layers.",
            "inputSchema": {
                "type": "object",
                "additionalProperties": false,
                "required": ["query"],
                "properties": {
                    "query": { "type": "string" },
                    "project_scope_id": { "type": ["string", "null"] },
                    "task_scope_id": { "type": ["string", "null"] },
                    "limit": { "type": "integer", "default": 20 }
                }
            }
        }),
        json!({
            "name": "commit_work",
            "description": "Call exactly once after successful durable work to submit concise learned facts, decisions, conventions, or handoff state. Project/task changes auto-apply when safe; global, conflicting, or locked changes await human review. Never submit raw transcripts or secrets. request_id must be stable for retries.",
            "inputSchema": {
                "type": "object",
                "additionalProperties": false,
                "required": ["request_id", "actor", "proposals"],
                "properties": {
                    "request_id": {
                        "type": "string",
                        "description": "Stable idempotency key reused if this submission is retried."
                    },
                    "actor": {
                        "type": "string",
                        "description": "Harness or agent identity responsible for this proposal."
                    },
                    "run": {
                        "type": ["object", "null"],
                        "additionalProperties": false,
                        "required": ["source"],
                        "properties": {
                            "id": { "type": ["string", "null"] },
                            "project_scope_id": { "type": ["string", "null"] },
                            "task_scope_id": { "type": ["string", "null"] },
                            "source": { "type": "string" },
                            "metadata": { "type": "object", "default": {} }
                        }
                    },
                    "proposals": {
                        "type": "array",
                        "minItems": 1,
                        "items": {
                            "type": "object",
                            "additionalProperties": false,
                            "required": ["scope", "entry"],
                            "properties": {
                                "scope": {
                                    "type": "object",
                                    "additionalProperties": false,
                                    "required": ["kind", "id"],
                                    "properties": {
                                        "kind": { "type": "string", "enum": ["global", "project", "task"] },
                                        "id": { "type": "string" }
                                    }
                                },
                                "pack_name": { "type": "string", "default": "main" },
                                "entry": {
                                    "type": "object",
                                    "required": ["key", "kind", "format"],
                                    "properties": {
                                        "key": { "type": "string" },
                                        "title": { "type": ["string", "null"] },
                                        "kind": { "type": "string" },
                                        "format": { "type": "string", "enum": ["markdown", "json"] },
                                        "body": { "type": "string", "description": "Required when format is markdown." },
                                        "value": { "description": "Required when format is json." },
                                        "tags": { "type": "array", "items": { "type": "string" }, "default": [] },
                                        "metadata": { "type": "object", "default": {} },
                                        "locked": { "type": "boolean", "default": false },
                                        "provenance": { "type": ["object", "null"] }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }),
    ]
}

fn tool_call(client: &ContextClient, params: Value) -> anyhow::Result<Value> {
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("missing tool name"))?;
    let arguments = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let structured = match name {
        "compose_context" => {
            let request =
                normalize_compose_request(serde_json::from_value::<ComposeRequest>(arguments)?)?;
            serde_json::to_value(client.compose_context(request)?)?
        }
        "search_context" => {
            let request =
                normalize_search_request(serde_json::from_value::<SearchRequest>(arguments)?)?;
            serde_json::to_value(client.search_context(request)?)?
        }
        "commit_work" => {
            let request =
                normalize_commit_request(serde_json::from_value::<CommitWorkRequest>(arguments)?)?;
            serde_json::to_value(client.commit_work(request)?)?
        }
        other => return Err(anyhow::anyhow!("unknown tool: {other}")),
    };
    Ok(json!({
        "content": [{
            "type": "text",
            "text": serde_json::to_string_pretty(&structured)?
        }],
        "structuredContent": structured,
        "isError": false
    }))
}

fn normalize_compose_request(mut request: ComposeRequest) -> anyhow::Result<ComposeRequest> {
    normalize_optional_project_scope_id(&mut request.project_scope_id)?;
    Ok(request)
}

fn normalize_search_request(mut request: SearchRequest) -> anyhow::Result<SearchRequest> {
    normalize_optional_project_scope_id(&mut request.project_scope_id)?;
    Ok(request)
}

fn normalize_commit_request(mut request: CommitWorkRequest) -> anyhow::Result<CommitWorkRequest> {
    if let Some(run) = request.run.as_mut() {
        normalize_optional_project_scope_id(&mut run.project_scope_id)?;
    }
    for proposal in &mut request.proposals {
        if proposal.scope.kind == ScopeKind::Project {
            proposal.scope.id = normalize_project_scope_id(&proposal.scope.id)?;
        }
    }
    Ok(request)
}

fn normalize_optional_project_scope_id(
    project_scope_id: &mut Option<String>,
) -> anyhow::Result<()> {
    if let Some(value) = project_scope_id {
        *value = normalize_project_scope_id(value)?;
    }
    Ok(())
}

fn jsonrpc_result(id: Value, result: Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result,
    })
}

fn jsonrpc_error(id: Value, code: i64, message: String) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": code,
            "message": message,
        }
    })
}

fn write_json(writer: &mut impl Write, value: &Value) -> anyhow::Result<()> {
    writeln!(writer, "{}", serde_json::to_string(value)?)?;
    writer.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use context_client::ClientConfig;
    use context_core::{
        CommitProposal, CommitStatus, ComposeResponse, ContextPaths, EntryInput, EntryValue,
        IpcRequest, IpcResponse, RunInput, ScopeRef, SearchResponse,
    };
    use std::fs;
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixListener;
    use std::path::PathBuf;
    use std::process::Command as ProcessCommand;
    use std::thread;
    use tempfile::{TempDir, tempdir};

    fn temp_client() -> ContextClient {
        let dir = tempdir().expect("tempdir");
        let paths = ContextPaths {
            data_dir: dir.path().join("data"),
            db_path: dir.path().join("data/context.db"),
            socket_path: dir.path().join("data/contextd.sock"),
            spool_dir: dir.path().join("data/spool"),
        };
        ContextClient::new(ClientConfig {
            paths,
            autostart: false,
            contextd_bin: None,
            connect_timeout: std::time::Duration::from_secs(1),
            start_timeout: std::time::Duration::from_secs(1),
        })
    }

    #[cfg(unix)]
    fn git_project_paths() -> (TempDir, PathBuf, PathBuf, PathBuf) {
        use std::os::unix::fs::symlink;

        let dir = tempdir().expect("tempdir");
        let root = dir.path().join("repository");
        let nested = root.join("nested/project");
        fs::create_dir_all(&nested).expect("nested project");
        let status = ProcessCommand::new("git")
            .arg("-C")
            .arg(&root)
            .args(["init", "--quiet"])
            .status()
            .expect("git init");
        assert!(status.success());
        let alias = dir.path().join("project-alias");
        symlink(&nested, &alias).expect("project symlink");
        let canonical_root = root.canonicalize().expect("canonical root");
        (dir, canonical_root, nested, alias)
    }

    fn serve_request<F>(socket_path: std::path::PathBuf, handler: F) -> thread::JoinHandle<()>
    where
        F: FnOnce(IpcRequest) -> IpcResponse + Send + 'static,
    {
        let parent = socket_path.parent().expect("parent");
        fs::create_dir_all(parent).expect("mkdir");
        let _ = fs::remove_file(&socket_path);
        let listener = UnixListener::bind(&socket_path).expect("bind");
        thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut line = String::new();
            BufReader::new(stream.try_clone().expect("clone"))
                .read_line(&mut line)
                .expect("read");
            let request: IpcRequest = serde_json::from_str(line.trim()).expect("request");
            let response = handler(request);
            stream
                .write_all(
                    format!("{}\n", serde_json::to_string(&response).expect("serialize"))
                        .as_bytes(),
                )
                .expect("write");
            stream.flush().expect("flush");
        })
    }

    fn serve_once(socket_path: std::path::PathBuf) -> thread::JoinHandle<()> {
        serve_request(socket_path, |request| {
            assert_eq!(request.method, "compose_context");
            let compose: ComposeRequest =
                serde_json::from_value(request.params).expect("compose request");
            assert_eq!(compose.project_scope_id.as_deref(), Some("proj"));
            IpcResponse::success(
                request.id,
                serde_json::to_value(ComposeResponse {
                    generated_at: context_core::now_utc(),
                    sections: vec![],
                    rendered_markdown: "mcp markdown".to_string(),
                    metrics: context_core::ComposeMetrics::default(),
                    exclusions: vec![],
                    warnings: vec![],
                })
                .expect("value"),
            )
        })
    }

    #[test]
    fn lists_expected_tools() {
        let tools = tool_definitions();
        assert_eq!(tools.len(), 3);
        assert!(
            tools
                .iter()
                .any(|tool| tool.get("name") == Some(&json!("compose_context")))
        );
    }

    #[test]
    fn calls_compose_tool_via_client() {
        let client = temp_client();
        let join = serve_once(client.config().paths.socket_path.clone());
        let result = tool_call(
            &client,
            json!({
                "name": "compose_context",
                "arguments": {
                    "project_scope_id": "proj",
                    "task_scope_id": null,
                    "include_archived": false
                }
            }),
        )
        .expect("tool call");
        join.join().expect("join");
        assert_eq!(
            result
                .get("structuredContent")
                .and_then(|value| value.get("rendered_markdown"))
                .and_then(Value::as_str),
            Some("mcp markdown")
        );
    }

    #[cfg(unix)]
    #[test]
    fn canonicalizes_compose_and_search_project_paths_before_ipc() {
        let (_dir, canonical_root, _nested, alias) = git_project_paths();
        let client = temp_client();
        let expected_root = canonical_root.display().to_string();
        let task_id = alias.display().to_string();

        let compose_root = expected_root.clone();
        let compose_task = task_id.clone();
        let join = serve_request(client.config().paths.socket_path.clone(), move |request| {
            assert_eq!(request.method, "compose_context");
            let compose: ComposeRequest =
                serde_json::from_value(request.params).expect("compose request");
            assert_eq!(
                compose.project_scope_id.as_deref(),
                Some(compose_root.as_str())
            );
            assert_eq!(
                compose.task_scope_id.as_deref(),
                Some(compose_task.as_str())
            );
            IpcResponse::success(
                request.id,
                serde_json::to_value(ComposeResponse {
                    generated_at: context_core::now_utc(),
                    sections: vec![],
                    rendered_markdown: String::new(),
                    metrics: context_core::ComposeMetrics::default(),
                    exclusions: vec![],
                    warnings: vec![],
                })
                .expect("compose response"),
            )
        });
        tool_call(
            &client,
            json!({
                "name": "compose_context",
                "arguments": {
                    "project_scope_id": alias.display().to_string(),
                    "task_scope_id": task_id,
                    "include_archived": false,
                }
            }),
        )
        .expect("compose tool");
        join.join().expect("compose server");

        let search_root = expected_root;
        let join = serve_request(client.config().paths.socket_path.clone(), move |request| {
            assert_eq!(request.method, "search_context");
            let search: SearchRequest =
                serde_json::from_value(request.params).expect("search request");
            assert_eq!(
                search.project_scope_id.as_deref(),
                Some(search_root.as_str())
            );
            IpcResponse::success(
                request.id,
                serde_json::to_value(SearchResponse {
                    query: search.query,
                    hits: vec![],
                })
                .expect("search response"),
            )
        });
        tool_call(
            &client,
            json!({
                "name": "search_context",
                "arguments": {
                    "query": "rules",
                    "project_scope_id": alias.display().to_string(),
                    "task_scope_id": null,
                    "limit": 10,
                }
            }),
        )
        .expect("search tool");
        join.join().expect("search server");
    }

    #[cfg(unix)]
    #[test]
    fn canonicalizes_commit_run_and_all_project_proposals_consistently() {
        let (_dir, canonical_root, nested, alias) = git_project_paths();
        let client = temp_client();
        let expected_root = canonical_root.display().to_string();
        let task_id = alias.display().to_string();
        let entry = |key: &str| EntryInput {
            key: key.to_string(),
            title: None,
            kind: "note".to_string(),
            value: EntryValue::Markdown {
                body: "body".to_string(),
            },
            tags: vec![],
            metadata: json!({}),
            locked: false,
            provenance: None,
        };
        let request = CommitWorkRequest {
            request_id: "mcp-scope-normalization".to_string(),
            actor: "mcp-test".to_string(),
            run: Some(RunInput {
                id: Some("run-1".to_string()),
                project_scope_id: Some(nested.display().to_string()),
                task_scope_id: Some(task_id.clone()),
                source: "mcp".to_string(),
                metadata: json!({}),
            }),
            proposals: vec![
                CommitProposal {
                    scope: ScopeRef::normalized(ScopeKind::Project, alias.display().to_string())
                        .expect("project scope"),
                    pack_name: "main".to_string(),
                    entry: entry("project-one"),
                },
                CommitProposal {
                    scope: ScopeRef::normalized(ScopeKind::Project, nested.display().to_string())
                        .expect("project scope"),
                    pack_name: "main".to_string(),
                    entry: entry("project-two"),
                },
                CommitProposal {
                    scope: ScopeRef::normalized(ScopeKind::Task, task_id.clone())
                        .expect("task scope"),
                    pack_name: "main".to_string(),
                    entry: entry("task"),
                },
            ],
        };

        let expected_task = task_id;
        let join = serve_request(
            client.config().paths.socket_path.clone(),
            move |ipc_request| {
                assert_eq!(ipc_request.method, "commit_work");
                let request: CommitWorkRequest =
                    serde_json::from_value(ipc_request.params).expect("commit request");
                let run = request.run.as_ref().expect("run");
                assert_eq!(
                    run.project_scope_id.as_deref(),
                    Some(expected_root.as_str())
                );
                assert_eq!(run.task_scope_id.as_deref(), Some(expected_task.as_str()));
                assert_eq!(request.proposals[0].scope.id, expected_root);
                assert_eq!(request.proposals[1].scope.id, expected_root);
                assert_eq!(request.proposals[2].scope.kind, ScopeKind::Task);
                assert_eq!(request.proposals[2].scope.id, expected_task);
                IpcResponse::success(
                    ipc_request.id,
                    serde_json::to_value(context_core::CommitWorkResult {
                        request_id: request.request_id,
                        status: CommitStatus::Applied,
                        run_id: run.id.clone(),
                        items: vec![],
                        spooled: false,
                        spool_path: None,
                    })
                    .expect("commit response"),
                )
            },
        );
        tool_call(
            &client,
            json!({
                "name": "commit_work",
                "arguments": serde_json::to_value(request).expect("commit arguments"),
            }),
        )
        .expect("commit tool");
        join.join().expect("commit server");
    }

    #[test]
    fn parses_serve_stdio_compatibility_flags() {
        let args = Args::try_parse_from(["context-mcp", "serve", "--adapter", "codex", "--stdio"])
            .expect("args");
        assert_eq!(args.adapter, "codex");
        assert!(args.stdio);
        assert!(matches!(args.command, Some(Command::Serve)));
    }

    #[test]
    fn version_flag_is_available() {
        let error = Args::try_parse_from(["context-mcp", "--version"]).expect_err("version exits");
        assert_eq!(error.kind(), clap::error::ErrorKind::DisplayVersion);
        assert!(error.to_string().contains(env!("CARGO_PKG_VERSION")));
    }
}
