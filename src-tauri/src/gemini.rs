use crate::error::{AppError, Result};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

const API_BASE: &str = "https://generativelanguage.googleapis.com/v1beta/interactions";
const MAX_ATTEMPTS: u32 = 5;
/// 429s are expected on the free tier: allow many more of them, with longer
/// delays sized to outlast the per-minute rate window.
const MAX_RATE_HITS: u32 = 12;

fn rate_limit_delay(hits: u32) -> Duration {
    Duration::from_secs((5 * hits as u64).min(30))
}

/// Free-tier requests-per-minute by model family. Flash-Lite tiers are far
/// more generous; unknown models get the conservative Flash limit.
pub fn model_rpm(model: &str) -> u32 {
    if model.contains("lite") {
        500
    } else {
        20
    }
}

/// Rough token estimate for mixed-script text: Latin scripts average ~4
/// chars per token, Burmese ~2.3 chars per token.
pub fn estimate_tokens(text: &str) -> usize {
    let (ascii, other) = text.chars().fold((0usize, 0usize), |(a, o), c| {
        if c.is_ascii() { (a + 1, o) } else { (a, o + 1) }
    });
    estimate_counts(ascii, other)
}

/// Token estimate from pre-counted ASCII / non-ASCII code points.
pub fn estimate_counts(ascii: usize, other: usize) -> usize {
    ascii / 4 + (other * 10) / 23
}

/// Free-tier-optimal batch budget in estimated input tokens. Flash models
/// get ~25x fewer free requests than Flash-Lite, so we pack twice as much
/// text into each request for them.
pub fn batch_token_budget(model: &str) -> usize {
    if model.contains("lite") {
        5_000
    } else {
        10_000
    }
}

struct LimiterState {
    base: Duration,
    interval: Duration,
    next_start: Option<Instant>,
    /// Consecutive successful requests since the last 429.
    streak: u32,
}

/// Spaces request starts at least `60s / rpm` apart so a job never exceeds
/// the model's free-tier rate limit. When the API still answers 429, the
/// limiter slows the whole job down (see [`RateLimiter::penalize`]) so every
/// worker backs off together instead of each burning retries.
pub struct RateLimiter {
    state: std::sync::Mutex<LimiterState>,
}

impl RateLimiter {
    pub fn new(rpm: u32) -> Self {
        let base = Duration::from_millis(60_000u64.saturating_div(rpm.max(1) as u64).max(1));
        Self {
            state: std::sync::Mutex::new(LimiterState {
                base,
                interval: base,
                next_start: None,
                streak: 0,
            }),
        }
    }

    pub async fn acquire(&self) {
        loop {
            let wait = {
                let now = Instant::now();
                let mut st = self.state.lock().unwrap();
                let earliest = st.next_start.unwrap_or(now);
                if now >= earliest {
                    st.next_start = Some(now + st.interval);
                    return;
                }
                earliest - now
            };
            tokio::time::sleep(wait).await;
        }
    }

    /// Called when the API returns 429: double the spacing (up to 4x base)
    /// and hold the next request past the cool-down.
    pub fn penalize(&self, cooldown: Duration) {
        let mut st = self.state.lock().unwrap();
        st.interval = (st.interval * 2).min(st.base * 4);
        st.streak = 0;
        let target = Instant::now()
            .checked_add(cooldown)
            .unwrap_or_else(Instant::now);
        st.next_start = Some(st.next_start.map_or(target, |t| t.max(target)));
    }

    /// Called after a clean request: once the job has been running without a
    /// 429 for a while, relax back toward the base pacing so an early 429
    /// storm does not slow an otherwise-idle job forever.
    pub fn note_success(&self) {
        let mut st = self.state.lock().unwrap();
        st.streak += 1;
        if st.streak >= 20 && st.interval > st.base {
            st.interval = st.base + (st.interval - st.base) * 3 / 4;
            st.streak = 0;
        }
    }

    #[cfg(test)]
    fn snapshot(&self) -> (Duration, Duration, Option<Instant>) {
        let st = self.state.lock().unwrap();
        (st.base, st.interval, st.next_start)
    }
}

pub struct GeminiClient {
    http: reqwest::Client,
    api_key: String,
    model: String,
}

pub struct BatchResult {
    /// batch-local 1-based index -> translated text
    pub translations: BTreeMap<usize, String>,
    pub total_tokens: u64,
}

pub enum GeminiFailure {
    /// Give up immediately; the request can never succeed (bad key, bad model...).
    Fatal(AppError),
    /// Transient; already retried the configured number of times.
    Exhausted(AppError),
    /// The model hit an output limit mid-answer. Retrying the same request
    /// fails the same way — the caller should split the batch instead.
    Truncated(AppError),
}

