use crate::error::{AppError, Result};
use crate::gemini::GeminiClient;
use crate::settings;
use crate::state::{AppState, BookRef};
use crate::types::{valid_mode, BookInfo, JobOptions, LoadedBook};
use serde::Serialize;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use tauri::State;

#[tauri::command]
pub fn get_settings(state: State<'_, AppState>) -> settings::Settings {
    state.settings.read().unwrap().clone()
}

#[tauri::command]
pub fn save_settings(state: State<'_, AppState>, new_settings: settings::Settings) -> Result<()> {
    settings::save(&state.settings_path, &new_settings)?;
    *state.settings.write().unwrap() = new_settings;
    Ok(())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TestKeyResult {
    pub reply: String,
    pub model: String,
    pub latency_ms: u128,
}

#[tauri::command]
pub async fn test_api_key(api_key: String, model: String) -> Result<TestKeyResult> {
    if api_key.trim().is_empty() {
        return Err(AppError::msg("enter an API key first"));
    }
    let client = GeminiClient::new(api_key.trim(), &model);
    let start = std::time::Instant::now();
    let reply = client.ping().await?;
    Ok(TestKeyResult {
        reply,
        model,
        latency_ms: start.elapsed().as_millis(),
    })
}

fn parse_book(path: &str) -> Result<LoadedBook> {
    let bytes = std::fs::read(path)
        .map_err(|e| AppError::msg(format!("could not read file: {e}")))?;
    if crate::epub::is_epub(&bytes) {
        return crate::epub::parse(bytes, path);
    }
    if crate::pdf::is_pdf(&bytes) {
        return crate::pdf::parse(bytes, path);
    }
    Err(AppError::msg("unsupported file — pick an EPUB or PDF"))
}

#[tauri::command]
pub async fn inspect_book(
    state: State<'_, AppState>,
    path: String,
) -> Result<BookInfo> {
    if state.job_is_running() {
        return Err(AppError::msg("a translation is running — cancel it first"));
    }
    let p = path.clone();
    let book = tauri::async_runtime::spawn_blocking(move || parse_book(&p))
        .await
        .map_err(|e| AppError::msg(format!("parse task failed: {e}")))??;
    let info = book.info.clone();
    *state.book.lock().unwrap() = Some(Arc::new(Mutex::new(book)));
    *state.output.lock().unwrap() = None;
    Ok(info)
}

#[tauri::command]
pub async fn start_job(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    options: JobOptions,
) -> Result<()> {
    if state.job_is_running() {
        return Err(AppError::msg("a job is already running"));
    }
    if !valid_mode(&options.mode) {
        return Err(AppError::msg("invalid mode"));
    }
    if options.target_lang.trim().is_empty() {
        return Err(AppError::msg("choose a target language"));
    }
    let api_key = state.settings.read().unwrap().api_key.clone();
    if api_key.trim().is_empty() {
        return Err(AppError::msg("set your Google AI Studio API key in Settings first"));
    }

    let book: BookRef = state
        .book
        .lock()
        .unwrap()
        .clone()
        .ok_or_else(|| AppError::msg("load a book first"))?;

    state.job_cancel.store(false, Ordering::SeqCst);
    state.job_running.store(true, Ordering::SeqCst);
    let data_dir = state.data_dir.clone();
    let cancel = state.job_cancel.clone();
    let running = state.job_running.clone();
    let output = state.output.clone();

    tauri::async_runtime::spawn(async move {
        crate::job::run(app, book, options, api_key, data_dir, cancel, running, output).await;
    });
    Ok(())
}

#[tauri::command]
pub fn cancel_job(state: State<'_, AppState>) -> Result<()> {
    if state.job_is_running() {
        state.job_cancel.store(true, Ordering::SeqCst);
    }
    Ok(())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveResult {
    pub path: String,
    pub bytes: usize,
}

#[tauri::command]
pub fn save_output(state: State<'_, AppState>, path: String) -> Result<SaveResult> {
    let bytes = state
        .output
        .lock()
        .unwrap()
        .clone()
        .ok_or_else(|| AppError::msg("no translated book available yet"))?;
    let mut path = path;
    if !path.to_lowercase().ends_with(".epub") {
        path.push_str(".epub");
    }
    std::fs::write(&path, &bytes).map_err(|e| AppError::msg(format!("could not save: {e}")))?;
    Ok(SaveResult {
        path,
        bytes: bytes.len(),
    })
}

#[tauri::command]
pub fn get_current_book(state: State<'_, AppState>) -> Option<BookInfo> {
    // While a job runs the book is still reachable through the shared Arc.
    state
        .book
        .lock()
        .unwrap()
        .as_ref()
        .map(|b| b.lock().unwrap().info.clone())
}
