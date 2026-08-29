use tauri::{AppHandle, State};
use tauri_plugin_dialog::DialogExt;

use crate::{
    client::{desktop_error, DesktopContextClient},
    models::*,
};

type CommandResult<T> = Result<T, String>;
type ApiCommandResult<T> = Result<T, DesktopError>;

async fn spawn_sync<T, F>(work: F) -> CommandResult<T>
where
    T: Send + 'static,
    F: FnOnce() -> CommandResult<T> + Send + 'static,
{
    tauri::async_runtime::spawn_blocking(work)
        .await
        .map_err(|error| error.to_string())?
}

async fn spawn_api<T, F>(work: F) -> ApiCommandResult<T>
where
    T: Send + 'static,
    F: FnOnce() -> CommandResult<T> + Send + 'static,
{
    tauri::async_runtime::spawn_blocking(work)
        .await
        .map_err(|error| DesktopError {
            code: DesktopErrorCode::Internal,
            message: error.to_string(),
            retryable: false,
        })?
        .map_err(desktop_error)
}

fn dialog_error(message: impl Into<String>) -> DesktopError {
    DesktopError {
        code: DesktopErrorCode::Internal,
        message: message.into(),
        retryable: false,
    }
}

#[tauri::command]
pub async fn select_project_directory(
    app: AppHandle,
    client: State<'_, DesktopContextClient>,
) -> ApiCommandResult<Option<PathGrantSelection>> {
    let selected =
        tauri::async_runtime::spawn_blocking(move || app.dialog().file().blocking_pick_folder())
            .await
            .map_err(|error| dialog_error(error.to_string()))?;
    let Some(selected) = selected else {
        return Ok(None);
    };
    let path = selected
        .into_path()
        .map_err(|error| dialog_error(error.to_string()))?;
    client
        .issue_path_grant(PathGrantPurpose::ProjectRegistration, vec![path])
        .map(Some)
        .map_err(desktop_error)
}

#[tauri::command]
pub async fn select_source_import_files(
    app: AppHandle,
    client: State<'_, DesktopContextClient>,
) -> ApiCommandResult<Option<PathGrantSelection>> {
    let selected =
        tauri::async_runtime::spawn_blocking(move || app.dialog().file().blocking_pick_files())
            .await
            .map_err(|error| dialog_error(error.to_string()))?;
    let Some(selected) = selected else {
        return Ok(None);
    };
    let paths = selected
        .into_iter()
        .map(|path| path.into_path().map_err(|error| error.to_string()))
        .collect::<Result<Vec<_>, _>>()
        .map_err(dialog_error)?;
    client
        .issue_path_grant(PathGrantPurpose::SourceImportPreview, paths)
        .map(Some)
        .map_err(desktop_error)
}

#[tauri::command]
pub async fn select_bundle_import_file(
    app: AppHandle,
    client: State<'_, DesktopContextClient>,
) -> ApiCommandResult<Option<PathGrantSelection>> {
    let selected =
        tauri::async_runtime::spawn_blocking(move || app.dialog().file().blocking_pick_file())
            .await
            .map_err(|error| dialog_error(error.to_string()))?;
    let Some(selected) = selected else {
        return Ok(None);
    };
    let path = selected
        .into_path()
        .map_err(|error| dialog_error(error.to_string()))?;
    client
        .issue_path_grant(PathGrantPurpose::BundleImportPreview, vec![path])
        .map(Some)
        .map_err(desktop_error)
}

#[tauri::command]
pub async fn select_export_destination(
    app: AppHandle,
    client: State<'_, DesktopContextClient>,
) -> ApiCommandResult<Option<PathGrantSelection>> {
    let selected =
        tauri::async_runtime::spawn_blocking(move || app.dialog().file().blocking_save_file())
            .await
            .map_err(|error| dialog_error(error.to_string()))?;
    let Some(selected) = selected else {
        return Ok(None);
    };
    let path = selected
        .into_path()
        .map_err(|error| dialog_error(error.to_string()))?;
    client
        .issue_path_grant(PathGrantPurpose::ExportArchive, vec![path])
        .map(Some)
        .map_err(desktop_error)
}

#[tauri::command]
pub async fn load_dashboard(
    client: State<'_, DesktopContextClient>,
) -> CommandResult<DashboardSnapshot> {
    let client = client.inner().clone();
    spawn_sync(move || client.load_dashboard()).await
}

#[tauri::command]
pub async fn list_packs(
    client: State<'_, DesktopContextClient>,
    scope_id: Option<String>,
) -> CommandResult<Vec<ContextPack>> {
    let client = client.inner().clone();
    spawn_sync(move || client.list_packs(scope_id)).await
}

#[tauri::command]
pub async fn save_pack(
    client: State<'_, DesktopContextClient>,
    input: SavePackInput,
) -> CommandResult<ContextPack> {
    let client = client.inner().clone();
    spawn_sync(move || client.save_pack(input)).await
}

