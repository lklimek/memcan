//! Multi-adapter LLM chat via the [`genai`] crate.
//!
//! [`GenaiLlmProvider`] serves the cloud backends (Anthropic's native
//! Messages API and OpenAI-compatible APIs) and, without the `ollama-rs-llm`
//! feature, Ollama.

use crate::config::{LlmProviderKind, LlmSlot};
use crate::error::{MemcanError, Result};
use crate::llm_telemetry::{self, CacheTokens};
use crate::traits::{LlmMessage, LlmOptions, LlmProvider, Role};
use async_trait::async_trait;
use genai::adapter::AdapterKind;
use genai::chat::{
    CacheControl, ChatMessage, ChatOptions, ChatRequest, ChatResponseFormat, ReasoningEffort,
};
use genai::resolver::{AuthData, Endpoint};
use genai::{Client, ModelIden};

pub use crate::ollama::strip_ollama_prefix;

/// Sort a chat failure into the availability class or the content class.
///
/// Transport faults and statuses that a retry elsewhere could survive (5xx,
/// 408, 429), rejected credentials (401), and an exhausted balance (402) map to
/// [`MemcanError::LlmUnavailable`]: they are properties of the backend, so
/// another backend may still serve the request. Every other failure — the
/// remaining 4xx (including 403, which providers also use for guardrail and
/// moderation blocks), malformed requests — is deterministic for this prompt
/// and maps to [`MemcanError::LlmChat`], because another backend would reject
/// it too.
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
/// 401 and 402 count as unavailable: a rejected credential and an exhausted
/// balance are both properties of the backend, not of the prompt. The other
/// backend authenticates and bills separately (or not at all), so it can still
/// serve the request — and a backend whose key is revoked or whose credits ran
/// out is genuinely unusable, which is what the breaker exists to record.
///
/// 403 deliberately does **not**. Providers overload it: OpenRouter returns 403
/// for guardrail blocks and moderation flags as well as for permissions, and a
/// blocked prompt is deterministic for that prompt. Classing it as unavailable
/// would mark a serving backend Down and re-send the blocked content to a third
/// party — the two things [`FallbackLlmProvider::chat`](crate::llm_fallback)
/// exists to avoid. A genuine permissions 403 is the rarer case, and it
/// degrades to a plain error rather than a wrong failover.
///
/// Takes a raw code rather than a `StatusCode`: genai links its own `reqwest`,
/// which need not be the one this crate depends on.
fn status_is_unavailable(status: u16) -> bool {
    (500..600).contains(&status) || matches!(status, 401 | 402 | 408 | 429)
}

/// `max_tokens` sent to Anthropic when the caller sets none.
///
/// The Messages API requires the field. genai's own fallback is the model's
/// output ceiling (64k for current models), far beyond anything our prompts
/// produce.
const ANTHROPIC_DEFAULT_MAX_TOKENS: u32 = 4096;

/// Reasoning effort sent with every Anthropic request.
///
/// Any explicit effort stops genai inferring one from a model-id suffix (`-low`,
/// `-high`, …). `Zero` turns thinking off — genai sends `thinking: {"type":
/// "disabled"}` to models that think by default — but the Messages API rejects
/// `disabled` with a 400 on Opus 5.5, Sonnet 5.5, Fable 5.x and Mythos 5.x, so
/// those get the lowest effort with adaptive thinking instead.
// TODO(genai): drop the model list once genai stops sending `disabled` to models that reject it.
fn anthropic_reasoning_effort(model: &str) -> ReasoningEffort {
    const DISABLED_THINKING_REJECTED: [&str; 4] = [
        "claude-opus-5-5",
        "claude-sonnet-5-5",
        "claude-fable-5",
        "claude-mythos-5",
    ];
    let name = model.rsplit("::").next().unwrap_or(model);
    if DISABLED_THINKING_REJECTED
        .iter()
        .any(|prefix| name.starts_with(prefix))
    {
        ReasoningEffort::Low
    } else {
        ReasoningEffort::Zero
    }
}

/// Wire protocol spoken by a cloud backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CloudApi {
    /// Native Anthropic Messages API (`/messages`, `x-api-key`).
    Anthropic,
    /// OpenAI Chat Completions API (`/chat/completions`, bearer key).
    OpenAiCompatible,
}

