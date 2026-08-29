mod completion;
mod diagnostics;
mod doctor;
mod output;
mod scope;
mod setup;
mod source;

use crate::completion::{CompletionShell, generate as generate_completion};
use crate::diagnostics::AdapterKind;
use crate::doctor::{DoctorState, format_doctor_report, run_doctor};
use crate::output::{
    format_bundle_import, format_commit, format_compose, format_entries, format_entry,
    format_entry_detail, format_health, format_pack, format_packs, format_policy, format_review,
    format_reviews, format_run, format_runs, format_search, format_source_apply,
    format_source_preview, format_spool, format_stats, print_human, print_json,
};
use crate::scope::{
    current_directory, default_project_directory, resolve_project_directory,
    resolve_project_scope_id, resolve_scope,
};
use crate::setup::{SetupOptions, SetupState, format_setup_report, run_setup};
use crate::source::{canonicalize_source_paths, read_source_documents};
use clap::{Args, Parser, Subcommand, ValueEnum};
use context_client::{ClientConfig, ContextClient};
use context_core::{
    CommitWorkRequest, ComposeRequest, CreatePackRequest, DeleteEntryRequest, EntryInput,
    EntrySelector, EntryValue, ExportRequest, ImportFormat, ImportRequest, PackSelector,
    PutEntryRequest, RevertEntryRequest, ReviewDecisionRequest, ReviewEditRequest, ReviewMode,
    ReviewState, RunInput, ScopeKind, ScopeRef, SearchRequest, SetReviewPolicyRequest,
    SourceImportApplyRequest, SourceImportDocument, SourceImportKind, SourceImportPreviewRequest,
    UpdatePackRequest,
};
use serde::Serialize;
use serde_json::{Value, json};
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use tracing_subscriber::EnvFilter;

const TOP_LEVEL_EXAMPLES: &str = "\
Examples:
  contextctl ping
  contextctl compose
  contextctl entry list --scope project
  contextctl source-import preview AGENTS.md
  contextctl setup --adapter codex
  contextctl doctor
  contextctl --json policy show";

#[derive(Debug, Parser)]
#[command(
    name = "contextctl",
    version,
    about = "Manage local, reviewed context for coding agents",
    long_about = "Universal Context Manager CLI. Human-readable output is the default; pass --json for stable typed JSON suitable for automation.",
    after_help = TOP_LEVEL_EXAMPLES,
    arg_required_else_help = true
)]
struct Cli {
    /// Emit typed JSON instead of human-readable output. Hook commands always emit their stable JSON contract.
    #[arg(long, global = true)]
    json: bool,

    /// Override the private SQLite database path.
    #[arg(long, global = true, env = "CONTEXT_DB_PATH")]
    db: Option<PathBuf>,

    /// Override the local contextd Unix socket path.
    #[arg(long, global = true, env = "CONTEXT_SOCKET_PATH")]
    socket: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Initialize private runtime paths and ensure contextd can start.
    #[command(after_help = "Example:\n  contextctl init")]
    Init,

    /// Show local data, database, socket, and spool paths.
    #[command(after_help = "Example:\n  contextctl paths")]
    Paths,

    /// Verify contextd reachability and summarize stored record counts.
    #[command(after_help = "Examples:\n  contextctl ping\n  contextctl --json ping")]
    Ping,

    /// Show schema and store counts without a full diagnostic pass.
    #[command(after_help = "Examples:\n  contextctl stats\n  contextctl --json stats")]
    Stats,

    /// Compose active global and project context, plus optional task context.
    Compose(ComposeCmd),

    /// Search active context in the resolved project and optional task scope.
    Search(SearchCmd),

    /// Submit a typed commit-work request from a file or stdin.
    CommitWork(CommitWorkCmd),

    /// Retry queued commit-work requests from the private spool.
    #[command(after_help = "Example:\n  contextctl retry-spool")]
    RetrySpool,

    /// Create, list, or update context packs.
    Pack {
        #[command(subcommand)]
        command: PackCommand,
    },

    /// Put, get, list, delete, or revert context entries.
    Entry {
        #[command(subcommand)]
        command: EntryCommand,
    },

    /// List and resolve review-gated proposals.
    Review {
        #[command(subcommand)]
        command: ReviewCommand,
    },

    /// Export a UCM bundle as JSON or marker-preserving Markdown.
    Export(ExportCmd),

    /// Import a UCM export bundle. Use source-import for ordinary instruction files.
    Import(ImportCmd),

    /// Preview or apply staged imports from instruction and rule files.
    SourceImport {
        #[command(subcommand)]
        command: SourceImportCommand,
    },

    /// Show or set the global review policy.
    Policy {
        #[command(subcommand)]
        command: PolicyCommand,
    },

    /// Create or list recorded agent/tool runs.
    Run {
        #[command(subcommand)]
        command: RunCommand,
    },

    /// Preflight a project, detect instruction files, and optionally apply safe setup changes.
    Setup(SetupCmd),

    /// Diagnose component version compatibility, daemon/storage health, MCP, spool, permissions, and adapters.
    Doctor(DoctorCmd),

    /// Generate a shell completion script for bash, zsh, or fish.
    Completion(CompletionCmd),

    /// Use stable hook-oriented JSON APIs for Codex and Claude Code adapters.
    Hook(HookCmd),
}

#[derive(Debug, Args)]
#[command(after_help = "\
Examples:
  contextctl compose
  contextctl compose --project /path/to/repo --task issue-123
  contextctl --json compose")]
struct ComposeCmd {
    /// Project scope ID. Defaults to the current Git root, then the current directory.
    #[arg(long)]
    project: Option<String>,

    /// Optional stable task or issue scope ID.
    #[arg(long)]
    task: Option<String>,

    /// Include entries from archived packs.
    #[arg(long, default_value_t = false)]
    include_archived: bool,
}

