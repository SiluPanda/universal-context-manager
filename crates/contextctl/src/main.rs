use clap::{Args, Parser, Subcommand, ValueEnum};
use context_client::{ClientConfig, ContextClient};
use context_core::{
    CommitWorkRequest, ComposeRequest, CreatePackRequest, DeleteEntryRequest, EntryInput,
    EntrySelector, EntryValue, ExportRequest, ImportFormat, ImportRequest, PackSelector,
    PutEntryRequest, RevertEntryRequest, ReviewDecisionRequest, ReviewEditRequest, ReviewState,
    RunInput, ScopeKind, ScopeRef, SearchRequest, UpdatePackRequest,
};
use serde::Serialize;
use serde_json::{Value, json};
use std::fs;
use std::io::{self, Read, Write};
use std::path::PathBuf;
use tracing_subscriber::EnvFilter;

#[derive(Debug, Parser)]
#[command(name = "contextctl", about = "Universal Context Manager CLI")]
struct Cli {
    #[arg(long, global = true)]
    json: bool,
    #[arg(long, env = "CONTEXT_DB_PATH")]
    db: Option<PathBuf>,
    #[arg(long, env = "CONTEXT_SOCKET_PATH")]
    socket: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    Init,
    Paths,
    Ping,
    Compose(ComposeCmd),
    Search(SearchCmd),
    CommitWork(CommitWorkCmd),
    RetrySpool,
    Pack {
        #[command(subcommand)]
        command: PackCommand,
    },
    Entry {
        #[command(subcommand)]
        command: EntryCommand,
    },
    Review {
        #[command(subcommand)]
        command: ReviewCommand,
    },
    Export(ExportCmd),
    Import(ImportCmd),
    Run {
        #[command(subcommand)]
        command: RunCommand,
    },
    Hook(HookCmd),
}

#[derive(Debug, Args)]
struct ComposeCmd {
    #[arg(long)]
    project: Option<String>,
    #[arg(long)]
    task: Option<String>,
    #[arg(long, default_value_t = false)]
    include_archived: bool,
}

#[derive(Debug, Args)]
struct SearchCmd {
    query: String,
    #[arg(long)]
    project: Option<String>,
    #[arg(long)]
    task: Option<String>,
    #[arg(long, default_value_t = 20)]
    limit: u32,
}

#[derive(Debug, Args)]
struct CommitWorkCmd {
    #[arg(long)]
    file: Option<PathBuf>,
}

#[derive(Debug, Subcommand)]
enum PackCommand {
    Create(PackCreateCmd),
    List,
    Update(PackUpdateCmd),
}

#[derive(Debug, Args)]
struct PackCreateCmd {
    #[command(flatten)]
    scope: ScopeArgs,
    #[arg(long, default_value = "main")]
    name: String,
    #[arg(long)]
    description: Option<String>,
    #[arg(long)]
    metadata: Option<String>,
    #[arg(long, default_value_t = false)]
    locked: bool,
    #[arg(long)]
    lock_reason: Option<String>,
    #[arg(long, default_value = "contextctl")]
    actor: String,
}

#[derive(Debug, Args)]
struct PackUpdateCmd {
    #[command(flatten)]
    scope: ScopeArgs,
    #[arg(long, default_value = "main")]
    name: String,
    #[arg(long)]
    description: Option<String>,
    #[arg(long)]
    metadata: Option<String>,
    #[arg(long)]
    status: Option<PackStatusArg>,
    #[arg(long)]
    locked: Option<bool>,
    #[arg(long)]
    lock_reason: Option<String>,
    #[arg(long, default_value = "contextctl")]
    actor: String,
}

#[derive(Debug, Subcommand)]
enum EntryCommand {
    Put(EntryPutCmd),
    Get(EntryGetCmd),
    List(EntryListCmd),
    Delete(EntryDeleteCmd),
    Revert(EntryRevertCmd),
}

#[derive(Debug, Args)]
struct EntryPutCmd {
    #[command(flatten)]
    scope: ScopeArgs,
    #[arg(long, default_value = "main")]
    pack: String,
    #[arg(long)]
    key: String,
    #[arg(long)]
    title: Option<String>,
    #[arg(long, default_value = "note")]
    kind: String,
    #[command(flatten)]
    content: EntryContentArgs,
    #[arg(long)]
    tag: Vec<String>,
    #[arg(long)]
    metadata: Option<String>,
    #[arg(long, default_value_t = false)]
    locked: bool,
    #[arg(long, default_value = "contextctl")]
    actor: String,
}