/// Split a system prompt so its cache breakpoint survives genai.
///
/// genai's Anthropic adapter renders `ChatRequest::system` as the first system
/// block and never marks it with `cache_control`, so a lone prompt is silently
/// sent uncached. Splitting after the first line yields an unmarked
/// head and a marked tail; the breakpoint caches the whole prefix, and
/// `head + tail` is the original prompt byte for byte. `None` when no split
/// leaves both halves non-blank (Anthropic rejects empty text blocks) — such a
/// one-line prompt is far below the minimum cacheable length anyway.
// TODO: drop the split once genai honours a breakpoint on the first block.
fn split_for_cache_breakpoint(prompt: &str) -> Option<(&str, &str)> {
    prompt
        .match_indices('\n')
        .map(|(index, _)| prompt.split_at(index + 1))
        .find(|(head, tail)| !head.trim().is_empty() && !tail.trim().is_empty())
}

/// LLM provider backed by [`genai::Client`].
///
/// Clients from [`from_cloud_slot`](Self::from_cloud_slot) pin the
/// adapter and endpoint of the configured backend. Otherwise the model name
/// (e.g. `"ollama::qwen3.5:4b"`, `"gpt-4o"`) selects the adapter at call time.
#[derive(Clone)]
pub struct GenaiLlmProvider {
    client: Client,
    default_model: String,
    ollama_host: Option<String>,
    ollama_api_key: Option<String>,
    /// Protocol of the cloud backend; `None` for Ollama and model-name routing.
    ///
    /// Cloud backends never receive `LlmOptions::temperature`: current Claude
    /// models (natively and through Anthropic's OpenAI-compatible endpoint) and
    /// OpenAI reasoning models reject non-default temperatures with a 400.
    cloud_api: Option<CloudApi>,
}

impl std::fmt::Debug for GenaiLlmProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GenaiLlmProvider")
            .field("default_model", &self.default_model)
            .field(
                "ollama_host",
                &self.ollama_host.as_deref().map(crate::config::redact_url),
            )
            .field(
                "ollama_api_key",
                &self.ollama_api_key.as_ref().map(|_| "***"),
            )
            .field("cloud_api", &self.cloud_api)
            .finish_non_exhaustive()
    }
}

impl GenaiLlmProvider {
    /// Create with a pre-built [`Client`] and a default model name.
    pub fn new(client: Client, default_model: String) -> Self {
        Self {
            client,
            default_model,
            ollama_host: None,
            ollama_api_key: None,
            cloud_api: None,
        }
    }

    /// Build an Ollama client for `slot`.
    ///
    /// Applies the slot's base URL (`OLLAMA_HOST` by default) through a
    /// `ServiceTargetResolver`; [`chat`](LlmProvider::chat) sends the slot's
    /// API key (`OLLAMA_API_KEY` by default) as a bearer header, because
    /// genai's native Ollama adapter ignores resolved auth.
    ///
    /// The genai crate reads neither `OLLAMA_HOST` nor `OLLAMA_API_KEY` from
    /// the environment on its own.
    pub fn from_ollama_slot(slot: &LlmSlot) -> Result<Self> {
        // genai's Ollama adapter speaks the native API and appends `api/chat`.
        let endpoint = slot
            .base_url
            .as_deref()
            .map(|host| Endpoint::from_owned(format!("{}/", host.trim_end_matches('/'))));

        let client = Client::builder()
            .with_service_target_resolver_fn(move |mut st: genai::ServiceTarget| {
                if st.model.adapter_kind == AdapterKind::Ollama
                    && let Some(ref ep) = endpoint
                {
                    st.endpoint = ep.clone();
                }
                Ok(st)
            })
            .build()
            .map_err(|source| MemcanError::LlmClientBuild {
                role: slot.role,
                source: Box::new(source),
            })?;

        Ok(Self {
            client,
            default_model: slot.model.clone(),
            ollama_host: slot.base_url.clone(),
            ollama_api_key: slot.api_key.clone(),
            cloud_api: None,
        })
    }

