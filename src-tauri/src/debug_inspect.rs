//! Offline debugging helper: set INSPECT_PATH (and optionally INSPECT_MODEL)
//! to dump the batching/assembly layout of a book. Skipped in normal runs.

use crate::types::{BookSource, MODE_BILINGUAL};

fn docs_of(book: &crate::types::LoadedBook) -> &Vec<crate::types::ContentDoc> {
    match &book.source {
        BookSource::Epub { docs, .. } | BookSource::Pdf { docs, .. } => docs,
    }
}

#[test]
fn debug_inspect_book() {
    let Ok(path) = std::env::var("INSPECT_PATH") else {
        return;
    };
    let model = std::env::var("INSPECT_MODEL").unwrap_or("gemini-3.5-flash-lite".into());

    let bytes = std::fs::read(&path).expect("read file");
    let book = if crate::epub::is_epub(&bytes) {
        crate::epub::parse(bytes, &path).expect("parse epub")
    } else {
        crate::pdf::parse(bytes, &path).expect("parse pdf")
    };

    println!("title: {:?}", book.info.title);
    println!("format: {}", book.info.format);
    println!("docs: {}", docs_of(&book).len());
    println!("total chars: {}", book.info.total_chars);

    let docs = docs_of(&book);
    for (d, doc) in docs.iter().enumerate() {
        let t = doc.blocks.iter().filter(|b| !b.skipped).count();
        let s = doc.blocks.iter().filter(|b| b.skipped).count();
        println!("  doc {d} {:?}: {t} translatable, {s} skipped, title {:?}", doc.path, doc.title);
    }

    let batches = crate::job::build_batches(docs, &model);
    println!(
        "batches: {} (budget {} tokens, max {} blocks)",
        batches.len(),
        crate::gemini::batch_token_budget(&model),
        crate::job::MAX_BATCH_BLOCKS
    );
    let mut translatable = 0usize;
    let mut split_blocks = std::collections::BTreeSet::new();
    for (bi, batch) in batches.iter().enumerate() {
        for item in &batch.items {
            translatable += 1;
            if let Some((pi, total)) = item.part {
                split_blocks.insert(format!(
                    "doc{} block{} part {pi}/{total}: {:?}...",
                    item.d,
                    item.b,
                    item.text.chars().take(40).collect::<String>()
                ));
            }
        }
        if bi < 5 || bi + 1 == batches.len() {
            println!(
                "  batch {:>3}: {} items, {} chars, ~{} tokens",
                bi, batch.items.len(), batch.chars, batch.tokens
            );
        }
    }
    println!("translatable items (incl. parts): {translatable}");
    println!("split blocks: {}", split_blocks.len());
    for s in split_blocks.iter().take(12) {
        println!("  {s}");
    }
    let skipped: usize = docs.iter().map(|d| d.blocks.iter().filter(|b| b.skipped).count()).sum();
    println!("skipped blocks: {skipped}");

    // Assembly coverage: each block with a translation must appear as a
    // bilingual pair in the rewritten output.
    let mut missing = 0usize;
    for (d, doc) in docs.iter().enumerate() {
        for (b, block) in doc.blocks.iter().enumerate() {
            if block.skipped {
                continue;
            }
            let blocks: Vec<_> = doc
                .blocks
                .iter()
                .enumerate()
                .map(|(i, blk)| {
                    let mut x = blk.clone();
                    x.translation = if i == b { Some("[TR]".into()) } else { None };
                    x
                })
                .collect();
            let rewritten =
                crate::epub::html::rewrite_doc(&doc.html, &blocks, MODE_BILINGUAL).expect("rewrite");
            if !rewritten.contains("[TR]") {
                missing += 1;
                println!(
                    "  MISSING in output: doc {d} block {b} (tag {}): {:?}",
                    block.tag,
                    block.text.chars().take(60).collect::<String>()
                );
            }
        }
    }
    println!("blocks whose translation never appears in output: {missing}");
}
