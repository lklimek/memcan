//! Ollama LLM provider via the [`ollama_rs`] crate.

use async_trait::async_trait;
use ollama_rs::Ollama;
use ollama_rs::generation::chat::ChatMessage;
use ollama_rs::generation::chat::request::ChatMessageRequest;
use ollama_rs::generation::parameters::{FormatType, ThinkType};
use ollama_rs::models::ModelOptions;

use crate::config::Settings;
use crate::error::{MemcanError, Result};
use crate::ollama::strip_ollama_prefix;
use crate::traits::{LlmMessage, LlmOptions, LlmProvider, Role};

/// LLM provider backed by [`ollama_rs::Ollama`].
pub struct OllamaRsLlmProvider {
    client: Ollama,
    default_model: String,
}

impl OllamaRsLlmProvider {
    /// Build from application settings.
    ///
    /// Parses `OLLAMA_HOST` into (scheme+host, port). Strips the `ollama::`
    /// prefix from the configured model name. When `OLLAMA_API_KEY` is set,
    /// injects a Bearer auth header via `Ollama::new_with_request_headers`.
    pub fn from_settings(settings: &Settings) -> Self {
        let raw_host = settings
            .ollama_host
            .as_deref()
            .unwrap_or("http://localhost:11434");
        let (host, port) = parse_host_port(raw_host);

        let default_model = strip_ollama_prefix(&settings.llm_model).to_string();

        tracing::trace!(
            host = %host,
            port = port,
            model = %default_model,
            auth = settings.ollama_api_key.is_some(),
            "OllamaRsLlmProvider: initializing"
        );

        let client = if let Some(ref api_key) = settings.ollama_api_key {
            match reqwest::header::HeaderValue::from_str(&format!("Bearer {api_key}")) {
                Ok(val) => {
                    let mut headers = reqwest::header::HeaderMap::new();
                    headers.insert(reqwest::header::AUTHORIZATION, val);
                    Ollama::new_with_request_headers(host, port, headers)
                }
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        "OLLAMA_API_KEY contains invalid characters, connecting without auth"
                    );
                    Ollama::new(host.clone(), port)
                }
            }
        } else {
            Ollama::new(host, port)
        };

        Self {
            client,
            default_model,
        }
    }

    /// Return the default model name (prefix-stripped).
    pub fn default_model(&self) -> &str {
        &self.default_model
    }

    /// Return the Ollama base URL (for diagnostics).
    pub fn url(&self) -> &str {
        self.client.url_str()
    }
}

/// Parse an Ollama host string into (base_url, port).
///
/// Validates that the scheme is `http` or `https`. Falls back to
/// `http://localhost:11434` with a warning on unparseable input.
pub(crate) fn parse_host_port(host: &str) -> (String, u16) {
    let host = host.trim_end_matches('/');

    if let Ok(url) = reqwest::Url::parse(host) {
        let scheme = url.scheme();
        if scheme != "http" && scheme != "https" {
            tracing::warn!(
                scheme = scheme,
                "OLLAMA_HOST has unsupported scheme, falling back to http://localhost:11434"
            );
            return ("http://localhost".to_string(), 11434);
        }
        let port = url.port().unwrap_or(11434);
        let base = format!("{}://{}", scheme, url.host_str().unwrap_or("localhost"));
        return (base, port);
    }

    tracing::warn!(
        host = host,
        "OLLAMA_HOST is not a valid URL, falling back to http://localhost:11434"
    );
    ("http://localhost".to_string(), 11434)
}

