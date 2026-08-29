use context_core::secret::reject_commit_request_for_storage;
use context_core::{
    CommitWorkRequest, CommitWorkResult, ComposeRequest, ComposeResponse, ContextExportBundle,
    ContextPaths, CreatePackRequest, DeleteEntryRequest, EntryRecord, EntrySelector, ExportRequest,
    HealthReport, ImportRequest, IpcRequest, IpcResponse, PackRecord, PutEntryRequest,
    RevertEntryRequest, ReviewDecisionRequest, ReviewEditAndApproveRequest, ReviewEditRequest,
    ReviewItem, ReviewPolicy, ReviewState, RunInput, RunRecord, SearchRequest, SearchResponse,
    SetReviewPolicyRequest, SourceImportApplyRequest, SourceImportApplyResult, SourceImportPreview,
    SourceImportPreviewRequest, StoreStats, UpdatePackRequest,
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};
use thiserror::Error;
use tracing::{debug, warn};

const MAX_IPC_FRAME_BYTES: usize = 8 * 1024 * 1024;
const MAX_SPOOL_FILE_BYTES: u64 = 8 * 1024 * 1024;

pub type ClientResult<T> = Result<T, ClientError>;

#[derive(Debug, Error)]
pub enum ClientError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("core error: {0}")]
    Core(#[from] context_core::ContextError),
    #[error("transport error: {0}")]
    Transport(String),
    #[error("remote error: {0}")]
    Remote(String),
    #[error("spawn error: {0}")]
    Spawn(String),
    #[error("timeout waiting for daemon: {0}")]
    Timeout(String),
}

#[derive(Clone, Debug)]
pub struct ClientConfig {
    pub paths: ContextPaths,
    pub autostart: bool,
    pub contextd_bin: Option<PathBuf>,
    pub connect_timeout: Duration,
    pub start_timeout: Duration,
}

impl ClientConfig {
    pub fn discover() -> ClientResult<Self> {
        Ok(Self {
            paths: ContextPaths::discover()?,
            autostart: true,
            contextd_bin: std::env::var_os("CONTEXTD_BIN").map(PathBuf::from),
            connect_timeout: Duration::from_secs(2),
            start_timeout: Duration::from_secs(5),
        })
    }

