use crate::cache::TranslationCache;
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
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tauri::Emitter;

use crate::state::BookRef;

pub const EVENT_PROGRESS: &str = "job-progress";
pub const EVENT_LOG: &str = "job-log";

/// Parallel in-flight requests, tuned for the free tier: enough to keep the
/// pipe full at the 20 RPM Flash limit, harmless at Flash-Lite's 500 RPM.
const CONCURRENCY: usize = 3;
/// Safety cap on paragraphs per request, so alignment stays manageable.
pub(crate) const MAX_BATCH_BLOCKS: usize = 100;
/// Sample runs translate roughly this many paragraphs before stopping.
const SAMPLE_ITEMS: usize = 25;

/// The persistent cache plus this job's language/instructions. The key
/// prefix is derived per model, so segments land under whichever model
/// actually translated them even mid-job fallback switches.
#[derive(Clone)]
struct SegmentCache {
    store: Arc<Mutex<TranslationCache>>,
    lang: String,
    instructions: String,
}

impl SegmentCache {
    fn lookup(&self, model: &str, text: &str) -> Option<String> {
        self.store
            .lock()
            .unwrap()
            .get(&self.prefix(model), text)
    }
    fn record(&self, model: &str, text: &str, translation: &str) {
        self.store
            .lock()
            .unwrap()
            .put(&self.prefix(model), text, translation);
    }
    fn prefix(&self, model: &str) -> String {
        TranslationCache::prefix(model, &self.lang, &self.instructions)
    }
}

/// Hands workers the client for the job's current model, and advances to a
/// fallback model (with its own free-tier quota) when one dies for the day.
/// All state is mutex-guarded so concurrent workers switch together.
struct ClientPool {
    api_key: String,
    enabled: bool,
    current: Mutex<String>,
    exhausted: Mutex<std::collections::HashSet<String>>,
    /// Quota day the `exhausted` set belongs to, so a job that runs across
    /// the daily reset forgets dead models once their quota returns.
    exhausted_day: Mutex<u64>,
    clients: Mutex<HashMap<String, Arc<GeminiClient>>>,
    usage: Arc<crate::usage::UsageTracker>,
}

impl ClientPool {
    fn new(api_key: &str, model: &str, enabled: bool, usage: Arc<crate::usage::UsageTracker>) -> Self {
        // Seed with models the server already declared dead today, so a new
        // job skips them instead of paying a 429 to rediscover each one.
        let day = usage.day();
        let exhausted = usage
            .snapshot()
            .models
            .into_iter()
            .filter(|(_, m)| m.exhausted)
            .map(|(m, _)| m)
            .collect();
        Self {
            api_key: api_key.to_string(),
            enabled,
            current: Mutex::new(model.to_string()),
            exhausted: Mutex::new(exhausted),
            exhausted_day: Mutex::new(day),
            clients: Mutex::new(HashMap::new()),
            usage,
        }
    }

    /// The client + model every new request should use right now.
    fn pair(&self) -> (Arc<GeminiClient>, String) {
        let model = self.current.lock().unwrap().clone();
        (self.client_for(&model), model)
    }

    fn client_for(&self, model: &str) -> Arc<GeminiClient> {
        let mut clients = self.clients.lock().unwrap();
        clients
            .entry(model.to_string())
            .or_insert_with(|| {
                Arc::new(GeminiClient::with_usage(
                    &self.api_key,
                    model,
                    Some(self.usage.clone()),
                ))
            })
            .clone()
    }

    /// If the job's starting model is already exhausted today, hop to its
    /// first live fallback right away.
    fn skip_exhausted_current(&self) -> Option<String> {
        let current = self.current.lock().unwrap().clone();
        if !self.exhausted.lock().unwrap().contains(&current) {
            return None;
        }
        self.advance(&current)
    }

    fn mark_exhausted(&self, model: &str) {
        self.usage.mark_exhausted(model);
    }

    /// Mark `died` quota-dead and move to its next fallback. Returns the new
    /// model — including when a peer worker already switched past `died`, so
    /// the caller just retries on the new current. `None` means switching is
    /// disabled or every same-tier model is exhausted.
    fn advance(&self, died: &str) -> Option<String> {
        let mut current = self.current.lock().unwrap();
        let mut exhausted = self.exhausted.lock().unwrap();
        // Across the daily reset the dead models come back to life.
        {
            let today = self.usage.day();
            let mut day = self.exhausted_day.lock().unwrap();
            if *day != today {
                *day = today;
                exhausted.clear();
            }
        }
        exhausted.insert(died.to_string());
        if !self.enabled {
            return None;
        }
        if *current != died {
            return Some(current.clone());
        }
        let next = crate::gemini::fallback_models(died)
            .into_iter()
            .find(|m| !exhausted.contains(m))?;
        *current = next.clone();
        Some(next)
    }
}

