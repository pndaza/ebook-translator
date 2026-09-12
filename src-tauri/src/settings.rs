use crate::error::{AppError, Result};
use crate::types::JobOptions;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    pub api_key: String,
    pub model: String,
    pub mode: String,
    pub custom_instructions: String,
    /// Continue on the next same-tier model when the daily quota dies.
    pub auto_switch_model: bool,
}

impl Default for Settings {
    fn default() -> Self {
        let o = JobOptions::default();
        Self {
            api_key: String::new(),
            model: o.model,
            mode: o.mode,
            custom_instructions: o.custom_instructions,
            auto_switch_model: true,
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
    match serde_json::from_str::<Settings>(&raw) {
        Ok(s) => Ok(s),
        // A truncated/corrupt file must not silently swallow the user's API
        // key: preserve the bytes for recovery, then start from defaults.
        Err(e) => {
            let backup = path.with_extension("json.corrupt");
            let _ = std::fs::copy(path, &backup);
            Err(AppError::msg(format!(
                "settings file was corrupt (kept a copy at {}): {e}",
                backup.display()
            )))
        }
    }
}

pub fn save(path: &PathBuf, s: &Settings) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    // Write-then-rename so a crash mid-write can't truncate the settings.
    let tmp = path.with_extension("json.tmp");
    {
        use std::io::Write;
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(serde_json::to_string_pretty(s)?.as_bytes())?;
        f.sync_all()?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
    }
    std::fs::rename(&tmp, path)?;
    Ok(())
}