    pub fn with_paths(paths: ContextPaths) -> Self {
        Self {
            paths,
            autostart: true,
            contextd_bin: None,
            connect_timeout: Duration::from_secs(2),
            start_timeout: Duration::from_secs(5),
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct SpoolRetryReport {
    pub attempted: usize,
    pub delivered: usize,
    pub retained: usize,
    pub errors: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct SpoolEnvelope {
    version: u32,
    request: CommitWorkRequest,
}

pub struct ContextClient {
    config: ClientConfig,
}

impl ContextClient {
    pub fn new(config: ClientConfig) -> Self {
        Self { config }
    }

    pub fn discover() -> ClientResult<Self> {
        Ok(Self::new(ClientConfig::discover()?))
    }

    pub fn config(&self) -> &ClientConfig {
        &self.config
    }

    pub fn ping(&self) -> ClientResult<HealthReport> {
        self.call("ping", &json!({}))
    }

    pub fn stats(&self) -> ClientResult<StoreStats> {
        self.call("stats", &json!({}))
    }

    pub fn get_review_policy(&self) -> ClientResult<ReviewPolicy> {
        self.call("get_review_policy", &json!({}))
    }

    pub fn set_review_policy(&self, request: SetReviewPolicyRequest) -> ClientResult<ReviewPolicy> {
        self.call("set_review_policy", &request)
    }

    pub fn compose_context(&self, request: ComposeRequest) -> ClientResult<ComposeResponse> {
        self.call("compose_context", &request)
    }

    pub fn search_context(&self, request: SearchRequest) -> ClientResult<SearchResponse> {
        self.call("search_context", &request)
    }

    pub fn create_pack(&self, request: CreatePackRequest) -> ClientResult<PackRecord> {
        self.call("create_pack", &request)
    }

    pub fn update_pack(&self, request: UpdatePackRequest) -> ClientResult<PackRecord> {
        self.call("update_pack", &request)
    }

    pub fn list_packs(&self) -> ClientResult<Vec<PackRecord>> {
        self.call::<_, Vec<PackRecord>>("list_packs", &json!({}))
    }

    pub fn put_entry(&self, request: PutEntryRequest) -> ClientResult<EntryRecord> {
        self.call("put_entry", &request)
    }

    pub fn get_entry(&self, request: EntrySelector) -> ClientResult<EntryRecord> {
        self.call("get_entry", &request)
    }

    pub fn list_entries(&self, request: ExportRequest) -> ClientResult<Vec<EntryRecord>> {
        self.call("list_entries", &request)
    }

    pub fn delete_entry(&self, request: DeleteEntryRequest) -> ClientResult<EntryRecord> {
        self.call("delete_entry", &request)
    }

    pub fn revert_entry(&self, request: RevertEntryRequest) -> ClientResult<EntryRecord> {
        self.call("revert_entry", &request)
    }

    pub fn commit_work(&self, request: CommitWorkRequest) -> ClientResult<CommitWorkResult> {
        match self.call::<_, CommitWorkResult>("commit_work", &request) {
            Ok(result) => Ok(result),
            Err(ClientError::Transport(_))
            | Err(ClientError::Spawn(_))
            | Err(ClientError::Timeout(_)) => {
                let path = self.spool_request(&request)?;
                Ok(CommitWorkResult {
                    request_id: request.request_id,
                    status: context_core::CommitStatus::Spooled,
                    run_id: request.run.and_then(|run| run.id),
                    items: Vec::new(),
                    spooled: true,
                    spool_path: Some(path.display().to_string()),
                })
            }
            Err(err) => Err(err),
        }
    }

    pub fn review_list(&self, state: Option<ReviewState>) -> ClientResult<Vec<ReviewItem>> {
        self.call("review_list", &json!({ "state": state }))
    }

    pub fn review_approve(&self, request: ReviewDecisionRequest) -> ClientResult<ReviewItem> {
        self.call("review_approve", &request)
    }

    pub fn review_reject(&self, request: ReviewDecisionRequest) -> ClientResult<ReviewItem> {
        self.call("review_reject", &request)
    }

    pub fn review_edit(&self, request: ReviewEditRequest) -> ClientResult<ReviewItem> {
        self.call("review_edit", &request)
    }

    pub fn review_edit_and_approve(
        &self,
        request: ReviewEditAndApproveRequest,
    ) -> ClientResult<ReviewItem> {
        self.call("review_edit_and_approve", &request)
    }

    pub fn export_bundle(&self, request: ExportRequest) -> ClientResult<ContextExportBundle> {
        self.call("export_bundle", &request)
    }

    pub fn export_json(&self, request: ExportRequest) -> ClientResult<String> {
        self.call("export_json", &request)
    }

    pub fn export_markdown(&self, request: ExportRequest) -> ClientResult<String> {
        self.call("export_markdown", &request)
    }

    pub fn import_data(&self, request: ImportRequest) -> ClientResult<ContextExportBundle> {
        self.call("import_data", &request)
    }

    pub fn preview_source_import(
        &self,
        request: SourceImportPreviewRequest,
    ) -> ClientResult<SourceImportPreview> {
        self.call("preview_source_import", &request)
    }

    pub fn apply_source_import(
        &self,
        request: SourceImportApplyRequest,
    ) -> ClientResult<SourceImportApplyResult> {
        self.call("apply_source_import", &request)
    }

    pub fn create_run(&self, request: RunInput) -> ClientResult<RunRecord> {
        self.call("create_run", &request)
    }

    pub fn list_runs(&self) -> ClientResult<Vec<RunRecord>> {
        self.call::<_, Vec<RunRecord>>("list_runs", &json!({}))
    }

    pub fn retry_spool(&self) -> ClientResult<SpoolRetryReport> {
        let mut report = SpoolRetryReport::default();
        if !self.config.paths.spool_dir.exists() {
            return Ok(report);
        }
        let mut entries = fs::read_dir(&self.config.paths.spool_dir)?
            .filter_map(Result::ok)
            .collect::<Vec<_>>();
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                continue;
            }
            let metadata = fs::symlink_metadata(&path)?;
            if !metadata.file_type().is_file() {
                report.retained += 1;
                report
                    .errors
                    .push(format!("{}: not a regular spool file", path.display()));
                continue;
            }
            report.attempted += 1;
            match self.retry_single_spool(&path) {
                Ok(true) => report.delivered += 1,
                Ok(false) => report.retained += 1,
                Err(err) => {
                    report.retained += 1;
                    report.errors.push(format!("{}: {err}", path.display()));
                }
            }
        }
        Ok(report)
    }

    pub fn ensure_daemon(&self) -> ClientResult<()> {
        self.config.paths.ensure_parent_dirs()?;
        if UnixStream::connect(&self.config.paths.socket_path).is_ok() {
            return Ok(());
        }
        if !self.config.autostart {
            return Err(ClientError::Transport(format!(
                "daemon socket {} is unavailable",
                self.config.paths.socket_path.display()
            )));
        }
        self.spawn_daemon()?;
        self.wait_for_socket()
    }

    fn call<P: Serialize, R: DeserializeOwned>(&self, method: &str, params: &P) -> ClientResult<R> {
        let value = serde_json::to_value(params)?;
        let response = self.send_value(method, value)?;
        Ok(serde_json::from_value(response)?)
    }

    fn send_value(&self, method: &str, params: Value) -> ClientResult<Value> {
        self.config.paths.ensure_parent_dirs()?;
        let request = IpcRequest {
            id: format!(
                "req-{}",
                chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
            ),
            method: method.to_string(),
            params,
        };
        match self.transact(&request) {
            Ok(response) => Ok(response),
            Err(ClientError::Transport(_)) if self.config.autostart => {
                self.ensure_daemon()?;
                self.transact(&request)
            }
            Err(err) => Err(err),
        }
    }

    fn transact(&self, request: &IpcRequest) -> ClientResult<Value> {
        let mut stream = UnixStream::connect(&self.config.paths.socket_path).map_err(|err| {
            ClientError::Transport(format!(
                "connect {} failed: {err}",
                self.config.paths.socket_path.display()
            ))
        })?;
        stream.set_read_timeout(Some(self.config.connect_timeout))?;
        stream.set_write_timeout(Some(self.config.connect_timeout))?;
        let line = format!("{}\n", serde_json::to_string(request)?);
        if line.len() > MAX_IPC_FRAME_BYTES {
            return Err(ClientError::Core(context_core::ContextError::validation(
                "request exceeds maximum IPC frame size",
            )));
        }
        stream.write_all(line.as_bytes()).map_err(|err| {
            ClientError::Transport(format!(
                "write {} failed: {err}",
                self.config.paths.socket_path.display()
            ))
        })?;
        stream.flush().map_err(|err| {
            ClientError::Transport(format!(
                "flush {} failed: {err}",
                self.config.paths.socket_path.display()
            ))
        })?;
        let mut reader = BufReader::new(stream);
        let response_line = read_bounded_line(&mut reader, MAX_IPC_FRAME_BYTES, "IPC response")
            .map_err(|err| {
                ClientError::Transport(format!(
                    "read {} failed: {err}",
                    self.config.paths.socket_path.display()
                ))
            })?;
        let Some(response_line) = response_line else {
            return Err(ClientError::Transport(
                "daemon closed the connection without responding".to_string(),
            ));
        };
        let response: IpcResponse = serde_json::from_str(response_line.trim())?;
        if response.ok {
            Ok(response.result.unwrap_or(Value::Null))
        } else {
            Err(ClientError::Remote(
                response
                    .error
                    .map(|error| format!("{} ({})", error.message, error.code))
                    .unwrap_or_else(|| "unknown remote error".to_string()),
            ))
        }
    }

    fn spool_request(&self, request: &CommitWorkRequest) -> ClientResult<PathBuf> {
        self.config.paths.ensure_parent_dirs()?;
        reject_commit_request_for_storage(request)?;
        let envelope = SpoolEnvelope {
            version: 1,
            request: request.clone(),
        };
        let file_name = format!(
            "{}-{}.json",
            chrono::Utc::now().format("%Y%m%dT%H%M%S%3fZ"),
            sanitize_filename(&request.request_id)
        );
        let path = self.config.paths.spool_dir.join(file_name);
        let bytes = serde_json::to_vec_pretty(&envelope)?;
        write_private_file(&path, &bytes)?;
        Ok(path)
    }

    fn retry_single_spool(&self, path: &Path) -> ClientResult<bool> {
        let metadata = fs::metadata(path)?;
        if metadata.len() > MAX_SPOOL_FILE_BYTES {
            return Err(ClientError::Core(context_core::ContextError::validation(
                format!(
                    "spool file {} exceeds maximum size of {} bytes",
                    path.display(),
                    MAX_SPOOL_FILE_BYTES
                ),
            )));
        }
        let payload = fs::read_to_string(path)?;
        let envelope: SpoolEnvelope = serde_json::from_str(&payload)?;
        match self.call::<_, CommitWorkResult>("commit_work", &envelope.request) {
            Ok(_) => {
                fs::remove_file(path)?;
                Ok(true)
            }
            Err(ClientError::Transport(err)) => {
                warn!(path = %path.display(), %err, "retry transport failed");
                Ok(false)
            }
            Err(err) => Err(err),
        }
    }

    fn spawn_daemon(&self) -> ClientResult<()> {
        let executable = self.resolve_contextd_binary()?;
        debug!(path = %executable.display(), "spawning contextd");
        let mut command = Command::new(&executable);
        command
            .arg("--db")
            .arg(&self.config.paths.db_path)
            .arg("--socket")
            .arg(&self.config.paths.socket_path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        if let Some(home) = std::env::var_os("CONTEXT_MANAGER_HOME") {
            command.env("CONTEXT_MANAGER_HOME", home);
        }
        command.env("CONTEXT_DB_PATH", &self.config.paths.db_path);
        command.env("CONTEXT_SOCKET_PATH", &self.config.paths.socket_path);
        command.env("CONTEXT_SPOOL_DIR", &self.config.paths.spool_dir);
        command.spawn().map_err(|err| {
            ClientError::Spawn(format!("failed to spawn {}: {err}", executable.display()))
        })?;
        Ok(())
    }

    fn wait_for_socket(&self) -> ClientResult<()> {
        let start = Instant::now();
        while start.elapsed() <= self.config.start_timeout {
            if UnixStream::connect(&self.config.paths.socket_path).is_ok() {
                return Ok(());
            }
            thread::sleep(Duration::from_millis(100));
        }
        Err(ClientError::Timeout(format!(
            "socket {} did not become ready within {:?}",
            self.config.paths.socket_path.display(),
            self.config.start_timeout
        )))
    }

    fn resolve_contextd_binary(&self) -> ClientResult<PathBuf> {
        if let Some(path) = &self.config.contextd_bin {
            return Ok(path.clone());
        }
        if let Some(path) = std::env::var_os("CONTEXTD_BIN") {
            return Ok(PathBuf::from(path));
        }
        let current = std::env::current_exe()?;
        if let Some(parent) = current.parent() {
            let sibling = parent.join("contextd");
            if sibling.exists() {
                return Ok(sibling);
            }
            if let Some(debug_dir) = parent.parent() {
                let nested = debug_dir.join("contextd");
                if nested.exists() {
                    return Ok(nested);
                }
            }
        }
        Ok(PathBuf::from(OsString::from("contextd")))
    }
}

fn sanitize_filename(input: &str) -> String {
    input
        .chars()
        .map(|ch| match ch {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_' => ch,
            _ => '_',
        })
        .collect()
}

fn write_private_file(path: &Path, bytes: &[u8]) -> ClientResult<()> {
    #[cfg(unix)]
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);

    let mut file = options.open(path)?;
    file.write_all(bytes)?;
    file.flush()?;
    #[cfg(unix)]
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use context_core::{CommitDisposition, CommitStatus, ScopeKind, ScopeRef};
    use tempfile::tempdir;

    fn temp_paths() -> ContextPaths {
        let dir = tempdir().expect("tempdir");
        ContextPaths {
            data_dir: dir.path().join("data"),
            db_path: dir.path().join("data/context.db"),
            socket_path: dir.path().join("data/contextd.sock"),
            spool_dir: dir.path().join("data/spool"),
        }
    }

    fn sample_commit(request_id: &str) -> CommitWorkRequest {
        CommitWorkRequest {
            request_id: request_id.to_string(),
            actor: "agent".to_string(),
            run: None,
            proposals: vec![context_core::CommitProposal {
                scope: ScopeRef::normalized(ScopeKind::Project, "proj").expect("scope"),
                pack_name: "main".to_string(),
                entry: context_core::EntryInput {
                    key: "k".to_string(),
                    title: None,
                    kind: "note".to_string(),
                    value: context_core::EntryValue::Markdown {
                        body: "body".to_string(),
                    },
                    tags: vec![],
                    metadata: json!({}),
                    locked: false,
                    provenance: None,
                },
            }],
        }
    }

    fn synthetic_secret(prefix: &str) -> String {
        [prefix, "abcdefghijklmnopqrstuvwxyz123456"].concat()
    }

    fn spawn_mock_server<F>(socket_path: PathBuf, handler: F) -> thread::JoinHandle<()>
    where
        F: Fn(IpcRequest) -> IpcResponse + Send + 'static,
    {
        let parent = socket_path.parent().expect("parent");
        fs::create_dir_all(parent).expect("mkdir");
        let _ = fs::remove_file(&socket_path);
        let listener = std::os::unix::net::UnixListener::bind(&socket_path).expect("bind");
        thread::spawn(move || {
            if let Ok((stream, _)) = listener.accept() {
                let mut reader = BufReader::new(stream.try_clone().expect("clone"));
                let mut line = String::new();
                reader.read_line(&mut line).expect("read line");
                let request: IpcRequest = serde_json::from_str(line.trim()).expect("request");
                let response = handler(request);
                let mut writer = stream;
                writer
                    .write_all(
                        format!("{}\n", serde_json::to_string(&response).expect("serialize"))
                            .as_bytes(),
                    )
                    .expect("write response");
                writer.flush().expect("flush");
            }
        })
    }

    #[test]
    fn spools_failed_commit_and_retries() {
        let paths = temp_paths();
        let client = ContextClient::new(ClientConfig {
            paths: paths.clone(),
            autostart: false,
            contextd_bin: None,
            connect_timeout: Duration::from_millis(200),
            start_timeout: Duration::from_millis(200),
        });
        let commit = sample_commit("req-spool");
        let result = client.commit_work(commit.clone()).expect("spooled result");
        assert_eq!(result.status, CommitStatus::Spooled);
        assert!(result.spooled);
        let spooled_files = fs::read_dir(&paths.spool_dir)
            .expect("spool dir")
            .filter_map(Result::ok)
            .collect::<Vec<_>>();
        assert_eq!(spooled_files.len(), 1);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(spooled_files[0].path())
                .expect("metadata")
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600);
        }

        let request_id = commit.request_id.clone();
        let handler = move |request: IpcRequest| {
            assert_eq!(request.method, "commit_work");
            let result = CommitWorkResult {
                request_id: request_id.clone(),
                status: CommitStatus::Applied,
                run_id: None,
                items: vec![context_core::CommitItemResult {
                    scope: ScopeRef::normalized(ScopeKind::Project, "proj").expect("scope"),
                    pack_name: "main".to_string(),
                    entry_key: "k".to_string(),
                    disposition: CommitDisposition::Applied,
                    reason: None,
                    entry_id: Some("entry-1".to_string()),
                    review_id: None,
                }],
                spooled: false,
                spool_path: None,
            };
            IpcResponse::success(request.id, serde_json::to_value(result).expect("value"))
        };
        let join = spawn_mock_server(paths.socket_path.clone(), handler);
        let report = client.retry_spool().expect("retry report");
        join.join().expect("server joined");
        assert_eq!(report.attempted, 1);
        assert_eq!(report.delivered, 1);
        let remaining = fs::read_dir(&paths.spool_dir)
            .expect("spool dir")
            .filter_map(Result::ok)
            .count();
        assert_eq!(remaining, 0);
    }

