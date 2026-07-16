//! Ollama LLM provider via the [`ollama_rs`] crate.

use async_trait::async_trait;
use ollama_rs::Ollama;
use ollama_rs::error::OllamaError;
use ollama_rs::generation::chat::ChatMessage;
use ollama_rs::generation::chat::ChatMessageFinalResponseData;
use ollama_rs::generation::chat::request::ChatMessageRequest;
use ollama_rs::generation::parameters::{FormatType, ThinkType};
use ollama_rs::models::ModelOptions;

use crate::config::Settings;
use crate::error::{MemcanError, Result};
use crate::llm_telemetry;
use crate::ollama::strip_ollama_prefix;
use crate::traits::{LlmMessage, LlmOptions, LlmProvider, Role};

/// LLM provider backed by [`ollama_rs::Ollama`].
pub struct OllamaRsLlmProvider {
    client: Ollama,
    default_model: String,
}

/// Sort a chat failure into the availability class or the content class.
///
/// [`OllamaError::ReqwestError`] proves the daemon was unreachable, so it maps
/// to [`MemcanError::LlmUnavailable`]. `Other` and `JsonError` join it, because
/// ollama-rs cannot tell us enough to do better:
///
/// `send_chat_messages` collapses **every** non-2xx response into
/// `OllamaError::Other`, discarding the status code, so a 400
/// (context-length-exceeded) is indistinguishable from a 503 (model failed to
/// load) without parsing the body text. Both therefore stay in the
/// availability class: guessing from prose would be worse than the status quo,
/// and treating a real 5xx as a content fault would silently disable failover.
/// The known cost of that trade is that an Ollama-primary
/// context-length-exceeded still reaches the fallback.
fn classify_chat_error(error: &OllamaError, model_name: &str) -> MemcanError {
    let context = format!("ollama-rs chat call to model '{model_name}' failed");
    let detail = error.to_string();
    match error {
        OllamaError::ReqwestError(_) | OllamaError::Other(_) | OllamaError::JsonError(_) => {
            MemcanError::LlmUnavailable { context, detail }
        }
        _ => MemcanError::LlmChat { context, detail },
    }
}