#[derive(Debug, Args)]
#[command(after_help = "\
Examples:
  contextctl search \"release checklist\"
  contextctl search auth --task issue-42 --limit 10")]
struct SearchCmd {
    /// Full-text query.
    query: String,

    /// Project scope ID. Defaults to the current Git root, then the current directory.
    #[arg(long)]
    project: Option<String>,

    /// Optional stable task or issue scope ID.
    #[arg(long)]
    task: Option<String>,

    /// Maximum number of matches.
    #[arg(long, default_value_t = 20)]
    limit: u32,
}

#[derive(Debug, Args)]
#[command(after_help = "\
Examples:
  contextctl commit-work --file request.json
  cat request.json | contextctl --json commit-work")]
struct CommitWorkCmd {
    /// JSON request file. Reads stdin when omitted.
    #[arg(long)]
    file: Option<PathBuf>,
}

#[derive(Debug, Subcommand)]
enum PackCommand {
    /// Create a pack in a global, project, or task scope.
    #[command(after_help = "\
Examples:
  contextctl pack create --scope project --name main
  contextctl pack create --scope global --name standards")]
    Create(PackCreateCmd),

    /// List every pack with its resolved scope and status.
    #[command(after_help = "Example:\n  contextctl pack list")]
    List,

    /// Update pack metadata, status, or lock state.
    #[command(after_help = "\
Example:
  contextctl pack update --scope project --name main --description \"Current project context\"")]
    Update(PackUpdateCmd),
}

#[derive(Debug, Args)]
struct PackCreateCmd {
    #[command(flatten)]
    scope: ScopeArgs,

    /// Pack name.
    #[arg(long, default_value = "main")]
    name: String,

    /// Optional human-readable description.
    #[arg(long)]
    description: Option<String>,

    /// Optional JSON metadata object.
    #[arg(long)]
    metadata: Option<String>,

    /// Create the pack locked.
    #[arg(long, default_value_t = false)]
    locked: bool,

    /// Explain why the pack is locked.
    #[arg(long)]
    lock_reason: Option<String>,

    /// Actor recorded in provenance.
    #[arg(long, default_value = "contextctl")]
    actor: String,
}

#[derive(Debug, Args)]
struct PackUpdateCmd {
    #[command(flatten)]
    scope: ScopeArgs,

    /// Pack name.
    #[arg(long, default_value = "main")]
    name: String,

    #[arg(long)]
    description: Option<String>,

    /// Replacement JSON metadata value.
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
    /// Create or replace one entry.
    #[command(after_help = "\
Examples:
  contextctl entry put --scope project --key build --body \"Run cargo test\"
  contextctl entry put --scope task --scope-id issue-42 --key handoff --body-file handoff.md")]
    Put(EntryPutCmd),

    /// Read one entry and its content.
    #[command(after_help = "Example:\n  contextctl entry get --scope project --key build")]
    Get(EntryGetCmd),

    /// List entries, optionally filtered by project, task, scope, or pack.
    #[command(after_help = "\
Examples:
  contextctl entry list --scope project
  contextctl entry list --project /path/to/repo --pack main")]
    List(EntryListCmd),

    /// Soft-delete one entry.
    #[command(after_help = "Example:\n  contextctl entry delete --scope project --key obsolete")]
    Delete(EntryDeleteCmd),

    /// Restore an entry to a selected or previous revision.
    #[command(after_help = "\
Examples:
  contextctl entry revert --scope project --key build
  contextctl entry revert --scope project --key build --revision 2")]
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
    /// Limit layered results to this project scope ID.
    #[arg(long)]
    project: Option<String>,

    /// Limit layered results to this task scope ID.
    #[arg(long)]
    task: Option<String>,

    /// Filter to one exact scope. A project scope ID defaults to the current Git root.
    #[arg(long)]
    scope: Option<ScopeKindArg>,

    /// Exact scope ID. Required for task; optional for project.
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
    /// List review items, optionally filtered by state.
    #[command(after_help = "\
Examples:
  contextctl review list
  contextctl review list --state pending")]
    List(ReviewListCmd),

    /// Approve a pending review item.
    #[command(after_help = "Example:\n  contextctl review approve <REVIEW_ID> --note \"Verified\"")]
    Approve(ReviewDecisionCmd),

    /// Reject a pending review item.
    #[command(after_help = "Example:\n  contextctl review reject <REVIEW_ID> --note \"Outdated\"")]
    Reject(ReviewDecisionCmd),

    /// Edit a pending proposal before approval.
    #[command(after_help = "\
Example:
  contextctl review edit <REVIEW_ID> --body \"Corrected durable fact\"")]
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
#[command(after_help = "\
Examples:
  contextctl export --format json --output context.json
  contextctl export --format markdown --scope project --output context.md")]
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
#[command(after_help = "\
Examples:
  contextctl import --format json --input context.json
  contextctl import --format markdown --input ucm-export.md

Ordinary Markdown is intentionally rejected here. Preview instruction files with:
  contextctl source-import preview AGENTS.md")]
struct ImportCmd {
    #[arg(long, value_enum)]
    format: ExportFormatArg,

    /// UCM bundle file. Reads stdin when omitted.
    #[arg(long)]
    input: Option<PathBuf>,

    #[arg(long, default_value = "contextctl")]
    actor: String,
}

#[derive(Debug, Subcommand)]
enum SourceImportCommand {
    /// Parse files and show candidates, conflicts, duplicates, and warnings without writing.
    #[command(after_help = "\
Examples:
  contextctl source-import preview AGENTS.md CLAUDE.md
  contextctl source-import preview notes.md --source-kind plain-markdown --pack imported")]
    Preview(SourceImportCmd),

