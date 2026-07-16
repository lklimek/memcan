//! Multi-adapter LLM chat via the [`genai`] crate.
//!
//! [`GenaiLlmProvider`] selects an adapter from the model name at call time;
//! OpenRouter is one supported configuration.

use crate::error::{MemcanError, Result};
use crate::llm_telemetry;
use crate::traits::{LlmMessage, LlmOptions, LlmProvider, Role};
use async_trait::async_trait;
use genai::adapter::AdapterKind;
use genai::chat::{ChatMessage, ChatOptions, ChatRequest, ChatResponseFormat};
use genai::resolver::{AuthData, Endpoint};
use genai::{Client, ModelIden};

pub use crate::ollama::strip_ollama_prefix;

/// Sort a chat failure into the availability class or the content class.
///
/// Transport faults and statuses that a retry elsewhere could survive (5xx,
/// 408, 429) map to [`MemcanError::LlmUnavailable`]. Every other failure —
/// 4xx, malformed requests, missing credentials — is deterministic for this
/// prompt and maps to [`MemcanError::LlmChat`], because another backend would
/// reject it too.
fn classify_chat_error(error: &genai::Error, model: &str) -> MemcanError {
    use genai::webc::Error as WebcError;

    let unavailable = match error {
        genai::Error::WebAdapterCall { webc_error, .. }
        | genai::Error::WebModelCall { webc_error, .. } => match webc_error {
            WebcError::Reqwest(_) => true,
            WebcError::ResponseFailedStatus { status, .. } => {
                status_is_unavailable(status.as_u16())
            }
            _ => false,
        },
        genai::Error::HttpError { status, .. } => status_is_unavailable(status.as_u16()),
        genai::Error::WebStream { .. } => true,
        _ => false,
    };

    let context = format!("genai chat call to model '{model}' failed");
    let detail = error.to_string();
    if unavailable {
        MemcanError::LlmUnavailable { context, detail }
    } else {
        MemcanError::LlmChat { context, detail }
    }
}

/// `true` for statuses that indicate the backend — not the prompt — is at
/// fault, so another backend may still serve the request.
///
/// Takes a raw code rather than a `StatusCode`: genai links its own `reqwest`,
/// which need not be the one this crate depends on.
fn status_is_unavailable(status: u16) -> bool {
    (500..600).contains(&status) || status == 408 || status == 429
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
    ollama_host: Option<String>,
    ollama_api_key: Option<String>,
}

impl GenaiLlmProvider {
    /// Create with a pre-built [`Client`] and a default model name.
    pub fn new(client: Client, default_model: String) -> Self {
        Self {
            client,
            default_model,
            ollama_host: None,
            ollama_api_key: None,
        }
    }

