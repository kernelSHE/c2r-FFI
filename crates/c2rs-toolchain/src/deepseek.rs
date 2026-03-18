//! DeepSeek API integration (OpenAI-compatible chat completions).
//!
//! Used as an alternative to Ollama for the Agent fix loop when `--fix-provider deepseek`.

use crate::ollama::{OllamaError, OllamaProvider};
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tracing::debug;

/// Default base URL for DeepSeek API (OpenAI-compatible).
pub const DEFAULT_DEEPSEEK_BASE_URL: &str = "https://api.deepseek.com";

/// Default model for code fixes.
pub const DEFAULT_DEEPSEEK_MODEL: &str = "deepseek-chat";

/// Configuration for DeepSeek API.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeepSeekConfig {
    /// Base URL (e.g. "https://api.deepseek.com").
    pub base_url: String,
    /// Model name (e.g. "deepseek-chat").
    pub model: String,
    /// API key (from env DEEPSEEK_API_KEY; never log).
    pub api_key: String,
    /// Request timeout in seconds.
    #[serde(default = "default_timeout_secs")]
    pub timeout_secs: u64,
}

fn default_timeout_secs() -> u64 {
    120
}

impl DeepSeekConfig {
    /// Build config from base_url, model, and api_key (e.g. from env).
    pub fn new(base_url: String, model: String, api_key: String) -> Self {
        Self {
            base_url,
            model,
            api_key,
            timeout_secs: 120,
        }
    }
}

/// Response shape for OpenAI-compatible chat completions (subset we need).
#[derive(Debug, Deserialize)]
struct ChatCompletionResponse {
    choices: Option<Vec<ChatChoice>>,
}

#[derive(Debug, Deserialize)]
struct ChatChoice {
    message: Option<ChatMessage>,
}

#[derive(Debug, Deserialize)]
struct ChatMessage {
    content: Option<String>,
}

/// HTTP client for DeepSeek (OpenAI-compatible) POST /v1/chat/completions.
pub struct DeepSeekProvider {
    config: DeepSeekConfig,
}

impl DeepSeekProvider {
    pub fn new(config: DeepSeekConfig) -> Self {
        Self { config }
    }

    fn do_generate(&self, prompt: &str) -> Result<String, OllamaError> {
        let url = format!(
            "{}/v1/chat/completions",
            self.config.base_url.trim_end_matches('/')
        );
        let body = serde_json::json!({
            "model": self.config.model,
            "messages": [{ "role": "user", "content": prompt }],
            "stream": false
        });
        debug!(
            "DeepSeek generate: {} model={} len(prompt)={}",
            url,
            self.config.model,
            prompt.len()
        );
        let timeout = Duration::from_secs(self.config.timeout_secs);
        let response = ureq::post(&url)
            .set("Content-Type", "application/json")
            .set("Authorization", &format!("Bearer {}", self.config.api_key))
            .timeout(timeout)
            .send_string(&body.to_string())
            .map_err(|e| OllamaError::Http(e.to_string()))?;
        let status = response.status();
        let body_str = response
            .into_string()
            .map_err(|e| OllamaError::Http(e.to_string()))?;
        if status != 200 {
            return Err(OllamaError::Http(format!(
                "status {} body: {}",
                status, body_str
            )));
        }
        let json: ChatCompletionResponse =
            serde_json::from_str(&body_str).map_err(|e| OllamaError::Json(e.to_string()))?;
        let out = json
            .choices
            .and_then(|c| c.into_iter().next())
            .and_then(|c| c.message)
            .and_then(|m| m.content)
            .filter(|s| !s.is_empty())
            .ok_or(OllamaError::EmptyResponse)?;
        Ok(out)
    }
}

impl OllamaProvider for DeepSeekProvider {
    fn generate(&self, prompt: &str) -> Result<String, OllamaError> {
        self.do_generate(prompt)
    }
}