    #[test]
    fn secret_bearing_commit_is_not_spooled() {
        let paths = temp_paths();
        let client = ContextClient::new(ClientConfig {
            paths: paths.clone(),
            autostart: false,
            contextd_bin: None,
            connect_timeout: Duration::from_millis(200),
            start_timeout: Duration::from_millis(200),
        });
        let mut commit = sample_commit("req-secret");
        commit.proposals[0].entry.provenance = Some(context_core::Provenance {
            actor: "agent".to_string(),
            source: "hook".to_string(),
            source_ref: None,
            run_id: None,
            request_id: None,
            note: Some(synthetic_secret("Bearer sk-live-")),
        });
        let err = client.commit_work(commit).expect_err("secret rejection");
        assert!(matches!(
            err,
            ClientError::Core(context_core::ContextError::SecretDetected(_))
        ));
        let spooled_count = fs::read_dir(&paths.spool_dir)
            .ok()
            .into_iter()
            .flat_map(|entries| entries.filter_map(Result::ok))
            .count();
        assert_eq!(spooled_count, 0);
    }

    #[test]
    fn sends_regular_request_to_socket() {
        let paths = temp_paths();
        let client = ContextClient::new(ClientConfig {
            paths: paths.clone(),
            autostart: false,
            contextd_bin: None,
            connect_timeout: Duration::from_secs(1),
            start_timeout: Duration::from_secs(1),
        });
        let join = spawn_mock_server(paths.socket_path.clone(), |request| {
            assert_eq!(request.method, "compose_context");
            let response = ComposeResponse {
                generated_at: chrono::Utc::now(),
                sections: vec![],
                rendered_markdown: "ok".to_string(),
                metrics: context_core::ComposeMetrics::default(),
                exclusions: vec![],
                warnings: vec![],
            };
            IpcResponse::success(request.id, serde_json::to_value(response).expect("value"))
        });
        let response = client
            .compose_context(ComposeRequest {
                project_scope_id: None,
                task_scope_id: None,
                include_archived: false,
            })
            .expect("compose");
        join.join().expect("joined");
        assert_eq!(response.rendered_markdown, "ok");
    }

