//! Ollama client -- implements [`EmbeddingProvider`] and [`LlmProvider`] via the
//! Ollama REST API using raw `reqwest` calls for full swappability.

use anyhow::{Context, bail};
use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

use crate::config::Settings;
use crate::traits::{EmbeddingProvider, LlmMessage, LlmOptions, LlmProvider};

/// Ollama HTTP client that implements both embedding and LLM traits.
#[derive(Debug, Clone)]
pub struct OllamaClient {
    http: Client,
    base_url: String,
    api_key: Option<String>,
    embed_model: String,
    llm_model: String,
    embed_dims: usize,
}

impl OllamaClient {
    /// Create a new client with explicit parameters.
    pub fn new(
        base_url: &str,
        api_key: Option<String>,
        embed_model: &str,
        embed_dims: usize,
    ) -> Self {
        let mut builder = Client::builder();
        if let Some(ref key) = api_key
            && !key.is_empty()
        {
            let mut headers = reqwest::header::HeaderMap::new();
            if let Ok(val) = reqwest::header::HeaderValue::from_str(&format!("Bearer {}", key)) {
                headers.insert("Authorization", val);
            }
            builder = builder.default_headers(headers);
        }

        Self {
            http: builder.build().expect("failed to build HTTP client"),
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key,
            embed_model: embed_model.to_string(),
            llm_model: String::new(),
            embed_dims,
        }
    }

    /// Create a new Ollama client from application settings.
    pub fn from_settings(settings: &Settings) -> Self {
        let api_key = if settings.ollama_api_key.is_empty() {
            None
        } else {
            Some(settings.ollama_api_key.clone())
        };

        let mut builder = Client::builder();
        if let Some(ref key) = api_key {
            let mut headers = reqwest::header::HeaderMap::new();
            if let Ok(val) = reqwest::header::HeaderValue::from_str(&format!("Bearer {}", key)) {
                headers.insert("Authorization", val);
            }
            builder = builder.default_headers(headers);
        }

        Self {
            http: builder.build().expect("failed to build HTTP client"),
            base_url: settings.ollama_url.trim_end_matches('/').to_string(),
            api_key,
            embed_model: settings.embed_model.clone(),
            llm_model: settings.llm_model.clone(),
            embed_dims: settings.embed_dims,
        }
    }

    /// Return the configured LLM model name.
    pub fn llm_model(&self) -> &str {
        &self.llm_model
    }

    /// Return the configured API key, if any.
    pub fn api_key(&self) -> Option<&str> {
        self.api_key.as_deref()
    }

    /// Check whether a model is available locally on the Ollama server.
    pub async fn model_available(&self, model: &str) -> anyhow::Result<bool> {
        let url = format!("{}/api/show", self.base_url);
        let resp = self
            .http
            .post(&url)
            .json(&serde_json::json!({ "name": model }))
            .send()
            .await?;
        Ok(resp.status().is_success())
    }

    /// Pull a model from the Ollama registry. Blocks until the pull completes.
    pub async fn pull_model(&self, model: &str) -> anyhow::Result<()> {
        info!(model, "Pulling Ollama model");
        let url = format!("{}/api/pull", self.base_url);
        let resp = self
            .http
            .post(&url)
            .json(&serde_json::json!({ "name": model, "stream": false }))
            .send()
            .await
            .context("pull request failed")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            bail!("failed to pull model {model}: {status} {body}");
        }
        info!(model, "Model pull complete");
        Ok(())
    }

    /// Ensure a model is available, pulling it if necessary.
    pub async fn ensure_model(&self, model: &str) -> anyhow::Result<()> {
        match self.model_available(model).await {
            Ok(true) => {
                debug!(model, "Model already available");
                Ok(())
            }
            Ok(false) => {
                warn!(model, "Model not found locally, pulling");
                self.pull_model(model).await
            }
            Err(e) => {
                warn!(model, error = %e, "Could not check model availability, attempting pull");
                self.pull_model(model).await
            }
        }
    }

    /// Ensure both LLM and embedding models are available.
    pub async fn ensure_models(&self) -> anyhow::Result<()> {
        self.ensure_model(&self.llm_model).await?;
        self.ensure_model(&self.embed_model).await?;
        Ok(())
    }
}

// -- Embedding ----------------------------------------------------------------

#[derive(Serialize)]
struct EmbedRequest {
    model: String,
    input: Vec<String>,
}