#[tauri::command]
pub async fn list_entries(
    client: State<'_, DesktopContextClient>,
    scope_id: Option<String>,
    pack_id: Option<String>,
) -> ApiCommandResult<Vec<ContextEntry>> {
    let client = client.inner().clone();
    spawn_api(move || client.list_entries(scope_id, pack_id)).await
}

#[tauri::command]
pub async fn save_entry(
    client: State<'_, DesktopContextClient>,
    input: SaveEntryInput,
) -> ApiCommandResult<ContextEntry> {
    let client = client.inner().clone();
    spawn_api(move || client.save_entry(input)).await
}

#[tauri::command]
pub async fn archive_entry(
    client: State<'_, DesktopContextClient>,
    entry_id: String,
) -> ApiCommandResult<ContextEntry> {
    let client = client.inner().clone();
    spawn_api(move || client.archive_entry(entry_id)).await
}

#[tauri::command]
pub async fn delete_entry(
    client: State<'_, DesktopContextClient>,
    entry_id: String,
) -> ApiCommandResult<ContextEntry> {
    let client = client.inner().clone();
    spawn_api(move || client.archive_entry(entry_id)).await
}

#[tauri::command]
pub async fn restore_entry(
    client: State<'_, DesktopContextClient>,
    entry_id: String,
) -> ApiCommandResult<ContextEntry> {
    let client = client.inner().clone();
    spawn_api(move || client.restore_entry(entry_id)).await
}

#[tauri::command]
pub async fn revert_entry_revision(
    client: State<'_, DesktopContextClient>,
    input: RevertEntryInput,
) -> ApiCommandResult<ContextEntry> {
    let client = client.inner().clone();
    spawn_api(move || client.revert_entry_revision(input)).await
}

#[tauri::command]
pub async fn compose_preview(
    client: State<'_, DesktopContextClient>,
    scope_id: String,
) -> CommandResult<ContextPreview> {
    let client = client.inner().clone();
    spawn_sync(move || client.compose_preview(scope_id)).await
}

#[tauri::command]
pub async fn compose_effective_context(
    client: State<'_, DesktopContextClient>,
    input: ComposeContextInput,
) -> ApiCommandResult<ContextPreview> {
    let client = client.inner().clone();
    spawn_api(move || client.compose_effective_context(input)).await
}

#[tauri::command]
pub async fn search_index(
    client: State<'_, DesktopContextClient>,
    query: String,
) -> CommandResult<Vec<SearchResult>> {
    let client = client.inner().clone();
    spawn_sync(move || client.search_index(query)).await
}

#[tauri::command]
pub async fn list_revisions(
    client: State<'_, DesktopContextClient>,
    entity_id: Option<String>,
) -> CommandResult<Vec<RevisionEntry>> {
    let client = client.inner().clone();
    spawn_sync(move || client.list_revisions(entity_id)).await
}

#[tauri::command]
pub async fn review_decision(
    client: State<'_, DesktopContextClient>,
    input: ReviewDecisionInput,
) -> CommandResult<()> {
    let client = client.inner().clone();
    spawn_sync(move || client.review_decision(input)).await
}

#[tauri::command]
pub async fn bulk_review_decision(
    client: State<'_, DesktopContextClient>,
    input: BulkReviewDecisionInput,
) -> ApiCommandResult<BulkReviewDecisionResult> {
    let client = client.inner().clone();
    spawn_api(move || client.bulk_review_decision(input)).await
}

#[tauri::command]
pub async fn set_review_policy(
    client: State<'_, DesktopContextClient>,
    input: SetReviewPolicyInput,
) -> ApiCommandResult<ReviewPolicy> {
    let client = client.inner().clone();
    spawn_api(move || client.set_review_policy(input)).await
}

#[tauri::command]
pub async fn restore_revision(
    client: State<'_, DesktopContextClient>,
    revision_id: String,
) -> CommandResult<RestoreRevisionResult> {
    let client = client.inner().clone();
    spawn_sync(move || client.restore_revision(revision_id)).await
}

#[tauri::command]
pub async fn list_adapters(
    client: State<'_, DesktopContextClient>,
) -> CommandResult<Vec<AdapterStatus>> {
    let client = client.inner().clone();
    spawn_sync(move || client.list_adapters()).await
}

#[tauri::command]
pub async fn toggle_adapter(
    client: State<'_, DesktopContextClient>,
    adapter_id: String,
    enabled: bool,
) -> CommandResult<AdapterStatus> {
    let client = client.inner().clone();
    spawn_sync(move || client.toggle_adapter(adapter_id, enabled)).await
}

#[tauri::command]
pub async fn load_diagnostics(
    client: State<'_, DesktopContextClient>,
) -> ApiCommandResult<DiagnosticsReport> {
    let client = client.inner().clone();
    spawn_api(move || client.load_diagnostics()).await
}

