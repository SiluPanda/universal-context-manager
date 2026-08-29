use clap::Parser;
use context_core::{ContextPaths, ContextResult, ContextStore, IpcRequest, IpcResponse};
use fs2::FileExt;
use serde::{Serialize, de::DeserializeOwned};
use serde_json::Value;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;
use tracing::{error, info};
use tracing_subscriber::EnvFilter;

const MAX_IPC_FRAME_BYTES: usize = 8 * 1024 * 1024;
const COMPONENT_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, Parser)]
#[command(name = "contextd", version, about = "Universal Context Manager daemon")]
struct Args {
    #[arg(long, env = "CONTEXT_DB_PATH")]
    db: Option<PathBuf>,
    #[arg(long, env = "CONTEXT_SOCKET_PATH")]
    socket: Option<PathBuf>,
    #[arg(long, default_value_t = false)]
    quiet: bool,
}

#[derive(Debug)]
struct RuntimeGuard {
    _lock_file: File,
    socket_path: PathBuf,
    listener: UnixListener,
}

impl Drop for RuntimeGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.socket_path);
    }
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    init_tracing(args.quiet);
    let mut paths = ContextPaths::discover()?;
    if let Some(db) = args.db {
        paths.db_path = db;
    }
    if let Some(socket) = args.socket {
        paths.socket_path = socket;
    }
    paths.ensure_parent_dirs()?;
    let runtime = prepare_runtime(&paths)?;
    let store = Arc::new(ContextStore::open(&paths.db_path)?);
    info!(db = %paths.db_path.display(), socket = %paths.socket_path.display(), "contextd listening");
    for stream in runtime.listener.incoming() {
        match stream {
            Ok(stream) => {
                let store = Arc::clone(&store);
                thread::spawn(move || {
                    if let Err(err) = handle_client(stream, store) {
                        error!(?err, "client handling failed");
                    }
                });
            }
            Err(err) => error!(?err, "accept failed"),
        }
    }
    Ok(())
}

fn init_tracing(quiet: bool) {
    let filter = if quiet {
        EnvFilter::new("error")
    } else {
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"))
    };
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .try_init();
}

fn prepare_runtime(paths: &ContextPaths) -> anyhow::Result<RuntimeGuard> {
    let lock_file = acquire_runtime_lock(paths)?;
    let listener = bind_listener(&paths.socket_path)?;
    Ok(RuntimeGuard {
        _lock_file: lock_file,
        socket_path: paths.socket_path.clone(),
        listener,
    })
}

fn acquire_runtime_lock(paths: &ContextPaths) -> anyhow::Result<File> {
    let lock_path = paths.data_dir.join("contextd.lock");
    let lock_file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)?;
    fs::set_permissions(&lock_path, fs::Permissions::from_mode(0o600))?;
    lock_file.try_lock_exclusive().map_err(|err| {
        anyhow::anyhow!(
            "contextd is already running for {}: {err}",
            paths.data_dir.display()
        )
    })?;
    Ok(lock_file)
}

fn bind_listener(socket_path: &Path) -> anyhow::Result<UnixListener> {
    if socket_path.exists() {
        match UnixStream::connect(socket_path) {
            Ok(_) => {
                return Err(anyhow::anyhow!(
                    "contextd socket {} is already live",
                    socket_path.display()
                ));
            }
            Err(_) => fs::remove_file(socket_path)?,
        }
    }
    let listener = UnixListener::bind(socket_path)?;
    fs::set_permissions(socket_path, fs::Permissions::from_mode(0o600))?;
    Ok(listener)
}

fn handle_client(stream: UnixStream, store: Arc<ContextStore>) -> anyhow::Result<()> {
    let reader_stream = stream.try_clone()?;
    let mut reader = BufReader::new(reader_stream);
    let mut writer = stream;
    loop {
        let line = match read_bounded_line(&mut reader, MAX_IPC_FRAME_BYTES, "IPC request") {
            Ok(Some(line)) => line,
            Ok(None) => break,
            Err(err) => {
                let response = IpcResponse::error("invalid", -32600, err.to_string());
                writer.write_all(format!("{}\n", serde_json::to_string(&response)?).as_bytes())?;
                writer.flush()?;
                break;
            }
        };
        if line.trim().is_empty() {
            continue;
        }
        let response = match serde_json::from_str::<IpcRequest>(line.trim()) {
            Ok(request) => dispatch_request(request, &store),
            Err(err) => IpcResponse::error("invalid", -32700, format!("invalid request: {err}")),
        };
        writer.write_all(format!("{}\n", serde_json::to_string(&response)?).as_bytes())?;
        writer.flush()?;
    }
    Ok(())
}

