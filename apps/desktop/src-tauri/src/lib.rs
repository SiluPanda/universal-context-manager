mod client;
mod commands;
mod models;

use client::DesktopContextClient;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
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
            commands::load_dashboard,
            commands::list_packs,
            commands::save_pack,
            commands::compose_preview,
            commands::search_index,
            commands::list_revisions,
            commands::review_decision,
            commands::restore_revision,
            commands::list_adapters,
            commands::toggle_adapter,
            commands::load_settings,
            commands::save_settings,
            commands::export_archive,
            commands::import_archive,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
