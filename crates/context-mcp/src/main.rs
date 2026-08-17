use clap::{Parser, Subcommand};
use context_client::{ClientConfig, ContextClient};
use context_core::{CommitWorkRequest, ComposeRequest, SearchRequest};
use serde_json::{Value, json};
use std::io::{self, BufRead, Write};
use tracing_subscriber::EnvFilter;

#[derive(Debug, Parser)]
#[command(
    name = "context-mcp",
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
        "compose_context" => serde_json::to_value(
            client.compose_context(serde_json::from_value::<ComposeRequest>(arguments)?)?,
        )?,
        "search_context" => serde_json::to_value(
            client.search_context(serde_json::from_value::<SearchRequest>(arguments)?)?,
        )?,
        "commit_work" => serde_json::to_value(
            client.commit_work(serde_json::from_value::<CommitWorkRequest>(arguments)?)?,
        )?,
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
    use context_core::{ComposeResponse, ContextPaths, IpcRequest, IpcResponse};
    use std::fs;
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixListener;
    use std::thread;
    use tempfile::tempdir;

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

    fn serve_once(socket_path: std::path::PathBuf) -> thread::JoinHandle<()> {
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
            assert_eq!(request.method, "compose_context");
            let response = IpcResponse::success(
                request.id,
                serde_json::to_value(ComposeResponse {
                    generated_at: context_core::now_utc(),
                    sections: vec![],
                    rendered_markdown: "mcp markdown".to_string(),
                })
                .expect("value"),
            );
            stream
                .write_all(
                    format!("{}\n", serde_json::to_string(&response).expect("serialize"))
                        .as_bytes(),
                )
                .expect("write");
            stream.flush().expect("flush");
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

    #[test]
    fn parses_serve_stdio_compatibility_flags() {
        let args = Args::try_parse_from(["context-mcp", "serve", "--adapter", "codex", "--stdio"])
            .expect("args");
        assert_eq!(args.adapter, "codex");
        assert!(args.stdio);
        assert!(matches!(args.command, Some(Command::Serve)));
    }
}