    /// Re-run the preview contract and apply eligible candidates through review policy.
    #[command(after_help = "\
Examples:
  contextctl source-import apply AGENTS.md
  contextctl source-import apply rules.mdc --source-kind cursor-rule --scope project")]
    Apply(SourceImportCmd),
}

#[derive(Debug, Args)]
struct SourceImportCmd {
    /// Source files to read locally.
    #[arg(required = true, value_name = "FILE")]
    files: Vec<PathBuf>,

    /// Source parser. Auto detects well-known filenames and UCM bundles.
    #[arg(long, value_enum, default_value_t = SourceImportKindArg::Auto)]
    source_kind: SourceImportKindArg,

    /// Destination scope. Project is the safe default.
    #[arg(long, value_enum, default_value_t = ScopeKindArg::Project)]
    scope: ScopeKindArg,

    /// Destination scope ID. Project defaults to the Git root; task always requires a value.
    #[arg(long)]
    scope_id: Option<String>,

    /// Destination pack. Defaults to main.
    #[arg(long)]
    pack: Option<String>,

    #[arg(long, default_value = "contextctl")]
    actor: String,
}

#[derive(Debug, Subcommand)]
enum PolicyCommand {
    /// Show the active strict, balanced, or fast review policy.
    #[command(after_help = "Example:\n  contextctl policy show")]
    Show,

    /// Set the active review policy.
    #[command(after_help = "\
Examples:
  contextctl policy set strict
  contextctl --json policy set balanced")]
    Set(PolicySetCmd),
}

#[derive(Debug, Args)]
struct PolicySetCmd {
    mode: ReviewModeArg,

    #[arg(long, default_value = "contextctl")]
    actor: String,
}

#[derive(Debug, Subcommand)]
enum RunCommand {
    /// Record a run for an adapter or tool.
    #[command(after_help = "\
Example:
  contextctl run create --source codex --task issue-42")]
    Create(RunCreateCmd),

    /// List recorded runs.
    #[command(after_help = "Example:\n  contextctl run list")]
    List,
}

#[derive(Debug, Args)]
struct RunCreateCmd {
    #[arg(long)]
    id: Option<String>,

    /// Project scope ID. Defaults to the current Git root, then current directory.
    #[arg(long)]
    project: Option<String>,

    /// Optional stable task or issue ID.
    #[arg(long)]
    task: Option<String>,

    #[arg(long)]
    source: String,

    #[arg(long)]
    metadata: Option<String>,
}

#[derive(Debug, Args)]
#[command(after_help = "\
Examples:
  contextctl setup
  contextctl setup --adapter codex --adapter claude-code
  contextctl setup --source AGENTS.md --review-mode balanced
  contextctl setup --source AGENTS.md --review-mode strict --apply --yes

Without --apply, setup starts/verifies the local daemon but does not import sources,
change policy, invoke adapter CLIs, or edit third-party configuration.")]
struct SetupCmd {
    /// Adapter preflights to run. Repeat for both; defaults to both when omitted.
    #[arg(long, value_enum)]
    adapter: Vec<AdapterKind>,

    /// Repository directory. Defaults to the current Git root, then current directory.
    #[arg(long)]
    project: Option<String>,

    /// Additional source file to preview. Repeatable.
    #[arg(long, value_name = "FILE")]
    source: Vec<PathBuf>,

    /// Desired review mode. Planned unless --apply is present.
    #[arg(long, value_enum)]
    review_mode: Option<ReviewModeArg>,

    /// Apply eligible source imports and the requested review policy.
    #[arg(long, default_value_t = false)]
    apply: bool,

    /// Confirm non-interactive application. Required with --apply.
    #[arg(long, default_value_t = false)]
    yes: bool,
}

#[derive(Debug, Args)]
#[command(after_help = "\
Examples:
  contextctl doctor
  contextctl doctor --repair
  contextctl --json doctor

Repair only ensures UCM directories, starts/verifies contextd through existing
client behavior, and retries the UCM spool. It never edits Codex or Claude config.
Known component version mismatches fail explicitly; older unversioned daemons are degraded.")]
struct DoctorCmd {
    /// Perform safe UCM-owned repairs before rerunning checks.
    #[arg(long, default_value_t = false)]
    repair: bool,
}

#[derive(Debug, Args)]
#[command(after_help = "\
Examples:
  contextctl completion bash > ~/.local/share/bash-completion/completions/contextctl
  contextctl completion zsh > ~/.zfunc/_contextctl
  contextctl completion fish > ~/.config/fish/completions/contextctl.fish")]
struct CompletionCmd {
    shell: CompletionShell,
}

#[derive(Debug, Args)]
#[command(
    about = "Hook-oriented APIs with a stable JSON contract",
    long_about = "Supports the adapter wrapper form `contextctl hook --adapter ... --mode ... --payload-file ... --project-dir ...`, plus explicit subcommands. Hook output is always JSON regardless of global output mode.",
    after_help = "\
Examples:
  contextctl hook compose
  contextctl hook --adapter codex --mode session-start --payload-file event.json"
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
    /// Emit a typed ComposeResponse for hook consumers.
    Compose(ComposeCmd),

    /// Submit a typed CommitWorkRequest and emit CommitWorkResult JSON.
    Commit(CommitWorkCmd),

    /// Retry the spool and emit SpoolRetryReport JSON.
    RetrySpool,
}

#[derive(Debug, Args)]
struct ScopeArgs {
    /// Scope kind. Global ignores --scope-id; project derives it when omitted; task requires it.
    #[arg(long, value_enum)]
    scope: ScopeKindArg,

    /// Scope ID.
    #[arg(long)]
    scope_id: Option<String>,
}

#[derive(Debug, Args, Default)]
struct EntryContentArgs {
    #[arg(long, conflicts_with_all = ["body_file", "json_value", "json_file"])]
    body: Option<String>,