impl OllamaRsLlmProvider {
    /// Build from application settings.
    ///
    /// Parses `OLLAMA_HOST` into (scheme+host, port). Strips the `ollama::`
    /// prefix from the configured model name. When `OLLAMA_API_KEY` is set,
    /// injects a Bearer auth header via `Ollama::new_with_request_headers`;
    /// otherwise uses the `Ollama::builder()` API.
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
                    Ollama::builder().host(host.clone()).port(port).build()
                }
            }
        } else {
            Ollama::builder().host(host).port(port).build()
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

/// Extract `(prompt_tokens, completion_tokens)` from Ollama's final response data.
///
/// Returns `(None, None)` when `final_data` is absent (e.g. streaming mode or
/// a provider that omits usage). Both fields are `Some` when the non-streaming
/// `/api/chat` endpoint returns a complete `done: true` response.
pub(crate) fn extract_token_counts(
    final_data: Option<&ChatMessageFinalResponseData>,
) -> (Option<u64>, Option<u64>) {
    match final_data {
        Some(d) => (Some(d.prompt_eval_count), Some(d.eval_count)),
        None => (None, None),
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

        let response = self
            .client
            .send_chat_messages(request)
            .await
            .map_err(|e| classify_chat_error(&e, model_name))?;

        // Emit structured token telemetry before consuming the response.
        let (prompt_tokens, completion_tokens) = extract_token_counts(response.final_data.as_ref());
        llm_telemetry::emit(opts.op, model_name, prompt_tokens, completion_tokens);

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
            Err(e) => {
                let err_str = e.to_string().to_lowercase();
                if err_str.contains("not found") || err_str.contains("404") {
                    tracing::info!(model = %model_name, "LLM model not found locally, pulling");
                } else {
                    return Err(MemcanError::Other(format!(
                        "failed to check Ollama model '{model_name}': {e}"
                    )));
                }
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
    use crate::traits::{LlmMessage, LlmProvider, Role};

    // ── classify_chat_error unit tests ──────────────────────────────────────

    #[test]
    fn non_2xx_response_is_classified_unavailable() {
        // send_chat_messages funnels every non-2xx into Other(body) without the
        // status, so this bucket cannot be narrowed without parsing prose.
        let error = OllamaError::Other("model is loading".into());
        assert!(classify_chat_error(&error, "qwen3.5:9b").is_llm_unavailable());
    }

    #[test]
    fn malformed_response_body_is_classified_unavailable() {
        let json_error = serde_json::from_str::<serde_json::Value>("{").unwrap_err();
        let error = OllamaError::JsonError(json_error);
        assert!(classify_chat_error(&error, "qwen3.5:9b").is_llm_unavailable());
    }

    #[test]
    fn classified_error_keeps_the_model_name_in_context() {
        let error = OllamaError::Other("boom".into());
        assert!(
            classify_chat_error(&error, "qwen3.5:9b")
                .to_string()
                .contains("qwen3.5:9b")
        );
    }

    // ── extract_token_counts unit tests ─────────────────────────────────────

    #[test]
    fn test_extract_token_counts_with_final_data() {
        let d = ChatMessageFinalResponseData {
            total_duration: 1_000_000,
            load_duration: 100_000,
            prompt_eval_count: 42,
            prompt_eval_duration: 500_000,
            eval_count: 15,
            eval_duration: 400_000,
        };
        let (p, c) = extract_token_counts(Some(&d));
        assert_eq!(p, Some(42));
        assert_eq!(c, Some(15));
    }

    #[test]
    fn test_extract_token_counts_none_when_no_final_data() {
        let (p, c) = extract_token_counts(None);
        assert_eq!(p, None);
        assert_eq!(c, None);
    }

    // ── chat() with token counts — mockito integration ────────────────────

    /// Verify chat() returns the assistant text AND handles token-count fields
    /// in the Ollama response without panicking. (Telemetry is a log side-effect;
    /// we assert the function contract: correct text returned, mock hit once.)
    #[tokio::test]
    async fn test_chat_returns_text_and_handles_token_counts() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/api/chat")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{
                    "model": "test-model",
                    "created_at": "2026-01-01T00:00:00Z",
                    "message": {"role": "assistant", "content": "Incremental code indexer."},
                    "done": true,
                    "total_duration": 1000000,
                    "load_duration": 100000,
                    "prompt_eval_count": 42,
                    "prompt_eval_duration": 500000,
                    "eval_count": 15,
                    "eval_duration": 400000
                }"#,
            )
            .create_async()
            .await;

        let provider = provider_at(&server.url(), "test-model");
        let messages = vec![LlmMessage {
            role: Role::User,
            content: "Describe this function.".into(),
        }];
        let opts = Some(crate::traits::LlmOptions {
            op: "code_description",
            ..Default::default()
        });

        let result = provider.chat("test-model", &messages, opts).await;

        assert!(result.is_ok(), "chat should succeed: {:?}", result);
        assert_eq!(result.unwrap(), "Incremental code indexer.");
        mock.assert_async().await;
    }

    /// An empty completion is a content fault: the daemon answered, so it must
    /// not be reported as unavailable (which would divert the prompt to a
    /// third-party backend and mark a healthy Ollama Down).
    #[tokio::test]
    async fn test_chat_empty_response_is_a_content_fault() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/api/chat")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{
                    "model": "test-model",
                    "created_at": "2026-01-01T00:00:00Z",
                    "message": {"role": "assistant", "content": ""},
                    "done": true
                }"#,
            )
            .create_async()
            .await;

        let provider = provider_at(&server.url(), "test-model");
        let messages = vec![LlmMessage {
            role: Role::User,
            content: "Describe this function.".into(),
        }];

        let error = provider
            .chat("test-model", &messages, None)
            .await
            .unwrap_err();

        assert!(error.is_llm_error());
        assert!(
            !error.is_llm_unavailable(),
            "an empty completion says nothing about availability: {error:?}"
        );
        mock.assert_async().await;
    }

    /// A 5xx means the daemon could not serve the request, which is exactly
    /// what the fallback and the circuit breaker exist for.
    #[tokio::test]
    async fn test_chat_server_error_is_an_availability_fault() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/api/chat")
            .with_status(503)
            .with_body("model is loading")
            .create_async()
            .await;

        let provider = provider_at(&server.url(), "test-model");
        let messages = vec![LlmMessage {
            role: Role::User,
            content: "Describe this function.".into(),
        }];

        let error = provider
            .chat("test-model", &messages, None)
            .await
            .unwrap_err();

        assert!(
            error.is_llm_unavailable(),
            "expected availability: {error:?}"
        );
        mock.assert_async().await;
    }

    /// Verify chat() handles a response with no token-count fields gracefully
    /// (final_data absent — older Ollama versions or truncated responses).
    #[tokio::test]
    async fn test_chat_handles_missing_token_counts() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/api/chat")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{
                    "model": "test-model",
                    "created_at": "2026-01-01T00:00:00Z",
                    "message": {"role": "assistant", "content": "result text"},
                    "done": true
                }"#,
            )
            .create_async()
            .await;

        let provider = provider_at(&server.url(), "test-model");
        let messages = vec![LlmMessage {
            role: Role::User,
            content: "hi".into(),
        }];
        let opts = Some(crate::traits::LlmOptions {
            op: "dedup",
            ..Default::default()
        });

        let result = provider.chat("test-model", &messages, opts).await;

        assert!(
            result.is_ok(),
            "should handle missing token counts: {:?}",
            result
        );
        assert_eq!(result.unwrap(), "result text");
        mock.assert_async().await;
    }

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

    /// Helper: create a provider pointing at the given base URL.
    fn provider_at(base_url: &str, model: &str) -> OllamaRsLlmProvider {
        let settings = Settings {
            llm_model: model.into(),
            ollama_host: Some(base_url.to_string()),
            ..Settings::default()
        };
        OllamaRsLlmProvider::from_settings(&settings)
    }

    #[tokio::test]
    async fn test_init_model_available() {
        use crate::traits::LlmProvider;

        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/api/show")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"modelfile":"","parameters":"","template":"","model_info":{}}"#)
            .create_async()
            .await;

        let provider = provider_at(&server.url(), "test-model");
        let result = provider.init().await;

        assert!(result.is_ok(), "init should succeed when model exists");
        mock.assert_async().await;
    }

    #[tokio::test]
    #[ignore = "ollama-rs pull_model expects streaming NDJSON that is hard to mock reliably"]
    async fn test_init_model_not_found_pulls() {
        use crate::traits::LlmProvider;

        let mut server = mockito::Server::new_async().await;
        let show_mock = server
            .mock("POST", "/api/show")
            .with_status(404)
            .with_body(r#"{"error":"model 'test-model' not found"}"#)
            .create_async()
            .await;
        let pull_mock = server
            .mock("POST", "/api/pull")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body("{\"status\": \"success\"}\n")
            .create_async()
            .await;

        let provider = provider_at(&server.url(), "test-model");
        let result = provider.init().await;

        assert!(result.is_ok(), "init should succeed after pulling model");
        show_mock.assert_async().await;
        pull_mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_init_connection_error_no_pull() {
        use crate::traits::LlmProvider;

        let provider = provider_at("http://127.0.0.1:1", "test-model");
        let result = provider.init().await;

        assert!(result.is_err(), "init should fail on connection error");
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("failed to check Ollama model"),
            "error should indicate check failure, got: {err_msg}"
        );
    }
}
