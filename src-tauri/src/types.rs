use serde::{Deserialize, Serialize};

/// Serializable book overview returned to the UI after loading a file.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BookInfo {
    pub format: String, // "epub" | "pdf"
    pub file_path: String,
    pub file_name: String,
    pub title: String,
    pub author: String,
    pub cover_data_url: Option<String>,
    pub total_chars: usize,
    pub segments: Vec<SegmentInfo>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SegmentInfo {
    pub id: String,
    pub title: String,
    pub blocks: usize,
    pub chars: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JobOptions {
    pub target_lang: String,
    pub mode: String, // "bilingual" | "translated"
    pub model: String,
    pub custom_instructions: String,
}

impl Default for JobOptions {
    fn default() -> Self {
        Self {
            target_lang: "Burmese".into(),
            mode: "bilingual".into(),
            model: "gemini-3.5-flash-lite".into(),
            custom_instructions: String::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SegmentStatus {
    pub id: String,
    pub title: String,
    pub state: String, // pending | active | done | failed
    pub done: usize,
    pub total: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JobProgress {
    pub status: String, // running | completed | cancelled | failed
    pub segments: Vec<SegmentStatus>,
    pub batches_total: usize,
    pub batches_done: usize,
    pub batches_failed: usize,
    pub chars_done: usize,
    pub chars_total: usize,
    pub tokens_used: u64,
    pub error: Option<String>,
    pub output_ready: bool,
}

/// A single translatable text block inside a content document.
#[derive(Debug, Clone, Default)]
pub struct Block {
    pub text: String,
    /// Blocks that are too short / numeric-only are skipped entirely.
    pub skipped: bool,
    pub translation: Option<String>,
    /// Original tag name (p, li, td, ...), used to pick the sibling tag for
    /// bilingual insertion so lists and tables stay valid.
    pub tag: String,
}

/// One content document (a spine XHTML file for EPUB, a generated chapter for PDF).
#[derive(Debug, Clone)]
pub struct ContentDoc {
    /// Zip entry path for EPUB, chapter id for PDF.
    pub path: String,
    pub title: String,
    pub html: String,
    pub blocks: Vec<Block>,
}

/// A book fully loaded in memory, ready for translation and assembly.
pub struct LoadedBook {
    pub info: BookInfo,
    pub source: BookSource,
}

pub enum BookSource {
    /// Original EPUB bytes; spine documents parsed into `docs`.
    Epub { bytes: Vec<u8>, docs: Vec<ContentDoc> },
    /// Original PDF bytes; content converted to synthesized chapter documents.
    Pdf { bytes: Vec<u8>, docs: Vec<ContentDoc> },
}

pub const MODE_BILINGUAL: &str = "bilingual";
pub const MODE_TRANSLATED: &str = "translated";

/// A block is worth translating when it has at least 2 characters and
/// contains at least one letter (digits-only blocks are page numbers etc.).
pub fn is_translatable(text: &str) -> bool {
    let t = text.trim();
    t.chars().count() >= 2 && t.chars().any(|c| c.is_alphabetic())
}

pub fn valid_mode(mode: &str) -> bool {
    mode == MODE_BILINGUAL || mode == MODE_TRANSLATED
}

#[cfg(test)]
pub fn docs_of(book: &LoadedBook) -> &Vec<ContentDoc> {
    match &book.source {
        BookSource::Epub { docs, .. } | BookSource::Pdf { docs, .. } => docs,
    }
}

#[cfg(test)]
pub fn docs_of_mut(book: &mut LoadedBook) -> &mut Vec<ContentDoc> {
    match &mut book.source {
        BookSource::Epub { docs, .. } | BookSource::Pdf { docs, .. } => docs,
    }
}