impl GeminiFailure {
    pub fn message(&self) -> String {
        match self {
            Self::Fatal(e) | Self::Exhausted(e) | Self::Truncated(e) => e.to_string(),
        }
    }
}

fn backoff_delay(attempt: u32, retry_after: Option<u64>) -> Duration {
    if let Some(secs) = retry_after {
        return Duration::from_secs(secs.min(120));
    }
    let base = 1000u64.saturating_mul(1u64 << attempt.min(5)); // 1s, 2s, 4s, 8s, 16s
    let jitter = rand::random_range(0..=500u64);
    Duration::from_millis(base + jitter)
}

/// The Interactions API reports "completed" only when the model finished its
/// answer; an output cut off by the cap surfaces as another status. Retrying
/// such a request unchanged always fails — the caller splits the batch.
fn check_output_status(value: &Value) -> std::result::Result<(), AppError> {
    let status = value.get("status").and_then(Value::as_str).unwrap_or("completed");
    if status == "completed" {
        Ok(())
    } else {
        Err(AppError::msg(format!(
            "model output incomplete (status: {status}) — response was cut off by the output limit"
        )))
    }
}

impl GeminiClient {
    pub fn new(api_key: &str, model: &str) -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(180))
            .connect_timeout(Duration::from_secs(20))
            .build()
            .expect("reqwest client");
        Self {
            http,
            api_key: api_key.to_string(),
            model: model.to_string(),
        }
    }

    async fn post(&self, body: &Value) -> Result<(reqwest::StatusCode, Value, Option<u64>)> {
        let resp = self
            .http
            .post(API_BASE)
            .header("x-goog-api-key", &self.api_key)
            .json(body)
            .send()
            .await
            .map_err(|e| AppError::msg(format!("network error: {e}")))?;
        let status = resp.status();
        let retry_after = resp
            .headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok());
        let value: Value = resp.json().await.unwrap_or(Value::Null);
        Ok((status, value, retry_after))
    }

    fn api_error(status: reqwest::StatusCode, body: &Value) -> AppError {
        // The API can return the error object directly or wrapped in an array.
        let msg = body
            .pointer("/error/message")
            .or_else(|| body.pointer("/0/error/message"))
            .and_then(|v| v.as_str())
            .unwrap_or("unknown API error")
            .to_string();
        if msg.contains("API key not valid") || msg.contains("API_KEY_INVALID") {
            return AppError::msg("API key is not valid — check it in Settings.");
        }
        match status.as_u16() {
            400 => AppError::msg(format!(
                "API rejected the request (400): {msg}. Check the model ID and key."
            )),
            401 | 403 => AppError::msg(format!("Invalid or unauthorized API key ({status}): {msg}")),
            404 => AppError::msg(format!("Model not found (404): {msg}")),
            s => AppError::msg(format!("API error {s}: {msg}")),
        }
    }

    /// Cheap connectivity/auth check: ask for a one-word reply.
    pub async fn ping(&self) -> Result<String> {
        let body = json!({
            "model": self.model,
            "input": "Reply with exactly: OK",
            "generation_config": { "thinking_level": "minimal" },
            "store": false,
        });
        let (status, value, _) = self.post(&body).await?;
        if !status.is_success() {
            return Err(Self::api_error(status, &value));
        }
        let text = extract_output_text(&value)
            .unwrap_or_default()
            .trim()
            .to_string();
        Ok(text)
    }

    /// Rate-limit retries get their own generous budget and longer delays:
    /// hitting the RPM cap is expected on the free tier and must never fail
    /// a batch. Every 429 also slows the shared limiter for all workers.
    pub async fn translate_batch(
        &self,
        limiter: &RateLimiter,
        target_lang: &str,
        custom_instructions: &str,
        paragraphs: &[(usize, String)],
    ) -> std::result::Result<BatchResult, GeminiFailure> {
        debug_assert!(!paragraphs.is_empty());

        let mut extra = String::new();
        if !custom_instructions.trim().is_empty() {
            extra = format!(
                "\nAdditional requirements from the user:\n{}\n",
                custom_instructions.trim()
            );
        }
        let system = format!(
            "You are a professional literary translator. You translate numbered text \
paragraphs into {lang}.\n\
Rules:\n\
- Translate faithfully, preserving meaning, tone, and register.\n\
- Keep paragraph numbering exact: return the same \"i\" values you were given.\n\
- Never merge, split, add, omit, or reorder paragraphs.\n\
- Preserve proper nouns, numbers, URLs, and code snippets; transliterate names only \
when customary in {lang}.\n\
- If a paragraph is already in {lang}, copy it through unchanged.\n\
- Return only JSON matching the required schema.{extra}",
            lang = target_lang,
            extra = extra
        );

        let items: Vec<Value> = paragraphs
            .iter()
            .map(|(i, t)| json!({ "i": i, "t": t }))
            .collect();

        let body = json!({
            "model": self.model,
            "system_instruction": system,
            "input": serde_json::to_string(&json!({ "paragraphs": items })).unwrap_or_default(),
            "generation_config": { "thinking_level": "low", "temperature": 0.2 },
            "response_format": {
                "type": "text",
                "mime_type": "application/json",
                "schema": {
                    "type": "object",
                    "properties": {
                        "translations": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "i": { "type": "integer" },
                                    "t": { "type": "string" }
                                },
                                "required": ["i", "t"]
                            }
                        }
                    },
                    "required": ["translations"]
                }
            },
            "store": false,
        });

        let mut last_err: Option<AppError> = None;
        let mut attempt: u32 = 0; // transient failures (network, 5xx, bad output)
        let mut rate_hits: u32 = 0; // 429s

        while attempt < MAX_ATTEMPTS && rate_hits <= MAX_RATE_HITS {
            let (status, value, retry_after) = match self.post(&body).await {
                Ok(r) => r,
                Err(e) => {
                    attempt += 1;
                    last_err = Some(e);
                    tokio::time::sleep(backoff_delay(attempt, None)).await;
                    continue;
                }
            };

            if status.as_u16() == 429 {
                rate_hits += 1;
                // Prefer the server's Retry-After; otherwise assume we blew
                // the sliding RPM window and wait out a share of a minute.
                let cooldown = retry_after
                    .filter(|s| *s > 0)
                    .map(Duration::from_secs)
                    .unwrap_or_else(|| rate_limit_delay(rate_hits));
                limiter.penalize(cooldown);
                last_err = Some(Self::api_error(status, &value));
                tokio::time::sleep(cooldown).await;
                continue;
            }
            if status.is_server_error() || status.as_u16() == 408 {
                attempt += 1;
                last_err = Some(Self::api_error(status, &value));
                tokio::time::sleep(backoff_delay(attempt, retry_after)).await;
                continue;
            }
            if !status.is_success() {
                return Err(GeminiFailure::Fatal(Self::api_error(status, &value)));
            }
            check_output_status(&value).map_err(GeminiFailure::Truncated)?;

            let tokens = value
                .pointer("/usage/total_tokens")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);

            let text = match extract_output_text(&value) {
                Some(t) => t,
                None => {
                    attempt += 1;
                    last_err = Some(AppError::msg("model returned no text output"));
                    tokio::time::sleep(backoff_delay(attempt, None)).await;
                    continue;
                }
            };

            return match parse_translations(&text) {
                Ok(mut map) => {
                    // Keep only requested indices; require full coverage.
                    let wanted: std::collections::BTreeSet<usize> =
                        paragraphs.iter().map(|(i, _)| *i).collect();
                    map.retain(|k, _| wanted.contains(k));
                    if map.len() == wanted.len() {
                        Ok(BatchResult {
                            translations: map,
                            total_tokens: tokens,
                        })
                    } else {
                        attempt += 1;
                        last_err = Some(AppError::msg(format!(
                            "model returned {} of {} paragraphs",
                            map.len(),
                            wanted.len()
                        )));
                        tokio::time::sleep(backoff_delay(attempt, None)).await;
                        continue;
                    }
                }
                Err(e) => {
                    attempt += 1;
                    last_err = Some(e);
                    tokio::time::sleep(backoff_delay(attempt, None)).await;
                    continue;
                }
            };
        }

        let err = last_err.unwrap_or_else(|| AppError::msg("translation attempts exhausted"));
        Err(GeminiFailure::Exhausted(err))
    }
}