    /// Build from application settings.
    ///
    /// Always installs a `ServiceTargetResolver` that:
    /// 1. Strips the `"ollama::"` prefix from model names (genai 0.5 compatibility)
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
                    // genai 0.5 keeps the "ollama::" prefix in model_name,
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
            ollama_host: settings.ollama_host.clone(),
            ollama_api_key: settings.ollama_api_key.clone(),
        }
    }

    /// Build an OpenRouter client; context-window discovery is unsupported.
    pub fn from_openrouter_settings(settings: &crate::config::Settings) -> Self {
        let endpoint = Endpoint::from_owned(format!(
            "{}/",
            settings.openrouter_base_url.trim_end_matches('/')
        ));
        let auth_key = settings.openrouter_api_key.clone().unwrap_or_default();
        let resolver_key = auth_key.clone();

        let client = Client::builder()
            // genai 0.5 resolves auth before applying the target override.
            .with_auth_resolver_fn(move |_| Ok(Some(AuthData::Key(auth_key.clone()))))
            .with_service_target_resolver_fn(move |mut st: genai::ServiceTarget| {
                st.endpoint = endpoint.clone();
                st.model = ModelIden::new(AdapterKind::OpenAI, st.model.model_name.clone());
                st.auth = AuthData::Key(resolver_key.clone());
                Ok(st)
            })
            .build();

        Self {
            client,
            default_model: settings.openrouter_model.clone(),
            ollama_host: None,
            ollama_api_key: None,
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

        // Disable thinking for Ollama models when explicitly requested
        let disable_think = opts.think == Some(false) && model.to_lowercase().contains("ollama");

        for msg in messages {
            match msg.role {
                Role::System => {
                    let content = if disable_think {
                        format!("/no_think\n{}", msg.content)
                    } else {
                        msg.content.clone()
                    };
                    req = req.with_system(&content);
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
        if opts.think == Some(false) {
            chat_opts = chat_opts.with_normalize_reasoning_content(true);
        }

        let response = self
            .client
            .exec_chat(model, req, Some(&chat_opts))
            .await
            .map_err(|e| classify_chat_error(&e, model))?;

        // Emit token telemetry before consuming the response.
        // genai Usage fields are Option<i32>; cast to u64 (negative = invalid, treat as None).
        let prompt_tokens = response
            .usage
            .prompt_tokens
            .and_then(|n| u64::try_from(n).ok());
        let completion_tokens = response
            .usage
            .completion_tokens
            .and_then(|n| u64::try_from(n).ok());
        // Mirror the ServiceTargetResolver's "ollama::" strip so the logged
        // model name matches what was actually sent to the provider.
        llm_telemetry::emit(
            opts.op,
            strip_ollama_prefix(model),
            prompt_tokens,
            completion_tokens,
        );

        response
            .into_first_text()
            .ok_or_else(|| MemcanError::LlmChat {
                context: "empty response from LLM".into(),
                detail: format!("model '{model}' returned no text content"),
            })
    }

    async fn context_window(&self, model: &str) -> Option<usize> {
        if !model.to_lowercase().contains("ollama") {
            return None;
        }

        let host = self
            .ollama_host
            .as_deref()
            .unwrap_or("http://localhost:11434");
        let url = format!("{}/api/show", host.trim_end_matches('/'));

        let model_name = strip_ollama_prefix(model);

        let client = reqwest::Client::new();
        let mut req = client
            .post(&url)
            .json(&serde_json::json!({"name": model_name}));

        if let Some(ref key) = self.ollama_api_key {
            req = req.header("Authorization", format!("Bearer {}", key));
        }

        let resp = req.send().await.ok()?;
        let body: serde_json::Value = resp.json().await.ok()?;

        let model_info = body.get("model_info")?.as_object()?;
        for (key, value) in model_info {
            if key.ends_with(".context_length") {
                return value.as_u64().map(|v| v as usize);
            }
        }

        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn tc13_openrouter_uses_openai_endpoint_auth_and_usage_response() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/chat/completions")
            .match_header("authorization", "Bearer test-openrouter-key")
            .match_body(mockito::Matcher::PartialJson(serde_json::json!({
                "model": "openai/gpt-4o-mini"
            })))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::json!({
                    "id": "chatcmpl-test",
                    "object": "chat.completion",
                    "created": 1,
                    "model": "openai/gpt-4o-mini",
                    "choices": [{
                        "index": 0,
                        "message": {"role": "assistant", "content": "from openrouter"},
                        "finish_reason": "stop"
                    }],
                    "usage": {
                        "prompt_tokens": 7,
                        "completion_tokens": 3,
                        "total_tokens": 10
                    }
                })
                .to_string(),
            )
            .create_async()
            .await;
        let settings = crate::config::Settings {
            openrouter_api_key: Some("test-openrouter-key".into()),
            openrouter_model: "openai/gpt-4o-mini".into(),
            openrouter_base_url: server.url(),
            ..crate::config::Settings::default()
        };
        let provider = GenaiLlmProvider::from_openrouter_settings(&settings);

        let result = provider
            .chat(
                provider.default_model(),
                &[LlmMessage {
                    role: Role::User,
                    content: "hello".into(),
                }],
                Some(LlmOptions {
                    op: "tc13_openrouter",
                    ..LlmOptions::default()
                }),
            )
            .await
            .unwrap();

        assert_eq!(result, "from openrouter");
        mock.assert_async().await;
    }

    #[test]
    fn test_default_model() {
        let provider = GenaiLlmProvider::new(Client::default(), "test-model".into());
        assert_eq!(provider.default_model(), "test-model");
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
