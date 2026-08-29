mod client;
mod commands;
mod diagnostics;
mod grants;
mod models;

use client::DesktopContextClient;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(DesktopContextClient::new().expect("desktop context service initialization failed"))
        .setup(|app| {
            if cfg!(debug_assertions) {
                app.handle().plugin(
                    tauri_plugin_log::Builder::default()
                        .level(log::LevelFilter::Info)
                        .build(),
                )?;
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::select_project_directory,
            commands::select_source_import_files,
            commands::select_bundle_import_file,
            commands::select_export_destination,
            commands::load_dashboard,
            commands::list_packs,
            commands::save_pack,
            commands::list_entries,
            commands::save_entry,
            commands::archive_entry,
            commands::delete_entry,
            commands::restore_entry,
            commands::revert_entry_revision,
            commands::compose_preview,
            commands::compose_effective_context,
            commands::search_index,
            commands::list_revisions,
            commands::review_decision,
            commands::bulk_review_decision,
            commands::set_review_policy,
            commands::restore_revision,
            commands::list_adapters,
            commands::toggle_adapter,
            commands::load_diagnostics,
            commands::refresh_diagnostics,
            commands::start_daemon,
            commands::restart_daemon,
            commands::retry_spool,
            commands::load_settings,
            commands::save_settings,
            commands::complete_onboarding,
            commands::reset_onboarding,
            commands::register_project,
            commands::set_selected_scope,
            commands::preview_source_import,
            commands::apply_source_import,
            commands::preview_bundle_import,
            commands::apply_bundle_import,
            commands::load_privacy_summary,
            commands::forget_scope,
            commands::archive_scope,
            commands::export_archive,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
