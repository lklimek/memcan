//! Multi-provider LLM chat via the [`genai`] crate.
//!
//! Replaces the old Ollama-only HTTP client with a provider-agnostic interface
//! that natively supports Ollama, OpenAI, Anthropic, Gemini, and others.

use crate::error::{MindojoError, Result};
use crate::traits::{LlmMessage, LlmOptions, LlmProvider, Role};
use async_trait::async_trait;
use genai::adapter::AdapterKind;
use genai::chat::{ChatMessage, ChatOptions, ChatRequest, ChatResponseFormat};
use genai::resolver::{AuthData, Endpoint, ServiceTargetResolver};
use genai::{Client, ClientConfig};

/// LLM provider backed by [`genai::Client`].
///
/// The model name string (e.g. `"ollama::qwen3.5:4b"`, `"gpt-4o"`,
/// `"claude-sonnet-4-20250514"`) determines which adapter/provider is used at
/// call time.
#[derive(Debug, Clone)]
pub struct GenaiLlmProvider {
    client: Client,
    default_model: String,
}

impl GenaiLlmProvider {
    /// Create with a pre-built [`Client`] and a default model name.
    pub fn new(client: Client, default_model: String) -> Self {
        Self {
            client,
            default_model,
        }
    }

    /// Build from application settings.
    ///
    /// Uses `Settings::llm_model` as the default model. When
    /// `ollama_host` or `ollama_api_key` is set, configures a
    /// `ServiceTargetResolver` that overrides the endpoint and/or auth
    /// for Ollama requests (the genai crate reads neither `OLLAMA_HOST`
    /// nor `OLLAMA_API_KEY` from the environment on its own).
    pub fn from_settings(settings: &crate::config::Settings) -> Self {
        let ollama_host = settings.ollama_host.clone();
        let ollama_api_key = settings.ollama_api_key.clone();

        let client = if ollama_host.is_some() || ollama_api_key.is_some() {
            let endpoint = ollama_host.map(|host| {
                let mut base = host.trim_end_matches('/').to_string();
                if !base.ends_with("/v1/") {
                    base.push_str("/v1/");
                }
                Endpoint::from_owned(base)
            });
            let resolver =
                ServiceTargetResolver::from_resolver_fn(move |mut st: genai::ServiceTarget| {
                    if st.model.adapter_kind == AdapterKind::Ollama {
                        if let Some(ref ep) = endpoint {
                            st.endpoint = ep.clone();
                        }
                        if let Some(ref key) = ollama_api_key {
                            st.auth = AuthData::Key(key.clone());
                        }
                    }
                    Ok(st)
                });
            let config = ClientConfig::default().with_service_target_resolver(resolver);
            Client::builder().with_config(config).build()
        } else {
            Client::default()
        };
        Self {
            client,
            default_model: settings.llm_model.clone(),
        }
    }

    /// Return the default model name.
    pub fn default_model(&self) -> &str {
        &self.default_model
    }
}

#[async_trait]
impl LlmProvider for GenaiLlmProvider {
    async fn chat(
        &self,
        model: &str,
        messages: &[LlmMessage],
        options: Option<LlmOptions>,
    ) -> Result<String> {
        let opts = options.unwrap_or_default();

        // Build the ChatRequest from our generic messages.
        let mut req = ChatRequest::default();

        for msg in messages {
            match msg.role {
                Role::System => {
                    req = req.with_system(&msg.content);
                }
                Role::User => {
                    req = req.append_message(ChatMessage::user(&msg.content));
                }
                Role::Assistant => {
                    req = req.append_message(ChatMessage::assistant(&msg.content));
                }
            }
        }

        // Build ChatOptions
        let mut chat_opts = ChatOptions::default();
        if let Some(temp) = opts.temperature {
            chat_opts = chat_opts.with_temperature(temp as f64);
        }
        if let Some(max) = opts.max_tokens {
            chat_opts = chat_opts.with_max_tokens(max);
        }
        if opts.format_json {
            chat_opts = chat_opts.with_response_format(ChatResponseFormat::JsonMode);
        }

        let response = self
            .client
            .exec_chat(model, req, Some(&chat_opts))
            .await
            .map_err(|e| MindojoError::LlmChat {
                context: format!("genai chat call to model '{model}' failed"),
                detail: e.to_string(),
            })?;

        response
            .content_text_into_string()
            .ok_or_else(|| MindojoError::LlmChat {
                context: "empty response from LLM".into(),
                detail: format!("model '{model}' returned no text content"),
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Settings;

    #[test]
    fn test_default_model() {
        let provider = GenaiLlmProvider::new(Client::default(), "test-model".into());
        assert_eq!(provider.default_model(), "test-model");
    }

    #[test]
    fn test_from_settings_without_ollama_host() {
        let settings = Settings::default();
        let provider = GenaiLlmProvider::from_settings(&settings);
        assert_eq!(provider.default_model(), "ollama::qwen3.5:4b");
    }

    #[test]
    fn test_from_settings_with_ollama_host() {
        let settings = Settings {
            ollama_host: Some("http://10.29.188.1:11434".into()),
            ..Settings::default()
        };
        let provider = GenaiLlmProvider::from_settings(&settings);
        assert_eq!(provider.default_model(), "ollama::qwen3.5:4b");
    }

    #[test]
    fn test_from_settings_with_ollama_api_key() {
        let settings = Settings {
            ollama_host: Some("http://10.29.188.1:11434".into()),
            ollama_api_key: Some("test-token".into()),
            ..Settings::default()
        };
        let provider = GenaiLlmProvider::from_settings(&settings);
        assert_eq!(provider.default_model(), "ollama::qwen3.5:4b");
    }

    #[test]
    fn test_from_settings_with_api_key_only() {
        let settings = Settings {
            ollama_api_key: Some("test-token".into()),
            ..Settings::default()
        };
        let provider = GenaiLlmProvider::from_settings(&settings);
        assert_eq!(provider.default_model(), "ollama::qwen3.5:4b");
    }
}