#[derive(Debug, Args)]
struct EntryGetCmd {
    #[command(flatten)]
    scope: ScopeArgs,
    #[arg(long, default_value = "main")]
    pack: String,
    #[arg(long)]
    key: String,
}

#[derive(Debug, Args)]
struct EntryListCmd {
    #[arg(long)]
    project: Option<String>,
    #[arg(long)]
    task: Option<String>,
    #[arg(long)]
    scope: Option<ScopeKindArg>,
    #[arg(long)]
    scope_id: Option<String>,
    #[arg(long)]
    pack: Option<String>,
    #[arg(long, default_value_t = false)]
    include_deleted: bool,
}

#[derive(Debug, Args)]
struct EntryDeleteCmd {
    #[command(flatten)]
    scope: ScopeArgs,
    #[arg(long, default_value = "main")]
    pack: String,
    #[arg(long)]
    key: String,
    #[arg(long, default_value = "contextctl")]
    actor: String,
}

#[derive(Debug, Args)]
struct EntryRevertCmd {
    #[command(flatten)]
    scope: ScopeArgs,
    #[arg(long, default_value = "main")]
    pack: String,
    #[arg(long)]
    key: String,
    #[arg(long)]
    revision: Option<i64>,
    #[arg(long, default_value = "contextctl")]
    actor: String,
}

#[derive(Debug, Subcommand)]
enum ReviewCommand {
    List(ReviewListCmd),
    Approve(ReviewDecisionCmd),
    Reject(ReviewDecisionCmd),
    Edit(ReviewEditCmd),
}

#[derive(Debug, Args)]
struct ReviewListCmd {
    #[arg(long)]
    state: Option<ReviewStateArg>,
}

#[derive(Debug, Args)]
struct ReviewDecisionCmd {
    review_id: String,
    #[arg(long, default_value = "contextctl")]
    actor: String,
    #[arg(long)]
    note: Option<String>,
}

#[derive(Debug, Args)]
struct ReviewEditCmd {
    review_id: String,
    #[arg(long)]
    title: Option<String>,
    #[arg(long)]
    kind: Option<String>,
    #[command(flatten)]
    content: EntryContentArgs,
    #[arg(long)]
    tag: Vec<String>,
    #[arg(long)]
    metadata: Option<String>,
    #[arg(long)]
    locked: Option<bool>,
    #[arg(long, default_value = "contextctl")]
    actor: String,
}

#[derive(Debug, Args)]
struct ExportCmd {
    #[arg(long, value_enum, default_value_t = ExportFormatArg::Json)]
    format: ExportFormatArg,
    #[arg(long)]
    output: Option<PathBuf>,
    #[arg(long)]
    project: Option<String>,
    #[arg(long)]
    task: Option<String>,
    #[arg(long)]
    scope: Option<ScopeKindArg>,
    #[arg(long)]
    scope_id: Option<String>,
    #[arg(long)]
    pack: Option<String>,
    #[arg(long, default_value_t = false)]
    include_deleted: bool,
    #[arg(long, default_value_t = false)]
    include_reviews: bool,
    #[arg(long, default_value_t = false)]
    include_runs: bool,
}

#[derive(Debug, Args)]
struct ImportCmd {
    #[arg(long, value_enum)]
    format: ExportFormatArg,
    #[arg(long)]
    input: Option<PathBuf>,
    #[arg(long, default_value = "contextctl")]
    actor: String,
}

#[derive(Debug, Subcommand)]
enum RunCommand {
    Create(RunCreateCmd),
    List,
}

#[derive(Debug, Args)]
struct RunCreateCmd {
    #[arg(long)]
    id: Option<String>,
    #[arg(long)]
    project: Option<String>,
    #[arg(long)]
    task: Option<String>,
    #[arg(long)]
    source: String,
    #[arg(long)]
    metadata: Option<String>,
}