    #[test]
    fn sends_atomic_review_edit_and_approve_request() {
        let paths = temp_paths();
        let client = ContextClient::new(ClientConfig {
            paths: paths.clone(),
            autostart: false,
            contextd_bin: None,
            connect_timeout: Duration::from_secs(1),
            start_timeout: Duration::from_secs(1),
        });
        let join = spawn_mock_server(paths.socket_path.clone(), |request| {
            assert_eq!(request.method, "review_edit_and_approve");
            let parsed: context_core::ReviewEditAndApproveRequest =
                serde_json::from_value(request.params).expect("atomic review request");
            assert_eq!(parsed.actor, "reviewer");
            assert_eq!(parsed.note.as_deref(), Some("ship it"));
            let now = chrono::Utc::now();
            let review = context_core::ReviewItem {
                id: parsed.review_id,
                request_id: "req-review".to_string(),
                scope: ScopeRef::global(),
                pack_name: "main".to_string(),
                entry_key: "key".to_string(),
                state: context_core::ReviewState::Approved,
                reason: context_core::ReviewReason::GlobalScope,
                proposed_entry: context_core::EntryInput {
                    key: "key".to_string(),
                    title: parsed.title,
                    kind: parsed.kind.unwrap_or_else(|| "note".to_string()),
                    value: parsed.value.unwrap_or(context_core::EntryValue::Markdown {
                        body: "body".to_string(),
                    }),
                    tags: parsed.tags.unwrap_or_default(),
                    metadata: parsed.metadata.unwrap_or_else(|| json!({})),
                    locked: parsed.locked.unwrap_or(false),
                    provenance: None,
                },
                existing_entry: None,
                resolution_note: parsed.note,
                created_at: now,
                updated_at: now,
                revision_no: 2,
            };
            IpcResponse::success(
                request.id,
                serde_json::to_value(review).expect("review response"),
            )
        });

        let approved = client
            .review_edit_and_approve(context_core::ReviewEditAndApproveRequest {
                review_id: "review-1".to_string(),
                title: Some("Approved".to_string()),
                kind: None,
                value: None,
                tags: None,
                metadata: None,
                locked: Some(true),
                actor: "reviewer".to_string(),
                note: Some("ship it".to_string()),
            })
            .expect("atomic review");
        join.join().expect("server joined");
        assert_eq!(approved.state, context_core::ReviewState::Approved);
        assert_eq!(approved.proposed_entry.title.as_deref(), Some("Approved"));
        assert!(approved.proposed_entry.locked);
        assert_eq!(approved.resolution_note.as_deref(), Some("ship it"));
    }

