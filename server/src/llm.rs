use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::{Mutex, Semaphore};
use tokio::time::Instant;

use crate::config::{Config, ProviderConfig};

/// Where to send a chat request: an OpenAI-compatible endpoint plus a model id.
#[derive(Debug, Clone)]
pub struct ModelRef {
    /// Provider key from providers.toml; requests to the same provider share
    /// one rate-limit queue.
    pub provider_key: String,
    pub base_url: String,
    pub api_key: Option<String>,
    pub model: String,
    pub max_concurrent: u32,
    pub requests_per_minute: Option<u32>,
}

impl ModelRef {
    pub fn from_provider(key: &str, provider: &ProviderConfig, model: &str) -> Self {
        Self {
            provider_key: key.to_string(),
            base_url: provider.base_url.trim_end_matches('/').to_string(),
            api_key: provider.resolve_api_key(),
            model: model.to_string(),
            max_concurrent: provider.max_concurrent.max(1),
            requests_per_minute: provider.requests_per_minute,
        }
    }

    pub fn for_agent(config: &Config, agent_id: &str) -> Result<Self> {
        let agent = config
            .agent(agent_id)
            .ok_or_else(|| anyhow!("unknown agent '{agent_id}'"))?;
        let provider = config
            .providers
            .get(&agent.provider)
            .ok_or_else(|| anyhow!("unknown provider '{}'", agent.provider))?;
        Ok(Self::from_provider(&agent.provider, provider, &agent.model))
    }
}

/// Per-provider request gate: bounded concurrency plus paced request starts.
/// This is what lets a whole population tournament run against a free tier
/// (e.g. Groq at 30 RPM) without tripping 429s — calls queue up here instead.
struct ProviderLimiter {
    semaphore: Semaphore,
    /// Next allowed request start, spaced by 60s / requests_per_minute.
    next_slot: Mutex<Instant>,
    min_interval: Option<Duration>,
}

impl ProviderLimiter {
    fn new(max_concurrent: u32, requests_per_minute: Option<u32>) -> Self {
        Self {
            semaphore: Semaphore::new(max_concurrent as usize),
            next_slot: Mutex::new(Instant::now()),
            min_interval: requests_per_minute
                .filter(|rpm| *rpm > 0)
                .map(|rpm| Duration::from_secs_f64(60.0 / rpm as f64)),
        }
    }

    async fn acquire(&self) -> tokio::sync::SemaphorePermit<'_> {
        let permit = self.semaphore.acquire().await.expect("limiter closed");
        if let Some(interval) = self.min_interval {
            let wake = {
                let mut next = self.next_slot.lock().await;
                let now = Instant::now();
                let slot = (*next).max(now);
                *next = slot + interval;
                slot
            };
            tokio::time::sleep_until(wake).await;
        }
        permit
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

impl ChatMessage {
    pub fn system(content: impl Into<String>) -> Self {
        Self { role: "system".into(), content: content.into() }
    }
    pub fn user(content: impl Into<String>) -> Self {
        Self { role: "user".into(), content: content.into() }
    }
    pub fn assistant(content: impl Into<String>) -> Self {
        Self { role: "assistant".into(), content: content.into() }
    }
}

#[derive(Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    messages: &'a [ChatMessage],
    temperature: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    response_format: Option<Value>,
}

#[derive(Deserialize)]
struct ChatResponse {
    choices: Vec<Choice>,
}

#[derive(Deserialize)]
struct Choice {
    message: ResponseMessage,
}

#[derive(Deserialize)]
struct ResponseMessage {
    content: Option<String>,
}

/// Provider-agnostic chat client. Everything we target (OpenRouter, Groq,
/// Ollama, ...) speaks the OpenAI chat-completions protocol.
#[derive(Clone)]
pub struct LlmClient {
    http: reqwest::Client,
    limiters: Arc<Mutex<HashMap<String, Arc<ProviderLimiter>>>>,
}

/// How many times a single request retries on 429/5xx before giving up.
const MAX_RATE_LIMIT_RETRIES: u32 = 5;