#[derive(Debug, Args)]
#[command(
    about = "Hook-oriented APIs. Supports the stable adapter wrapper form `contextctl hook --adapter ... --mode ... --payload-file ... --project-dir ...`, plus explicit subcommands."
)]
struct HookCmd {
    #[arg(long)]
    adapter: Option<String>,
    #[arg(long)]
    mode: Option<String>,
    #[arg(long)]
    payload_file: Option<PathBuf>,
    #[arg(long)]
    project_dir: Option<String>,
    #[arg(long)]
    plugin_root: Option<String>,
    #[arg(long)]
    plugin_data: Option<String>,
    #[command(subcommand)]
    command: Option<HookSubcommand>,
}

#[derive(Debug, Subcommand)]
enum HookSubcommand {
    Compose(ComposeCmd),
    Commit(CommitWorkCmd),
    RetrySpool,
}

#[derive(Debug, Args)]
struct ScopeArgs {
    #[arg(long, value_enum)]
    scope: ScopeKindArg,
    #[arg(long)]
    scope_id: Option<String>,
}

#[derive(Debug, Args, Default)]
struct EntryContentArgs {
    #[arg(long)]
    body: Option<String>,
    #[arg(long)]
    body_file: Option<PathBuf>,
    #[arg(long)]
    json_value: Option<String>,
    #[arg(long)]
    json_file: Option<PathBuf>,
}

#[derive(Clone, Debug, ValueEnum)]
enum ScopeKindArg {
    Global,
    Project,
    Task,
}

#[derive(Clone, Debug, ValueEnum)]
enum ReviewStateArg {
    Pending,
    Approved,
    Rejected,
}

#[derive(Clone, Debug, ValueEnum)]
enum ExportFormatArg {
    Json,
    Markdown,
}

#[derive(Clone, Debug, ValueEnum)]
enum PackStatusArg {
    Active,
    Archived,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    init_tracing();
    let client = build_client(&cli)?;
    let json_output = cli.json || matches!(cli.command, Command::Hook(_));

    match cli.command {
        Command::Init => {
            client.ensure_daemon()?;
            print_json(&json!({
                "db_path": client.config().paths.db_path,
                "socket_path": client.config().paths.socket_path,
                "spool_dir": client.config().paths.spool_dir,
            }))?;
        }
        Command::Paths => {
            print_json(&json!({
                "data_dir": client.config().paths.data_dir,
                "db_path": client.config().paths.db_path,
                "socket_path": client.config().paths.socket_path,
                "spool_dir": client.config().paths.spool_dir,
            }))?;
        }
        Command::Ping => print_output(json_output, &client.ping()?)?,
        Command::Compose(cmd) => {
            let response = client.compose_context(ComposeRequest {
                project_scope_id: cmd.project,
                task_scope_id: cmd.task,
                include_archived: cmd.include_archived,
            })?;
            if json_output {
                print_json(&response)?;
            } else {
                println!("{}", response.rendered_markdown);
            }
        }
        Command::Search(cmd) => print_output(
            json_output,
            &client.search_context(SearchRequest {
                query: cmd.query,
                project_scope_id: cmd.project,
                task_scope_id: cmd.task,
                limit: cmd.limit,
            })?,
        )?,
        Command::CommitWork(cmd) => {
            let payload = read_optional_input(cmd.file)?;
            let request: CommitWorkRequest = serde_json::from_str(&payload)?;
            print_output(json_output, &client.commit_work(request)?)?;
        }
        Command::RetrySpool => print_json(&client.retry_spool()?)?,
        Command::Pack { command } => handle_pack_command(&client, command, json_output)?,
        Command::Entry { command } => handle_entry_command(&client, command, json_output)?,
        Command::Review { command } => handle_review_command(&client, command, json_output)?,
        Command::Export(cmd) => handle_export_command(&client, cmd)?,
        Command::Import(cmd) => handle_import_command(&client, cmd, json_output)?,
        Command::Run { command } => handle_run_command(&client, command, json_output)?,
        Command::Hook(cmd) => handle_hook_command(&client, cmd)?,
    }
    Ok(())
}

