use crate::error::{AppError, Result};
use crate::types::{Block, BookInfo, ContentDoc, LoadedBook, SegmentInfo};
use std::collections::HashMap;

const CHAPTER_CHAR_TARGET: usize = 8000;

pub fn is_pdf(bytes: &[u8]) -> bool {
    bytes.len() > 4 && &bytes[..5] == b"%PDF-"
}

fn qualify(text: &str) -> bool {
    crate::types::is_translatable(text)
}

// ---------------------------------------------------------------------------
// Page furniture: running headers/footers and page numbers.
//
// PDF text extraction interleaves them with the prose, wasting requests and
// littering the translation with repeated titles. A line at a page edge is
// furniture when it is nothing but a page number, or when the same text
// (minus its page number) recurs at that edge on several pages — which also
// catches the classic verso-title / recto-chapter alternation.
// ---------------------------------------------------------------------------

/// A running line must recur at the same page edge this often to count as
/// furniture, so genuine one-off headings survive.
const FURNITURE_MIN_PAGES: usize = 3;

fn to_roman(mut n: u32) -> String {
    const PAIRS: [(&str, u32); 13] = [
        ("m", 1000),
        ("cm", 900),
        ("d", 500),
        ("cd", 400),
        ("c", 100),
        ("xc", 90),
        ("l", 50),
        ("xl", 40),
        ("x", 10),
        ("ix", 9),
        ("v", 5),
        ("iv", 4),
        ("i", 1),
    ];
    let mut out = String::new();
    for (sym, val) in PAIRS {
        while n >= val {
            out.push_str(sym);
            n -= val;
        }
    }
    out
}

/// Strictly parse a lowercase roman numeral (round-trip valid), so real
/// words like "civil" or "mill" never masquerade as page numbers.
fn roman_value(s: &str) -> Option<u32> {
    let val = |c: char| {
        Some(match c {
            'i' => 1,
            'v' => 5,
            'x' => 10,
            'l' => 50,
            'c' => 100,
            'd' => 500,
            'm' => 1000,
            _ => return None,
        })
    };
    let mut total = 0u32;
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        let v = val(c)?;
        match chars.peek().copied() {
            Some(n) => {
                let nv = val(n)?;
                if nv > v {
                    // Subtractive pairs only: iv, ix, xl, xc, cd, cm.
                    if !matches!(
                        (c, n),
                        ('i', 'v') | ('i', 'x') | ('x', 'l') | ('x', 'c') | ('c', 'd') | ('c', 'm')
                    ) {
                        return None;
                    }
                    total += nv - v;
                    chars.next();
                } else {
                    total += v;
                }
            }
            None => total += v,
        }
    }
    if s.is_empty() || total == 0 || total >= 4000 {
        return None;
    }
    (to_roman(total) == s).then_some(total)
}

/// Lowercase words that would otherwise parse as page numbers — valid
/// romans ("mix" = 1009) or digit-free OCR readings ("oil" -> 011). Real
/// text must never be furniture.
const NOT_PAGE_NUMBERS: &[&str] = &[
    "di", "mi", "ci", "li", "mix",
    "lo", "yo", "jo", "ill", "oil", "oily", "lily", "joy", "lol", "loll", "jill", "yoyo",
];

