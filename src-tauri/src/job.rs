use crate::error::Result;
use crate::gemini::{GeminiClient, GeminiFailure};
use crate::types::{
    BookSource, ContentDoc, JobOptions, JobProgress, LoadedBook, SegmentStatus,
};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap};
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tauri::Emitter;

pub const EVENT_PROGRESS: &str = "job-progress";
pub const EVENT_LOG: &str = "job-log";

/// Parallel in-flight requests, tuned for the free tier: enough to keep the
/// pipe full at the 20 RPM Flash limit, harmless at Flash-Lite's 500 RPM.
const CONCURRENCY: usize = 3;
/// Safety cap on paragraphs per request, so alignment stays manageable.
const MAX_BATCH_BLOCKS: usize = 100;

#[derive(Clone)]
pub struct Batch {
    pub items: Vec<(usize, usize)>, // (doc index, block index)
    pub chars: usize,
    pub tokens: usize, // estimated input tokens, used for budgeting
}

fn docs_of(book: &LoadedBook) -> &Vec<ContentDoc> {
    match &book.source {
        BookSource::Epub { docs, .. } | BookSource::Pdf { docs, .. } => docs,
    }
}

pub fn build_batches(docs: &[ContentDoc], model: &str) -> Vec<Batch> {
    let budget = crate::gemini::batch_token_budget(model);
    let mut batches: Vec<Batch> = Vec::new();
    let mut current = Batch {
        items: Vec::new(),
        chars: 0,
        tokens: 0,
    };
    for (d, doc) in docs.iter().enumerate() {
        for (b, block) in doc.blocks.iter().enumerate() {
            if block.skipped {
                continue;
            }
            let chars = block.text.chars().count();
            let tokens = crate::gemini::estimate_tokens(&block.text);
            if !current.items.is_empty()
                && (current.tokens + tokens > budget
                    || current.items.len() >= MAX_BATCH_BLOCKS)
            {
                batches.push(std::mem::replace(
                    &mut current,
                    Batch {
                        items: Vec::new(),
                        chars: 0,
                        tokens: 0,
                    },
                ));
            }
            current.items.push((d, b));
            current.chars += chars;
            current.tokens += tokens;
        }
    }
    if !current.items.is_empty() {
        batches.push(current);
    }
    batches
}

fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn resume_key(book: &LoadedBook, model: &str, lang: &str) -> String {
    let bytes = match &book.source {
        BookSource::Epub { bytes, .. } => bytes,
        BookSource::Pdf { bytes, .. } => bytes,
    };
    let mut h = Sha256::new();
    h.update(bytes);
    format!(
        "{}-{}-{}-{}-{}",
        to_hex(&h.finalize()),
        model,
        lang,
        crate::gemini::batch_token_budget(model),
        MAX_BATCH_BLOCKS
    )
}

fn load_resume(path: &PathBuf) -> HashMap<usize, BTreeMap<usize, String>> {
    let mut map = HashMap::new();
    let Ok(f) = File::open(path) else {
        return map;
    };
    for line in BufReader::new(f).lines().map_while(std::result::Result::ok) {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) {
            let Some(i) = v.get("i").and_then(|i| i.as_u64()) else {
                continue;
            };
            let mut tr = BTreeMap::new();
            if let Some(obj) = v.get("t").and_then(|t| t.as_object()) {
                for (k, val) in obj {
                    if let (Ok(k), Some(t)) = (k.parse::<usize>(), val.as_str()) {
                        tr.insert(k, t.to_string());
                    }
                }
            }
            if !tr.is_empty() {
                map.insert(i as usize, tr);
            }
        }
    }
    map
}

fn append_resume(file: &mut Option<File>, batch_idx: usize, tr: &BTreeMap<usize, String>) {
    let Some(f) = file.as_mut() else { return };
    let obj: serde_json::Map<String, serde_json::Value> =
        tr.iter().map(|(k, v)| (k.to_string(), json!(v))).collect();
    let _ = writeln!(f, "{}", json!({ "i": batch_idx, "t": obj }));
    let _ = f.flush();
}

/// Shared counters + emitter, cloned into every worker.
struct ProgressCtx {
    app: tauri::AppHandle,
    book: Arc<Mutex<LoadedBook>>,
    batches_total: usize,
    chars_total: usize,
    done: Mutex<usize>,
    failed: Mutex<usize>,
    chars_done: Mutex<usize>,
    tokens: AtomicU64,
}