fn read_bounded_line<R: BufRead>(
    reader: &mut R,
    max_bytes: usize,
    label: &str,
) -> std::io::Result<Option<String>> {
    let mut bytes = Vec::new();
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            if bytes.is_empty() {
                return Ok(None);
            }
            break;
        }

        if let Some(pos) = available.iter().position(|byte| *byte == b'\n') {
            let take = pos + 1;
            if bytes.len() + take > max_bytes {
                reader.consume(take);
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("{label} exceeds maximum size of {max_bytes} bytes"),
                ));
            }
            bytes.extend_from_slice(&available[..take]);
            reader.consume(take);
            break;
        }

        if bytes.len() + available.len() > max_bytes {
            let consume_len = available.len();
            reader.consume(consume_len);
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("{label} exceeds maximum size of {max_bytes} bytes"),
            ));
        }

        bytes.extend_from_slice(available);
        let consume_len = available.len();
        reader.consume(consume_len);
    }

    String::from_utf8(bytes)
        .map(Some)
        .map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidData, err))
}

fn dispatch_request(request: IpcRequest, store: &ContextStore) -> IpcResponse {
    let id = request.id.clone();
    let result = match request.method.as_str() {
        "ping" => serialize_core(store.health_with_component_version(COMPONENT_VERSION)),
        "stats" => serialize_core(store.stats()),
        "get_review_policy" => serialize_core(store.get_review_policy()),
        "set_review_policy" => {
            dispatch_owned(store, request.params, ContextStore::set_review_policy)
        }
        "compose_context" => dispatch_owned(store, request.params, ContextStore::compose_context),
        "search_context" => dispatch_owned(store, request.params, ContextStore::search_context),
        "create_pack" => dispatch_owned(store, request.params, ContextStore::create_pack),
        "update_pack" => dispatch_owned(store, request.params, ContextStore::update_pack),
        "list_packs" => serialize_core(store.list_packs()),
        "put_entry" => dispatch_owned(store, request.params, ContextStore::put_entry),
        "get_entry" => dispatch_ref(store, request.params, ContextStore::get_entry),
        "list_entries" => dispatch_owned(store, request.params, ContextStore::list_entries),
        "delete_entry" => dispatch_owned(store, request.params, ContextStore::delete_entry),
        "revert_entry" => dispatch_owned(store, request.params, ContextStore::revert_entry),
        "commit_work" => dispatch_owned(store, request.params, ContextStore::commit_work),
        "review_list" => dispatch_review_list(store, request.params),
        "review_approve" => dispatch_owned(store, request.params, ContextStore::review_approve),
        "review_reject" => dispatch_owned(store, request.params, ContextStore::review_reject),
        "review_edit" => dispatch_owned(store, request.params, ContextStore::review_edit),
        "review_edit_and_approve" => {
            dispatch_owned(store, request.params, ContextStore::review_edit_and_approve)
        }
        "export_bundle" => dispatch_owned(store, request.params, ContextStore::export_bundle),
        "export_json" => dispatch_owned(store, request.params, ContextStore::export_json),
        "export_markdown" => dispatch_owned(store, request.params, ContextStore::export_markdown),
        "import_data" => dispatch_owned(store, request.params, ContextStore::import_data),
        "preview_source_import" => {
            dispatch_owned(store, request.params, ContextStore::preview_source_import)
        }
        "apply_source_import" => {
            dispatch_owned(store, request.params, ContextStore::apply_source_import)
        }
        "create_run" => dispatch_owned(store, request.params, ContextStore::create_run),
        "list_runs" => serialize_core(store.list_runs()),
        other => Err(format!("unknown method: {other}")),
    };

    match result {
        Ok(value) => IpcResponse::success(id, value),
        Err(message) => IpcResponse::error(id, -32000, message),
    }
}

fn dispatch_owned<P, R>(
    store: &ContextStore,
    params: Value,
    f: fn(&ContextStore, P) -> ContextResult<R>,
) -> Result<Value, String>
where
    P: DeserializeOwned,
    R: Serialize,
{
    let parsed = serde_json::from_value::<P>(params).map_err(|err| err.to_string())?;
    serialize_core(f(store, parsed))
}