    #[arg(long, conflicts_with_all = ["body", "json_value", "json_file"])]
    body_file: Option<PathBuf>,

    #[arg(long, conflicts_with_all = ["body", "body_file", "json_file"])]
    json_value: Option<String>,

    #[arg(long, conflicts_with_all = ["body", "body_file", "json_value"])]
    json_file: Option<PathBuf>,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum ScopeKindArg {
    Global,
    Project,
    Task,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum ReviewStateArg {
    Pending,
    Approved,
    Rejected,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum ReviewModeArg {
    Strict,
    Balanced,
    Fast,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum ExportFormatArg {
    Json,
    Markdown,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum PackStatusArg {
    Active,
    Archived,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum SourceImportKindArg {
    Auto,
    UcmJson,
    UcmMarkdown,
    AgentsMd,
    ClaudeMd,
    CopilotInstructions,
    CursorRule,
    ContinueRule,
    PlainMarkdown,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    init_tracing();
    let client = build_client(&cli)?;
    let json_output = cli.json;

    match cli.command {
        Command::Init => {
            client.ensure_daemon()?;
            let value = json!({
                "db_path": client.config().paths.db_path,
                "socket_path": client.config().paths.socket_path,
                "spool_dir": client.config().paths.spool_dir,
            });
            if json_output {
                print_json(&value)?;
            } else {
                print_human(format!(
                    "Universal Context Manager is ready.\nDatabase: {}\nSocket: {}\nSpool: {}",
                    client.config().paths.db_path.display(),
                    client.config().paths.socket_path.display(),
                    client.config().paths.spool_dir.display()
                ));
            }
        }
        Command::Paths => {
            let value = json!({
                "data_dir": client.config().paths.data_dir,
                "db_path": client.config().paths.db_path,
                "socket_path": client.config().paths.socket_path,
                "spool_dir": client.config().paths.spool_dir,
            });
            if json_output {
                print_json(&value)?;
            } else {
                print_human(format!(
                    "Data: {}\nDatabase: {}\nSocket: {}\nSpool: {}",
                    client.config().paths.data_dir.display(),
                    client.config().paths.db_path.display(),
                    client.config().paths.socket_path.display(),
                    client.config().paths.spool_dir.display()
                ));
            }
        }
        Command::Ping => {
            let report = client.ping()?;
            emit(json_output, &report, || format_health(&report))?;
        }
        Command::Stats => {
            let stats = client.stats()?;
            emit(json_output, &stats, || format_stats(&stats))?;
        }
        Command::Compose(cmd) => handle_compose_command(&client, cmd, json_output)?,
        Command::Search(cmd) => handle_search_command(&client, cmd, json_output)?,
        Command::CommitWork(cmd) => {
            let payload = read_optional_input(cmd.file)?;
            let request: CommitWorkRequest = serde_json::from_str(&payload)?;
            let result = client.commit_work(request)?;
            emit(json_output, &result, || format_commit(&result))?;
        }
        Command::RetrySpool => {
            let report = client.retry_spool()?;
            emit(json_output, &report, || format_spool(&report))?;
        }
        Command::Pack { command } => handle_pack_command(&client, command, json_output)?,
        Command::Entry { command } => handle_entry_command(&client, command, json_output)?,
        Command::Review { command } => handle_review_command(&client, command, json_output)?,
        Command::Export(cmd) => handle_export_command(&client, cmd, json_output)?,
        Command::Import(cmd) => handle_import_command(&client, cmd, json_output)?,
        Command::SourceImport { command } => {
            handle_source_import_command(&client, command, json_output)?
        }
        Command::Policy { command } => handle_policy_command(&client, command, json_output)?,
        Command::Run { command } => handle_run_command(&client, command, json_output)?,
        Command::Setup(cmd) => handle_setup_command(&client, cmd, json_output)?,
        Command::Doctor(cmd) => {
            let report = run_doctor(&client, cmd.repair);
            emit(json_output, &report, || format_doctor_report(&report))?;
            ensure_report_succeeded(
                report.overall != DoctorState::Failed,
                "doctor reported failed state",
            )?;
        }
        Command::Completion(cmd) => {
            let completion = generate_completion(cmd.shell);
            if json_output {
                print_json(&completion)?;
            } else {
                print!("{}", completion.script);
            }
        }
        Command::Hook(cmd) => handle_hook_command(&client, cmd)?,
    }
    Ok(())
}

fn handle_compose_command(
    client: &ContextClient,
    cmd: ComposeCmd,
    json_output: bool,
) -> anyhow::Result<()> {
    let cwd = current_directory()?;
    let project = resolve_project_scope_id(cmd.project, &cwd)?;
    let task = normalize_task_id(cmd.task)?;
    let response = client.compose_context(ComposeRequest {
        project_scope_id: Some(project.clone()),
        task_scope_id: task.clone(),
        include_archived: cmd.include_archived,
    })?;
    emit(json_output, &response, || {
        format_compose(&response, &project, task.as_deref())
    })
}

fn handle_search_command(
    client: &ContextClient,
    cmd: SearchCmd,
    json_output: bool,
) -> anyhow::Result<()> {
    let cwd = current_directory()?;
    let project = resolve_project_scope_id(cmd.project, &cwd)?;
    let task = normalize_task_id(cmd.task)?;
    let response = client.search_context(SearchRequest {
        query: cmd.query,
        project_scope_id: Some(project.clone()),
        task_scope_id: task.clone(),
        limit: cmd.limit,
    })?;
    emit(json_output, &response, || {
        format_search(&response, &project, task.as_deref())
    })
}

fn handle_pack_command(
    client: &ContextClient,
    command: PackCommand,
    json_output: bool,
) -> anyhow::Result<()> {
    let cwd = current_directory()?;
    match command {
        PackCommand::Create(cmd) => {
            let pack = client.create_pack(CreatePackRequest {
                scope: cmd.scope.to_scope(&cwd)?,
                name: cmd.name,
                description: cmd.description,
                metadata: parse_json_object(cmd.metadata)?,
                locked: cmd.locked,
                lock_reason: cmd.lock_reason,
                actor: cmd.actor,
            })?;
            emit(json_output, &pack, || format_pack(&pack, "Created"))
        }
        PackCommand::List => {
            let packs = client.list_packs()?;
            emit(json_output, &packs, || format_packs(&packs))
        }
        PackCommand::Update(cmd) => {
            let pack = client.update_pack(UpdatePackRequest {
                selector: PackSelector {
                    scope: cmd.scope.to_scope(&cwd)?,
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
            })?;
            emit(json_output, &pack, || format_pack(&pack, "Updated"))
        }
    }
}

fn handle_entry_command(
    client: &ContextClient,
    command: EntryCommand,
    json_output: bool,
) -> anyhow::Result<()> {
    let cwd = current_directory()?;
    match command {
        EntryCommand::Put(cmd) => {
            let entry = client.put_entry(PutEntryRequest {
                scope: cmd.scope.to_scope(&cwd)?,
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
            })?;
            emit(json_output, &entry, || format_entry(&entry, "Stored"))
        }
        EntryCommand::Get(cmd) => {
            let entry = client.get_entry(EntrySelector {
                scope: cmd.scope.to_scope(&cwd)?,
                pack_name: cmd.pack,
                entry_key: cmd.key,
            })?;
            emit(json_output, &entry, || format_entry_detail(&entry))
        }
        EntryCommand::List(cmd) => {
            let resolved = export_request(
                cmd.project,
                cmd.task,
                cmd.scope,
                cmd.scope_id,
                cmd.pack,
                cmd.include_deleted,
                false,
                false,
                &cwd,
            )?;
            let entries = client.list_entries(resolved.request)?;
            emit(json_output, &entries, || {
                format_entries(&entries, resolved.exact_scope.as_ref())
            })
        }
        EntryCommand::Delete(cmd) => {
            let entry = client.delete_entry(DeleteEntryRequest {
                selector: EntrySelector {
                    scope: cmd.scope.to_scope(&cwd)?,
                    pack_name: cmd.pack,
                    entry_key: cmd.key,
                },
                actor: cmd.actor,
            })?;
            emit(json_output, &entry, || format_entry(&entry, "Deleted"))
        }
        EntryCommand::Revert(cmd) => {
            let entry = client.revert_entry(RevertEntryRequest {
                selector: EntrySelector {
                    scope: cmd.scope.to_scope(&cwd)?,
                    pack_name: cmd.pack,
                    entry_key: cmd.key,
                },
                revision_no: cmd.revision,
                actor: cmd.actor,
            })?;
            emit(json_output, &entry, || format_entry(&entry, "Reverted"))
        }
    }
}

fn handle_review_command(
    client: &ContextClient,
    command: ReviewCommand,
    json_output: bool,
) -> anyhow::Result<()> {
    match command {
        ReviewCommand::List(cmd) => {
            let reviews = client.review_list(cmd.state.map(Into::into))?;
            emit(json_output, &reviews, || format_reviews(&reviews))
        }
        ReviewCommand::Approve(cmd) => {
            let review = client.review_approve(ReviewDecisionRequest {
                review_id: cmd.review_id,
                actor: cmd.actor,
                note: cmd.note,
            })?;
            emit(json_output, &review, || format_review(&review, "Approved"))
        }
        ReviewCommand::Reject(cmd) => {
            let review = client.review_reject(ReviewDecisionRequest {
                review_id: cmd.review_id,
                actor: cmd.actor,
                note: cmd.note,
            })?;
            emit(json_output, &review, || format_review(&review, "Rejected"))
        }
        ReviewCommand::Edit(cmd) => {
            let review = client.review_edit(ReviewEditRequest {
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
            })?;
            emit(json_output, &review, || format_review(&review, "Updated"))
        }
    }
}

fn handle_export_command(
    client: &ContextClient,
    cmd: ExportCmd,
    json_output: bool,
) -> anyhow::Result<()> {
    if json_output && matches!(cmd.format, ExportFormatArg::Markdown) {
        return Err(anyhow::anyhow!(
            "--json cannot be combined with `export --format markdown`; use --format json for the typed ContextExportBundle"
        ));
    }
    let cwd = current_directory()?;
    let resolved = export_request(
        cmd.project,
        cmd.task,
        cmd.scope,
        cmd.scope_id,
        cmd.pack,
        cmd.include_deleted,
        cmd.include_reviews,
        cmd.include_runs,
        &cwd,
    )?;
    if json_output {
        let bundle = client.export_bundle(resolved.request)?;
        let text = serde_json::to_string_pretty(&bundle)?;
        write_text_output(cmd.output, &text)
    } else {
        match cmd.format {
            ExportFormatArg::Json => {
                write_text_output(cmd.output, &client.export_json(resolved.request)?)?
            }
            ExportFormatArg::Markdown => {
                write_text_output(cmd.output, &client.export_markdown(resolved.request)?)?
            }
        }
        Ok(())
    }
}

fn handle_import_command(
    client: &ContextClient,
    cmd: ImportCmd,
    json_output: bool,
) -> anyhow::Result<()> {
    let payload = read_optional_input(cmd.input)?;
    if matches!(cmd.format, ExportFormatArg::Markdown) {
        ensure_ucm_markdown_bundle(&payload)?;
    }
    let format = match cmd.format {
        ExportFormatArg::Json => ImportFormat::Json,
        ExportFormatArg::Markdown => ImportFormat::Markdown,
    };
    let bundle = client.import_data(ImportRequest {
        actor: cmd.actor,
        format,
        payload,
    })?;
    emit(json_output, &bundle, || format_bundle_import(&bundle))
}

fn handle_source_import_command(
    client: &ContextClient,
    command: SourceImportCommand,
    json_output: bool,
) -> anyhow::Result<()> {
    let cwd = current_directory()?;
    match command {
        SourceImportCommand::Preview(cmd) => {
            let request = source_preview_request(cmd, &cwd)?;
            let preview = client.preview_source_import(request)?;
            emit(json_output, &preview, || format_source_preview(&preview))
        }
        SourceImportCommand::Apply(cmd) => {
            let request = source_apply_request(cmd, &cwd)?;
            let result = client.apply_source_import(request)?;
            emit(json_output, &result, || format_source_apply(&result))
        }
    }
}

fn handle_policy_command(
    client: &ContextClient,
    command: PolicyCommand,
    json_output: bool,
) -> anyhow::Result<()> {
    match command {
        PolicyCommand::Show => {
            let policy = client.get_review_policy()?;
            emit(json_output, &policy, || format_policy(&policy, "Current"))
        }
        PolicyCommand::Set(cmd) => {
            let policy = client.set_review_policy(SetReviewPolicyRequest {
                mode: cmd.mode.into(),
                metadata: json!({}),
                actor: cmd.actor,
            })?;
            emit(json_output, &policy, || format_policy(&policy, "Updated"))
        }
    }
}

fn handle_run_command(
    client: &ContextClient,
    command: RunCommand,
    json_output: bool,
) -> anyhow::Result<()> {
    match command {
        RunCommand::Create(cmd) => {
            let cwd = current_directory()?;
            let project = resolve_project_scope_id(cmd.project, &cwd)?;
            let run = client.create_run(RunInput {
                id: cmd.id,
                project_scope_id: Some(project),
                task_scope_id: normalize_task_id(cmd.task)?,
                source: cmd.source,
                metadata: parse_json_object(cmd.metadata)?,
            })?;
            emit(json_output, &run, || format_run(&run, "Created"))
        }
        RunCommand::List => {
            let runs = client.list_runs()?;
            emit(json_output, &runs, || format_runs(&runs))
        }
    }
}

fn handle_setup_command(
    client: &ContextClient,
    cmd: SetupCmd,
    json_output: bool,
) -> anyhow::Result<()> {
    if cmd.apply && !cmd.yes {
        return Err(anyhow::anyhow!(
            "`contextctl setup --apply` requires --yes for non-interactive confirmation"
        ));
    }
    let cwd = current_directory()?;
    let project_dir = resolve_project_directory(cmd.project.as_deref(), &cwd)?;
    let report = run_setup(
        client,
        SetupOptions {
            project_dir,
            sources: cmd.source,
            adapters: cmd.adapter,
            review_mode: cmd.review_mode.map(Into::into),
            apply: cmd.apply,
        },
    );
    emit(json_output, &report, || format_setup_report(&report))?;
    ensure_report_succeeded(
        report.state != SetupState::Failed,
        "setup reported failed state",
    )
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
            let cwd = current_directory()?;
            let project = resolve_project_scope_id(compose.project, &cwd)?;
            let response = client.compose_context(ComposeRequest {
                project_scope_id: Some(project),
                task_scope_id: normalize_task_id(compose.task)?,
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
    let payload_text = match cmd.payload_file {
        Some(path) => fs::read_to_string(path)?,
        None => read_optional_input(None)?,
    };
    let payload_json = serde_json::from_str::<Value>(&payload_text)
        .unwrap_or_else(|_| json!({ "raw": payload_text }));

    let task_scope_id = payload_string(&payload_json, &["task_scope_id", "task_id", "task"]);

    match mode.as_str() {
        "session-start" => {
            let cwd = current_directory()?;
            let project_dir =
                resolve_hook_project_dir(&payload_json, cmd.project_dir.as_deref(), &cwd)?;
            let requested_run_id = payload_string(&payload_json, &["session_id", "session"]);
            let run = client.create_run(RunInput {
                id: requested_run_id,
                project_scope_id: Some(project_dir.clone()),
                task_scope_id: task_scope_id.clone(),
                source: adapter,
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
            if let Some(request) = extract_commit_request(&payload_json) {
                let _ = client.commit_work(request)?;
            }
            let _ = client.retry_spool()?;
        }
        "user-prompt-submit" | "pre-tool-use" | "post-tool-use" => {}
        other => return Err(anyhow::anyhow!("unsupported hook mode: {other}")),
    }

    let _ = (cmd.plugin_root, cmd.plugin_data);
    Ok(())
}

fn resolve_hook_project_dir(
    payload: &Value,
    explicit_project_dir: Option<&str>,
    cwd: &Path,
) -> anyhow::Result<String> {
    let selected = payload_string(payload, &["cwd", "project_dir"])
        .or_else(|| explicit_project_dir.map(ToString::to_string));
    Ok(resolve_project_directory(selected.as_deref(), cwd)?
        .display()
        .to_string())
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

fn source_preview_request(
    cmd: SourceImportCmd,
    cwd: &Path,
) -> anyhow::Result<SourceImportPreviewRequest> {
    let destination = resolve_scope(cmd.scope.into(), cmd.scope_id, cwd)?;
    Ok(SourceImportPreviewRequest {
        source_kind: cmd.source_kind.into(),
        documents: read_source_files(cmd.files, cwd, &destination)?,
        destination,
        pack_name: cmd.pack,
        actor: cmd.actor,
    })
}

fn source_apply_request(
    cmd: SourceImportCmd,
    cwd: &Path,
) -> anyhow::Result<SourceImportApplyRequest> {
    let destination = resolve_scope(cmd.scope.into(), cmd.scope_id, cwd)?;
    Ok(SourceImportApplyRequest {
        source_kind: cmd.source_kind.into(),
        documents: read_source_files(cmd.files, cwd, &destination)?,
        destination,
        pack_name: cmd.pack,
        actor: cmd.actor,
        expected_preview_fingerprint: None,
    })
}

fn read_source_files(
    files: Vec<PathBuf>,
    cwd: &Path,
    destination: &ScopeRef,
) -> anyhow::Result<Vec<SourceImportDocument>> {
    let paths = canonicalize_source_paths(files, cwd)?;
    let project_root = source_identity_project(destination, cwd);
    read_source_documents(&paths, project_root.as_deref())
}

fn source_identity_project(destination: &ScopeRef, cwd: &Path) -> Option<PathBuf> {
    if destination.kind == ScopeKind::Project {
        let project = PathBuf::from(&destination.id);
        if project.is_dir() {
            return project.canonicalize().ok();
        }
        return None;
    }
    default_project_directory(cwd).ok()
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

fn emit<T: Serialize>(
    json_output: bool,
    value: &T,
    human: impl FnOnce() -> String,
) -> anyhow::Result<()> {
    if json_output {
        print_json(value)
    } else {
        print_human(human());
        Ok(())
    }
}

fn ensure_report_succeeded(succeeded: bool, message: &str) -> anyhow::Result<()> {
    if succeeded {
        Ok(())
    } else {
        Err(anyhow::anyhow!(message.to_string()))
    }
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

fn normalize_task_id(task: Option<String>) -> anyhow::Result<Option<String>> {
    task.map(|task| {
        let task = task.trim().to_string();
        if task.is_empty() {
            Err(anyhow::anyhow!(
                "task ID cannot be empty; use a stable task or issue identifier"
            ))
        } else {
            Ok(task)
        }
    })
    .transpose()
}

struct ResolvedExportRequest {
    request: ExportRequest,
    exact_scope: Option<ScopeRef>,
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
    cwd: &Path,
) -> anyhow::Result<ResolvedExportRequest> {
    let task = normalize_task_id(task)?;
    let exact_scope = match scope {
        Some(ScopeKindArg::Global) => Some(ScopeRef::global()),
        Some(ScopeKindArg::Project) => Some(resolve_scope(
            ScopeKind::Project,
            scope_id.or_else(|| project.clone()),
            cwd,
        )?),
        Some(ScopeKindArg::Task) => Some(resolve_scope(
            ScopeKind::Task,
            scope_id.or_else(|| task.clone()),
            cwd,
        )?),
        None => {
            if scope_id.is_some() {
                return Err(anyhow::anyhow!("--scope-id requires --scope"));
            }
            None
        }
    };
    Ok(ResolvedExportRequest {
        request: ExportRequest {
            project_scope_id: project,
            task_scope_id: task,
            scope: exact_scope.clone(),
            pack_name: pack,
            include_deleted,
            include_reviews,
            include_runs,
        },
        exact_scope,
    })
}

fn ensure_ucm_markdown_bundle(payload: &str) -> anyhow::Result<()> {
    if payload.contains("<!-- UCM_ENTRY") {
        Ok(())
    } else {
        Err(anyhow::anyhow!(
            "ordinary Markdown is not a UCM export bundle (missing UCM_ENTRY markers); use `contextctl source-import preview <FILE>` and then `contextctl source-import apply <FILE>`"
        ))
    }
}

impl ScopeArgs {
    fn to_scope(&self, cwd: &Path) -> anyhow::Result<ScopeRef> {
        resolve_scope(self.scope.into(), self.scope_id.clone(), cwd)
    }
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
            (Some(_), Some(_)) => Err(anyhow::anyhow!("choose markdown or JSON content, not both")),
            (Some(body), None) => Ok(Some(EntryValue::Markdown { body })),
            (None, Some(value)) => Ok(Some(EntryValue::Json { value })),
            (None, None) => Ok(None),
        }
    }
}

impl From<ScopeKindArg> for ScopeKind {
    fn from(value: ScopeKindArg) -> Self {
        match value {
            ScopeKindArg::Global => Self::Global,
            ScopeKindArg::Project => Self::Project,
            ScopeKindArg::Task => Self::Task,
        }
    }
}

impl From<ReviewStateArg> for ReviewState {
    fn from(value: ReviewStateArg) -> Self {
        match value {
            ReviewStateArg::Pending => Self::Pending,
            ReviewStateArg::Approved => Self::Approved,
            ReviewStateArg::Rejected => Self::Rejected,
        }
    }
}

impl From<ReviewModeArg> for ReviewMode {
    fn from(value: ReviewModeArg) -> Self {
        match value {
            ReviewModeArg::Strict => Self::Strict,
            ReviewModeArg::Balanced => Self::Balanced,
            ReviewModeArg::Fast => Self::Fast,
        }
    }
}

impl From<SourceImportKindArg> for SourceImportKind {
    fn from(value: SourceImportKindArg) -> Self {
        match value {
            SourceImportKindArg::Auto => Self::Auto,
            SourceImportKindArg::UcmJson => Self::UcmJson,
            SourceImportKindArg::UcmMarkdown => Self::UcmMarkdown,
            SourceImportKindArg::AgentsMd => Self::AgentsMd,
            SourceImportKindArg::ClaudeMd => Self::ClaudeMd,
            SourceImportKindArg::CopilotInstructions => Self::CopilotInstructions,
            SourceImportKindArg::CursorRule => Self::CursorRule,
            SourceImportKindArg::ContinueRule => Self::ContinueRule,
            SourceImportKindArg::PlainMarkdown => Self::PlainMarkdown,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;
    use context_core::{ContextStore, SourceImportDisposition};
    use tempfile::{tempdir, tempdir_in};

    #[test]
    fn top_level_help_is_discoverable() {
        let help = Cli::command().render_long_help().to_string();
        for command in ["setup", "doctor", "policy", "source-import", "completion"] {
            assert!(help.contains(command), "missing {command}");
        }
        assert!(help.contains("Examples:"));
    }

    #[test]
    fn version_flag_is_available() {
        let error = Cli::try_parse_from(["contextctl", "--version"]).expect_err("version exits");
        assert_eq!(error.kind(), clap::error::ErrorKind::DisplayVersion);
        assert!(error.to_string().contains(env!("CARGO_PKG_VERSION")));
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
        let output = session_start_hook_output("/example/demo", "run-1", "# Context\nRemember me");
        assert_eq!(
            output["hookSpecificOutput"]["hookEventName"],
            "SessionStart"
        );
        let additional = output["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .expect("additional context");
        assert!(additional.contains("/example/demo"));
        assert!(additional.contains("run-1"));
        assert!(additional.contains("Remember me"));
        assert!(additional.contains("commit_work"));
        assert!(output.get("payload").is_none());
    }

    #[test]
    fn hook_contract_is_independent_of_global_json_flag() {
        let plain = Cli::try_parse_from(["contextctl", "hook", "compose"]).expect("plain");
        let json =
            Cli::try_parse_from(["contextctl", "--json", "hook", "compose"]).expect("json flag");
        assert!(!plain.json);
        assert!(json.json);
        assert_eq!(
            session_start_hook_output("/repo", "run", ""),
            session_start_hook_output("/repo", "run", "")
        );
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
    fn compatibility_hook_resolves_payload_subdirectory_to_git_root() {
        let workspace = tempdir_in(env!("CARGO_MANIFEST_DIR")).expect("workspace");
        let project = workspace.path().join("project");
        let nested = project.join("nested/work");
        let alias = workspace.path().join("project-alias");
        fs::create_dir_all(&nested).expect("nested");
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(&project)
            .args(["init", "--quiet"])
            .status()
            .expect("git init");
        assert!(status.success());
        std::os::unix::fs::symlink(&project, &alias).expect("project symlink");
        let aliased_nested = alias.join("nested/work");

        let payload = json!({ "cwd": aliased_nested });
        let hook_scope =
            resolve_hook_project_dir(&payload, Some("/ignored"), workspace.path()).expect("scope");
        let explicit_scope = resolve_project_scope_id(
            Some(alias.join("nested/work").display().to_string()),
            workspace.path(),
        )
        .expect("explicit project scope");
        assert_eq!(explicit_scope, hook_scope);
        assert_eq!(
            PathBuf::from(&hook_scope).canonicalize().expect("resolved"),
            project.canonicalize().expect("project")
        );
        let explicit = nested.display().to_string();
        let resolved = resolve_hook_project_dir(&json!({}), Some(&explicit), workspace.path())
            .expect("explicit scope");
        assert_eq!(
            PathBuf::from(resolved).canonicalize().expect("resolved"),
            project.canonicalize().expect("project")
        );
    }

    #[test]
    fn setup_and_direct_source_import_share_identity_and_preview_duplicate() {
        let workspace = tempdir_in(env!("CARGO_MANIFEST_DIR")).expect("workspace");
        let project = workspace.path().join("project");
        let nested = project.join("nested");
        fs::create_dir_all(&nested).expect("nested");
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(&project)
            .args(["init", "--quiet"])
            .status()
            .expect("git init");
        assert!(status.success());
        fs::write(project.join("AGENTS.md"), "# Instructions\n\nBe precise.").expect("source");
        let project = project.canonicalize().expect("project canonical");

        let setup_paths =
            crate::setup::collect_setup_sources(&project, &[]).expect("setup sources");
        let setup_documents =
            read_source_documents(&setup_paths, Some(&project)).expect("setup documents");
        assert_eq!(setup_documents[0].path.as_deref(), Some("AGENTS.md"));

        let direct = source_preview_request(
            SourceImportCmd {
                files: vec![PathBuf::from("../AGENTS.md")],
                source_kind: SourceImportKindArg::Auto,
                scope: ScopeKindArg::Project,
                scope_id: None,
                pack: None,
                actor: "contextctl".to_string(),
            },
            &nested,
        )
        .expect("direct request");
        assert_eq!(direct.documents[0].path.as_deref(), Some("AGENTS.md"));

        let store = ContextStore::open_in_memory().expect("store");
        store
            .apply_source_import(SourceImportApplyRequest {
                source_kind: SourceImportKind::Auto,
                documents: setup_documents,
                destination: direct.destination.clone(),
                pack_name: None,
                actor: "contextctl-setup".to_string(),
                expected_preview_fingerprint: None,
            })
            .expect("setup apply");
        let preview = store.preview_source_import(direct).expect("direct preview");
        assert_eq!(
            preview.candidates[0].disposition,
            SourceImportDisposition::Duplicate
        );
    }

    #[test]
    fn ordinary_markdown_import_points_to_staged_import() {
        let error =
            ensure_ucm_markdown_bundle("# Ordinary Markdown").expect_err("ordinary markdown");
        assert!(error.to_string().contains("source-import preview"));
        assert!(error.to_string().contains("UCM_ENTRY"));
    }

    #[test]
    fn project_scope_defaults_for_existing_scope_commands() {
        let dir = tempdir().expect("tempdir");
        let args = ScopeArgs {
            scope: ScopeKindArg::Project,
            scope_id: None,
        };
        let scope = args.to_scope(dir.path()).expect("scope");
        assert_eq!(scope.kind, ScopeKind::Project);
        assert!(!scope.id.is_empty());
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

    #[test]
    fn failed_report_returns_an_error_after_output() {
        let error = ensure_report_succeeded(false, "doctor reported failed state")
            .expect_err("failed report");
        assert_eq!(error.to_string(), "doctor reported failed state");
    }
}