#[tauri::command]
pub async fn refresh_diagnostics(
    client: State<'_, DesktopContextClient>,
) -> ApiCommandResult<DiagnosticsReport> {
    let client = client.inner().clone();
    spawn_api(move || client.load_diagnostics()).await
}

#[tauri::command]
pub async fn start_daemon(
    client: State<'_, DesktopContextClient>,
) -> ApiCommandResult<DaemonControlResult> {
    let client = client.inner().clone();
    spawn_api(move || client.start_daemon()).await
}

#[tauri::command]
pub async fn restart_daemon(
    client: State<'_, DesktopContextClient>,
) -> ApiCommandResult<DaemonControlResult> {
    let client = client.inner().clone();
    spawn_api(move || client.restart_daemon()).await
}

#[tauri::command]
pub async fn retry_spool(
    client: State<'_, DesktopContextClient>,
) -> ApiCommandResult<SpoolRetryResult> {
    let client = client.inner().clone();
    spawn_api(move || client.retry_spool()).await
}

#[tauri::command]
pub async fn load_settings(client: State<'_, DesktopContextClient>) -> CommandResult<Settings> {
    let client = client.inner().clone();
    spawn_sync(move || client.load_settings()).await
}

#[tauri::command]
pub async fn save_settings(
    client: State<'_, DesktopContextClient>,
    settings: Settings,
) -> CommandResult<Settings> {
    let client = client.inner().clone();
    spawn_sync(move || client.save_settings(settings)).await
}

#[tauri::command]
pub async fn complete_onboarding(
    client: State<'_, DesktopContextClient>,
) -> ApiCommandResult<OnboardingState> {
    let client = client.inner().clone();
    spawn_api(move || client.complete_onboarding()).await
}

#[tauri::command]
pub async fn reset_onboarding(
    client: State<'_, DesktopContextClient>,
) -> ApiCommandResult<OnboardingState> {
    let client = client.inner().clone();
    spawn_api(move || client.reset_onboarding()).await
}

#[tauri::command]
pub async fn register_project(
    client: State<'_, DesktopContextClient>,
    path: String,
    grant_token: Option<String>,
) -> ApiCommandResult<ProjectRegistration> {
    let client = client.inner().clone();
    spawn_api(move || client.register_project(path, grant_token)).await
}

#[tauri::command]
pub async fn set_selected_scope(
    client: State<'_, DesktopContextClient>,
    scope_id: String,
    project_path: Option<String>,
) -> ApiCommandResult<Settings> {
    let client = client.inner().clone();
    spawn_api(move || client.set_selected_scope(scope_id, project_path)).await
}

#[tauri::command]
pub async fn preview_source_import(
    client: State<'_, DesktopContextClient>,
    input: SourceImportPreviewInput,
) -> ApiCommandResult<SourceImportPreviewResult> {
    let client = client.inner().clone();
    spawn_api(move || client.preview_source_import(input)).await
}

#[tauri::command]
pub async fn apply_source_import(
    client: State<'_, DesktopContextClient>,
    input: SourceImportApplyInput,
) -> ApiCommandResult<SourceImportApplyResult> {
    let client = client.inner().clone();
    spawn_api(move || client.apply_source_import(input)).await
}

#[tauri::command]
pub async fn preview_bundle_import(
    client: State<'_, DesktopContextClient>,
    path: String,
    grant_token: Option<String>,
) -> ApiCommandResult<BundleImportPreview> {
    let client = client.inner().clone();
    spawn_api(move || client.preview_bundle_import(path, grant_token)).await
}

#[tauri::command]
pub async fn apply_bundle_import(
    client: State<'_, DesktopContextClient>,
    input: BundleImportApplyInput,
) -> ApiCommandResult<ImportExportSummary> {
    let client = client.inner().clone();
    spawn_api(move || client.apply_bundle_import(input)).await
}

#[tauri::command]
pub async fn load_privacy_summary(
    client: State<'_, DesktopContextClient>,
) -> ApiCommandResult<PrivacySummary> {
    let client = client.inner().clone();
    spawn_api(move || client.load_privacy_summary()).await
}

#[tauri::command]
pub async fn forget_scope(
    client: State<'_, DesktopContextClient>,
    input: ForgetScopeInput,
) -> ApiCommandResult<ForgetScopeResult> {
    let client = client.inner().clone();
    spawn_api(move || client.forget_scope(input)).await
}

#[tauri::command]
pub async fn archive_scope(
    client: State<'_, DesktopContextClient>,
    input: ForgetScopeInput,
) -> ApiCommandResult<ForgetScopeResult> {
    let client = client.inner().clone();
    spawn_api(move || client.forget_scope(input)).await
}

#[tauri::command]
pub async fn export_archive(
    client: State<'_, DesktopContextClient>,
    path: String,
    grant_token: Option<String>,
) -> ApiCommandResult<ImportExportSummary> {
    let client = client.inner().clone();
    spawn_api(move || client.export_archive(path, grant_token)).await
}