fn dispatch_ref<P, R>(
    store: &ContextStore,
    params: Value,
    f: fn(&ContextStore, &P) -> ContextResult<R>,
) -> Result<Value, String>
where
    P: DeserializeOwned,
    R: Serialize,
{
    let parsed = serde_json::from_value::<P>(params).map_err(|err| err.to_string())?;
    serialize_core(f(store, &parsed))
}

fn dispatch_review_list(store: &ContextStore, params: Value) -> Result<Value, String> {
    let state = match params {
        Value::Object(object) => match object.get("state").cloned() {
            None | Some(Value::Null) => None,
            Some(value) => Some(serde_json::from_value(value).map_err(|err| err.to_string())?),
        },
        _ => None,
    };
    serialize_core(store.review_list(state))
}

fn serialize_core<T: Serialize>(result: ContextResult<T>) -> Result<Value, String> {
    match result {
        Ok(value) => serde_json::to_value(value).map_err(|err| err.to_string()),
        Err(err) => Err(err.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use context_core::{
        CommitDisposition, CommitProposal, CommitStatus, CommitWorkRequest, ComposeRequest,
        EntryInput, EntryValue, ReviewEditAndApproveRequest, ReviewMode, ScopeKind, ScopeRef,
        SetReviewPolicyRequest, SourceImportApplyRequest, SourceImportDocument, SourceImportKind,
        SourceImportPreviewRequest,
    };
    use serde_json::json;
    use tempfile::tempdir;

    fn sample_commit(scope: ScopeRef) -> CommitWorkRequest {
        CommitWorkRequest {
            request_id: "req-1".to_string(),
            actor: "agent".to_string(),
            run: None,
            proposals: vec![CommitProposal {
                scope,
                pack_name: "main".to_string(),
                entry: EntryInput {
                    key: "key".to_string(),
                    title: Some("Title".to_string()),
                    kind: "note".to_string(),
                    value: EntryValue::Markdown {
                        body: "hello daemon".to_string(),
                    },
                    tags: vec![],
                    metadata: json!({}),
                    locked: false,
                    provenance: None,
                },
            }],
        }
    }

    #[test]
    fn dispatches_commit_and_compose() {
        let store = ContextStore::open_in_memory().expect("store");
        let project_scope = ScopeRef::normalized(ScopeKind::Project, "proj").expect("scope");
        let commit_response = dispatch_request(
            IpcRequest {
                id: "1".to_string(),
                method: "commit_work".to_string(),
                params: serde_json::to_value(sample_commit(project_scope.clone())).expect("value"),
            },
            &store,
        );
        assert!(commit_response.ok);
        let result: context_core::CommitWorkResult =
            serde_json::from_value(commit_response.result.expect("result")).expect("deserialize");
        assert_eq!(result.status, CommitStatus::Applied);
        assert_eq!(result.items[0].disposition, CommitDisposition::Applied);

        let compose_response = dispatch_request(
            IpcRequest {
                id: "2".to_string(),
                method: "compose_context".to_string(),
                params: serde_json::to_value(ComposeRequest {
                    project_scope_id: Some("proj".to_string()),
                    task_scope_id: None,
                    include_archived: false,
                })
                .expect("value"),
            },
            &store,
        );
        assert!(compose_response.ok);
        let composed: context_core::ComposeResponse =
            serde_json::from_value(compose_response.result.expect("result")).expect("deserialize");
        assert!(composed.rendered_markdown.contains("hello daemon"));
    }

    #[test]
    fn version_flag_is_available() {
        let error = Args::try_parse_from(["contextd", "--version"]).expect_err("version exits");
        assert_eq!(error.kind(), clap::error::ErrorKind::DisplayVersion);
        assert!(error.to_string().contains(COMPONENT_VERSION));
    }

    #[test]
    fn ping_reports_the_running_component_version() {
        let store = ContextStore::open_in_memory().expect("store");
        let response = dispatch_request(
            IpcRequest {
                id: "ping".to_string(),
                method: "ping".to_string(),
                params: json!({}),
            },
            &store,
        );
        assert!(response.ok);

        let health: context_core::HealthReport =
            serde_json::from_value(response.result.expect("result")).expect("health");
        assert_eq!(health.component_version.as_deref(), Some(COMPONENT_VERSION));
        assert_eq!(health.api_version, Some(context_core::CONTEXT_API_VERSION));
    }

    #[test]
    fn dispatches_atomic_review_edit_and_approve() {
        let store = ContextStore::open_in_memory().expect("store");
        let pending = dispatch_request(
            IpcRequest {
                id: "commit".to_string(),
                method: "commit_work".to_string(),
                params: serde_json::to_value(sample_commit(ScopeRef::global())).expect("request"),
            },
            &store,
        );
        assert!(pending.ok);
        let pending: context_core::CommitWorkResult =
            serde_json::from_value(pending.result.expect("result")).expect("commit result");
        let review_id = pending.items[0].review_id.clone().expect("review id");

        let response = dispatch_request(
            IpcRequest {
                id: "atomic-review".to_string(),
                method: "review_edit_and_approve".to_string(),
                params: serde_json::to_value(ReviewEditAndApproveRequest {
                    review_id,
                    title: Some("Approved over IPC".to_string()),
                    kind: None,
                    value: Some(EntryValue::Markdown {
                        body: "approved daemon body".to_string(),
                    }),
                    tags: None,
                    metadata: None,
                    locked: None,
                    actor: "reviewer".to_string(),
                    note: Some("atomic".to_string()),
                })
                .expect("request"),
            },
            &store,
        );
        assert!(response.ok);
        let approved: context_core::ReviewItem =
            serde_json::from_value(response.result.expect("result")).expect("approved review");
        assert_eq!(approved.state, context_core::ReviewState::Approved);
        assert_eq!(
            approved.proposed_entry.title.as_deref(),
            Some("Approved over IPC")
        );
        assert_eq!(approved.resolution_note.as_deref(), Some("atomic"));
    }

    #[test]
    fn returns_error_for_unknown_method() {
        let store = ContextStore::open_in_memory().expect("store");
        let response = dispatch_request(
            IpcRequest {
                id: "bad".to_string(),
                method: "missing".to_string(),
                params: json!({}),
            },
            &store,
        );
        assert!(!response.ok);
        assert!(
            response
                .error
                .expect("error")
                .message
                .contains("unknown method")
        );
    }

    #[test]
    fn review_list_accepts_null_state_filter() {
        let store = ContextStore::open_in_memory().expect("store");
        let response = dispatch_request(
            IpcRequest {
                id: "seed".to_string(),
                method: "commit_work".to_string(),
                params: serde_json::to_value(CommitWorkRequest {
                    request_id: "req-review".to_string(),
                    actor: "agent".to_string(),
                    run: None,
                    proposals: vec![CommitProposal {
                        scope: ScopeRef::global(),
                        pack_name: "main".to_string(),
                        entry: EntryInput {
                            key: "key".to_string(),
                            title: None,
                            kind: "note".to_string(),
                            value: EntryValue::Markdown {
                                body: "needs review".to_string(),
                            },
                            tags: vec![],
                            metadata: json!({}),
                            locked: false,
                            provenance: None,
                        },
                    }],
                })
                .expect("value"),
            },
            &store,
        );
        assert!(response.ok);

        let reviews = dispatch_request(
            IpcRequest {
                id: "list".to_string(),
                method: "review_list".to_string(),
                params: json!({ "state": null }),
            },
            &store,
        );
        assert!(reviews.ok);
        let items: Vec<context_core::ReviewItem> =
            serde_json::from_value(reviews.result.expect("result")).expect("reviews");
        assert_eq!(items.len(), 1);
    }

    #[test]
    fn dispatches_review_policy_and_staged_source_import() {
        let store = ContextStore::open_in_memory().expect("store");
        let set_policy = dispatch_request(
            IpcRequest {
                id: "policy-set".to_string(),
                method: "set_review_policy".to_string(),
                params: serde_json::to_value(SetReviewPolicyRequest {
                    mode: ReviewMode::Fast,
                    metadata: json!({"ui": "desktop"}),
                    actor: "tester".to_string(),
                })
                .expect("params"),
            },
            &store,
        );
        assert!(set_policy.ok);
        let policy: context_core::ReviewPolicy =
            serde_json::from_value(set_policy.result.expect("policy")).expect("deserialize");
        assert_eq!(policy.mode, ReviewMode::Fast);

        let destination = ScopeRef::normalized(ScopeKind::Project, "proj").expect("scope");
        let document = SourceImportDocument {
            path: Some("AGENTS.md".to_string()),
            payload: "# Agent instructions\n\nBe precise.".to_string(),
        };
        let preview = dispatch_request(
            IpcRequest {
                id: "import-preview".to_string(),
                method: "preview_source_import".to_string(),
                params: serde_json::to_value(SourceImportPreviewRequest {
                    source_kind: SourceImportKind::Auto,
                    documents: vec![document.clone()],
                    destination: destination.clone(),
                    pack_name: None,
                    actor: "tester".to_string(),
                })
                .expect("params"),
            },
            &store,
        );
        assert!(preview.ok);
        let preview: context_core::SourceImportPreview =
            serde_json::from_value(preview.result.expect("preview")).expect("deserialize");
        assert_eq!(
            preview.candidates[0].detected_source_kind,
            SourceImportKind::AgentsMd
        );
        assert!(!preview.destination_pack.exists);
        assert!(preview.preview_fingerprint.is_some());

        let applied = dispatch_request(
            IpcRequest {
                id: "import-apply".to_string(),
                method: "apply_source_import".to_string(),
                params: serde_json::to_value(SourceImportApplyRequest {
                    source_kind: SourceImportKind::Auto,
                    documents: vec![document],
                    destination,
                    pack_name: None,
                    actor: "tester".to_string(),
                    expected_preview_fingerprint: preview.preview_fingerprint,
                })
                .expect("params"),
            },
            &store,
        );
        assert!(applied.ok);
        let applied: context_core::SourceImportApplyResult =
            serde_json::from_value(applied.result.expect("apply")).expect("deserialize");
        assert_eq!(applied.applied_count, 1);

        let get_policy = dispatch_request(
            IpcRequest {
                id: "policy-get".to_string(),
                method: "get_review_policy".to_string(),
                params: json!({}),
            },
            &store,
        );
        assert!(get_policy.ok);
    }

    #[test]
    fn second_runtime_is_refused_for_same_data_dir() {
        let dir = tempdir().expect("tempdir");
        let paths = ContextPaths {
            data_dir: dir.path().join("data"),
            db_path: dir.path().join("data/context.db"),
            socket_path: dir.path().join("data/contextd.sock"),
            spool_dir: dir.path().join("data/spool"),
        };
        paths.ensure_parent_dirs().expect("paths");
        let _first = prepare_runtime(&paths).expect("first runtime");
        let err = prepare_runtime(&paths).expect_err("second runtime should fail");
        assert!(err.to_string().contains("already running"));
    }

    #[test]
    fn live_socket_is_not_replaced() {
        let dir = tempdir().expect("tempdir");
        let socket_path = dir.path().join("contextd.sock");
        let _listener = UnixListener::bind(&socket_path).expect("bind socket");
        let err = bind_listener(&socket_path).expect_err("live socket should be refused");
        assert!(err.to_string().contains("already live"));
    }

    #[test]
    fn runtime_guard_removes_socket_on_drop() {
        let dir = tempdir().expect("tempdir");
        let paths = ContextPaths {
            data_dir: dir.path().join("data"),
            db_path: dir.path().join("data/context.db"),
            socket_path: dir.path().join("data/contextd.sock"),
            spool_dir: dir.path().join("data/spool"),
        };
        paths.ensure_parent_dirs().expect("paths");
        {
            let runtime = prepare_runtime(&paths).expect("runtime");
            assert!(runtime.socket_path.exists());
        }
        assert!(!paths.socket_path.exists());
    }

    #[test]
    fn oversized_request_is_rejected() {
        let store = Arc::new(ContextStore::open_in_memory().expect("store"));
        let (mut client, server) = UnixStream::pair().expect("pair");
        let join = thread::spawn(move || handle_client(server, store).expect("handle client"));
        let oversized = format!("\"{}\"\n", "x".repeat(MAX_IPC_FRAME_BYTES + 1));
        client
            .write_all(oversized.as_bytes())
            .expect("write oversized request");
        client.flush().expect("flush");

        let mut response = String::new();
        BufReader::new(client)
            .read_line(&mut response)
            .expect("read response");
        join.join().expect("join");

        let response: IpcResponse = serde_json::from_str(response.trim()).expect("response json");
        assert!(!response.ok);
        assert!(
            response
                .error
                .expect("error")
                .message
                .contains("exceeds maximum size")
        );
    }
}