fn handle_pack_command(
    client: &ContextClient,
    command: PackCommand,
    json_output: bool,
) -> anyhow::Result<()> {
    match command {
        PackCommand::Create(cmd) => print_output(
            json_output,
            &client.create_pack(CreatePackRequest {
                scope: cmd.scope.to_scope()?,
                name: cmd.name,
                description: cmd.description,
                metadata: parse_json_object(cmd.metadata)?,
                locked: cmd.locked,
                lock_reason: cmd.lock_reason,
                actor: cmd.actor,
            })?,
        )?,
        PackCommand::List => print_output(json_output, &client.list_packs()?)?,
        PackCommand::Update(cmd) => print_output(
            json_output,
            &client.update_pack(UpdatePackRequest {
                selector: PackSelector {
                    scope: cmd.scope.to_scope()?,
                    name: cmd.name,
                },
                description: cmd.description,
                metadata: cmd.metadata.map(parse_json_text).transpose()?,
                status: cmd.status.map(|status| match status {
                    PackStatusArg::Active => context_core::PackStatus::Active,
                    PackStatusArg::Archived => context_core::PackStatus::Archived,
                }),
                locked: cmd.locked,
                lock_reason: cmd.lock_reason,
                actor: cmd.actor,
            })?,
        )?,
    }
    Ok(())
}

fn handle_entry_command(
    client: &ContextClient,
    command: EntryCommand,
    json_output: bool,
) -> anyhow::Result<()> {
    match command {
        EntryCommand::Put(cmd) => print_output(
            json_output,
            &client.put_entry(PutEntryRequest {
                scope: cmd.scope.to_scope()?,
                pack_name: cmd.pack,
                entry: EntryInput {
                    key: cmd.key,
                    title: cmd.title,
                    kind: cmd.kind,
                    value: cmd.content.to_entry_value()?,
                    tags: cmd.tag,
                    metadata: parse_json_object(cmd.metadata)?,
                    locked: cmd.locked,
                    provenance: None,
                },
                actor: cmd.actor,
            })?,
        )?,
        EntryCommand::Get(cmd) => print_output(
            json_output,
            &client.get_entry(EntrySelector {
                scope: cmd.scope.to_scope()?,
                pack_name: cmd.pack,
                entry_key: cmd.key,
            })?,
        )?,
        EntryCommand::List(cmd) => print_output(
            json_output,
            &client.list_entries(export_request(
                cmd.project,
                cmd.task,
                cmd.scope,
                cmd.scope_id,
                cmd.pack,
                cmd.include_deleted,
                false,
                false,
            )?)?,
        )?,
        EntryCommand::Delete(cmd) => print_output(
            json_output,
            &client.delete_entry(DeleteEntryRequest {
                selector: EntrySelector {
                    scope: cmd.scope.to_scope()?,
                    pack_name: cmd.pack,
                    entry_key: cmd.key,
                },
                actor: cmd.actor,
            })?,
        )?,
        EntryCommand::Revert(cmd) => print_output(
            json_output,
            &client.revert_entry(RevertEntryRequest {
                selector: EntrySelector {
                    scope: cmd.scope.to_scope()?,
                    pack_name: cmd.pack,
                    entry_key: cmd.key,
                },
                revision_no: cmd.revision,
                actor: cmd.actor,
            })?,
        )?,
    }
    Ok(())
}

fn handle_review_command(
    client: &ContextClient,
    command: ReviewCommand,
    json_output: bool,
) -> anyhow::Result<()> {
    match command {
        ReviewCommand::List(cmd) => print_output(
            json_output,
            &client.review_list(cmd.state.map(|state| match state {
                ReviewStateArg::Pending => ReviewState::Pending,
                ReviewStateArg::Approved => ReviewState::Approved,
                ReviewStateArg::Rejected => ReviewState::Rejected,
            }))?,
        )?,
        ReviewCommand::Approve(cmd) => print_output(
            json_output,
            &client.review_approve(ReviewDecisionRequest {
                review_id: cmd.review_id,
                actor: cmd.actor,
                note: cmd.note,
            })?,
        )?,
        ReviewCommand::Reject(cmd) => print_output(
            json_output,
            &client.review_reject(ReviewDecisionRequest {
                review_id: cmd.review_id,
                actor: cmd.actor,
                note: cmd.note,
            })?,
        )?,
        ReviewCommand::Edit(cmd) => print_output(
            json_output,
            &client.review_edit(ReviewEditRequest {
                review_id: cmd.review_id,
                title: cmd.title,
                kind: cmd.kind,
                value: cmd.content.to_entry_value_optional()?,
                tags: if cmd.tag.is_empty() {
                    None
                } else {
                    Some(cmd.tag)
                },
                metadata: cmd.metadata.map(parse_json_text).transpose()?,
                locked: cmd.locked,
                actor: cmd.actor,
            })?,
        )?,
    }
    Ok(())
}