impl ProgressCtx {
    fn snapshot(&self, status: &str, error: Option<String>, output_ready: bool) -> JobProgress {
        let book = self.book.lock().unwrap();
        let segments: Vec<SegmentStatus> = docs_of(&book)
            .iter()
            .map(|doc| {
                let total = doc.blocks.iter().filter(|b| !b.skipped).count();
                let done = doc
                    .blocks
                    .iter()
                    .filter(|b| !b.skipped && b.translation.is_some())
                    .count();
                let state = if total == 0 || done == 0 {
                    "pending"
                } else if done >= total {
                    "done"
                } else {
                    "active"
                };
                SegmentStatus {
                    id: doc.path.clone(),
                    title: doc.title.clone(),
                    state: state.into(),
                    done,
                    total,
                }
            })
            .collect();
        JobProgress {
            status: status.into(),
            segments,
            batches_total: self.batches_total,
            batches_done: *self.done.lock().unwrap(),
            batches_failed: *self.failed.lock().unwrap(),
            chars_done: *self.chars_done.lock().unwrap(),
            chars_total: self.chars_total,
            tokens_used: self.tokens.load(Ordering::Relaxed),
            error,
            output_ready,
        }
    }

    fn emit(&self, status: &str, error: Option<String>, output_ready: bool) {
        let _ = self.app.emit(EVENT_PROGRESS, self.snapshot(status, error, output_ready));
    }

    fn log(&self, msg: impl Into<String>) {
        let _ = self.app.emit(EVENT_LOG, json!({ "message": msg.into() }));
    }
}

/// Runs a full translation job. Emits `job-progress` / `job-log` events;
/// on success leaves the assembled EPUB bytes in `output`.
#[allow(clippy::too_many_arguments)]
pub async fn run(
    app: tauri::AppHandle,
    book: Arc<Mutex<LoadedBook>>,
    opts: JobOptions,
    api_key: String,
    data_dir: PathBuf,
    cancel: Arc<AtomicBool>,
    running: Arc<AtomicBool>,
    output: Arc<Mutex<Option<Vec<u8>>>>,
) {
    let res = run_inner(&app, &book, &opts, &api_key, &data_dir, &cancel, &output).await;
    running.store(false, Ordering::SeqCst);
    if let Err(e) = res {
        let ctx = ProgressCtx {
            app: app.clone(),
            book: book.clone(),
            batches_total: 0,
            chars_total: book.lock().unwrap().info.total_chars,
            done: Mutex::new(0),
            failed: Mutex::new(0),
            chars_done: Mutex::new(0),
            tokens: AtomicU64::new(0),
        };
        ctx.log(format!("Job failed: {e}"));
        ctx.emit("failed", Some(e.to_string()), false);
    }
}

