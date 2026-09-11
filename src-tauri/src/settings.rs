use crate::error::Result;
use crate::types::JobOptions;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    pub api_key: String,
    pub model: String,
    pub target_lang: String,
    pub mode: String,
    pub custom_instructions: String,
}

impl Default for Settings {
    fn default() -> Self {
        let o = JobOptions::default();
        Self {
            api_key: String::new(),
            model: o.model,
            target_lang: o.target_lang,
            mode: o.mode,
            custom_instructions: o.custom_instructions,
        }
    }
}

pub fn load(path: &PathBuf) -> Result<Settings> {
    if !path.exists() {
        return Ok(Settings::default());
    }
    let raw = std::fs::read_to_string(path)?;
    if raw.trim().is_empty() {
        return Ok(Settings::default());
    }
    let s: Settings = serde_json::from_str(&raw).unwrap_or_default();
    Ok(s)
}

pub fn save(path: &PathBuf, s: &Settings) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, serde_json::to_string_pretty(s)?)?;
    Ok(())
}