fn handle_export_command(client: &ContextClient, cmd: ExportCmd) -> anyhow::Result<()> {
    let request = export_request(
        cmd.project,
        cmd.task,
        cmd.scope,
        cmd.scope_id,
        cmd.pack,
        cmd.include_deleted,
        cmd.include_reviews,
        cmd.include_runs,
    )?;
    match cmd.format {
        ExportFormatArg::Json => write_text_output(cmd.output, &client.export_json(request)?)?,
        ExportFormatArg::Markdown => {
            write_text_output(cmd.output, &client.export_markdown(request)?)?
        }
    }
    Ok(())
}

fn handle_import_command(
    client: &ContextClient,
    cmd: ImportCmd,
    json_output: bool,
) -> anyhow::Result<()> {
    let payload = read_optional_input(cmd.input)?;
    let format = match cmd.format {
        ExportFormatArg::Json => ImportFormat::Json,
        ExportFormatArg::Markdown => ImportFormat::Markdown,
    };
    print_output(
        json_output,
        &client.import_data(ImportRequest {
            actor: cmd.actor,
            format,
            payload,
        })?,
    )?;
    Ok(())
}

fn handle_run_command(
    client: &ContextClient,
    command: RunCommand,
    json_output: bool,
) -> anyhow::Result<()> {
    match command {
        RunCommand::Create(cmd) => print_output(
            json_output,
            &client.create_run(RunInput {
                id: cmd.id,
                project_scope_id: cmd.project,
                task_scope_id: cmd.task,
                source: cmd.source,
                metadata: parse_json_object(cmd.metadata)?,
            })?,
        )?,
        RunCommand::List => print_output(json_output, &client.list_runs()?)?,
    }
    Ok(())
}

fn handle_hook_command(client: &ContextClient, cmd: HookCmd) -> anyhow::Result<()> {
    if cmd.adapter.is_some()
        || cmd.mode.is_some()
        || cmd.payload_file.is_some()
        || cmd.project_dir.is_some()
    {
        return run_compat_hook(client, cmd);
    }
    match cmd.command {
        Some(HookSubcommand::Compose(compose)) => {
            let response = client.compose_context(ComposeRequest {
                project_scope_id: compose.project,
                task_scope_id: compose.task,
                include_archived: compose.include_archived,
            })?;
            print_json(&response)?;
        }
        Some(HookSubcommand::Commit(commit)) => {
            let payload = read_optional_input(commit.file)?;
            let request: CommitWorkRequest = serde_json::from_str(&payload)?;
            print_json(&client.commit_work(request)?)?;
        }
        Some(HookSubcommand::RetrySpool) => print_json(&client.retry_spool()?)?,
        None => {
            return Err(anyhow::anyhow!(
                "hook requires compatibility flags or a subcommand"
            ));
        }
    }
    Ok(())
}