async fn run_inner(
    app: &tauri::AppHandle,
    book_ref: &Arc<Mutex<LoadedBook>>,
    opts: &JobOptions,
    api_key: &str,
    data_dir: &std::path::Path,
    cancel: &Arc<AtomicBool>,
    output: &Arc<Mutex<Option<Vec<u8>>>>,
) -> Result<()> {
    let key = resume_key(&book_ref.lock().unwrap(), &opts.model, &opts.target_lang);
    let log_path = data_dir.join("jobs").join(format!("{key}.jsonl"));
    if let Some(dir) = log_path.parent() {
        std::fs::create_dir_all(dir)?;
    }

    let batches = build_batches(docs_of(&book_ref.lock().unwrap()), &opts.model);

    // Reuse translations from a previous interrupted run, if any.
    let resumed = load_resume(&log_path);
    {
        let mut book = book_ref.lock().unwrap();
        for (bi, batch) in batches.iter().enumerate() {
            if let Some(tr) = resumed.get(&bi) {
                let docs = match &mut book.source {
                    BookSource::Epub { docs, .. } | BookSource::Pdf { docs, .. } => docs,
                };
                for (k, (d, b)) in batch.items.iter().enumerate() {
                    if let Some(t) = tr.get(&(k + 1)) {
                        docs[*d].blocks[*b].translation = Some(t.clone());
                    }
                }
            }
        }
    }
    let pending: Vec<usize> = (0..batches.len())
        .filter(|bi| !resumed.contains_key(bi))
        .collect();
    let chars_total: usize = batches.iter().map(|b| b.chars).sum();
    let chars_start: usize = (0..batches.len())
        .filter(|bi| !pending.contains(bi))
        .map(|bi| batches[bi].chars)
        .sum();

    let resume_file = OpenOptions::new().create(true).append(true).open(&log_path)?;
    let resume_writer = Arc::new(Mutex::new(Some(resume_file)));
    let client = Arc::new(GeminiClient::new(api_key, &opts.model));
    let limiter = Arc::new(crate::gemini::RateLimiter::new(crate::gemini::model_rpm(
        &opts.model,
    )));

    let ctx = Arc::new(ProgressCtx {
        app: app.clone(),
        book: book_ref.clone(),
        batches_total: batches.len(),
        chars_total,
        done: Mutex::new(0),
        failed: Mutex::new(0),
        chars_done: Mutex::new(chars_start),
        tokens: AtomicU64::new(0),
    });

    if pending.is_empty() && !resumed.is_empty() {
        ctx.log("All batches already translated — resuming from cache.");
    } else if !pending.is_empty() {
        let rpm = crate::gemini::model_rpm(&opts.model);
        ctx.log(format!(
            "Translating {} of {} batches into {} with {} (paced to {rpm} requests/min)",
            pending.len(),
            batches.len(),
            opts.target_lang,
            opts.model
        ));
    }
    ctx.emit("running", None, false);

    let fatal: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));

    if !pending.is_empty() {
        let (tx, rx) = tokio::sync::mpsc::channel::<usize>(CONCURRENCY);
        let rx = Arc::new(tokio::sync::Mutex::new(rx));

        let dispatcher = {
            let tx = tx.clone();
            tokio::spawn(async move {
                for bi in pending {
                    if tx.send(bi).await.is_err() {
                        break;
                    }
                }
            })
        };

        let mut workers = Vec::new();
        for _ in 0..CONCURRENCY {
            workers.push(tokio::spawn(worker_loop(
                rx.clone(),
                client.clone(),
                limiter.clone(),
                book_ref.clone(),
                batches.clone(),
                opts.clone(),
                cancel.clone(),
                ctx.clone(),
                fatal.clone(),
                resume_writer.clone(),
            )));
        }
        drop(tx);
        let _ = dispatcher.await;
        for w in workers {
            let _ = w.await;
        }
    }

    if let Some(err) = fatal.lock().unwrap().take() {
        ctx.log(format!("Stopping: {err}"));
        ctx.emit("failed", Some(err), false);
        return Ok(());
    }

    if cancel.load(Ordering::SeqCst) {
        ctx.log("Cancelled — finished batches are saved; run again to resume.");
        ctx.emit("cancelled", None, false);
        return Ok(());
    }

    // Assemble the output EPUB.
    let mode = opts.mode.clone();
    let assembled: Result<Vec<u8>> = {
        let book = book_ref.lock().unwrap();
        match &book.source {
            BookSource::Epub { bytes, docs } => {
                let mut replacements = HashMap::new();
                for doc in docs {
                    if doc.blocks.iter().any(|b| b.translation.is_some()) {
                        let rewritten =
                            crate::epub::html::rewrite_doc(&doc.html, &doc.blocks, &mode)?;
                        replacements.insert(doc.path.clone(), rewritten);
                    }
                }
                crate::epub::build::repack(bytes, &replacements)
            }
            BookSource::Pdf { docs, .. } => crate::epub::build::build_new_epub(
                &book.info.title,
                &book.info.author,
                &opts.target_lang,
                docs,
                &mode,
            ),
        }
    };

    match assembled {
        Ok(bytes) => {
            *output.lock().unwrap() = Some(bytes);
            ctx.log("Translation complete.");
            ctx.emit("completed", None, true);
            Ok(())
        }
        Err(e) => {
            ctx.log(format!("Assembly failed: {e}"));
            ctx.emit("failed", Some(e.to_string()), false);
            Ok(())
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn worker_loop(
    rx: Arc<tokio::sync::Mutex<tokio::sync::mpsc::Receiver<usize>>>,
    client: Arc<GeminiClient>,
    limiter: Arc<crate::gemini::RateLimiter>,
    book_ref: Arc<Mutex<LoadedBook>>,
    batches: Vec<Batch>,
    opts: JobOptions,
    cancel: Arc<AtomicBool>,
    ctx: Arc<ProgressCtx>,
    fatal: Arc<Mutex<Option<String>>>,
    resume_writer: Arc<Mutex<Option<File>>>,
) {
    loop {
        let bi = {
            let mut guard = rx.lock().await;
            match guard.recv().await {
                Some(bi) => bi,
                None => break,
            }
        };
        if cancel.load(Ordering::SeqCst) || fatal.lock().unwrap().is_some() {
            continue; // drain the queue without doing work
        }

        limiter.acquire().await;

        let paras: Vec<(usize, String)> = {
            let book = book_ref.lock().unwrap();
            batches[bi]
                .items
                .iter()
                .enumerate()
                .map(|(k, (d, b))| {
                    (k + 1, docs_of(&book)[*d].blocks[*b].text.clone())
                })
                .collect()
        };
        let chars = batches[bi].chars;

        match client
            .translate_batch(&limiter, &opts.target_lang, &opts.custom_instructions, &paras)
            .await
        {
            Ok(res) => {
                {
                    let mut book = book_ref.lock().unwrap();
                    let docs = match &mut book.source {
                        BookSource::Epub { docs, .. } | BookSource::Pdf { docs, .. } => docs,
                    };
                    for (k, (d, b)) in batches[bi].items.iter().enumerate() {
                        if let Some(t) = res.translations.get(&(k + 1)) {
                            docs[*d].blocks[*b].translation = Some(t.clone());
                        }
                    }
                }
                append_resume(&mut resume_writer.lock().unwrap(), bi, &res.translations);
                ctx.tokens.fetch_add(res.total_tokens, Ordering::Relaxed);
                *ctx.chars_done.lock().unwrap() += chars;
                *ctx.done.lock().unwrap() += 1;
            }
            Err(GeminiFailure::Fatal(e)) => {
                *fatal.lock().unwrap() = Some(e.to_string());
                cancel.store(true, Ordering::SeqCst);
            }
            Err(GeminiFailure::Exhausted(e)) => {
                *ctx.failed.lock().unwrap() += 1;
                *ctx.done.lock().unwrap() += 1;
                ctx.log(format!("Batch {} failed after retries: {e}", bi + 1));
            }
        }
        ctx.emit("running", None, false);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Block;

    fn doc(path: &str, texts: &[&str]) -> ContentDoc {
        ContentDoc {
            path: path.into(),
            title: path.into(),
            html: String::new(),
            blocks: texts
                .iter()
                .map(|t| Block {
                    text: t.to_string(),
                    skipped: !crate::types::is_translatable(t),
                    translation: None,
                    tag: "p".into(),
                })
                .collect(),
        }
    }

    #[test]
    fn batches_respect_token_budget() {
        let paras: Vec<String> = (0..200)
            .map(|i| format!("paragraph number {i} ").repeat(30)) // ~600 chars each
            .collect();
        let refs: Vec<&str> = paras.iter().map(String::as_str).collect();
        let docs = vec![doc("a", &refs), doc("b", &["tiny", "a full sentence here"])];
        let batches = build_batches(&docs, "gemini-3.5-flash-lite");
        assert!(batches.len() > 1);
        for b in &batches {
            assert!(b.items.len() <= MAX_BATCH_BLOCKS);
            assert!(b.tokens <= 5_000 + 200); // one ~150-token paragraph of slack
        }
        // 200 medium paragraphs from doc a + both qualifying blocks in doc b
        let total: usize = batches.iter().map(|b| b.items.len()).sum();
        assert_eq!(total, 202);

        // Flash packs roughly twice as much per request.
        let flash = build_batches(&docs, "gemini-3.8-flash");
        assert!(flash.len() * 2 <= batches.len() + 2);
    }

    #[test]
    fn skips_short_blocks() {
        let docs = vec![doc("a", &["ok", "x", "42", "another one"])];
        let batches = build_batches(&docs, "gemini-3.5-flash-lite");
        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].items.len(), 2); // "ok" and "another one"
    }
}