    /// Build a client for the cloud slot `slot`; context-window discovery is
    /// unsupported.
    ///
    /// Anthropic uses genai's native Anthropic adapter; every other cloud kind
    /// uses its OpenAI adapter against the slot's base URL. Fails for an Ollama
    /// slot or a slot without a base URL or API key.
    pub fn from_cloud_slot(slot: &LlmSlot) -> Result<Self> {
        let kind = slot.provider;
        let (adapter, cloud_api) = match kind {
            LlmProviderKind::Anthropic => (AdapterKind::Anthropic, CloudApi::Anthropic),
            LlmProviderKind::OpenAi
            | LlmProviderKind::OpenRouter
            | LlmProviderKind::OpenAiCompatible => {
                (AdapterKind::OpenAI, CloudApi::OpenAiCompatible)
            }
            LlmProviderKind::Ollama => {
                return Err(MemcanError::Config(
                    "Ollama is not a cloud LLM backend".into(),
                ));
            }
        };
        let base_url = slot.base_url.as_deref().ok_or_else(|| {
            MemcanError::Config(format!(
                "the {} '{kind}' LLM slot has no base URL",
                slot.role
            ))
        })?;
        // genai appends the route (`messages`, `chat/completions`) verbatim.
        let endpoint = Endpoint::from_owned(format!("{}/", base_url.trim_end_matches('/')));
        // Without a key every call would 401, which the breaker counts as an
        // outage — quietly shifting all traffic to the fallback.
        let auth_key = slot
            .api_key
            .as_deref()
            .map(str::trim)
            .filter(|key| !key.is_empty())
            .map(str::to_string)
            .ok_or_else(|| {
                MemcanError::Config(format!(
                    "the {} '{kind}' LLM slot has no API key",
                    slot.role
                ))
            })?;
        let resolver_key = auth_key.clone();

        let client = Client::builder()
            // genai resolves auth before applying the target override.
            .with_auth_resolver_fn(move |_| Ok(Some(AuthData::Key(auth_key.clone()))))
            .with_service_target_resolver_fn(move |mut st: genai::ServiceTarget| {
                // Model ids such as `openai/gpt-4o-mini` or `claude-*` make genai
                // infer an adapter; the configured backend must win.
                st.endpoint = endpoint.clone();
                st.model = ModelIden::new(adapter, st.model.model_name.clone());
                st.auth = AuthData::Key(resolver_key.clone());
                Ok(st)
            })
            .build()
            .map_err(|source| MemcanError::LlmClientBuild {
                role: slot.role,
                source: Box::new(source),
            })?;

        Ok(Self {
            client,
            default_model: slot.model.clone(),
            ollama_host: None,
            ollama_api_key: None,
            cloud_api: Some(cloud_api),
        })
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

        let anthropic = self.cloud_api == Some(CloudApi::Anthropic);
        for msg in messages {
            match msg.role {
                Role::System => {
                    let content = if disable_think {
                        format!("/no_think\n{}", msg.content)
                    } else {
                        msg.content.clone()
                    };
                    req = match anthropic
                        .then(|| split_for_cache_breakpoint(&content))
                        .flatten()
                    {
                        // System prompts are static templates; the volatile
                        // input travels in the user message after the breakpoint.
                        Some((head, tail)) => req.with_system(head).append_message(
                            ChatMessage::system(tail).with_options(CacheControl::Ephemeral),
                        ),
                        None => req.with_system(&content),
                    };
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
        if let Some(temp) = opts.temperature.filter(|_| self.cloud_api.is_none()) {
            chat_opts = chat_opts.with_temperature(temp as f64);
        }
        let max_tokens = opts
            .max_tokens
            .or_else(|| anthropic.then_some(ANTHROPIC_DEFAULT_MAX_TOKENS));
        if let Some(max) = max_tokens {
            chat_opts = chat_opts.with_max_tokens(max);
        }
        // Anthropic has no `response_format`; replies are parsed leniently.
        if opts.format_json && !anthropic {
            chat_opts = chat_opts.with_response_format(ChatResponseFormat::JsonMode);
        }
        if opts.think == Some(false) {
            chat_opts = chat_opts.with_normalize_reasoning_content(true);
        }
        if let Some(key) = &self.ollama_api_key {
            chat_opts = chat_opts.with_extra_headers(("Authorization", format!("Bearer {key}")));
        }
        if anthropic {
            chat_opts = chat_opts.with_reasoning_effort(anthropic_reasoning_effort(model));
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
        let cache_details = response.usage.prompt_tokens_details.as_ref();
        let cache = CacheTokens {
            creation: cache_details
                .and_then(|details| details.cache_creation_tokens)
                .and_then(|n| u64::try_from(n).ok()),
            read: cache_details
                .and_then(|details| details.cached_tokens)
                .and_then(|n| u64::try_from(n).ok()),
        };
        // Mirror the ServiceTargetResolver's "ollama::" strip so the logged
        // model name matches what was actually sent to the provider.
        llm_telemetry::emit(
            opts.op,
            strip_ollama_prefix(model),
            prompt_tokens,
            completion_tokens,
            cache,
        );

        response
            .into_first_text()
            .ok_or_else(|| MemcanError::LlmChat {
                context: "empty response from LLM".into(),
                detail: format!("model '{model}' returned no text content"),
            })
    }

    /// Cloud slots cannot report a window, and every supported cloud model
    /// far exceeds the local default, so they assume the larger cloud window.
    fn assumed_context_window(&self) -> usize {
        if self.cloud_api.is_some() {
            crate::pipeline::CLOUD_DEFAULT_CONTEXT_WINDOW
        } else {
            crate::pipeline::DEFAULT_CONTEXT_WINDOW
        }
    }

    async fn context_window(&self, model: &str) -> Option<usize> {
        if !model.to_lowercase().contains("ollama") {
            return None;
        }

        let host = self
            .ollama_host
            .as_deref()
            .unwrap_or(crate::ollama::DEFAULT_OLLAMA_HOST);
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
    use crate::config::LlmRole;

    fn cloud_slot(kind: LlmProviderKind, model: &str, key: &str, base_url: String) -> LlmSlot {
        LlmSlot {
            role: LlmRole::Primary,
            provider: kind,
            model: model.into(),
            api_key: Some(key.into()),
            base_url: Some(base_url),
        }
    }

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
        let provider = GenaiLlmProvider::from_cloud_slot(&cloud_slot(
            LlmProviderKind::OpenRouter,
            "openai/gpt-4o-mini",
            "test-openrouter-key",
            server.url(),
        ))
        .unwrap();

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

    #[tokio::test]
    async fn ollama_slot_uses_native_chat_api_with_bearer_key() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/api/chat")
            .match_header("authorization", "Bearer test-ollama-key")
            .match_body(mockito::Matcher::PartialJson(serde_json::json!({
                "model": "qwen3.5:9b"
            })))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::json!({
                    "model": "qwen3.5:9b",
                    "created_at": "2026-01-01T00:00:00Z",
                    "message": {"role": "assistant", "content": "from ollama"},
                    "done": true,
                    "done_reason": "stop",
                    "prompt_eval_count": 7,
                    "eval_count": 3
                })
                .to_string(),
            )
            .create_async()
            .await;
        let provider = GenaiLlmProvider::from_ollama_slot(&cloud_slot(
            LlmProviderKind::Ollama,
            "qwen3.5:9b",
            "test-ollama-key",
            server.url(),
        ))
        .unwrap();

        let result = provider
            .chat(
                "ollama::qwen3.5:9b",
                &[LlmMessage {
                    role: Role::User,
                    content: "hello".into(),
                }],
                Some(LlmOptions {
                    op: "ollama_native",
                    ..LlmOptions::default()
                }),
            )
            .await
            .unwrap();

        assert_eq!(result, "from ollama");
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn openai_compat_backend_omits_temperature() {
        // Anthropic's OpenAI-compatible endpoint and OpenAI reasoning models
        // reject non-default temperatures with a 400, so the cloud backend
        // must never forward the 0.0 used by the indexing call sites.
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/chat/completions")
            .match_request(|req| {
                req.utf8_lossy_body()
                    .map(|body| !body.contains("temperature"))
                    .unwrap_or(false)
            })
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::json!({
                    "id": "chatcmpl-test",
                    "object": "chat.completion",
                    "created": 1,
                    "model": "claude-haiku-5-5",
                    "choices": [{
                        "index": 0,
                        "message": {"role": "assistant", "content": "ok"},
                        "finish_reason": "stop"
                    }]
                })
                .to_string(),
            )
            .create_async()
            .await;
        let provider = GenaiLlmProvider::from_cloud_slot(&cloud_slot(
            LlmProviderKind::OpenAiCompatible,
            "claude-haiku-5-5",
            "test-key",
            server.url(),
        ))
        .unwrap();

        let result = provider
            .chat(
                provider.default_model(),
                &[LlmMessage {
                    role: Role::User,
                    content: "hello".into(),
                }],
                Some(LlmOptions {
                    op: "omit_temperature",
                    temperature: Some(0.0),
                    ..LlmOptions::default()
                }),
            )
            .await
            .unwrap();

        assert_eq!(result, "ok");
        mock.assert_async().await;
    }

    /// Fixed Anthropic Messages API reply carrying prompt-cache usage.
    fn anthropic_reply(text: &str) -> String {
        serde_json::json!({
            "id": "msg_test",
            "type": "message",
            "role": "assistant",
            "model": "claude-haiku-5-5",
            "content": [{"type": "text", "text": text}],
            "stop_reason": "end_turn",
            "usage": {
                "input_tokens": 12,
                "output_tokens": 3,
                "cache_creation_input_tokens": 0,
                "cache_read_input_tokens": 1800
            }
        })
        .to_string()
    }

    fn anthropic_provider(base_url: String) -> GenaiLlmProvider {
        GenaiLlmProvider::from_cloud_slot(&cloud_slot(
            LlmProviderKind::Anthropic,
            "claude-haiku-5-5",
            "test-anthropic-key",
            base_url,
        ))
        .unwrap()
    }

    fn system_and_user(system: &str, user: &str) -> Vec<LlmMessage> {
        vec![
            LlmMessage {
                role: Role::System,
                content: system.into(),
            },
            LlmMessage {
                role: Role::User,
                content: user.into(),
            },
        ]
    }

    /// The request body as JSON (`Null` when it is not JSON).
    fn request_json(req: &mockito::Request) -> serde_json::Value {
        req.utf8_lossy_body()
            .ok()
            .and_then(|body| serde_json::from_str(&body).ok())
            .unwrap_or_default()
    }

    /// `true` when the raw request body contains none of `keys`.
    fn body_lacks(req: &mockito::Request, keys: &[&str]) -> bool {
        req.utf8_lossy_body()
            .map(|body| keys.iter().all(|key| !body.contains(key)))
            .unwrap_or(false)
    }

    #[tokio::test]
    async fn anthropic_request_is_native_cached_and_free_of_rejected_params() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/messages")
            .match_header("x-api-key", "test-anthropic-key")
            .match_header("authorization", mockito::Matcher::Missing)
            .match_body(mockito::Matcher::PartialJson(serde_json::json!({
                "model": "claude-haiku-5-5",
                "max_tokens": ANTHROPIC_DEFAULT_MAX_TOKENS,
                "messages": [{"role": "user", "content": "volatile input"}]
            })))
            .match_request(|req| {
                let body = request_json(req);
                let top_level_absent = ["temperature", "output_config", "response_format"]
                    .iter()
                    .all(|key| body.get(key).is_none());
                top_level_absent
                    && body["thinking"] == serde_json::json!({"type": "disabled"})
                    && body_lacks(req, &["budget_tokens"])
                    && body["system"]
                        == serde_json::json!([
                            {"type": "text", "text": "# Role\n"},
                            {
                                "type": "text",
                                "text": "Extract facts.\nReturn JSON.",
                                "cache_control": {"type": "ephemeral"}
                            }
                        ])
            })
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(anthropic_reply("{\"facts\": []}"))
            .create_async()
            .await;
        let provider = anthropic_provider(server.url());

        let result = provider
            .chat(
                provider.default_model(),
                &system_and_user("# Role\nExtract facts.\nReturn JSON.", "volatile input"),
                Some(LlmOptions {
                    format_json: true,
                    temperature: Some(0.0),
                    think: Some(false),
                    op: "anthropic_native",
                    ..LlmOptions::default()
                }),
            )
            .await
            .unwrap();

        assert_eq!(result, "{\"facts\": []}");
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn anthropic_cache_breakpoint_wire_shape_is_pinned_for_a_real_prompt() {
        // Pins genai's wire output for the split workaround: exactly two system
        // text blocks, the breakpoint on the last only, and their concatenation
        // equal to the prompt. A genai upgrade that changes how it renders
        // cached system blocks (fixing or moving the index-0 bug) fails here
        // instead of silently double-marking or dropping the breakpoint.
        let prompt = crate::prompts::FACT_EXTRACTION_PROMPT;
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/messages")
            .match_request(move |req| {
                let body = request_json(req);
                let Some(blocks) = body["system"].as_array() else {
                    return false;
                };
                let text_only = blocks.iter().all(|block| {
                    block["type"] == "text"
                        && block["text"].as_str().is_some_and(|t| !t.trim().is_empty())
                });
                let marks: Vec<bool> = blocks
                    .iter()
                    .map(|block| block.get("cache_control").is_some())
                    .collect();
                let joined: String = blocks
                    .iter()
                    .filter_map(|block| block["text"].as_str())
                    .collect();
                blocks.len() == 2
                    && text_only
                    && marks == [false, true]
                    && blocks[1]["cache_control"] == serde_json::json!({"type": "ephemeral"})
                    && joined == prompt
            })
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(anthropic_reply("ok"))
            .create_async()
            .await;
        let provider = anthropic_provider(server.url());

        provider
            .chat(
                provider.default_model(),
                &system_and_user(prompt, "volatile input"),
                Some(LlmOptions {
                    format_json: true,
                    think: Some(false),
                    ..LlmOptions::default()
                }),
            )
            .await
            .unwrap();

        mock.assert_async().await;
    }

    #[tokio::test]
    async fn anthropic_keeps_the_callers_max_tokens() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/messages")
            .match_body(mockito::Matcher::PartialJson(
                serde_json::json!({"max_tokens": 256}),
            ))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(anthropic_reply("ok"))
            .create_async()
            .await;
        let provider = anthropic_provider(server.url());

        provider
            .chat(
                provider.default_model(),
                &system_and_user("Describe.\nBriefly.", "fn main() {}"),
                Some(LlmOptions {
                    max_tokens: Some(256),
                    ..LlmOptions::default()
                }),
            )
            .await
            .unwrap();

        mock.assert_async().await;
    }

    #[tokio::test]
    async fn anthropic_model_suffix_never_enables_legacy_thinking() {
        // genai reads a `-high`/`-low` model-id suffix as a reasoning effort
        // and strips it from the id; thinking must stay off and the id must
        // reach the API unmodified.
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/messages")
            .match_body(mockito::Matcher::PartialJson(
                serde_json::json!({"model": "claude-haiku-5-5-high"}),
            ))
            .match_request(|req| {
                let body = request_json(req);
                body["thinking"] == serde_json::json!({"type": "disabled"})
                    && body.get("output_config").is_none()
                    && body_lacks(req, &["budget_tokens"])
            })
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(anthropic_reply("ok"))
            .create_async()
            .await;
        let provider = anthropic_provider(server.url());

        provider
            .chat(
                "claude-haiku-5-5-high",
                &system_and_user("Answer.", "hello"),
                None,
            )
            .await
            .unwrap();

        mock.assert_async().await;
    }

    #[tokio::test]
    async fn anthropic_models_rejecting_disabled_thinking_get_low_effort() {
        for model in ["claude-opus-5-5", "claude-sonnet-5-5", "claude-fable-5-1"] {
            let mut server = mockito::Server::new_async().await;
            let mock = server
                .mock("POST", "/messages")
                .match_body(mockito::Matcher::PartialJson(serde_json::json!({
                    "model": model,
                    "output_config": {"effort": "low"}
                })))
                .match_request(|req| {
                    request_json(req)["thinking"]["type"] != "disabled"
                        && body_lacks(req, &["budget_tokens"])
                })
                .with_status(200)
                .with_header("content-type", "application/json")
                .with_body(anthropic_reply("ok"))
                .create_async()
                .await;
            let provider = anthropic_provider(server.url());

            provider
                .chat(model, &system_and_user("Answer.", "hello"), None)
                .await
                .unwrap_or_else(|e| panic!("{model}: {e}"));

            mock.assert_async().await;
        }
    }

    #[test]
    fn anthropic_reasoning_effort_disables_thinking_where_the_api_allows_it() {
        for model in [
            "claude-haiku-5-5",
            "claude-haiku-4-5",
            "claude-opus-5",
            "claude-sonnet-5",
            "claude-opus-4-8",
        ] {
            assert!(
                matches!(anthropic_reasoning_effort(model), ReasoningEffort::Zero),
                "{model}"
            );
        }
        for model in [
            "claude-opus-5-5",
            "claude-sonnet-5-5",
            "claude-fable-5",
            "claude-fable-5-1",
            "claude-mythos-5-1",
            "anthropic::claude-opus-5-5",
        ] {
            assert!(
                matches!(anthropic_reasoning_effort(model), ReasoningEffort::Low),
                "{model}"
            );
        }
    }

    #[tokio::test]
    async fn anthropic_single_line_system_prompt_is_sent_uncached() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/messages")
            .match_body(mockito::Matcher::PartialJson(
                serde_json::json!({"system": "Answer."}),
            ))
            .match_request(|req| body_lacks(req, &["cache_control"]))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(anthropic_reply("ok"))
            .create_async()
            .await;
        let provider = anthropic_provider(server.url());

        provider
            .chat(
                provider.default_model(),
                &system_and_user("Answer.", "hello"),
                None,
            )
            .await
            .unwrap();

        mock.assert_async().await;
    }

    #[tokio::test]
    async fn openai_backend_keeps_json_mode_and_bearer_auth() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/chat/completions")
            .match_header("authorization", "Bearer test-openai-key")
            .match_body(mockito::Matcher::PartialJson(serde_json::json!({
                "model": "gpt-4o-mini",
                "response_format": {"type": "json_object"}
            })))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::json!({
                    "id": "chatcmpl-test",
                    "object": "chat.completion",
                    "created": 1,
                    "model": "gpt-4o-mini",
                    "choices": [{
                        "index": 0,
                        "message": {"role": "assistant", "content": "{}"},
                        "finish_reason": "stop"
                    }]
                })
                .to_string(),
            )
            .create_async()
            .await;
        let provider = GenaiLlmProvider::from_cloud_slot(&cloud_slot(
            LlmProviderKind::OpenAi,
            "gpt-4o-mini",
            "test-openai-key",
            format!("{}/", server.url()),
        ))
        .unwrap();