#[async_trait]
impl LlmProvider for OllamaRsLlmProvider {
    async fn chat(
        &self,
        model: &str,
        messages: &[LlmMessage],
        options: Option<LlmOptions>,
    ) -> Result<String> {
        let model_name = strip_ollama_prefix(model);
        let opts = options.unwrap_or_default();

        tracing::trace!(
            model = model_name,
            messages = messages.len(),
            format_json = opts.format_json,
            think = ?opts.think,
            temperature = ?opts.temperature,
            max_tokens = ?opts.max_tokens,
            "ollama-rs: sending chat request"
        );

        let chat_messages: Vec<ChatMessage> = messages
            .iter()
            .map(|m| match m.role {
                Role::System => ChatMessage::system(m.content.clone()),
                Role::User => ChatMessage::user(m.content.clone()),
                Role::Assistant => ChatMessage::assistant(m.content.clone()),
            })
            .collect();

        let mut request = ChatMessageRequest::new(model_name.to_string(), chat_messages);

        // Temperature and max_tokens via ModelOptions
        let mut model_opts = ModelOptions::default();
        let mut has_opts = false;
        if let Some(temp) = opts.temperature {
            model_opts = model_opts.temperature(temp);
            has_opts = true;
        }
        if let Some(max) = opts.max_tokens {
            model_opts = model_opts.num_predict(max.min(i32::MAX as u32) as i32);
            has_opts = true;
        }
        if has_opts {
            request = request.options(model_opts);
        }

        if opts.format_json {
            request = request.format(FormatType::Json);
        }

        match opts.think {
            Some(false) => {
                request = request.think(ThinkType::False);
            }
            Some(true) => {
                request = request.think(ThinkType::True);
            }
            None => {}
        }

        let response =
            self.client
                .send_chat_messages(request)
                .await
                .map_err(|e| MemcanError::LlmChat {
                    context: format!("ollama-rs chat call to model '{model_name}' failed"),
                    detail: e.to_string(),
                })?;

        let text = response.message.content;
        tracing::trace!(
            model = model_name,
            response_len = text.len(),
            "ollama-rs: chat response received"
        );
        if text.is_empty() {
            return Err(MemcanError::LlmChat {
                context: "empty response from LLM".into(),
                detail: format!("model '{model_name}' returned no text content"),
            });
        }

        Ok(text)
    }

    async fn init(&self) -> Result<()> {
        let model_name = &self.default_model;

        match self.client.show_model_info(model_name.to_string()).await {
            Ok(_) => {
                tracing::info!(model = %model_name, "LLM model available");
                return Ok(());
            }
            Err(_) => {
                tracing::info!(model = %model_name, "LLM model not found locally, pulling");
            }
        }

        self.client
            .pull_model(model_name.to_string(), false)
            .await
            .map_err(|e| {
                MemcanError::Other(format!("failed to pull Ollama model '{model_name}': {e}"))
            })?;

        tracing::info!(model = %model_name, "LLM model pulled successfully");
        Ok(())
    }

    async fn context_window(&self, model: &str) -> Option<usize> {
        let model_name = strip_ollama_prefix(model).to_string();
        tracing::trace!(model = %model_name, "ollama-rs: querying context window");

        let info = self.client.show_model_info(model_name.clone()).await.ok()?;

        for (key, value) in &info.model_info {
            if key.ends_with(".context_length") {
                let ctx = value.as_u64().map(|v| v as usize);
                tracing::trace!(model = %model_name, context_window = ?ctx, "ollama-rs: context window resolved");
                return ctx;
            }
        }

        tracing::trace!(model = %model_name, "ollama-rs: no context_length found in model_info");
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_model() {
        let provider = OllamaRsLlmProvider {
            client: Ollama::default(),
            default_model: "test-model".into(),
        };
        assert_eq!(provider.default_model(), "test-model");
    }

    #[test]
    fn test_from_settings_stores_model() {
        let settings = Settings {
            llm_model: "ollama::qwen3.5:9b".into(),
            ..Settings::default()
        };
        let provider = OllamaRsLlmProvider::from_settings(&settings);
        assert_eq!(provider.default_model(), "qwen3.5:9b");
    }

    #[test]
    fn test_from_settings_with_api_key() {
        let settings = Settings {
            llm_model: "ollama::qwen3.5:9b".into(),
            ollama_api_key: Some("test-key".into()),
            ..Settings::default()
        };
        let provider = OllamaRsLlmProvider::from_settings(&settings);
        assert_eq!(provider.default_model(), "qwen3.5:9b");
    }

    #[test]
    fn test_parse_host_port_default() {
        let (host, port) = parse_host_port("http://localhost:11434");
        assert_eq!(host, "http://localhost");
        assert_eq!(port, 11434);
    }

    #[test]
    fn test_parse_host_port_custom() {
        let (host, port) = parse_host_port("http://10.29.188.1:11434");
        assert_eq!(host, "http://10.29.188.1");
        assert_eq!(port, 11434);
    }

    #[test]
    fn test_parse_host_port_no_port() {
        let (host, port) = parse_host_port("http://myserver");
        assert_eq!(host, "http://myserver");
        assert_eq!(port, 11434);
    }
}