#[derive(Deserialize)]
struct EmbedResponse {
    embeddings: Vec<Vec<f32>>,
}

#[async_trait]
impl EmbeddingProvider for OllamaClient {
    async fn embed(&self, texts: &[String]) -> anyhow::Result<Vec<Vec<f32>>> {
        let url = format!("{}/api/embed", self.base_url);
        let body = EmbedRequest {
            model: self.embed_model.clone(),
            input: texts.to_vec(),
        };

        let resp = self
            .http
            .post(&url)
            .json(&body)
            .send()
            .await
            .context("embed request failed")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            bail!("Embed failed ({}): {}", status, text);
        }

        let parsed: EmbedResponse = resp.json().await.context("embed response parse failed")?;
        Ok(parsed.embeddings)
    }

    fn dimensions(&self) -> usize {
        self.embed_dims
    }
}

// -- LLM Chat -----------------------------------------------------------------

#[derive(Serialize)]
struct ChatRequest {
    model: String,
    messages: Vec<ChatMessage>,
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    format: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    options: Option<ChatOptions>,
    #[serde(skip_serializing_if = "Option::is_none")]
    think: Option<bool>,
}

#[derive(Serialize)]
struct ChatMessage {
    role: String,
    content: String,
}

#[derive(Serialize)]
struct ChatOptions {
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    num_predict: Option<u32>,
}

#[derive(Deserialize)]
struct ChatResponse {
    message: ChatResponseMessage,
}

#[derive(Deserialize)]
struct ChatResponseMessage {
    content: String,
}

#[async_trait]
impl LlmProvider for OllamaClient {
    async fn chat(
        &self,
        model: &str,
        messages: &[LlmMessage],
        options: Option<LlmOptions>,
    ) -> anyhow::Result<String> {
        let url = format!("{}/api/chat", self.base_url);
        let opts = options.unwrap_or_default();

        let chat_options = if opts.temperature.is_some() || opts.max_tokens.is_some() {
            Some(ChatOptions {
                temperature: opts.temperature,
                num_predict: opts.max_tokens,
            })
        } else {
            None
        };

        let body = ChatRequest {
            model: model.to_string(),
            messages: messages
                .iter()
                .map(|m| ChatMessage {
                    role: m.role.clone(),
                    content: m.content.clone(),
                })
                .collect(),
            stream: false,
            format: if opts.format_json {
                Some("json".to_string())
            } else {
                None
            },
            options: chat_options,
            think: opts.think,
        };

        let resp = self
            .http
            .post(&url)
            .json(&body)
            .send()
            .await
            .context("chat request failed")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            bail!("Chat failed ({}): {}", status, text);
        }

        let parsed: ChatResponse = resp.json().await.context("chat response parse failed")?;
        Ok(parsed.message.content)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_client_creation() {
        let client = OllamaClient::new("http://localhost:11434", None, "test-model", 768);
        assert_eq!(client.base_url, "http://localhost:11434");
        assert_eq!(client.embed_model, "test-model");
        assert_eq!(client.embed_dims, 768);
        assert!(client.api_key.is_none());
    }

    #[test]
    fn test_client_trailing_slash() {
        let client = OllamaClient::new("http://localhost:11434/", None, "m", 512);
        assert_eq!(client.base_url, "http://localhost:11434");
    }

    #[test]
    fn test_client_with_api_key() {
        let client = OllamaClient::new(
            "http://localhost:11434",
            Some("secret-key".into()),
            "m",
            512,
        );
        assert_eq!(client.api_key.as_deref(), Some("secret-key"));
    }

    #[test]
    fn test_dimensions() {
        let client = OllamaClient::new("http://localhost:11434", None, "m", 2560);
        assert_eq!(client.dimensions(), 2560);
    }

    #[test]
    fn test_from_settings() {
        let settings = Settings {
            ollama_url: "http://example.com:11434/".into(),
            ollama_api_key: "test-key".into(),
            embed_model: "embed-v1".into(),
            llm_model: "llm-v1".into(),
            embed_dims: 1024,
            ..Settings::default()
        };
        let client = OllamaClient::from_settings(&settings);
        assert_eq!(client.base_url, "http://example.com:11434");
        assert_eq!(client.embed_model, "embed-v1");
        assert_eq!(client.llm_model, "llm-v1");
        assert_eq!(client.embed_dims, 1024);
        assert_eq!(client.llm_model(), "llm-v1");
    }
}