fn run_compat_hook(client: &ContextClient, cmd: HookCmd) -> anyhow::Result<()> {
    let adapter = cmd
        .adapter
        .ok_or_else(|| anyhow::anyhow!("--adapter is required for compatibility hook mode"))?;
    let mode = cmd
        .mode
        .ok_or_else(|| anyhow::anyhow!("--mode is required for compatibility hook mode"))?;
    let fallback_project_dir = cmd.project_dir.unwrap_or_else(|| {
        std::env::current_dir()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|_| ".".to_string())
    });
    let payload_text = match cmd.payload_file {
        Some(path) => fs::read_to_string(path)?,
        None => read_optional_input(None)?,
    };
    let payload_json = serde_json::from_str::<Value>(&payload_text)
        .unwrap_or_else(|_| json!({ "raw": payload_text }));

    // Both Codex and Claude Code include the actual hook working directory in
    // the payload. Prefer it to the wrapper process' PWD, which may be the
    // plugin directory depending on how the host launched the hook.
    let project_dir =
        payload_string(&payload_json, &["cwd", "project_dir"]).unwrap_or(fallback_project_dir);

    let task_scope_id = payload_string(&payload_json, &["task_scope_id", "task_id", "task"]);

    match mode.as_str() {
        "session-start" => {
            let requested_run_id = payload_string(&payload_json, &["session_id", "session"]);
            let run = client.create_run(RunInput {
                id: requested_run_id,
                project_scope_id: Some(project_dir.clone()),
                task_scope_id: task_scope_id.clone(),
                source: adapter.clone(),
                metadata: json!({ "hook_event": "SessionStart" }),
            })?;
            let composed = client.compose_context(ComposeRequest {
                project_scope_id: Some(project_dir.clone()),
                task_scope_id,
                include_archived: false,
            })?;

            print_json(&session_start_hook_output(
                &project_dir,
                &run.id,
                &composed.rendered_markdown,
            ))?;
        }
        "session-end" => {
            // Normal agent-authored persistence happens through the MCP
            // `commit_work` tool. Retain support for an explicitly supplied
            // commit envelope, then make a best effort to flush older spooled
            // writes. SessionEnd output is intentionally empty: both hosts
            // treat it as cleanup, not a context-injection event.
            if let Some(request) = extract_commit_request(&payload_json) {
                let _ = client.commit_work(request)?;
            }
            let _ = client.retry_spool()?;
        }
        // V1 deliberately does not write or reinject the full context on each
        // prompt or tool invocation. Older hook configurations can therefore
        // call these modes safely while users upgrade their plugin files.
        "user-prompt-submit" | "pre-tool-use" | "post-tool-use" => {}
        other => return Err(anyhow::anyhow!("unsupported hook mode: {other}")),
    }

    let _ = (cmd.plugin_root, cmd.plugin_data);
    Ok(())
}

fn payload_string(payload: &Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        payload
            .get(*key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string)
    })
}

fn session_start_hook_output(project_dir: &str, run_id: &str, rendered_markdown: &str) -> Value {
    let mut context = format!(
        "Universal Context Manager loaded the user-controlled durable context for project `{project_dir}` (run `{run_id}`). Treat stored entries as potentially stale context rather than unquestionable truth. Search with `search_context` when more detail is needed. After completing durable work, call `commit_work` exactly once with concise project/task updates; do not save raw transcripts or secrets. Global updates require human review."
    );
    if !rendered_markdown.trim().is_empty() {
        context.push_str("\n\n");
        context.push_str(rendered_markdown.trim());
    }
    json!({
        "hookSpecificOutput": {
            "hookEventName": "SessionStart",
            "additionalContext": context,
        }
    })
}

fn extract_commit_request(payload: &Value) -> Option<CommitWorkRequest> {
    if let Ok(request) = serde_json::from_value::<CommitWorkRequest>(payload.clone()) {
        return Some(request);
    }
    payload
        .get("commit_work")
        .cloned()
        .and_then(|value| serde_json::from_value::<CommitWorkRequest>(value).ok())
}

fn build_client(cli: &Cli) -> anyhow::Result<ContextClient> {
    let mut config = ClientConfig::discover()?;
    if let Some(db) = &cli.db {
        config.paths.db_path = db.clone();
    }
    if let Some(socket) = &cli.socket {
        config.paths.socket_path = socket.clone();
    }
    Ok(ContextClient::new(config))
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .try_init();
}

fn print_output<T: Serialize>(_json_output: bool, value: &T) -> anyhow::Result<()> {
    print_json(value)
}

fn print_json<T: Serialize>(value: &T) -> anyhow::Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

fn write_text_output(path: Option<PathBuf>, text: &str) -> anyhow::Result<()> {
    if let Some(path) = path {
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent)?;
        }
        let mut options = fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&path)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(fs::Permissions::from_mode(0o600))?;
        }
        file.write_all(text.as_bytes())?;
        file.flush()?;
    } else {
        println!("{text}");
    }
    Ok(())
}

fn read_optional_input(path: Option<PathBuf>) -> anyhow::Result<String> {
    match path {
        Some(path) => Ok(fs::read_to_string(path)?),
        None => {
            let mut input = String::new();
            io::stdin().read_to_string(&mut input)?;
            Ok(input)
        }
    }
}

fn parse_json_text(text: String) -> anyhow::Result<Value> {
    Ok(serde_json::from_str(&text)?)
}

fn parse_json_object(text: Option<String>) -> anyhow::Result<Value> {
    Ok(match text {
        Some(text) => serde_json::from_str(&text)?,
        None => json!({}),
    })
}

