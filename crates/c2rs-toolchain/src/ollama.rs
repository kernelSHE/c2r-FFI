//! Ollama integration for LLM-based fix (configurable; HTTP API or mock for tests).

use serde::{Deserialize, Serialize};
use std::time::Duration;
use thiserror::Error;
use tracing::debug;

/// Default model for code fixes.
pub const DEFAULT_MODEL: &str = "qwen2.5-coder:32b";

/// Default base URL for Ollama API.
pub const DEFAULT_BASE_URL: &str = "http://localhost:11434";

/// Configuration for Ollama API (HTTP).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OllamaConfig {
    /// Base URL (e.g. "http://localhost:11434").
    pub base_url: String,
    /// Model name (e.g. "qwen2.5-coder:32b").
    pub model: String,
    /// Request timeout in seconds.
    #[serde(default = "default_timeout_secs")]
    pub timeout_secs: u64,
}

fn default_timeout_secs() -> u64 {
    120
}

impl Default for OllamaConfig {
    fn default() -> Self {
        Self {
            base_url: DEFAULT_BASE_URL.to_string(),
            model: DEFAULT_MODEL.to_string(),
            timeout_secs: 120,
        }
    }
}

/// Provider that returns generated text for a prompt (real Ollama or mock for tests).
pub trait OllamaProvider: Send + Sync {
    fn generate(&self, prompt: &str) -> Result<String, OllamaError>;
}

#[derive(Debug, Error)]
pub enum OllamaError {
    #[error("HTTP/network: {0}")]
    Http(String),
    #[error("JSON: {0}")]
    Json(String),
    #[error("empty response")]
    EmptyResponse,
}

/// HTTP client for Ollama /api/generate (non-streaming).
pub struct HttpOllamaProvider {
    config: OllamaConfig,
}

impl HttpOllamaProvider {
    pub fn new(config: OllamaConfig) -> Self {
        Self { config }
    }

    fn do_generate(&self, prompt: &str) -> Result<String, OllamaError> {
        let url = format!(
            "{}/api/generate",
            self.config.base_url.trim_end_matches('/')
        );
        let body = serde_json::json!({
            "model": self.config.model,
            "prompt": prompt,
            "stream": false
        });
        debug!(
            "Ollama generate: {} model={} len(prompt)={}",
            url,
            self.config.model,
            prompt.len()
        );
        let timeout = Duration::from_secs(self.config.timeout_secs);
        let response = ureq::post(&url)
            .set("Content-Type", "application/json")
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
        let json: serde_json::Value =
            serde_json::from_str(&body_str).map_err(|e| OllamaError::Json(e.to_string()))?;
        let out = json
            .get("response")
            .and_then(|v| v.as_str())
            .map(String::from)
            .filter(|s| !s.is_empty())
            .ok_or(OllamaError::EmptyResponse)?;
        Ok(out)
    }
}

impl OllamaProvider for HttpOllamaProvider {
    fn generate(&self, prompt: &str) -> Result<String, OllamaError> {
        self.do_generate(prompt)
    }
}

/// Mock provider that returns a fixed string (for tests).
pub struct MockOllamaProvider {
    pub response: String,
}

impl MockOllamaProvider {
    pub fn new(response: String) -> Self {
        Self { response }
    }
}

impl OllamaProvider for MockOllamaProvider {
    fn generate(&self, _prompt: &str) -> Result<String, OllamaError> {
        Ok(self.response.clone())
    }
}

/// Convenience: generate using config (creates a temporary HTTP provider).
pub fn ollama_generate(prompt: &str, config: &OllamaConfig) -> Result<String, OllamaError> {
    let provider = HttpOllamaProvider::new(config.clone());
    provider.do_generate(prompt)
}
