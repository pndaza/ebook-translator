//! Persistent per-segment translation cache.
//!
//! The per-job resume log only covers whole batches of one specific file.
//! This cache keys every translated segment by everything that shapes the
//! translation — model, target language, custom instructions, prompt version,
//! and the text itself — so anything translated once is never paid for again:
//! across files, re-runs, or a sample run followed by the full book.
//!
//! Storage is an append-only JSONL file; a corrupt line costs one segment's
//! reuse, not the app.

use serde_json::json;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

/// Bump when the translation function itself changes (prompt wording,
/// temperature, part-joining) so stale entries stop matching. v2: prompt now
/// asks for natural, idiomatic translation instead of faithful/literal.
pub(crate) const PROMPT_VERSION: u8 = 2;

pub struct TranslationCache {
    map: HashMap<String, String>,
    file: Option<File>,
    path: PathBuf,
}

impl TranslationCache {
    /// Everything that shapes a translation besides the text itself, hashed
    /// into a per-job prefix so entries never leak across models, languages,
    /// or instruction sets.
    pub fn prefix(model: &str, lang: &str, instructions: &str) -> String {
        let mut ih = Sha256::new();
        ih.update(instructions.trim().as_bytes());
        let mut h = Sha256::new();
        h.update(model.as_bytes());
        h.update([0u8]);
        h.update(lang.as_bytes());
        h.update([0u8]);
        h.update(&ih.finalize()[..4]);
        h.update([PROMPT_VERSION]);
        to_hex(&h.finalize())[..16].to_string()
    }

    pub fn open(dir: &Path) -> Self {
        let _ = std::fs::create_dir_all(dir);
        let path = dir.join("translation-cache.jsonl");
        let mut map = HashMap::new();
        if let Ok(f) = File::open(&path) {
            for line in BufReader::new(f).lines().map_while(std::result::Result::ok) {
                let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else {
                    continue;
                };
                if let (Some(k), Some(t)) = (
                    v.get("k").and_then(|k| k.as_str()),
                    v.get("t").and_then(|t| t.as_str()),
                ) {
                    map.insert(k.to_string(), t.to_string());
                }
            }
        }
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .ok();
        Self { map, file, path }
    }

    pub fn get(&self, prefix: &str, text: &str) -> Option<String> {
        self.map.get(&key(prefix, text)).cloned()
    }

    pub fn put(&mut self, prefix: &str, text: &str, translation: &str) {
        let k = key(prefix, text);
        if self.map.get(&k).map(String::as_str) == Some(translation) {
            return;
        }
        self.map.insert(k.clone(), translation.to_string());
        if let Some(f) = self.file.as_mut() {
            let _ = writeln!(f, "{}", json!({ "k": k, "t": translation }));
            let _ = f.flush();
        }
    }

    pub fn clear(&mut self) {
        self.map.clear();
        self.file = None; // drop the append handle before truncating
        let _ = std::fs::write(&self.path, b"");
        self.file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .ok();
    }

    /// (entries, bytes on disk)
    pub fn stats(&self) -> (usize, u64) {
        (
            self.map.len(),
            std::fs::metadata(&self.path)
                .map(|m| m.len())
                .unwrap_or(0),
        )
    }
}

fn key(prefix: &str, text: &str) -> String {
    let mut h = Sha256::new();
    h.update(prefix.as_bytes());
    h.update([0u8]);
    h.update(text.as_bytes());
    to_hex(&h.finalize())
}

fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_across_reopen_and_is_context_scoped() {
        let dir = std::env::temp_dir().join(format!("ebtr-cache-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        {
            let mut c = TranslationCache::open(&dir);
            let p = TranslationCache::prefix("model-a", "Burmese", "");
            assert!(c.get(&p, "hello world").is_none());
            c.put(&p, "hello world", "မင်္ဂလာပါ");
            assert_eq!(c.get(&p, "hello world").as_deref(), Some("မင်္ဂလာပါ"));
        }
        {
            let c = TranslationCache::open(&dir);
            let p = TranslationCache::prefix("model-a", "Burmese", "");
            assert_eq!(c.get(&p, "hello world").as_deref(), Some("မင်္ဂလာပါ"));
            // Any change of context must stop the entry from matching.
            for p in [
                TranslationCache::prefix("model-b", "Burmese", ""),
                TranslationCache::prefix("model-a", "English", ""),
                TranslationCache::prefix("model-a", "Burmese", "formal tone"),
            ] {
                assert!(c.get(&p, "hello world").is_none());
            }
        }
        // Whitespace-only instruction changes share a prefix with empty ones,
        // mirroring the resume-key normalization.
        assert_eq!(
            TranslationCache::prefix("m", "l", "  "),
            TranslationCache::prefix("m", "l", "")
        );
        {
            let mut c = TranslationCache::open(&dir);
            c.clear();
            let p = TranslationCache::prefix("model-a", "Burmese", "");
            assert!(c.get(&p, "hello world").is_none());
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
