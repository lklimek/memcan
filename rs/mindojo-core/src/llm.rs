//! Multi-provider LLM chat via the [`genai`] crate.
//!
//! Replaces the old Ollama-only HTTP client with a provider-agnostic interface
//! that natively supports Ollama, OpenAI, Anthropic, Gemini, and others.

use crate::error::{MindojoError, Result};
use crate::traits::{LlmMessage, LlmOptions, LlmProvider, Role};
use async_trait::async_trait;
use genai::adapter::AdapterKind;
use genai::chat::{ChatMessage, ChatOptions, ChatRequest, ChatResponseFormat};
use genai::resolver::{AuthData, Endpoint};
use genai::{Client, ModelIden};

/// Strip the `"ollama::"` provider prefix from a model name if present.
///
/// genai v0.3.5 does not strip the prefix from `ModelIden.model_name`, so
/// `"ollama::qwen3.5:9b"` is sent as-is in the HTTP body. Ollama rejects any
/// model name containing `"::"`.
pub fn strip_ollama_prefix(name: &str) -> &str {
    name.strip_prefix("ollama::").unwrap_or(name)
}

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
    /// Always installs a `ServiceTargetResolver` that:
    /// 1. Strips the `"ollama::"` prefix from model names (genai v0.3.5 bug workaround)
    /// 2. Applies `OLLAMA_HOST` endpoint override when configured
    /// 3. Applies `OLLAMA_API_KEY` bearer auth when configured
    ///
    /// The genai crate reads neither `OLLAMA_HOST` nor `OLLAMA_API_KEY` from
    /// the environment on its own.
    pub fn from_settings(settings: &crate::config::Settings) -> Self {
        let ollama_host = settings.ollama_host.clone();
        let ollama_api_key = settings.ollama_api_key.clone();

        let endpoint = ollama_host.map(|host| {
            let mut base = host.trim_end_matches('/').to_string();
            if !base.ends_with("/v1/") {
                base.push_str("/v1/");
            }
            Endpoint::from_owned(base)
        });

        let client = Client::builder()
            .with_service_target_resolver_fn(move |mut st: genai::ServiceTarget| {
                if st.model.adapter_kind == AdapterKind::Ollama {
                    // genai v0.3.5 keeps the "ollama::" prefix in model_name,
                    // which Ollama rejects with "model is required".
                    let raw_name: &str = &st.model.model_name;
                    let stripped = strip_ollama_prefix(raw_name);
                    if stripped != raw_name {
                        st.model = ModelIden::new(AdapterKind::Ollama, stripped);
                    }

                    if let Some(ref ep) = endpoint {
                        st.endpoint = ep.clone();
                    }
                    if let Some(ref key) = ollama_api_key {
                        st.auth = AuthData::Key(key.clone());
                    }
                }
                Ok(st)
            })
            .build();

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

    #[test]
    fn test_default_model() {
        let provider = GenaiLlmProvider::new(Client::default(), "test-model".into());
        assert_eq!(provider.default_model(), "test-model");
    }

    #[test]
    fn test_strip_ollama_prefix_with_prefix() {
        assert_eq!(strip_ollama_prefix("ollama::qwen3.5:9b"), "qwen3.5:9b");
    }

    #[test]
    fn test_strip_ollama_prefix_without_prefix() {
        assert_eq!(strip_ollama_prefix("gpt-4o"), "gpt-4o");
    }

    #[test]
    fn test_strip_ollama_prefix_empty() {
        assert_eq!(strip_ollama_prefix(""), "");
    }

    #[test]
    fn test_strip_ollama_prefix_partial() {
        assert_eq!(strip_ollama_prefix("ollama:model"), "ollama:model");
    }

    #[test]
    fn test_from_settings_stores_model() {
        let settings = crate::config::Settings {
            llm_model: "ollama::qwen3.5:4b".into(),
            ..crate::config::Settings::default()
        };
        let provider = GenaiLlmProvider::from_settings(&settings);
        assert_eq!(provider.default_model(), "ollama::qwen3.5:4b");
    }
}
