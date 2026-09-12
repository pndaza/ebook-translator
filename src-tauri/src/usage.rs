//! Per-model daily request usage, tracked locally.
//!
//! The free tier's daily quota resets at 08:00 UTC (2:30 PM Myanmar,
//! UTC+6:30). We count every successful translation request per model so
//! the UI can show today's usage and the next reset time, and we remember
//! which models the server has declared exhausted for today — server truth
//! beats our local count, and it lets a new job skip dead models without
//! paying a 429 to find out.

use serde::Serialize;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

const DAY_SECS: u64 = 86_400;
/// 08:00 UTC == 2:30 PM Myanmar.
const RESET_UTC_SECS: u64 = 8 * 3600;
/// Days are counted from this shift so the boundary lands at RESET_UTC_SECS.
const RESET_SHIFT: u64 = DAY_SECS - RESET_UTC_SECS;

#[derive(Default, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelUsage {
    pub requests: u64,
    pub exhausted: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageSnapshot {
    /// Epoch seconds of the next daily reset.
    pub reset_at: u64,
    pub models: HashMap<String, ModelUsage>,
}

#[derive(Default)]
struct UsageState {
    day: u64,
    models: HashMap<String, ModelUsage>,
}

pub struct UsageTracker {
    path: PathBuf,
    state: Mutex<UsageState>,
}

fn now_epoch() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn quota_day(t: u64) -> u64 {
    (t + RESET_SHIFT) / DAY_SECS
}

impl UsageTracker {
    pub fn open(path: PathBuf) -> Self {
        let mut state = UsageState {
            day: quota_day(now_epoch()),
            ..Default::default()
        };
        if let Ok(raw) = std::fs::read_to_string(&path) {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&raw) {
                state.day = v.get("day").and_then(|d| d.as_u64()).unwrap_or(state.day);
                if let Some(obj) = v.get("models").and_then(|m| m.as_object()) {
                    for (model, mv) in obj {
                        state.models.insert(
                            model.clone(),
                            ModelUsage {
                                requests: mv.get("requests").and_then(|x| x.as_u64()).unwrap_or(0),
                                exhausted: mv
                                    .get("exhausted")
                                    .and_then(|x| x.as_bool())
                                    .unwrap_or(false),
                            },
                        );
                    }
                }
            }
        }
        // A file from a previous quota day starts clean.
        let today = quota_day(now_epoch());
        if state.day != today {
            state.day = today;
            state.models.clear();
        }
        Self {
            path,
            state: Mutex::new(state),
        }
    }

    /// Clear the counters when the quota day rolls over under our feet.
    fn roll(state: &mut UsageState) {
        let today = quota_day(now_epoch());
        if state.day != today {
            state.day = today;
            state.models.clear();
        }
    }

    fn persist(&self, state: &UsageState) {
        let obj = serde_json::json!({ "day": state.day, "models": state.models });
        // Write-then-rename so a crash mid-write cannot truncate the file.
        let tmp = self.path.with_extension("json.tmp");
        if std::fs::write(&tmp, obj.to_string()).is_ok() {
            let _ = std::fs::rename(&tmp, &self.path);
        }
    }

    /// Count one successful (2xx) request against `model`'s daily total.
    pub fn record_request(&self, model: &str) {
        let mut st = self.state.lock().unwrap();
        Self::roll(&mut st);
        st.models
            .entry(model.to_string())
            .or_default()
            .requests += 1;
        self.persist(&st);
    }

    /// Remember that the server declared `model` out of daily quota.
    pub fn mark_exhausted(&self, model: &str) {
        let mut st = self.state.lock().unwrap();
        Self::roll(&mut st);
        st.models.entry(model.to_string()).or_default().exhausted = true;
        self.persist(&st);
    }

    pub fn snapshot(&self) -> UsageSnapshot {
        let mut st = self.state.lock().unwrap();
        Self::roll(&mut st);
        let reset_at = (st.day + 1) * DAY_SECS - RESET_SHIFT;
        UsageSnapshot {
            reset_at,
            models: st.models.clone(),
        }
    }

    /// Current quota-day number (rolls at 08:00 UTC).
    pub fn day(&self) -> u64 {
        let mut st = self.state.lock().unwrap();
        Self::roll(&mut st);
        st.day
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_path(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("ebtr-usage-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_file(&p);
        p
    }

    #[test]
    fn quota_day_boundary_is_08utc() {
        let e = 1_767_225_600; // 2026-01-01T00:00:00Z
        assert_eq!(quota_day(e + 7 * 3600 + 3599), quota_day(e)); // 07:59:59
        assert_eq!(quota_day(e + 8 * 3600), quota_day(e) + 1); // 08:00:00
    }

    #[test]
    fn counts_persist_and_day_rolls_over() {
        let path = tmp_path("roll");
        {
            let t = UsageTracker::open(path.clone());
            t.record_request("gemini-3.5-flash");
            t.record_request("gemini-3.5-flash");
            t.mark_exhausted("gemini-3.5-flash");
            let s = t.snapshot();
            let m = &s.models["gemini-3.5-flash"];
            assert_eq!(m.requests, 2);
            assert!(m.exhausted);
            assert_eq!(s.reset_at % DAY_SECS, RESET_UTC_SECS); // lands on 08:00 UTC
        }
        // Simulate the tracker being opened after the next reset.
        let raw = std::fs::read_to_string(&path).unwrap();
        let mut v: serde_json::Value = serde_json::from_str(&raw).unwrap();
        *v.get_mut("day").unwrap() = json_day_minus_one();
        std::fs::write(&path, v.to_string()).unwrap();
        {
            let t = UsageTracker::open(path.clone());
            let s = t.snapshot();
            assert!(s.models.is_empty(), "a new quota day starts clean");
        }
        let _ = std::fs::remove_file(&path);
    }

    fn json_day_minus_one() -> serde_json::Value {
        serde_json::json!(quota_day(now_epoch()) - 1)
    }
}
