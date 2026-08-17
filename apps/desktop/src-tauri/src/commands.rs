use tauri::State;

use crate::{client::DesktopContextClient, models::*};

type CommandResult<T> = Result<T, String>;

async fn spawn_sync<T, F>(work: F) -> CommandResult<T>
where
    T: Send + 'static,
    F: FnOnce() -> CommandResult<T> + Send + 'static,
{
    tauri::async_runtime::spawn_blocking(work)
        .await
        .map_err(|error| error.to_string())?
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
pub async fn compose_preview(
    client: State<'_, DesktopContextClient>,
    scope_id: String,
) -> CommandResult<ContextPreview> {
    let client = client.inner().clone();
    spawn_sync(move || client.compose_preview(scope_id)).await
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
pub async fn export_archive(
    client: State<'_, DesktopContextClient>,
    path: String,
) -> CommandResult<ImportExportSummary> {
    let client = client.inner().clone();
    spawn_sync(move || client.export_archive(path)).await
}

#[tauri::command]
pub async fn import_archive(
    client: State<'_, DesktopContextClient>,
    path: String,
) -> CommandResult<ImportExportSummary> {
    let client = client.inner().clone();
    spawn_sync(move || client.import_archive(path)).await
}
