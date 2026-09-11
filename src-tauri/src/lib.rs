mod commands;
mod error;
mod epub;
mod gemini;
mod job;
mod pdf;
mod settings;
mod state;
mod types;

use state::AppState;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, RwLock};
use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let handle = app.handle().clone();
            let config_dir = handle.path().app_config_dir()?;
            let data_dir = handle.path().app_data_dir()?;
            std::fs::create_dir_all(&config_dir)?;
            std::fs::create_dir_all(&data_dir)?;
            let settings_path = config_dir.join("settings.json");
            let loaded = settings::load(&settings_path).unwrap_or_else(|e| {
                eprintln!("settings: {e}");
                Default::default()
            });
            app.manage(AppState {
                settings: RwLock::new(loaded),
                settings_path,
                data_dir,
                book: Mutex::new(None),
                output: Arc::new(Mutex::new(None)),
                job_cancel: Arc::new(AtomicBool::new(false)),
                job_running: Arc::new(AtomicBool::new(false)),
                progress: Arc::new(Mutex::new(None)),
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_settings,
            commands::save_settings,
            commands::test_api_key,
            commands::inspect_book,
            commands::start_job,
            commands::cancel_job,
            commands::save_output,
            commands::get_current_book,
            commands::get_job_progress,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