#[derive(Clone)]
pub struct BatchItem {
    pub d: usize, // doc index
    pub b: usize, // block index
    /// (part index, part count) when an oversized block was split.
    pub part: Option<(usize, usize)>,
    /// Exact text sent to the model (whole block, or one part of it).
    pub text: String,
}

#[derive(Clone)]
pub struct Batch {
    pub items: Vec<BatchItem>,
    pub chars: usize,
    pub tokens: usize, // estimated input tokens, used for budgeting
}

fn docs_of(book: &LoadedBook) -> &Vec<ContentDoc> {
    match &book.source {
        BookSource::Epub { docs, .. } | BookSource::Pdf { docs, .. } => docs,
    }
}

/// Split a block whose token estimate alone busts the request budget into
/// whitespace-bounded parts. Concatenating the parts restores the original
/// text exactly, so the joined translation can replace it wholesale.
pub fn split_oversized(text: &str, budget: usize) -> Vec<String> {
    let target = (budget / 2).max(200);
    let mut parts: Vec<String> = Vec::new();
    let mut start = 0usize;
    let mut ascii = 0usize;
    let mut other = 0usize;
    let mut hard_run = 0usize;
    for (i, c) in text.char_indices() {
        if c.is_ascii() {
            ascii += 1;
        } else {
            other += 1;
        }
        hard_run += 1;
        let at_target = crate::gemini::estimate_counts(ascii, other) >= target;
        // Prefer cutting after whitespace; force a cut if a single run of
        // non-whitespace blows past the target by a wide margin.
        if at_target && (c.is_whitespace() || hard_run >= target * 4) {
            let end = i + c.len_utf8();
            parts.push(text[start..end].to_string());
            start = end;
            ascii = 0;
            other = 0;
            hard_run = 0;
        }
    }
    if start < text.len() {
        parts.push(text[start..].to_string());
    }
    parts
}

pub fn build_batches(docs: &[ContentDoc], model: &str) -> Vec<Batch> {
    let budget = crate::gemini::batch_token_budget(model);
    let mut batches: Vec<Batch> = Vec::new();
    let mut current = Batch {
        items: Vec::new(),
        chars: 0,
        tokens: 0,
    };
    macro_rules! flush {
        () => {
            if !current.items.is_empty() {
                batches.push(std::mem::replace(
                    &mut current,
                    Batch {
                        items: Vec::new(),
                        chars: 0,
                        tokens: 0,
                    },
                ));
            }
        };
    }
    for (d, doc) in docs.iter().enumerate() {
        for (b, block) in doc.blocks.iter().enumerate() {
            if block.skipped {
                continue;
            }
            let tokens = crate::gemini::estimate_tokens(&block.text);
            let pieces: Vec<(Option<(usize, usize)>, String)> = if tokens > budget {
                let parts = split_oversized(&block.text, budget);
                let n = parts.len();
                parts
                    .into_iter()
                    .enumerate()
                    .map(|(pi, t)| (Some((pi, n)), t))
                    .collect()
            } else {
                vec![(None, block.text.clone())]
            };
            for (part, text) in pieces {
                let chars = text.chars().count();
                let tokens = crate::gemini::estimate_tokens(&text);
                if !current.items.is_empty()
                    && (current.tokens + tokens > budget
                        || current.items.len() >= MAX_BATCH_BLOCKS)
                {
                    flush!();
                }
                current.items.push(BatchItem {
                    d,
                    b,
                    part,
                    text: text.to_string(),
                });
                current.chars += chars;
                current.tokens += tokens;
            }
        }
    }
    flush!();
    batches
}

/// Keep only the first `max_items` items across batches, trimming the
/// boundary batch instead of inventing a tiny extra request.
fn truncate_for_sample(batches: Vec<Batch>, max_items: usize) -> Vec<Batch> {
    let mut out = Vec::new();
    let mut used = 0usize;
    for mut batch in batches {
        if used >= max_items {
            break;
        }
        let room = max_items - used;
        if batch.items.len() > room {
            batch.items.truncate(room);
            batch.chars = batch.items.iter().map(|i| i.text.chars().count()).sum();
            batch.tokens = batch
                .items
                .iter()
                .map(|i| crate::gemini::estimate_tokens(&i.text))
                .sum();
        }
        used += batch.items.len();
        out.push(batch);
    }
    out
}