        provider
            .chat(
                provider.default_model(),
                &system_and_user("Return JSON.\nOnly JSON.", "hello"),
                Some(LlmOptions {
                    format_json: true,
                    ..LlmOptions::default()
                }),
            )
            .await
            .unwrap();

        mock.assert_async().await;
    }

    #[test]
    fn cloud_construction_rejects_ollama_and_a_missing_base_url() {
        let ollama = LlmSlot {
            provider: LlmProviderKind::Ollama,
            ..cloud_slot(LlmProviderKind::OpenAi, "m", "k", String::new())
        };
        assert!(GenaiLlmProvider::from_cloud_slot(&ollama).is_err());

        let no_url = LlmSlot {
            base_url: None,
            ..cloud_slot(LlmProviderKind::OpenAiCompatible, "m", "k", String::new())
        };
        let error = GenaiLlmProvider::from_cloud_slot(&no_url)
            .err()
            .unwrap()
            .to_string();
        assert!(error.contains("base URL"), "{error}");

        for api_key in [None, Some(String::new()), Some("  ".to_string())] {
            let no_key = LlmSlot {
                api_key,
                ..cloud_slot(
                    LlmProviderKind::Anthropic,
                    "m",
                    "k",
                    "https://a.example/".into(),
                )
            };
            let error = GenaiLlmProvider::from_cloud_slot(&no_key)
                .err()
                .unwrap()
                .to_string();
            assert!(error.contains("API key"), "{error}");
        }
    }

    #[test]
    fn cache_split_preserves_the_prompt_and_keeps_both_halves_non_blank() {
        for prompt in [
            "# Role\nExtract facts.\nReturn JSON.",
            "\n\n# Role\nbody",
            "head\n\n\ntail\n",
        ] {
            let (head, tail) = split_for_cache_breakpoint(prompt).unwrap();
            assert_eq!(format!("{head}{tail}"), prompt);
            assert!(!head.trim().is_empty(), "{prompt:?}");
            assert!(!tail.trim().is_empty(), "{prompt:?}");
        }
        assert_eq!(
            split_for_cache_breakpoint("# Role\nbody"),
            Some(("# Role\n", "body"))
        );

        for unsplittable in ["", "one line", "one line\n", "\n\none line", "  \n  "] {
            assert_eq!(
                split_for_cache_breakpoint(unsplittable),
                None,
                "{unsplittable:?}"
            );
        }
    }

    #[test]
    fn test_default_model() {
        let provider = GenaiLlmProvider::new(Client::new().unwrap(), "test-model".into());
        assert_eq!(provider.default_model(), "test-model");
    }

    #[test]
    fn rejected_credentials_are_an_availability_fault() {
        // A rotated cloud API key must not strand requests that a healthy
        // local Ollama could serve: 401 describes the backend, not the prompt,
        // so it has to reach the fallback and the breaker.
        assert!(super::status_is_unavailable(401));
    }

    #[test]
    fn an_exhausted_balance_is_an_availability_fault() {
        // OpenRouter returns 402 when the account is out of credits. The
        // fallback bills separately, so it can still serve the request — and a
        // backend that cannot be paid for is as unusable as one with a revoked
        // key.
        assert!(super::status_is_unavailable(402));
    }

    #[test]
    fn forbidden_stays_a_content_fault() {
        // Providers overload 403: OpenRouter returns it for guardrail and
        // moderation blocks, which are deterministic for the prompt. Classing
        // it as unavailable would mark a serving backend Down and ship the
        // blocked content to a third party.
        assert!(!super::status_is_unavailable(403));
    }

    #[test]
    fn backend_faults_are_availability_faults() {
        for status in [500, 502, 503, 599, 408, 429] {
            assert!(
                super::status_is_unavailable(status),
                "{status} should be an availability fault"
            );
        }
    }

    #[test]
    fn request_faults_stay_content_faults() {
        // These are deterministic for the prompt: another backend rejects them
        // too, so they must not egress or trip the breaker.
        for status in [400, 404, 413, 422] {
            assert!(
                !super::status_is_unavailable(status),
                "{status} should be a content fault"
            );
        }
    }

    #[test]
    fn test_from_ollama_slot_stores_model() {
        let settings = crate::config::Settings {
            llm_model: Some("ollama::qwen3.5:4b".into()),
            ..crate::config::Settings::default()
        };
        let provider =
            GenaiLlmProvider::from_ollama_slot(&settings.primary_slot().unwrap()).unwrap();
        assert_eq!(provider.default_model(), "ollama::qwen3.5:4b");
    }

    #[tokio::test]
    async fn cloud_slots_assume_the_cloud_window_and_ollama_keeps_the_local_one() {
        let cloud = GenaiLlmProvider::from_cloud_slot(&cloud_slot(
            LlmProviderKind::Anthropic,
            "claude-haiku-5-5",
            "key",
            "https://api.anthropic.com/v1/".into(),
        ))
        .unwrap();
        assert_eq!(cloud.context_window("claude-haiku-5-5").await, None);
        assert_eq!(
            cloud.assumed_context_window(),
            crate::pipeline::CLOUD_DEFAULT_CONTEXT_WINDOW
        );

        let ollama = GenaiLlmProvider::from_ollama_slot(&cloud_slot(
            LlmProviderKind::Ollama,
            "qwen3.5:9b",
            "key",
            "http://gpu:11434".into(),
        ))
        .unwrap();
        assert_eq!(
            ollama.assumed_context_window(),
            crate::pipeline::DEFAULT_CONTEXT_WINDOW
        );
    }

    #[test]
    fn debug_never_prints_an_api_key() {
        let ollama = GenaiLlmProvider::from_ollama_slot(&cloud_slot(
            LlmProviderKind::Ollama,
            "qwen3.5:9b",
            "raw-ollama-secret",
            "http://user:host-secret@gpu:11434".into(),
        ))
        .unwrap();
        let debug = format!("{ollama:?}");
        assert!(!debug.contains("raw-ollama-secret"), "{debug}");
        assert!(!debug.contains("host-secret"), "{debug}");
        assert!(debug.contains("***"), "{debug}");

        let cloud = GenaiLlmProvider::from_cloud_slot(&cloud_slot(
            LlmProviderKind::Anthropic,
            "claude-haiku-5-5",
            "raw-cloud-secret",
            "https://api.anthropic.com/v1/".into(),
        ))
        .unwrap();
        let debug = format!("{cloud:?}");
        assert!(!debug.contains("raw-cloud-secret"), "{debug}");
    }
}
