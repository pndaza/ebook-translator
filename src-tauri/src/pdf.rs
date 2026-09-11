use crate::error::{AppError, Result};
use crate::types::{Block, BookInfo, ContentDoc, LoadedBook, SegmentInfo};

const CHAPTER_CHAR_TARGET: usize = 8000;

pub fn is_pdf(bytes: &[u8]) -> bool {
    bytes.len() > 4 && &bytes[..5] == b"%PDF-"
}

fn qualify(text: &str) -> bool {
    crate::types::is_translatable(text)
}

/// Extract PDF title/author from the trailer Info dictionary via lopdf.
fn metadata(bytes: &[u8]) -> (String, String) {
    let Ok(doc) = pdf_extract::Document::load_mem(bytes) else {
        return (String::new(), String::new());
    };
    let Some(info_ref) = doc
        .trailer
        .get(b"Info")
        .ok()
        .and_then(|o| o.as_reference().ok())
    else {
        return (String::new(), String::new());
    };
    let Ok(pdf_extract::Object::Dictionary(dict)) = doc.get_object(info_ref) else {
        return (String::new(), String::new());
    };
    let get = |key: &[u8]| -> String {
        match dict.get(key).ok() {
            Some(pdf_extract::Object::String(s, _)) => Some(decode_pdf_string(s)),
            _ => None,
        }
        .unwrap_or_default()
    };
    (get(b"Title"), get(b"Author"))
}

/// PDF strings are either UTF-16BE (BOM \xFE\xFF) or PDFDocEncoding
/// (roughly ASCII); decode the common case lossily.
fn decode_pdf_string(s: &[u8]) -> String {
    if s.len() >= 2 && s[0] == 0xFE && s[1] == 0xFF {
        let units: Vec<u16> = s[2..]
            .chunks_exact(2)
            .map(|c| u16::from_be_bytes([c[0], c[1]]))
            .collect();
        String::from_utf16_lossy(&units)
    } else {
        String::from_utf8_lossy(s).into_owned()
    }
}

/// Turn the flat page text into paragraph blocks: blank lines split
/// paragraphs; when a page has no blank lines, group consecutive lines
/// so paragraphs stay reasonably sized for translation.
fn paragraphs_from_pages(pages: &[String]) -> Vec<String> {
    let mut paras: Vec<String> = Vec::new();
    for page in pages {
        let lines: Vec<&str> = page.lines().map(str::trim).collect();
        let mut current: Vec<&str> = Vec::new();
        let flush = |current: &mut Vec<&str>, paras: &mut Vec<String>| {
            if current.is_empty() {
                return;
            }
            let joined = current.join(" ").split_whitespace().collect::<Vec<_>>().join(" ");
            if !joined.is_empty() {
                paras.push(joined);
            }
            current.clear();
        };
        let has_blanks = lines.iter().any(|l| l.is_empty());
        for line in lines {
            if line.is_empty() {
                if has_blanks {
                    flush(&mut current, &mut paras);
                }
                continue;
            }
            current.push(line);
            if !has_blanks && current.len() >= 6 {
                flush(&mut current, &mut paras);
            }
        }
        flush(&mut current, &mut paras);
    }
    paras
}

fn chunk_chapters(paras: Vec<String>) -> Vec<Vec<String>> {
    let mut chapters: Vec<Vec<String>> = Vec::new();
    let mut current: Vec<String> = Vec::new();
    let mut len = 0usize;
    for p in paras {
        let plen = p.chars().count();
        if !current.is_empty() && len + plen > CHAPTER_CHAR_TARGET {
            chapters.push(std::mem::take(&mut current));
            len = 0;
        }
        current.push(p);
        len += plen;
    }
    if !current.is_empty() {
        chapters.push(current);
    }
    chapters
}

/// Parse a PDF into synthesized chapter documents ready for translation.
pub fn parse(bytes: Vec<u8>, file_path: &str) -> Result<LoadedBook> {
    let pages = pdf_extract::extract_text_from_mem_by_pages(&bytes)
        .map_err(|e| AppError::msg(format!("could not extract PDF text: {e}")))?;

    let (meta_title, meta_author) = metadata(&bytes);
    let file_name = file_path.rsplit('/').next().unwrap_or(file_path).to_string();
    let fallback_title = file_name.trim_end_matches(".pdf").to_string();

    let paras = paragraphs_from_pages(&pages);
    if paras.is_empty() {
        return Err(AppError::msg(
            "no extractable text in this PDF — it may be a scanned document (OCR is not supported)",
        ));
    }

    let chapters = chunk_chapters(paras);
    let docs: Vec<ContentDoc> = chapters
        .into_iter()
        .enumerate()
        .map(|(i, chapter)| ContentDoc {
            path: format!("ch{:03}", i + 1),
            title: format!("Part {}", i + 1),
            html: String::new(), // generated at assembly time from blocks
            blocks: chapter
                .into_iter()
                .map(|text| Block {
                    skipped: !qualify(&text),
                    text,
                    translation: None,
                    tag: "p".into(),
                    parts: Vec::new(),
                })
                .collect(),
        })
        .collect();

    let total_chars: usize = docs
        .iter()
        .flat_map(|d| d.blocks.iter())
        .filter(|b| !b.skipped)
        .map(|b| b.text.chars().count())
        .sum();

    let info = BookInfo {
        format: "pdf".into(),
        file_path: file_path.to_string(),
        file_name,
        title: if meta_title.is_empty() {
            fallback_title
        } else {
            meta_title
        },
        author: meta_author,
        cover_data_url: None,
        total_chars,
        segments: docs
            .iter()
            .map(|d| SegmentInfo {
                id: d.path.clone(),
                title: d.title.clone(),
                blocks: d.blocks.iter().filter(|b| !b.skipped).count(),
                chars: d
                    .blocks
                    .iter()
                    .filter(|b| !b.skipped)
                    .map(|b| b.text.chars().count())
                    .sum(),
            })
            .collect(),
        warnings: Vec::new(),
    };

    Ok(LoadedBook {
        info,
        source: crate::types::BookSource::Pdf {
            bytes,
            docs,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_paragraphs_on_blank_lines() {
        let pages = vec!["One two\nthree\n\nFour five".to_string()];
        let paras = paragraphs_from_pages(&pages);
        assert_eq!(paras, vec!["One two three", "Four five"]);
    }

    #[test]
    fn groups_unbroken_lines() {
        let page = (0..13).map(|i| format!("line{i}")).collect::<Vec<_>>().join("\n");
        let paras = paragraphs_from_pages(&[page]);
        assert_eq!(paras.len(), 3); // 6 + 6 + 1
        assert_eq!(paras[0], "line0 line1 line2 line3 line4 line5");
    }

    #[test]
    fn chunks_by_size() {
        let paras: Vec<String> = (0..20).map(|i| "x".repeat(1000) + &i.to_string()).collect();
        let chapters = chunk_chapters(paras);
        assert!(chapters.len() >= 2);
        assert!(chapters.iter().all(|c| !c.is_empty()));
    }
}