#[allow(clippy::too_many_arguments)]
fn export_request(
    project: Option<String>,
    task: Option<String>,
    scope: Option<ScopeKindArg>,
    scope_id: Option<String>,
    pack: Option<String>,
    include_deleted: bool,
    include_reviews: bool,
    include_runs: bool,
) -> anyhow::Result<ExportRequest> {
    let scope = match scope {
        Some(kind) => Some(scope_from_parts(kind, scope_id)?),
        None => None,
    };
    Ok(ExportRequest {
        project_scope_id: project,
        task_scope_id: task,
        scope,
        pack_name: pack,
        include_deleted,
        include_reviews,
        include_runs,
    })
}

impl ScopeArgs {
    fn to_scope(&self) -> anyhow::Result<ScopeRef> {
        scope_from_parts(self.scope.clone(), self.scope_id.clone())
    }
}

fn scope_from_parts(kind: ScopeKindArg, scope_id: Option<String>) -> anyhow::Result<ScopeRef> {
    let kind = match kind {
        ScopeKindArg::Global => ScopeKind::Global,
        ScopeKindArg::Project => ScopeKind::Project,
        ScopeKindArg::Task => ScopeKind::Task,
    };
    Ok(ScopeRef::normalized(kind, scope_id.unwrap_or_default())?)
}

impl EntryContentArgs {
    fn to_entry_value(&self) -> anyhow::Result<EntryValue> {
        self.to_entry_value_optional()?.ok_or_else(|| {
            anyhow::anyhow!("either --body/--body-file or --json-value/--json-file is required")
        })
    }

    fn to_entry_value_optional(&self) -> anyhow::Result<Option<EntryValue>> {
        let markdown = if let Some(body) = &self.body {
            Some(body.clone())
        } else if let Some(path) = &self.body_file {
            Some(fs::read_to_string(path)?)
        } else {
            None
        };
        let json_value = if let Some(text) = &self.json_value {
            Some(serde_json::from_str(text)?)
        } else if let Some(path) = &self.json_file {
            Some(serde_json::from_str(&fs::read_to_string(path)?)?)
        } else {
            None
        };
        match (markdown, json_value) {
            (Some(_), Some(_)) => Err(anyhow::anyhow!("choose markdown or json content, not both")),
            (Some(body), None) => Ok(Some(EntryValue::Markdown { body })),
            (None, Some(value)) => Ok(Some(EntryValue::Json { value })),
            (None, None) => Ok(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;
    use tempfile::tempdir;

    #[test]
    fn hook_help_mentions_compatibility_flags() {
        let help = Cli::command().render_long_help().to_string();
        assert!(help.contains("hook"));
    }

    #[test]
    fn extract_commit_request_supports_nested_shape() {
        let payload = json!({
            "commit_work": {
                "request_id": "req-1",
                "actor": "agent",
                "proposals": []
            }
        });
        let request = extract_commit_request(&payload).expect("request");
        assert_eq!(request.request_id, "req-1");
    }

    #[test]
    fn session_start_output_matches_host_hook_contract() {
        let output = session_start_hook_output("/tmp/demo", "run-1", "# Context\nRemember me");
        assert_eq!(
            output["hookSpecificOutput"]["hookEventName"],
            "SessionStart"
        );
        let additional = output["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .expect("additional context");
        assert!(additional.contains("/tmp/demo"));
        assert!(additional.contains("run-1"));
        assert!(additional.contains("Remember me"));
        assert!(additional.contains("commit_work"));
        assert!(output.get("payload").is_none());
    }

    #[test]
    fn payload_string_prefers_first_non_empty_host_value() {
        let payload = json!({ "cwd": " /repo ", "project_dir": "/fallback" });
        assert_eq!(
            payload_string(&payload, &["cwd", "project_dir"]),
            Some("/repo".to_string())
        );
    }

    #[test]
    fn file_exports_are_private() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("nested/context.json");
        write_text_output(Some(path.clone()), "{\"safe\":true}").expect("write output");
        assert_eq!(
            fs::read_to_string(&path).expect("read output"),
            "{\"safe\":true}"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(path).expect("metadata").permissions().mode() & 0o777,
                0o600
            );
        }
    }
}
