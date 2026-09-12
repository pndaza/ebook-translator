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
    *state.progress.lock().unwrap() = None;
    Ok(info)
}

#[tauri::command]
pub async fn start_job(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    options: JobOptions,
) -> Result<()> {
    // Claim the job slot atomically — a check-then-set here let a double
    // click spawn two concurrent jobs over the same book and log.
    if state
        .job_running
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return Err(AppError::msg("a job is already running"));
    }
    let release = |state: &AppState| state.job_running.store(false, Ordering::SeqCst);
    if !valid_mode(&options.mode) {
        release(&state);
        return Err(AppError::msg("invalid mode"));
    }
    if options.target_lang.trim().is_empty() {
        release(&state);
        return Err(AppError::msg("choose a target language"));
    }
    let api_key = state.settings.read().unwrap().api_key.clone();
    if api_key.trim().is_empty() {
        release(&state);
        return Err(AppError::msg("set your Google AI Studio API key in Settings first"));
    }

    let book: BookRef = match state.book.lock().unwrap().clone() {
        Some(b) => b,
        None => {
            release(&state);
            return Err(AppError::msg("load a book first"));
        }
    };

    state.job_cancel.store(false, Ordering::SeqCst);
    let data_dir = state.data_dir.clone();
    let cancel = state.job_cancel.clone();
    let running = state.job_running.clone();
    let output = state.output.clone();
    let progress = state.progress.clone();
    let cache = state.cache.clone();
    let usage = state.usage.clone();
    let auto_switch = state.settings.read().unwrap().auto_switch_model;

    tauri::async_runtime::spawn(async move {
        crate::job::run(
            app, book, options, api_key, data_dir, cache, usage, auto_switch, cancel, running,
            output, progress,
        )
        .await;
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

/// Latest job-progress snapshot, so a webview reload can catch up without
/// waiting for the next event.
#[tauri::command]
pub fn get_job_progress(state: State<'_, AppState>) -> Option<crate::types::JobProgress> {
    state.progress.lock().unwrap().clone()
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EstimateResult {
    /// Requests the job will actually send (fully cached batches excluded).
    pub requests: usize,
    /// Batches every segment of which is already in the translation cache.
    pub cached_batches: usize,
}

/// Number of API requests the loaded book needs with the given model,
/// computed with the same batching the job will run and discounted by
/// whatever the persistent translation cache already covers.
#[tauri::command]
pub async fn estimate_requests(
    state: State<'_, AppState>,
    model: String,
    target_lang: String,
    custom_instructions: String,
) -> Result<EstimateResult> {
    let book = state
        .book
        .lock()
        .unwrap()
        .clone()
        .ok_or_else(|| AppError::msg("load a book first"))?;
    let (batches, prefix) = {
        let prefix = crate::cache::TranslationCache::prefix(&model, &target_lang, &custom_instructions);
        let batches = tauri::async_runtime::spawn_blocking(move || {
            let book = book.lock().unwrap();
            let docs = match &book.source {
                crate::types::BookSource::Epub { docs, .. }
                | crate::types::BookSource::Pdf { docs, .. } => docs,
            };
            crate::job::build_batches(docs, &model)
        })
        .await
        .map_err(|e| AppError::msg(format!("estimate task failed: {e}")))?;
        (batches, prefix)
    };
    let cache = state.cache.lock().unwrap();
    let cached_batches = batches
        .iter()
        .filter(|b| b.items.iter().all(|i| cache.get(&prefix, &i.text).is_some()))
        .count();
    Ok(EstimateResult {
        requests: batches.len() - cached_batches,
        cached_batches,
    })
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CacheStatsResult {
    pub entries: usize,
    pub bytes: u64,
}

#[tauri::command]
pub fn cache_stats(state: State<'_, AppState>) -> CacheStatsResult {
    let (entries, bytes) = state.cache.lock().unwrap().stats();
    CacheStatsResult { entries, bytes }
}

#[tauri::command]
pub fn clear_cache(state: State<'_, AppState>) -> CacheStatsResult {
    state.cache.lock().unwrap().clear();
    CacheStatsResult {
        entries: 0,
        bytes: 0,
    }
}

/// Today's request usage per model and the next quota-reset time.
#[tauri::command]
pub fn get_usage(state: State<'_, AppState>) -> crate::usage::UsageSnapshot {
    state.usage.snapshot()
}