/// Exact number of API requests a run of `batches` would send right now,
/// and how many batches need none: resume-log batches are replayed from
/// disk, batches whose every segment sits in the translation cache are
/// served from it, and the rest cost one request each (partially cached
/// ones a smaller one).
pub(crate) fn count_pending_requests(
    batches: &[Batch],
    resumed: &HashMap<usize, BTreeMap<usize, String>>,
    prefix: &str,
    cache: &TranslationCache,
) -> (usize, usize) {
    let mut requests = 0usize;
    let mut done = 0usize;
    for (bi, batch) in batches.iter().enumerate() {
        if resumed.contains_key(&bi)
            || batch
                .items
                .iter()
                .all(|i| cache.get(prefix, &i.text).is_some())
        {
            done += 1;
        } else {
            requests += 1;
        }
    }
    (requests, done)
}

/// Store one translated piece, joining split blocks once all parts arrived.
pub(crate) fn apply_translation(docs: &mut [ContentDoc], item: &BatchItem, t: &str) {
    let block = &mut docs[item.d].blocks[item.b];
    match item.part {
        None => block.translation = Some(t.to_string()),
        Some((pi, total)) => {
            if block.parts.len() != total {
                block.parts.clear();
                block.parts.resize(total, None);
            }
            block.parts[pi] = Some(t.to_string());
            if block.parts.iter().all(Option::is_some) {
                let joined: String =
                    block.parts.iter().flatten().map(String::as_str).collect();
                block.translation = Some(joined);
                block.parts.clear();
            }
        }
    }
}

fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Identity of a translation run for resume caching. Custom instructions are
/// part of the translation function — changing them must invalidate the cache,
/// or stale translations would be silently replayed. So is the prompt
/// version, so a resumed book never mixes old- and new-prompt batches.
pub fn resume_key(book: &LoadedBook, model: &str, lang: &str, instructions: &str) -> String {
    let bytes = match &book.source {
        BookSource::Epub { bytes, .. } => bytes,
        BookSource::Pdf { bytes, .. } => bytes,
    };
    let mut h = Sha256::new();
    h.update(bytes);
    let mut ih = Sha256::new();
    ih.update(instructions.trim().as_bytes());
    format!(
        "{}-{}-{}-{}-{}-{}-v{}",
        to_hex(&h.finalize()),
        model,
        lang,
        crate::gemini::batch_token_budget(model),
        MAX_BATCH_BLOCKS,
        &to_hex(&ih.finalize())[..8],
        crate::cache::PROMPT_VERSION
    )
}

