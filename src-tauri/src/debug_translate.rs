//! Debug helper: run the real translation pipeline against a book, mirroring
//! job.rs but without Tauri (no events, sequential), and report every batch
//! failure verbatim. Set INSPECT_PATH (book), GEMINI_KEY; optionally
//! INSPECT_MODEL / INSPECT_LANG. Skipped in normal runs.

use crate::types::{BookSource, MODE_BILINGUAL};

#[test]
fn debug_translate_run() {
    let (Ok(path), Ok(key)) = (
        std::env::var("INSPECT_PATH"),
        std::env::var("GEMINI_KEY"),
    ) else {
        return;
    };
    let model = std::env::var("INSPECT_MODEL").unwrap_or("gemini-3.5-flash-lite".into());
    let lang = std::env::var("INSPECT_LANG").unwrap_or("Burmese (မြန်မာ)".into());
    let max_batches: usize = std::env::var("INSPECT_MAX_BATCHES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(usize::MAX);

    let bytes = std::fs::read(&path).expect("read file");
    let book = crate::pdf::parse(bytes, &path).expect("parse pdf");
    let book = std::sync::Mutex::new(book);
    let batches = crate::job::build_batches(match book.lock().unwrap().source {
        BookSource::Epub { ref docs, .. } | BookSource::Pdf { ref docs, .. } => docs,
    }, &model);
    println!("batches: {}", batches.len());

    let rt = tokio::runtime::Runtime::new().expect("runtime");
    let client = crate::gemini::GeminiClient::new(&key, &model);
    let limiter = crate::gemini::RateLimiter::new(crate::gemini::model_rpm(&model));

    let mut failed = 0usize;
    for (bi, batch) in batches.iter().enumerate() {
        if bi >= max_batches {
            println!("stopping at INSPECT_MAX_BATCHES={max_batches}");
            break;
        }
        let paras: Vec<(usize, String)> = batch
            .items
            .iter()
            .enumerate()
            .map(|(k, item)| (k + 1, item.text.clone()))
            .collect();
        let res = rt.block_on(client.translate_batch(&limiter, &lang, "", &paras));
        match res {
            Ok(r) => {
                let mut book = book.lock().unwrap();
                let docs = match &mut book.source {
                    BookSource::Epub { docs, .. } | BookSource::Pdf { docs, .. } => docs,
                };
                for (k, item) in batch.items.iter().enumerate() {
                    if let Some(t) = r.translations.get(&(k + 1)) {
                        crate::job::apply_translation(&mut docs[..], item, t);
                    }
                }
                drop(book);
                println!("batch {:>2}: ok ({} tokens)", bi, r.total_tokens);
            }
            Err(e) => {
                failed += 1;
                println!("batch {:>2}: FAILED — {}", bi, e.message());
            }
        }
    }
    println!("failed batches: {failed}");

    let mut missing = 0usize;
    {
        let book = book.lock().unwrap();
        let docs = match &book.source {
            BookSource::Epub { docs, .. } | BookSource::Pdf { docs, .. } => docs,
        };
        for doc in docs {
            for block in &doc.blocks {
                if !block.skipped && block.translation.is_none() {
                    missing += 1;
                    println!(
                        "  untranslated: {:?}",
                        block.text.chars().take(50).collect::<String>()
                    );
                }
            }
        }
    }
    println!("untranslated blocks: {missing}");

    let out = crate::epub::build::build_new_epub(
        &book.lock().unwrap().info.title,
        &book.lock().unwrap().info.author,
        &lang,
        match &book.lock().unwrap().source {
            BookSource::Epub { docs, .. } | BookSource::Pdf { docs, .. } => &docs[..],
        },
        MODE_BILINGUAL,
    )
    .expect("assemble");
    let out_path = "/tmp/jayatilleke-bilingual.epub";
    std::fs::write(out_path, &out).expect("write");
    println!("wrote {out_path}");
}
