//! Multi-provider LLM chat via the [`genai`] crate.
//!
//! Replaces the old Ollama-only HTTP client with a provider-agnostic interface
//! that natively supports Ollama, OpenAI, Anthropic, Gemini, and others.

use async_trait::async_trait;
use genai::chat::{ChatMessage, ChatOptions, ChatRequest, ChatResponseFormat};
use genai::Client;
use tracing::debug;

use crate::error::{MindojoError, Result};
use crate::traits::{LlmMessage, LlmOptions, LlmProvider};

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
    /// Uses `Settings::llm_model` as the default model. Provider-specific
    /// configuration (API keys, endpoints) is resolved by `genai` from
    /// environment variables automatically:
    ///
    /// * Ollama: `OLLAMA_HOST` (defaults to `http://localhost:11434`)
    /// * OpenAI: `OPENAI_API_KEY`
    /// * Anthropic: `ANTHROPIC_API_KEY`
    pub fn from_settings(settings: &crate::config::Settings) -> Self {
        let client = Client::default();
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
            match msg.role.as_str() {
                "system" => {
                    req = req.with_system(&msg.content);
                }
                "user" => {
                    req = req.append_message(ChatMessage::user(&msg.content));
                }
                "assistant" => {
                    req = req.append_message(ChatMessage::assistant(&msg.content));
                }
                other => {
                    debug!(role = other, "Unknown message role, treating as user");
                    req = req.append_message(ChatMessage::user(&msg.content));
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
}