pub(crate) fn load_resume(path: &PathBuf) -> HashMap<usize, BTreeMap<usize, String>> {
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
    sample: bool,
    cached_segments: AtomicUsize,
    model: String,
    target_lang: String,
    mode: String,
    /// Latest snapshot, readable from the UI at any time (`get_job_progress`).
    snapshot_slot: Arc<Mutex<Option<JobProgress>>>,
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
            sample: self.sample,
            cached_segments: self.cached_segments.load(Ordering::Relaxed),
            model: self.model.clone(),
            target_lang: self.target_lang.clone(),
            mode: self.mode.clone(),
        }
    }

    fn emit(&self, status: &str, error: Option<String>, output_ready: bool) {
        let snap = self.snapshot(status, error, output_ready);
        *self.snapshot_slot.lock().unwrap() = Some(snap.clone());
        let _ = self.app.emit(EVENT_PROGRESS, snap);
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
    cache: Arc<Mutex<TranslationCache>>,
    usage: Arc<crate::usage::UsageTracker>,
    auto_switch: bool,
    cancel: Arc<AtomicBool>,
    running: Arc<AtomicBool>,
    output: Arc<Mutex<Option<Vec<u8>>>>,
    snapshot_slot: Arc<Mutex<Option<JobProgress>>>,
) {
    // Set once run_inner builds its ProgressCtx, so a late failure can emit
    // the real counters instead of a zeroed snapshot.
    let ctx_slot: Arc<Mutex<Option<Arc<ProgressCtx>>>> = Arc::new(Mutex::new(None));
    let res = run_inner(
        &app,
        &book,
        &opts,
        &api_key,
        &data_dir,
        &cache,
        &usage,
        auto_switch,
        &cancel,
        &output,
        &snapshot_slot,
        &ctx_slot,
    )
    .await;
    running.store(false, Ordering::SeqCst);
    if let Err(e) = res {
        if let Some(ctx) = ctx_slot.lock().unwrap().as_ref() {
            ctx.log(format!("Job failed: {e}"));
            ctx.emit("failed", Some(e.to_string()), false);
        } else {
            let ctx = ProgressCtx {
                app: app.clone(),
                book: book.clone(),
                batches_total: 0,
                chars_total: book.lock().unwrap().info.total_chars,
                done: Mutex::new(0),
                failed: Mutex::new(0),
                chars_done: Mutex::new(0),
                tokens: AtomicU64::new(0),
                sample: opts.sample,
                cached_segments: AtomicUsize::new(0),
                model: opts.model.clone(),
                target_lang: opts.target_lang.clone(),
                mode: opts.mode.clone(),
                snapshot_slot,
            };
            ctx.log(format!("Job failed: {e}"));
            ctx.emit("failed", Some(e.to_string()), false);
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_inner(
    app: &tauri::AppHandle,
    book_ref: &Arc<Mutex<LoadedBook>>,
    opts: &JobOptions,
    api_key: &str,
    data_dir: &std::path::Path,
    cache: &Arc<Mutex<TranslationCache>>,
    usage: &Arc<crate::usage::UsageTracker>,
    auto_switch: bool,
    cancel: &Arc<AtomicBool>,
    output: &Arc<Mutex<Option<Vec<u8>>>>,
    snapshot_slot: &Arc<Mutex<Option<JobProgress>>>,
    ctx_slot: &Arc<Mutex<Option<Arc<ProgressCtx>>>>,
) -> Result<()> {
    let seg_cache = SegmentCache {
        store: cache.clone(),
        lang: opts.target_lang.clone(),
        instructions: opts.custom_instructions.clone(),
    };

    let mut batches = build_batches(docs_of(&book_ref.lock().unwrap()), &opts.model);
    if opts.sample {
        batches = truncate_for_sample(batches, SAMPLE_ITEMS);
    }

    // Sample runs never touch the resume log: their truncated batch list
    // would misalign batch indices with a full run's log. The persistent
    // cache makes re-running a sample nearly free anyway.
    let log_path = (!opts.sample).then(|| {
        let key = resume_key(
            &book_ref.lock().unwrap(),
            &opts.model,
            &opts.target_lang,
            &opts.custom_instructions,
        );
        let p = data_dir.join("jobs").join(format!("{key}.jsonl"));
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        p
    });

    // Reuse translations from a previous interrupted run, if any.
    let resumed = log_path.as_ref().map(load_resume).unwrap_or_default();    {
        let mut book = book_ref.lock().unwrap();
        let docs = match &mut book.source {
            BookSource::Epub { docs, .. } | BookSource::Pdf { docs, .. } => docs,
        };
        for (bi, batch) in batches.iter().enumerate() {
            if let Some(tr) = resumed.get(&bi) {
                for (k, item) in batch.items.iter().enumerate() {
                    if let Some(t) = tr.get(&(k + 1)) {
                        apply_translation(docs, item, t);
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

    let resume_file = match &log_path {
        Some(p) => Some(OpenOptions::new().create(true).append(true).open(p)?),
        None => None,
    };
    let resume_writer = Arc::new(Mutex::new(resume_file));
    let pool = Arc::new(ClientPool::new(api_key, &opts.model, auto_switch, usage.clone()));
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
        sample: opts.sample,
        cached_segments: AtomicUsize::new(0),
        model: opts.model.clone(),
        target_lang: opts.target_lang.clone(),
        mode: opts.mode.clone(),
        snapshot_slot: snapshot_slot.clone(),
    });
    *ctx_slot.lock().unwrap() = Some(ctx.clone());

    // Surface rate-limit pauses in the Activity log so a slow, throttled
    // job never looks frozen.
    limiter.set_wait_logger(Arc::new({
        let ctx = ctx.clone();
        move |d| {
            ctx.log(format!(
                "Rate limited — pausing {}s before the next request",
                d.as_secs().max(1)
            ))
        }
    }));

    // If today's run already killed the starting model, start on a fallback
    // instead of paying a 429 to rediscover that.
    if let Some(next) = pool.skip_exhausted_current() {
        ctx.log(format!(
            "{} is exhausted for today — starting with {next}",
            opts.model
        ));
    }

    if opts.sample {
        let items: usize = batches.iter().map(|b| b.items.len()).sum();
        ctx.log(format!(
            "Sampling the first {items} paragraphs into {} with {}",
            opts.target_lang, opts.model
        ));
    } else if pending.is_empty() && !resumed.is_empty() {
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
                pool.clone(),
                limiter.clone(),
                seg_cache.clone(),
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
            // A panicked worker (debug builds; release aborts) must not be
            // silently ignored — its batches would never be translated.
            if w.await.is_err() {
                *fatal.lock().unwrap() = Some("a worker task crashed".into());
            }
        }
    }

    let cached = ctx.cached_segments.load(Ordering::Relaxed);
    if cached > 0 {
        ctx.log(format!(
            "{cached} paragraphs reused from the translation cache — no requests spent."
        ));
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

    let failed = *ctx.failed.lock().unwrap();
    if ctx.batches_total > 0 && failed >= ctx.batches_total {
        let msg = "every batch failed after retries".to_string();
        ctx.log(format!("Stopping: {msg} — nothing was translated."));
        ctx.emit("failed", Some(msg), false);
        return Ok(());
    }

    // Assemble the output EPUB.
    let mode = opts.mode.clone();
    let assembled: Result<(Vec<u8>, usize)> = {
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
            )
            .map(|bytes| (bytes, 0)),
        }
    };

    match assembled {
        Ok((bytes, missed)) => {
            if missed > 0 {
                ctx.log(format!(
                    "Warning: {missed} rewritten document(s) matched no archive entry — their originals were kept."
                ));
            }
            *output.lock().unwrap() = Some(bytes);
            if failed > 0 {
                ctx.log(format!(
                    "{failed} of {} batches failed after retries — those paragraphs stay in the original language.",
                    ctx.batches_total
                ));
                ctx.log("Translation finished with gaps.");
                ctx.emit("partial", None, true);
            } else {
                ctx.log("Translation complete.");
                ctx.emit("completed", None, true);
            }
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
    pool: Arc<ClientPool>,
    limiter: Arc<crate::gemini::RateLimiter>,
    cache: SegmentCache,
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

        let outcome = translate_batch_items(
            pool.clone(),
            limiter.clone(),
            cache.clone(),
            opts.clone(),
            book_ref.clone(),
            bi,
            &batches[bi],
            cancel.clone(),
            ctx.clone(),
            fatal.clone(),
            resume_writer.clone(),
        )
        .await;
        match outcome {
            BatchOutcome::Done => {
                *ctx.done.lock().unwrap() += 1;
            }
            BatchOutcome::Failed(msg) => {
                *ctx.failed.lock().unwrap() += 1;
                *ctx.done.lock().unwrap() += 1;
                ctx.log(format!("Batch {} failed after retries: {msg}", bi + 1));
            }
            BatchOutcome::Skipped => {}
        }
        ctx.emit("running", None, false);
    }
}

/// Outcome of translating one batch (possibly via recursive half-splits).
#[derive(PartialEq)]
enum BatchOutcome {
    /// Every item translated.
    Done,
    /// Exhausted after retries (and splits, for truncation).
    Failed(String),
    /// Cancelled or fatally aborted before doing anything.
    Skipped,
}

/// Translate one full batch, keeping its resume entry all-or-nothing: the
/// union map only reaches the resume log when every item translated, so a
/// replayed batch is always fully covered. Segments already in the
/// persistent cache are applied without spending a request.
#[allow(clippy::too_many_arguments)]
async fn translate_batch_items(
    pool: Arc<ClientPool>,
    limiter: Arc<crate::gemini::RateLimiter>,
    cache: SegmentCache,
    opts: JobOptions,
    book: BookRef,
    bi: usize,
    batch: &Batch,
    cancel: Arc<AtomicBool>,
    ctx: Arc<ProgressCtx>,
    fatal: Arc<Mutex<Option<String>>>,
    resume_writer: Arc<Mutex<Option<File>>>,
) -> BatchOutcome {
    let mut union: BTreeMap<usize, String> = BTreeMap::new();

    // Partition first: cache hits cost nothing, misses go to the API.
    let (client, model) = pool.pair();
    let mut fresh: Vec<BatchItem> = Vec::with_capacity(batch.items.len());
    let mut positions: Vec<usize> = Vec::with_capacity(batch.items.len());
    let mut hits: Vec<(usize, String)> = Vec::new();
    for (k, item) in batch.items.iter().enumerate() {
        match cache.lookup(&model, &item.text) {
            Some(t) => hits.push((k, t)),
            None => {
                fresh.push(item.clone());
                positions.push(k);
            }
        }
    }
    if !hits.is_empty() {
        let hit_chars: usize = hits
            .iter()
            .map(|(k, _)| batch.items[*k].text.chars().count())
            .sum();
        {
            let mut book = book.lock().unwrap();
            let docs = match &mut book.source {
                BookSource::Epub { docs, .. } | BookSource::Pdf { docs, .. } => docs,
            };
            for (k, t) in &hits {
                apply_translation(docs, &batch.items[*k], t);
            }
        }
        *ctx.chars_done.lock().unwrap() += hit_chars;
        ctx.cached_segments
            .fetch_add(hits.len(), Ordering::Relaxed);
        for (k, t) in hits {
            union.insert(k + 1, t);
        }
    }
    if fresh.is_empty() {
        return BatchOutcome::Done;
    }
    drop(client);

    let outcome = translate_items(
        &pool,
        &limiter,
        &cache,
        &opts,
        &book,
        &fresh,
        &positions,
        &format!("Batch {}", bi + 1),
        &cancel,
        &ctx,
        &fatal,
        &mut union,
    )
    .await;
    if outcome == BatchOutcome::Done && union.len() == batch.items.len() {
        append_resume(&mut resume_writer.lock().unwrap(), bi, &union);
    }
    outcome
}

/// Translate a slice of batch items; `positions[j]` is item j's index in the
/// parent batch, kept so resume coverage stays correct through recursive
/// splits. Output-truncated requests are split in half and retried —
/// retrying the same oversized request would fail identically, while halves
/// eventually fit.
#[allow(clippy::too_many_arguments)]
async fn translate_items(
    pool: &ClientPool,
    limiter: &crate::gemini::RateLimiter,
    cache: &SegmentCache,
    opts: &JobOptions,
    book: &BookRef,
    items: &[BatchItem],
    positions: &[usize],
    label: &str,
    cancel: &Arc<AtomicBool>,
    ctx: &ProgressCtx,
    fatal: &Arc<Mutex<Option<String>>>,
    union: &mut BTreeMap<usize, String>,
) -> BatchOutcome {
    if cancel.load(Ordering::SeqCst) || fatal.lock().unwrap().is_some() {
        return BatchOutcome::Skipped;
    }

    let (client, model) = pool.pair();
    limiter.acquire().await;
    let paras: Vec<(usize, String)> = items
        .iter()
        .enumerate()
        .map(|(k, item)| (k + 1, item.text.clone()))
        .collect();
    let chars: usize = items.iter().map(|i| i.text.chars().count()).sum();

    match client
        .translate_batch(limiter, &opts.target_lang, &opts.custom_instructions, &paras)
        .await
    {
        Ok(res) => {
            {
                let mut book = book.lock().unwrap();
                let docs = match &mut book.source {
                    BookSource::Epub { docs, .. } | BookSource::Pdf { docs, .. } => docs,
                };
                for (k, item) in items.iter().enumerate() {
                    if let Some(t) = res.translations.get(&(k + 1)) {
                        apply_translation(docs, item, t);
                    }
                }
            }
            for (k, t) in &res.translations {
                if let Some(item) = items.get(k - 1) {
                    cache.record(&model, &item.text, t);
                    if let Some(&pos) = positions.get(k - 1) {
                        union.insert(pos + 1, t.clone());
                    }
                }
            }
            limiter.note_success();
            ctx.tokens.fetch_add(res.total_tokens, Ordering::Relaxed);
            *ctx.chars_done.lock().unwrap() += chars;
            BatchOutcome::Done
        }
        Err(GeminiFailure::QuotaExhausted(e, confirmed)) => {
            // Only message-confirmed quota deaths go on the model's
            // permanent record; a suspicious Retry-After alone must not ban
            // a healthy model until tomorrow.
            if confirmed {
                pool.mark_exhausted(&model);
            }
            match pool.advance(&model) {
                Some(next) => {
                    ctx.log(format!("{e} — continuing with {next}"));
                    Box::pin(translate_items(
                        pool, limiter, cache, opts, book, items, positions, label, cancel, ctx,
                        fatal, union,
                    ))
                    .await
                }
                None => {
                    let msg = format!(
                        "{e} and every fallback model — finished batches are saved; \
                         resume tomorrow or use another API key"
                    );
                    *fatal.lock().unwrap() = Some(msg.clone());
                    cancel.store(true, Ordering::SeqCst);
                    BatchOutcome::Failed(msg)
                }
            }
        }
        Err(GeminiFailure::Fatal(e)) => {
            *fatal.lock().unwrap() = Some(e.to_string());
            cancel.store(true, Ordering::SeqCst);
            BatchOutcome::Failed(e.to_string())
        }
        Err(GeminiFailure::Truncated(e)) if items.len() > 1 => {
            ctx.log(format!("{label}: {e} — splitting into halves and retrying"));
            let mid = items.len() / 2;
            let first = Box::pin(translate_items(
                pool,
                limiter,
                cache,
                opts,
                book,
                &items[..mid],
                &positions[..mid],
                &format!("{label} (part 1)"),
                cancel,
                ctx,
                fatal,
                union,
            ))
            .await;
            let second = Box::pin(translate_items(
                pool,
                limiter,
                cache,
                opts,
                book,
                &items[mid..],
                &positions[mid..],
                &format!("{label} (part 2)"),
                cancel,
                ctx,
                fatal,
                union,
            ))
            .await;
            match (first, second) {
                (BatchOutcome::Done, BatchOutcome::Done) => BatchOutcome::Done,
                (BatchOutcome::Skipped, other) | (other, BatchOutcome::Skipped) => other,
                (BatchOutcome::Failed(m), _) | (_, BatchOutcome::Failed(m)) => {
                    BatchOutcome::Failed(m)
                }
            }
        }
        Err(e) => BatchOutcome::Failed(e.message()),
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
                    parts: Vec::new(),
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

    #[test]
    fn splits_oversized_block_into_budgeted_parts() {
        // ~26k ascii chars ≈ 6.5k tokens: over the 5k flash-lite budget alone.
        let huge = "word ".repeat(5_200);
        let docs = vec![doc("a", &["small one", &huge])];
        let batches = build_batches(&docs, "gemini-3.5-flash-lite");
        assert!(batches.len() >= 2, "oversized block must span batches");
        let mut part_items = 0;
        for b in &batches {
            assert!(b.tokens <= 5_000 + 200, "batch over budget: {}", b.tokens);
            for item in &b.items {
                if item.part.is_some() {
                    part_items += 1;
                    assert_eq!((item.d, item.b), (0, 1));
                }
            }
        }
        assert!(part_items >= 2);

        // Parts must reassemble into the exact original text.
        let mut joined = String::new();
        for b in &batches {
            for item in &b.items {
                if item.part.is_some() {
                    joined.push_str(&item.text);
                }
            }
        }
        assert_eq!(joined, huge);

        // apply_translation only sets the joined result once all parts land.
        let mut docs = docs;
        let all_items: Vec<BatchItem> = batches
            .iter()
            .flat_map(|b| b.items.iter().filter(|i| i.part.is_some()).cloned())
            .collect();
        for (k, item) in all_items.iter().enumerate() {
            apply_translation(&mut docs, item, &format!("[T{k}] "));
            if k + 1 < all_items.len() {
                assert!(docs[0].blocks[1].translation.is_none());
            }
        }
        let expected: String = (0..all_items.len())
            .map(|k| format!("[T{k}] "))
            .collect();
        assert_eq!(docs[0].blocks[1].translation.as_deref(), Some(expected.as_str()));
        assert!(docs[0].blocks[1].parts.is_empty());
    }

    #[test]
    fn client_pool_walks_the_chain_until_empty() {
        let usage = Arc::new(crate::usage::UsageTracker::open(std::env::temp_dir().join(format!(
            "ebtr-pool-{}-a",
            std::process::id()
        ))));
        let pool = ClientPool::new("key", "gemini-3.5-flash", true, usage);
        assert_eq!(pool.pair().1, "gemini-3.5-flash");
        assert_eq!(pool.advance("gemini-3.5-flash").as_deref(), Some("gemini-3.6-flash"));
        // A stale worker reporting the old model just gets the current one.
        assert_eq!(pool.advance("gemini-3.5-flash").as_deref(), Some("gemini-3.6-flash"));
        assert_eq!(pool.advance("gemini-3.6-flash").as_deref(), Some("gemini-3.7-flash"));
        assert_eq!(pool.advance("gemini-3.7-flash").as_deref(), Some("gemini-3.8-flash"));
        // A model reported dead twice must not come back.
        assert_eq!(pool.advance("gemini-3.8-flash"), None);
        // Stale workers still just get the (exhausted) current model back;
        // their next 429 on it reaches the dead end and stops the job.
        assert_eq!(pool.advance("gemini-3.5-flash").as_deref(), Some("gemini-3.8-flash"));
        assert_eq!(pool.pair().1, "gemini-3.8-flash"); // current unchanged at dead end
    }

    #[test]
    fn client_pool_respects_disabled_switching() {
        let usage = Arc::new(crate::usage::UsageTracker::open(std::env::temp_dir().join(format!(
            "ebtr-pool-{}-b",
            std::process::id()
        ))));
        let pool = ClientPool::new("key", "gemini-3.5-flash", false, usage);
        assert_eq!(pool.advance("gemini-3.5-flash"), None);
        assert_eq!(pool.pair().1, "gemini-3.5-flash");
    }

    #[test]
    fn client_pool_skips_models_exhausted_earlier_today() {
        let path = std::env::temp_dir().join(format!("ebtr-pool-{}-c", std::process::id()));
        let usage = Arc::new(crate::usage::UsageTracker::open(path.clone()));
        usage.mark_exhausted("gemini-3.5-flash");
        let pool = ClientPool::new("key", "gemini-3.5-flash", true, usage.clone());
        // The dead starting model is skipped up front, no 429 needed...
        assert_eq!(pool.skip_exhausted_current().as_deref(), Some("gemini-3.6-flash"));
        assert_eq!(pool.pair().1, "gemini-3.6-flash");
        // ...and a fresh pool seeded with it exhausted does the same.
        let pool2 = ClientPool::new("key", "gemini-3.5-flash", false, usage);
        assert_eq!(pool2.skip_exhausted_current(), None); // switching off: no skip
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn sample_truncation_keeps_a_prefix_of_items() {
        let mk = |n: usize| Batch {
            items: (0..n)
                .map(|i| BatchItem {
                    d: 0,
                    b: i,
                    part: None,
                    text: format!("paragraph number {i} with words"),
                })
                .collect(),
            chars: 0,
            tokens: 0,
        };
        let batches = truncate_for_sample(vec![mk(10), mk(10), mk(10)], 25);
        assert_eq!(batches.iter().map(|b| b.items.len()).sum::<usize>(), 25);
        assert_eq!(batches[2].items.len(), 5);
        // Truncated batches keep their counters consistent.
        assert_eq!(
            batches[2].chars,
            batches[2]
                .items
                .iter()
                .map(|i| i.text.chars().count())
                .sum::<usize>()
        );
        assert_eq!(
            batches[2].tokens,
            batches[2]
                .items
                .iter()
                .map(|i| crate::gemini::estimate_tokens(&i.text))
                .sum::<usize>()
        );
        // Below the cap nothing is trimmed.
        let small = truncate_for_sample(vec![mk(4), mk(4)], 25);
        assert_eq!(small.iter().map(|b| b.items.len()).sum::<usize>(), 8);
    }

    #[test]
    fn request_count_discounts_resume_and_cache() {
        let dir = std::env::temp_dir().join(format!("ebtr-estimate-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut cache = TranslationCache::open(&dir);
        let mk = |tag: usize| Batch {
            items: (0..2)
                .map(|i| BatchItem {
                    d: 0,
                    b: i,
                    part: None,
                    text: format!("para {tag}-{i} text"),
                })
                .collect(),
            chars: 0,
            tokens: 0,
        };
        let batches = vec![mk(0), mk(1), mk(2)];
        let prefix = TranslationCache::prefix("model-a", "Burmese", "");

        // Nothing done yet: every batch costs a request.
        assert_eq!(
            count_pending_requests(&batches, &HashMap::new(), &prefix, &cache),
            (3, 0)
        );
        // Batch 1 in the resume log: replayed, no request.
        let mut resumed = HashMap::new();
        resumed.insert(1usize, BTreeMap::from([(1usize, "done".to_string())]));
        assert_eq!(count_pending_requests(&batches, &resumed, &prefix, &cache), (2, 1));
        // Batch 2 fully cached: served, no request.
        cache.put(&prefix, "para 2-0 text", "x");
        cache.put(&prefix, "para 2-1 text", "y");
        assert_eq!(count_pending_requests(&batches, &resumed, &prefix, &cache), (1, 2));
        // Batch 0 only partially cached: still one (smaller) request.
        cache.put(&prefix, "para 0-0 text", "z");
        assert_eq!(count_pending_requests(&batches, &resumed, &prefix, &cache), (1, 2));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resume_key_reflects_instructions() {
        let book = LoadedBook {
            info: crate::types::BookInfo {
                format: "epub".into(),
                file_path: "/tmp/x.epub".into(),
                file_name: "x.epub".into(),
                title: "x".into(),
                author: String::new(),
                cover_data_url: None,
                total_chars: 0,
                segments: Vec::new(),
                warnings: Vec::new(),
            },
            source: BookSource::Epub {
                bytes: b"same bytes".to_vec(),
                docs: Vec::new(),
            },
        };
        let base = resume_key(&book, "gemini-3.5-flash-lite", "Burmese", "");
        assert_eq!(base, resume_key(&book, "gemini-3.5-flash-lite", "Burmese", "  "));
        assert_ne!(
            base,
            resume_key(&book, "gemini-3.5-flash-lite", "Burmese", "formal tone")
        );
        assert_ne!(
            base,
            resume_key(&book, "gemini-3.5-flash", "Burmese", "")
        );
    }
}