impl LlmClient {
    pub fn new() -> Self {
        Self {
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(300))
                .build()
                .expect("building http client"),
            limiters: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    async fn limiter_for(&self, model: &ModelRef) -> Arc<ProviderLimiter> {
        let mut limiters = self.limiters.lock().await;
        limiters
            .entry(model.provider_key.clone())
            .or_insert_with(|| {
                Arc::new(ProviderLimiter::new(
                    model.max_concurrent,
                    model.requests_per_minute,
                ))
            })
            .clone()
    }

    /// Plain-text chat; used for free-form outputs (kept for upcoming
    /// GM narration / curriculum features).
    #[allow(dead_code)]
    pub async fn chat(&self, model: &ModelRef, messages: &[ChatMessage]) -> Result<String> {
        self.chat_inner(model, messages, false).await
    }

    async fn chat_inner(
        &self,
        model: &ModelRef,
        messages: &[ChatMessage],
        json_mode: bool,
    ) -> Result<String> {
        let url = format!("{}/chat/completions", model.base_url);
        let body = ChatRequest {
            model: &model.model,
            messages,
            temperature: 0.8,
            // OpenAI-style JSON mode, honored by Ollama/Groq/OpenRouter alike.
            response_format: json_mode.then(|| serde_json::json!({ "type": "json_object" })),
        };
        let limiter = self.limiter_for(model).await;

        for retry in 0..=MAX_RATE_LIMIT_RETRIES {
            let permit = limiter.acquire().await;
            let mut req = self.http.post(&url).json(&body);
            if let Some(key) = &model.api_key {
                req = req.bearer_auth(key);
            }
            let resp = req.send().await.with_context(|| format!("POST {url}"))?;
            let status = resp.status();
            let retry_after = resp
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<f64>().ok());
            let text = resp.text().await?;
            drop(permit);

            // 429 / transient 5xx: back off (honoring Retry-After) and requeue.
            if status.as_u16() == 429 || status.is_server_error() {
                if retry == MAX_RATE_LIMIT_RETRIES {
                    bail!("{} returned {status} after {retry} retries: {text}", model.model);
                }
                let wait = retry_after
                    .map(Duration::from_secs_f64)
                    .unwrap_or_else(|| Duration::from_secs(2u64.pow(retry + 1)));
                tracing::warn!(
                    model = %model.model,
                    provider = %model.provider_key,
                    %status,
                    wait_s = wait.as_secs_f64(),
                    retry,
                    "rate limited / server error, backing off"
                );
                tokio::time::sleep(wait).await;
                continue;
            }
            if !status.is_success() {
                bail!("{} returned {status}: {text}", model.model);
            }
            let parsed: ChatResponse = serde_json::from_str(&text)
                .with_context(|| format!("parsing response: {text}"))?;
            return parsed
                .choices
                .into_iter()
                .next()
                .and_then(|c| c.message.content)
                .ok_or_else(|| anyhow!("empty completion from {}", model.model));
        }
        unreachable!("retry loop always returns or bails")
    }

    /// Chat that must produce JSON, optionally validated against a JSON Schema.
    /// On malformed output, re-prompts with the error (repair loop) up to
    /// `max_repairs` times. This is the main defense against flaky free-tier
    /// providers that ignore formatting instructions.
    pub async fn chat_json(
        &self,
        model: &ModelRef,
        messages: &[ChatMessage],
        schema: Option<&Value>,
        max_repairs: u32,
    ) -> Result<Value> {
        let validator = schema
            .map(|s| jsonschema::validator_for(s).context("compiling JSON schema"))
            .transpose()?;

        let mut conversation = messages.to_vec();
        let mut last_error = String::new();

        for attempt in 0..=max_repairs {
            if attempt > 0 {
                conversation.push(ChatMessage::user(format!(
                    "Your previous reply was invalid: {last_error}\n\
                     Reply again with ONLY a valid JSON object, no prose, no code fences."
                )));
            }
            let raw = self.chat_inner(model, &conversation, true).await?;
            conversation.push(ChatMessage::assistant(raw.clone()));

            let value = match extract_json(&raw) {
                Ok(v) => v,
                Err(e) => {
                    last_error = e.to_string();
                    tracing::warn!(model = %model.model, attempt, error = %last_error, "JSON parse failed");
                    continue;
                }
            };
            if let Some(validator) = &validator {
                let errors: Vec<String> = validator
                    .iter_errors(&value)
                    .map(|e| format!("{} at {}", e, e.instance_path()))
                    .collect();
                if !errors.is_empty() {
                    last_error = errors.join("; ");
                    tracing::warn!(model = %model.model, attempt, error = %last_error, "schema validation failed");
                    continue;
                }
            }
            return Ok(value);
        }
        bail!(
            "{} failed to produce valid JSON after {} repairs: {last_error}",
            model.model,
            max_repairs
        )
    }
}

/// Pulls a JSON object out of an LLM reply: tolerates code fences and
/// surrounding prose by scanning for the outermost braces.
fn extract_json(raw: &str) -> Result<Value> {
    let trimmed = raw.trim();
    if let Ok(v) = serde_json::from_str::<Value>(trimmed) {
        return Ok(v);
    }
    let start = trimmed.find('{');
    let end = trimmed.rfind('}');
    if let (Some(start), Some(end)) = (start, end) {
        if end > start {
            let candidate = &trimmed[start..=end];
            return serde_json::from_str(candidate)
                .with_context(|| format!("no valid JSON object in reply: {}", truncate(raw, 200)));
        }
    }
    bail!("reply contains no JSON object: {}", truncate(raw, 200))
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        let cut: String = s.chars().take(max).collect();
        format!("{cut}...")
    }
}