    #[test]
    fn sends_typed_policy_and_source_import_requests() {
        let paths = temp_paths();
        let client = ContextClient::new(ClientConfig {
            paths: paths.clone(),
            autostart: false,
            contextd_bin: None,
            connect_timeout: Duration::from_secs(1),
            start_timeout: Duration::from_secs(1),
        });

        let policy_join = spawn_mock_server(paths.socket_path.clone(), |request| {
            assert_eq!(request.method, "set_review_policy");
            let parsed: context_core::SetReviewPolicyRequest =
                serde_json::from_value(request.params).expect("policy request");
            assert_eq!(parsed.mode, context_core::ReviewMode::Strict);
            IpcResponse::success(
                request.id,
                serde_json::to_value(context_core::ReviewPolicy {
                    mode: parsed.mode,
                    metadata: parsed.metadata,
                    updated_at: chrono::Utc::now(),
                    updated_by: parsed.actor,
                    revision_no: 2,
                })
                .expect("policy response"),
            )
        });
        let policy = client
            .set_review_policy(context_core::SetReviewPolicyRequest {
                mode: context_core::ReviewMode::Strict,
                metadata: json!({"source": "test"}),
                actor: "tester".to_string(),
            })
            .expect("set policy");
        policy_join.join().expect("policy server");
        assert_eq!(policy.mode, context_core::ReviewMode::Strict);

        let destination = ScopeRef::normalized(ScopeKind::Project, "proj").expect("destination");
        let document = context_core::SourceImportDocument {
            path: Some("AGENTS.md".to_string()),
            payload: "# Instructions".to_string(),
        };
        let preview_join = spawn_mock_server(paths.socket_path.clone(), |request| {
            assert_eq!(request.method, "preview_source_import");
            let parsed: context_core::SourceImportPreviewRequest =
                serde_json::from_value(request.params).expect("preview request");
            IpcResponse::success(
                request.id,
                serde_json::to_value(context_core::SourceImportPreview {
                    destination: parsed.destination,
                    pack_name: parsed.pack_name.unwrap_or_else(|| "main".to_string()),
                    review_mode: context_core::ReviewMode::Strict,
                    destination_pack: context_core::SourceImportPackGovernance {
                        exists: true,
                        status: Some(context_core::PackStatus::Active),
                        locked: true,
                        lock_reason: Some("hold".to_string()),
                        revision_no: Some(3),
                    },
                    preview_fingerprint: Some("preview-test".to_string()),
                    candidates: vec![],
                    warnings: vec![],
                    apply_allowed: true,
                })
                .expect("preview response"),
            )
        });
        let preview = client
            .preview_source_import(context_core::SourceImportPreviewRequest {
                source_kind: context_core::SourceImportKind::Auto,
                documents: vec![document.clone()],
                destination: destination.clone(),
                pack_name: None,
                actor: "tester".to_string(),
            })
            .expect("preview");
        preview_join.join().expect("preview server");
        assert_eq!(preview.destination, destination);
        assert_eq!(preview.preview_fingerprint.as_deref(), Some("preview-test"));
        assert!(preview.destination_pack.exists);
        assert!(preview.destination_pack.locked);
        assert_eq!(preview.destination_pack.revision_no, Some(3));

        let apply_join = spawn_mock_server(paths.socket_path.clone(), |request| {
            assert_eq!(request.method, "apply_source_import");
            let parsed: context_core::SourceImportApplyRequest =
                serde_json::from_value(request.params).expect("apply request");
            assert_eq!(parsed.documents.len(), 1);
            assert_eq!(
                parsed.expected_preview_fingerprint.as_deref(),
                Some("preview-test")
            );
            IpcResponse::success(
                request.id,
                serde_json::to_value(context_core::SourceImportApplyResult {
                    request_id: "source-import-test".to_string(),
                    candidate_count: 1,
                    imported_count: 1,
                    applied_count: 1,
                    pending_count: 0,
                    skipped_count: 0,
                    rejected_count: 0,
                    items: vec![],
                    affected_entry_ids: vec!["entry-1".to_string()],
                    affected_review_ids: vec![],
                    affected_entry_keys: vec!["agents-instructions".to_string()],
                })
                .expect("apply response"),
            )
        });
        let applied = client
            .apply_source_import(context_core::SourceImportApplyRequest {
                source_kind: context_core::SourceImportKind::Auto,
                documents: vec![document],
                destination,
                pack_name: None,
                actor: "tester".to_string(),
                expected_preview_fingerprint: preview.preview_fingerprint,
            })
            .expect("apply");
        apply_join.join().expect("apply server");
        assert_eq!(applied.applied_count, 1);
    }

    #[test]
    fn context_manager_home_drives_discovery() {
        let dir = tempdir().expect("tempdir");
        unsafe {
            std::env::set_var("CONTEXT_MANAGER_HOME", dir.path());
        }
        let config = ClientConfig::discover().expect("discover");
        assert_eq!(config.paths.data_dir, dir.path());
        unsafe {
            std::env::remove_var("CONTEXT_MANAGER_HOME");
        }
    }
}