/// A token that looks like a page number: real digits with common OCR
/// misreads ("i6" = 16, "2o" = 20, "j6", "6y"), digit-free misreads ("ioo" =
/// 100, "IOI" = 101), or a strict roman numeral — but never a real word.
fn token_is_numberish(tok: &str) -> bool {
    let lower = tok.to_lowercase();
    if NOT_PAGE_NUMBERS.contains(&lower.as_str()) {
        return false;
    }
    if tok.chars().any(|c| c.is_ascii_digit())
        && tok.chars().all(|c| {
            c.is_ascii_digit()
                || matches!(c, 'i' | 'I' | 'l' | 'L' | 'o' | 'O' | 'j' | 'J' | 'y' | 'Y')
                || "*<>,.-–—".contains(c)
        })
    {
        return true;
    }
    // Digit-free OCR numbers: map the misreads and keep only plausible page
    // numbers, so real words stay words ("jolly" -> 10117 is rejected by the
    // length cap alone).
    let mapped: String = tok
        .chars()
        .filter(|c| !"*<>,.-–—".contains(*c))
        .map(|c| match c {
            'i' | 'I' | 'l' | 'L' | 'j' | 'J' => '1',
            'o' | 'O' => '0',
            'y' | 'Y' => '7',
            other => other,
        })
        .collect();
    if (2..=4).contains(&mapped.chars().count())
        && mapped.chars().all(|c| c.is_ascii_digit())
        && mapped.parse::<u32>().is_ok_and(|n| n > 0)
    {
        return true;
    }
    // A lone "i" stays peeling-eligible ("Background 4 i" = 41); a whole
    // line of "i" is too rare to worry about.
    roman_value(&lower).is_some()
}

/// A whole line that is nothing but a page number: bare digits (with OCR
/// noise) or a roman numeral, optionally prefixed with "page"/"p." and
/// wrapped in dashes, brackets, or dots.
fn is_page_number(line: &str) -> bool {
    let lower = line.trim().to_lowercase();
    let core = ["page ", "page, ", "p. ", "p "]
        .iter()
        .find_map(|p| lower.strip_prefix(p))
        .unwrap_or(lower.as_str());
    let compact: String = core.chars().filter(|c| !c.is_whitespace()).collect();
    if compact.chars().count() > 10 {
        return false;
    }
    let core: String = compact
        .trim_matches(|c: char| !c.is_ascii_alphanumeric())
        .to_string();
    token_is_numberish(&core)
}

/// Clustering key for an edge line: lowercase with leading/trailing
/// number-ish tokens peeled, so "22 Early Buddhist Theory of Knowledge",
/// "Early Buddhist Theory of Knowledge 4 i" and "<5  Foreword" all map to
/// their running text. `None` for lines that are only a number.
fn header_key(line: &str) -> Option<String> {
    let mut toks: Vec<&str> = line.split_whitespace().collect();
    for _ in 0..2 {
        match toks.first() {
            Some(t) if token_is_numberish(t) => {
                toks.remove(0);
            }
            _ => break,
        }
    }
    for _ in 0..2 {
        match toks.last() {
            Some(t) if token_is_numberish(t) => {
                toks.pop();
            }
            _ => break,
        }
    }
    if toks.is_empty() {
        return None;
    }
    let key = toks.join(" ").to_lowercase();
    (!key.is_empty()).then_some(key)
}

fn is_furniture(counts: &HashMap<String, usize>, line: &str) -> bool {
    header_key(line).is_some_and(|key| {
        counts.get(&key).is_some_and(|n| *n >= FURNITURE_MIN_PAGES)
    })
}

/// Drop up to two furniture lines from one edge of a page (the first or
/// last non-empty line), returning how many were removed. Bare page numbers
/// are only stripped when the book actually paginates that way.
fn strip_edge(
    lines: &mut Vec<&str>,
    top: bool,
    counts: &HashMap<String, usize>,
    strip_numbers: bool,
) -> usize {
    let mut stripped = 0usize;
    for _ in 0..2 {
        let idx = if top {
            lines.iter().position(|l| !l.is_empty())
        } else {
            lines.iter().rposition(|l| !l.is_empty())
        };
        let Some(i) = idx else { break };
        let line = lines[i];
        if (strip_numbers && is_page_number(line)) || is_furniture(counts, line) {
            lines.remove(i);
            stripped += 1;
        } else {
            break;
        }
    }
    stripped
}