/// Pull concatenated model text out of an Interactions API response:
/// steps[type=model_output].content[type=text].text
pub fn extract_output_text(value: &Value) -> Option<String> {
    let mut out = String::new();
    let steps = value.get("steps")?.as_array()?;
    for step in steps {
        if step.get("type").and_then(|t| t.as_str()) != Some("model_output") {
            continue;
        }
        if let Some(content) = step.get("content").and_then(|c| c.as_array()) {
            for block in content {
                if block.get("type").and_then(|t| t.as_str()) == Some("text") {
                    if let Some(t) = block.get("text").and_then(|t| t.as_str()) {
                        out.push_str(t);
                    }
                }
            }
        }
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

/// Parse the model's JSON reply. Tolerates markdown fences around the JSON.
pub fn parse_translations(text: &str) -> Result<BTreeMap<usize, String>> {
    let trimmed = text.trim();
    let body = trimmed
        .strip_prefix("```")
        .map(|s| {
            let s = s.strip_prefix("json").unwrap_or(s);
            s.trim_start_matches(['\n', '\r'])
        })
        .and_then(|s| s.strip_suffix("```"))
        .unwrap_or(trimmed);

    let value: Value = serde_json::from_str(body.trim()).map_err(|e| {
        AppError::msg(format!("model returned invalid JSON: {e}"))
    })?;

    let arr = value
        .get("translations")
        .and_then(|t| t.as_array())
        .ok_or_else(|| AppError::msg("missing \"translations\" array in model output"))?;

    let mut map = BTreeMap::new();
    for item in arr {
        let i = item
            .get("i")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| AppError::msg("translation item missing \"i\""))?;
        let t = item
            .get("t")
            .and_then(|v| v.as_str())
            .ok_or_else(|| AppError::msg("translation item missing \"t\""))?;
        map.insert(i as usize, t.to_string());
    }
    if map.is_empty() {
        return Err(AppError::msg("translations array was empty"));
    }
    Ok(map)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_text_from_interactions_response() {
        let v: Value = serde_json::from_str(
            r#"{
              "id": "x", "status": "completed",
              "usage": { "total_tokens": 42 },
              "steps": [
                { "type": "thought", "signature": "..." },
                { "type": "model_output", "content": [
                    { "type": "text", "text": "{\"translations\":" },
                    { "type": "text", "text": "[{\"i\":1,\"t\":\"မင်္ဂလာပါ\"}]}" }
                ]}
              ]
            }"#,
        )
        .unwrap();
        let text = extract_output_text(&v).unwrap();
        assert!(text.contains("translations"));
        let map = parse_translations(&text).unwrap();
        assert_eq!(map.get(&1).map(String::as_str), Some("မင်္ဂလာပါ"));
    }

    #[test]
    fn limiter_slows_down_on_penalize() {
        let l = RateLimiter::new(60); // base spacing 1s
        let (base, interval, _) = l.snapshot();
        assert_eq!(base, Duration::from_secs(1));
        assert_eq!(interval, Duration::from_secs(1));

        l.penalize(Duration::from_secs(3));
        let (_, interval, next) = l.snapshot();
        assert_eq!(interval, Duration::from_secs(2)); // doubled
        let now = Instant::now();
        assert!(next.map(|t| t > now + Duration::from_secs(2)).unwrap_or(false)); // held ~3s out

        l.penalize(Duration::from_secs(1));
        l.penalize(Duration::from_secs(1));
        let (_, interval, _) = l.snapshot();
        assert_eq!(interval, Duration::from_secs(4)); // capped at 4x base
    }

    #[test]
    fn maps_model_rpm() {
        assert_eq!(model_rpm("gemini-3.5-flash-lite"), 500);
        assert_eq!(model_rpm("gemini-3.8-flash"), 20);
        assert_eq!(model_rpm("something-else"), 20);
    }

    #[test]
    fn limiter_relaxes_after_a_clean_stretch() {
        let l = RateLimiter::new(60); // base spacing 1s
        l.penalize(Duration::ZERO);
        l.penalize(Duration::ZERO);
        l.penalize(Duration::ZERO);
        l.penalize(Duration::ZERO);
        let (_, interval, _) = l.snapshot();
        assert_eq!(interval, Duration::from_secs(4)); // 4x base after 429s

        // Successes only count once the cap is reached; then the interval
        // decays back toward base in steps of 25% of the excess per 20.
        for _ in 0..19 {
            l.note_success();
        }
        assert_eq!(l.snapshot().1, Duration::from_secs(4));
        for _ in 0..8 {
            for _ in 0..20 {
                l.note_success();
            }
        }
        let (_, interval, _) = l.snapshot();
        assert!(
            interval <= Duration::from_millis(1400),
            "interval should decay toward base, got {interval:?}"
        );
    }

    #[test]
    fn estimates_tokens_by_script() {
        assert_eq!(estimate_tokens("Hello world, this is fine."), 6); // 26 ascii / 4
        assert_eq!(estimate_tokens("မင်္ဂလာပါကျွန်ုပ်"), 7); // 17 non-ascii code points / 2.3
        assert_eq!(batch_token_budget("gemini-3.5-flash-lite"), 5_000);
        assert_eq!(batch_token_budget("gemini-3.8-flash"), 10_000);
    }

    #[test]
    fn flags_incomplete_status_as_truncation() {
        let v: Value = serde_json::from_str(r#"{"status": "incomplete"}"#).unwrap();
        let err = check_output_status(&v).unwrap_err();
        assert!(err.to_string().contains("incomplete"), "{err}");
        let v: Value = serde_json::from_str(r#"{"status": "completed"}"#).unwrap();
        assert!(check_output_status(&v).is_ok());
        // Missing status (older shapes) is treated as completed.
        assert!(check_output_status(&serde_json::json!({})).is_ok());
    }

    #[test]
    fn parses_fenced_json() {
        let map = parse_translations("```json\n{\"translations\":[{\"i\":1,\"t\":\"a\"}]}\n```")
            .unwrap();
        assert_eq!(map.get(&1).map(String::as_str), Some("a"));
    }
}