/// Remove running headers/footers and page numbers from page edges, keeping
/// blank lines (they carry the paragraph structure). Returns the cleaned
/// pages and how many lines were dropped.
fn strip_page_furniture(pages: &[String]) -> (Vec<String>, usize) {
    // Trimmed lines per page, blanks kept; non-empty indices for edges.
    let page_lines: Vec<Vec<&str>> = pages
        .iter()
        .map(|p| p.lines().map(str::trim).collect())
        .collect();

    let mut tops: HashMap<String, usize> = HashMap::new();
    let mut bottoms: HashMap<String, usize> = HashMap::new();
    let mut number_pages = 0usize;
    for lines in &page_lines {
        let non_empty: Vec<&&str> = lines.iter().filter(|l| !l.is_empty()).collect();
        // Single-line pages are usually real title/divider pages, not
        // furniture, and must not pollute the counts either.
        if non_empty.len() < 2 {
            continue;
        }
        if let Some(key) = header_key(non_empty[0]) {
            *tops.entry(key).or_default() += 1;
        }
        if let Some(key) = header_key(non_empty[non_empty.len() - 1]) {
            *bottoms.entry(key).or_default() += 1;
        }
        if non_empty.first().copied().is_some_and(|l| is_page_number(l))
            || non_empty.last().copied().is_some_and(|l| is_page_number(l))
        {
            number_pages += 1;
        }
    }
    // Page numbers appear on most numbered pages; a lone "II" chapter
    // numeral or stray year is content, so strip bare numbers only when the
    // pattern repeats like furniture does.
    let strip_numbers = number_pages >= FURNITURE_MIN_PAGES || number_pages * 3 >= pages.len();

    let mut stripped = 0usize;
    let cleaned: Vec<String> = page_lines
        .into_iter()
        .map(|mut lines| {
            let non_empty = lines.iter().filter(|l| !l.is_empty()).count();
            if non_empty >= 2 {
                stripped += strip_edge(&mut lines, true, &tops, strip_numbers);
                stripped += strip_edge(&mut lines, false, &bottoms, strip_numbers);
            }
            lines.join("\n")
        })
        .collect();
    (cleaned, stripped)
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

/// Flatten the light markdown pdf-inspector emits (headings, emphasis,
/// links, tables, code fences) into the plain paragraph text the pipeline
/// expects. Blank lines — the paragraph structure — are preserved.
fn strip_markdown(md: &str) -> String {
    let mut out = String::with_capacity(md.len());
    for line in md.lines() {
        let t = line.trim();
        if t.is_empty() {
            out.push('\n');
            continue;
        }
        // Code fences and table separator rows are pure markup.
        if t.starts_with("```") || (t.starts_with('|') && t.contains("---")) {
            continue;
        }
        let mut l = t.trim_start_matches('#').trim().to_string();
        l = strip_links(&l);
        l = l.replace("**", "").replace("__", "").replace('*', "");
        if t.starts_with('|') {
            l = l.replace('|', " ");
        }
        out.push_str(l.trim());
        out.push('\n');
    }
    out
}

/// Reduce `[text](url)` to `text` without a regex dependency.
fn strip_links(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(open) = rest.find('[') {
        let Some(close_rel) = rest[open..].find(']') else {
            break;
        };
        let close = open + close_rel;
        let after = &rest[close..]; // starts with ']'
        if let Some(end) = after.find(')').filter(|_| after.starts_with("](")) {
            out.push_str(&rest[..open]);
            out.push_str(&rest[open + 1..close]);
            rest = &after[end + 1..];
        } else {
            out.push_str(&rest[..open + 1]);
            rest = &rest[open + 1..];
        }
    }
    out.push_str(rest);
    out
}

/// Extract page texts with pdf-inspector (pure Rust, ToUnicode CMap-aware):
/// the bundled fallback for fonts the built-in reader cannot decode.
fn inspector_pages(bytes: &[u8]) -> Option<std::result::Result<Vec<String>, String>> {
    match pdf_inspector::extract_pages_markdown_mem(bytes, None) {
        Ok(res) => Some(Ok(res
            .pages
            .into_iter()
            .map(|p| strip_markdown(&p.markdown))
            .collect())),
        Err(e) => Some(Err(format!("{e}"))),
    }
}

/// Extract page texts with poppler's `pdftotext`, returning `None` when the
/// binary is not installed. Final fallback after the bundled readers.
/// Pages come back split on form feeds, matching the shape of
/// `pdf_extract::extract_text_from_mem_by_pages`.
fn pdftotext_pages(bytes: &[u8]) -> Option<std::result::Result<Vec<String>, String>> {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let tmp = std::env::temp_dir().join(format!("ebtr-{}-{stamp}.pdf", std::process::id()));
    if let Err(e) = std::fs::write(&tmp, bytes) {
        return Some(Err(e.to_string()));
    }
    // A file argument avoids stdin/stdout pipe deadlocks on large books;
    // plain "pdftotext" first, then the Homebrew paths a GUI app misses
    // because it launches without the user's shell PATH.
    let result = ["pdftotext", "/opt/homebrew/bin/pdftotext", "/usr/local/bin/pdftotext"]
        .iter()
        .find_map(|bin| {
            match std::process::Command::new(bin)
                .arg("-enc")
                .arg("UTF-8")
                .arg(&tmp)
                .arg("-")
                .output()
            {
                // The tool ran: its verdict is final for every candidate.
                Ok(out) => Some(if out.status.success() {
                    let text = String::from_utf8_lossy(&out.stdout).into_owned();
                    let mut pages: Vec<String> =
                        text.split('\u{c}').map(str::to_string).collect();
                    if pages.last().is_some_and(|p| p.is_empty()) {
                        pages.pop(); // trailing form feed after the last page
                    }
                    Ok(pages)
                } else {
                    Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
                }),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
                Err(e) => Some(Err(e.to_string())),
            }
        });
    let _ = std::fs::remove_file(&tmp);
    result
}

/// Parse a PDF into synthesized chapter documents ready for translation.
pub fn parse(bytes: Vec<u8>, file_path: &str) -> Result<LoadedBook> {
    // pdf-extract silently returns empty pages for fonts it cannot decode
    // (e.g. CID/Identity-H). Chain: built-in reader → pdf-inspector
    // (bundled, ToUnicode-aware) → poppler's pdftotext.
    let has_text =
        |p: &[String]| p.iter().any(|page| !page.trim().is_empty());
    let mut fallback_note = String::new();
    let pages = match pdf_extract::extract_text_from_mem_by_pages(&bytes) {
        Ok(p) if has_text(&p) => p,
        extract => {
            let mut chain: Option<Vec<String>> = None;
            if let Some(Ok(p)) = inspector_pages(&bytes).filter(|r| r.as_ref().is_ok_and(|p| has_text(p))) {
                fallback_note = "pdf-inspector".into();
                chain = Some(p);
            } else if let Some(Ok(p)) =
                pdftotext_pages(&bytes).filter(|r| r.as_ref().is_ok_and(|p| has_text(p)))
            {
                fallback_note = "pdftotext (poppler)".into();
                chain = Some(p);
            }
            match chain {
                Some(p) => p,
                // Every reader failed or found nothing: report the most
                // specific error we have.
                None => {
                    return Err(match extract {
                        Err(e) => {
                            AppError::msg(format!("could not extract PDF text: {e}"))
                        }
                        Ok(_) => AppError::msg(
                            "no reader could find text in this PDF — it is likely a \
                             scanned document (OCR is not supported)",
                        ),
                    });
                }
            }
        }
    };

    let (meta_title, meta_author) = metadata(&bytes);
    let file_name = file_path.rsplit('/').next().unwrap_or(file_path).to_string();
    let fallback_title = file_name.trim_end_matches(".pdf").to_string();

    let (cleaned, stripped) = strip_page_furniture(&pages);
    let paras = paragraphs_from_pages(&cleaned);
    if paras.is_empty() {
        return Err(AppError::msg(
            "no extractable text in this PDF — it may be a scanned document (OCR is not supported)",
        ));
    }

    let mut warnings = Vec::new();
    if !fallback_note.is_empty() {
        warnings.push(format!(
            "Text extracted with {fallback_note} — the built-in reader found none."
        ));
    }
    if stripped > 0 {
        warnings.push(format!(
            "Skipped {stripped} header/footer line{} (running titles and page numbers).",
            if stripped == 1 { "" } else { "s" }
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
        warnings,
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
    fn strips_running_headers_with_embedded_numbers() {
        let mut pages = Vec::new();
        for i in 0..6usize {
            // Verso: number + book title at the top. Recto: chapter title +
            // number at the bottom — the classic alternation.
            pages.push(
                [
                    format!("{} Early Buddhist Theory of Knowledge", 20 + i),
                    String::new(),
                    format!("Body paragraph {i} opens the page."),
                    "more body text here".to_string(),
                    String::new(),
                    format!("Closing paragraph {i} of the verso."),
                ]
                .join("\n"),
            );
            pages.push(
                [
                    format!("Recto body starts {i} somewhere mid-sentence."),
                    "continues down the page".to_string(),
                    String::new(),
                    format!("The Historical Background {}", 21 + i),
                ]
                .join("\n"),
            );
        }
        let (cleaned, stripped) = strip_page_furniture(&pages);
        assert_eq!(stripped, 12); // one header line per page
        for p in &cleaned {
            assert!(!p.contains("Early Buddhist Theory of Knowledge"));
            assert!(!p.contains("The Historical Background"));
        }
        assert!(cleaned[0].contains("Body paragraph 0 opens the page."));
        assert!(cleaned[0].contains("Closing paragraph 0 of the verso."));
        assert!(cleaned[1].contains("Recto body starts 0"));
    }

    #[test]
    fn strips_bare_page_numbers_in_many_dresses() {
        let nums = ["12", "- 13 -", "[14]", "Page 15", "xvi", " p. 16 ", "2o", "i6"];
        let mut pages = Vec::new();
        for (i, n) in nums.iter().enumerate() {
            pages.push(
                [
                    format!("Opening line {i} of real prose."),
                    "second line".to_string(),
                    String::new(),
                    n.to_string(),
                ]
                .join("\n"),
            );
        }
        let (cleaned, stripped) = strip_page_furniture(&pages);
        assert_eq!(stripped, nums.len());
        for p in &cleaned {
            assert!(!p.trim().is_empty());
            assert!(p.contains("real prose"));
        }
    }

    #[test]
    fn keeps_one_off_headings_and_divider_pages() {
        let pages = vec![
            "CHAPTER ONE\n\nThe chapter opens.".to_string(),
            "Regular body text\n\non this page".to_string(),
            "A different heading\n\nMore prose here.".to_string(),
            "Even more prose\n\ncontinues".to_string(),
            "Trailing page\n\nof the sample".to_string(),
        ];
        let (cleaned, stripped) = strip_page_furniture(&pages);
        assert_eq!(stripped, 0);
        assert!(cleaned[0].contains("CHAPTER ONE"));
    }

    #[test]
    fn keeps_lone_numerals_and_years() {
        // A single roman chapter numeral or bare year is content, not a page
        // number — bare-number stripping needs the pattern to repeat.
        let pages = vec![
            "Intro text here\n\nsome prose".to_string(),
            "II\n\nTHE PROBLEM OF KNOWLEDGE\n\nChapter body starts here.".to_string(),
            "More body text\n\non this page".to_string(),
            "In 1947 the events\n\nunfolded over years".to_string(),
            "Final page of\n\nthe sample".to_string(),
        ];
        let (cleaned, stripped) = strip_page_furniture(&pages);
        assert_eq!(stripped, 0);
        assert!(cleaned[1].contains("II"));
    }

    #[test]
    fn roman_guard_rejects_words() {
        for w in ["civil", "mill", "mid", "did", "mix", "jolly", "zoo", "joy", "oil", "lily", "ill", "lol"] {
            assert!(!token_is_numberish(w), "{w} must not count as a page number");
        }
        for n in ["xiv", "mmxxvi", "xvii"] {
            assert!(token_is_numberish(n), "{n} should count as a page number");
        }
        // OCR-mangled numbers must count.
        for n in ["i6", "2o", "3*", "j6", "6y", "ioo", "IOI", "I0", "16", "iv"] {
            assert!(token_is_numberish(n), "{n} should be numberish");
        }
        assert_eq!(roman_value("iiv"), None);
    }

    #[test]
    fn real_pdf_furniture_is_stripped() {
        let Ok(bytes) =
            std::fs::read("../Early Buddhist Theory of Knowledge - KN Jayatilleke_1-100.pdf")
        else {
            return; // local fixture only
        };
        let pages = pdf_extract::extract_text_from_mem_by_pages(&bytes).unwrap();
        let (cleaned, stripped) = strip_page_furniture(&pages);
        let headerish_tops: Vec<String> = cleaned
            .iter()
            .filter_map(|p| {
                let ne: Vec<&str> = p.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
                if ne.len() < 2 {
                    return None;
                }
                let l = ne[0].to_lowercase();
                let named = l.contains("early buddhist theory")
                    || l.contains("historical background")
                    || l.ends_with("foreword")
                    || l.ends_with("preface")
                    || l.ends_with("contents")
                    || l.ends_with("abbreviations");
                // True running headers carry a page number at an edge; body
                // sentences merely quoting the book title must not match.
                let numbered = ne[0]
                    .split_whitespace()
                    .any(|t| token_is_numberish(t.trim_matches(|c: char| !c.is_alphanumeric())));
                (named && numbered).then(|| ne[0].to_string())
            })
            .collect();
        for t in &headerish_tops {
            println!("remaining header-like top: {t}");
        }
        println!(
            "pages: {}, stripped: {stripped}, header-like tops remaining: {}",
            pages.len(),
            headerish_tops.len()
        );
        assert!(stripped > 60, "expected most pages to lose a header");
        // Only the 2-page "Abbreviations" section (below the recurrence
        // threshold) may survive.
        assert_eq!(headerish_tops.len(), 1, "running headers should be gone");
    }

    #[test]
    fn markdown_is_flattened_to_plain_paragraphs() {
        let md = "# Preface\n\nSome **bold** and *italic* text.\n\n|Dhp|Dhammapada|\n|---|---|\n|AN|Aṅguttara|\n\nSee [the license](https://example.com) for details.\n```\ncode\n```\n";
        let plain = strip_markdown(md);
        assert!(plain.contains("Preface\n"));
        assert!(plain.contains("Some bold and italic text."));
        assert!(plain.contains("Dhp Dhammapada"));
        assert!(plain.contains("AN Aṅguttara"));
        assert!(plain.contains("See the license for details."));
        assert!(!plain.contains('['));
        assert!(!plain.contains('|'));
        assert!(!plain.contains('#'));
        assert!(!plain.contains("```"));
        assert!(!plain.contains("---"));
    }

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

#[cfg(test)]
mod wings_fixture {
    use super::*;

    // Local fixture only; skips silently when the PDF is absent.
    #[test]
    fn cid_font_pdf_parses_via_inspector_fallback() {
        let Ok(bytes) = std::fs::read("../Wings To Awakening - Thanissaro.pdf") else {
            return;
        };
        // The built-in reader genuinely finds nothing in this file: every
        // font is CID Type 0C with Identity-H encoding.
        let builtin = pdf_extract::extract_text_from_mem_by_pages(&bytes).unwrap();
        assert!(!builtin.iter().any(|p| !p.trim().is_empty()));

        let book = parse(bytes, "/tmp/Wings To Awakening - Thanissaro.pdf").unwrap();
        assert!(book.info.total_chars > 100_000, "got {}", book.info.total_chars);
        assert!(!book.info.segments.is_empty());
        assert!(
            book.info.warnings.iter().any(|w| w.contains("pdf-inspector")),
            "fallback should be flagged: {:?}",
            book.info.warnings
        );
        // Markdown syntax must not leak into translatable text.
        let docs = match &book.source {
            crate::types::BookSource::Pdf { docs, .. } => docs,
            _ => unreachable!(),
        };
        assert!(
            docs.iter()
                .flat_map(|d| d.blocks.iter())
                .filter(|b| !b.skipped)
                .all(|b| !b.text.contains("**") && !b.text.contains("](")),
            "markdown artifacts leaked into blocks"
        );
    }
}


