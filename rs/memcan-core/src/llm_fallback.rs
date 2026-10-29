use std::sync::Arc;

use async_trait::async_trait;
use tracing::{debug, error, warn};

use crate::config::LlmProviderKind;
use crate::error::{MemcanError, Result};
use crate::health::{DependencyHealth, DependencyId};
use crate::traits::{LlmMessage, LlmOptions, LlmProvider};

/// One wired LLM slot: its client and what it runs.
pub(crate) struct LlmBackend {
    pub provider: Arc<dyn LlmProvider>,
    pub kind: LlmProviderKind,
    pub model: String,
}

/// A backend and the circuit breaker that tracks it.
struct Slot {
    backend: LlmBackend,
    dep: DependencyId,
}

impl Slot {
    fn wire(backend: LlmBackend, dep: DependencyId, health: &DependencyHealth) -> Self {
        health.describe_llm(dep, &backend.kind.to_string(), &backend.model);
        Self { backend, dep }
    }

    fn kind(&self) -> LlmProviderKind {
        self.backend.kind
    }

    fn model(&self) -> &str {
        &self.backend.model
    }

    /// Send `messages` to this slot and record the outcome on its breaker.
    async fn chat(
        &self,
        health: &DependencyHealth,
        model: &str,
        messages: &[LlmMessage],
        options: Option<LlmOptions>,
    ) -> Result<String> {
        let result = self.backend.provider.chat(model, messages, options).await;
        match &result {
            Ok(_) => health.report_success(self.dep),
            Err(error) if error.is_llm_unavailable() => {
                health.report_failure(self.dep, &error.to_string());
            }
            Err(_) => {}
        }
        result
    }

    /// Initialize this slot and record the outcome on its breaker.
    async fn init(&self, health: &DependencyHealth) -> Result<()> {
        let result = self.backend.provider.init().await;
        match &result {
            Ok(()) => health.report_success(self.dep),
            Err(error) => health.report_failure(self.dep, &error.to_string()),
        }
        result
    }
}

/// LLM provider that dispatches to a circuit-breaker-aware fallback.
///
/// Each slot has its own breaker ([`DependencyId::LlmPrimary`] /
/// [`DependencyId::LlmFallback`]), so two slots running the same backend kind
/// — say two Anthropic keys — never share a health state.
pub struct FallbackLlmProvider {
    primary: Slot,
    fallback: Option<Slot>,
    health: Arc<DependencyHealth>,
}

impl FallbackLlmProvider {
    pub(crate) fn new(
        primary: LlmBackend,
        fallback: Option<LlmBackend>,
        health: Arc<DependencyHealth>,
    ) -> Self {
        let primary = Slot::wire(primary, DependencyId::LlmPrimary, &health);
        let fallback =
            fallback.map(|backend| Slot::wire(backend, DependencyId::LlmFallback, &health));
        Self {
            primary,
            fallback,
            health,
        }
    }

    fn combined_error(
        &self,
        fallback: &Slot,
        primary_error: &MemcanError,
        fallback_error: &MemcanError,
    ) -> MemcanError {
        MemcanError::LlmChat {
            context: format!(
                "both LLM slots failed: primary '{}' ({}) and fallback '{}' ({})",
                self.primary.kind(),
                self.primary.model(),
                fallback.kind(),
                fallback.model()
            ),
            detail: format!("primary: {primary_error}; fallback: {fallback_error}"),
        }
    }
}

impl std::fmt::Debug for FallbackLlmProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FallbackLlmProvider")
            .field("primary_kind", &self.primary.kind())
            .field("primary_model", &self.primary.model())
            .field("fallback_kind", &self.fallback.as_ref().map(Slot::kind))
            .field("fallback_model", &self.fallback.as_ref().map(Slot::model))
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl LlmProvider for FallbackLlmProvider {
    /// Serve a chat request from the primary, falling back only when the
    /// primary is *unavailable*.
    ///
    /// Only [`MemcanError::LlmUnavailable`] is an availability signal. A
    /// content fault ([`MemcanError::LlmChat`] — empty response, rejected
    /// prompt) returns to the caller unchanged: it would fail identically on
    /// the fallback, so re-sending would leak `messages` to a third party for
    /// nothing, and reporting it to the breaker would mark a serving backend
    /// Down for a fault it did not cause.
    ///
    /// This keeps egress scoped to genuine outages, so caller-supplied content
    /// cannot itself trigger a cross-provider send.
    async fn chat(
        &self,
        model: &str,
        messages: &[LlmMessage],
        options: Option<LlmOptions>,
    ) -> Result<String> {
        let primary = &self.primary;
        let primary_error = match self.health.check(primary.dep) {
            Ok(()) => match primary
                .chat(&self.health, model, messages, options.clone())
                .await
            {
                Ok(response) => return Ok(response),
                Err(primary_error) if !primary_error.is_llm_unavailable() => {
                    debug!(
                        primary = %primary.kind(),
                        primary_model = %primary.model(),
                        error = %primary_error,
                        "Primary LLM rejected the request; keeping it local because the backend is healthy"
                    );
                    return Err(primary_error);
                }
                Err(primary_error) => {
                    if self.fallback.is_none() {
                        return Err(primary_error);
                    }
                    warn!(
                        primary = %primary.kind(),
                        primary_model = %primary.model(),
                        error = %primary_error,
                        "Primary LLM backend is unavailable; attempting configured fallback"
                    );
                    primary_error
                }
            },
            Err(primary_error) => {
                if self.fallback.is_none() {
                    return Err(MemcanError::LlmUnavailable {
                        context: format!(
                            "primary LLM slot '{}' ({}) is unavailable",
                            primary.kind(),
                            primary.model()
                        ),
                        detail: primary_error.to_string(),
                    });
                }
                debug!(
                    primary = %primary.kind(),
                    primary_model = %primary.model(),
                    "Skipping primary LLM request because its circuit breaker is open"
                );
                primary_error
            }
        };

        let Some(fallback) = self.fallback.as_ref() else {
            return Err(primary_error);
        };
        if let Err(fallback_error) = self.health.check(fallback.dep) {
            error!(
                primary = %primary.kind(),
                primary_model = %primary.model(),
                fallback = %fallback.kind(),
                fallback_model = %fallback.model(),
                "Both LLM circuit breakers are open"
            );
            return Err(self.combined_error(fallback, &primary_error, &fallback_error));
        }

        warn!(
            primary = %primary.kind(),
            primary_model = %primary.model(),
            fallback = %fallback.kind(),
            fallback_model = %fallback.model(),
            "LLM fallback engaged: request content is being sent to the fallback provider"
        );
        match fallback
            .chat(&self.health, fallback.model(), messages, options)
            .await
        {
            Ok(response) => Ok(response),
            Err(fallback_error) => {
                error!(
                    primary = %primary.kind(),
                    primary_model = %primary.model(),
                    fallback = %fallback.kind(),
                    fallback_model = %fallback.model(),
                    "Primary and fallback LLM requests both failed"
                );
                Err(self.combined_error(fallback, &primary_error, &fallback_error))
            }
        }
    }

    /// Report a context window sized for the backend [`chat`](Self::chat) will
    /// actually use.
    ///
    /// Each backend runs its own model with its own window, so when both are
    /// known this returns the minimum: a prompt sized for the larger one would
    /// be rejected if the request landed on the smaller.
    ///
    /// When only the *fallback's* window is unknown, this reports the primary's
    /// rather than `None`. Some backends cannot report a window at all — the
    /// cloud API configuration this provider exists to serve is one — so
    /// `None` there would not mean "be careful once", it would permanently pin
    /// every caller to the primary's assumed window, shrinking real budgets by
    /// an order of magnitude for a fallback that normally never engages. Callers memoize this for the process lifetime and
    /// content-hash-skip their way past the damage, so silent over-truncation
    /// is the worse failure: an oversized request to the fallback fails loudly
    /// and only while the primary is down.
    ///
    /// A backend whose breaker is already open is not queried at all — a
    /// degraded (hung rather than refusing) backend would otherwise block this
    /// memoized call. An unknown (or unqueried) *primary* window counts as the
    /// primary's [`assumed_context_window`](LlmProvider::assumed_context_window),
    /// capped by a known fallback window; with neither known this is `None`.
    async fn context_window(&self, model: &str) -> Option<usize> {
        let primary_window = match self.health.check(self.primary.dep) {
            Ok(()) => self.primary.backend.provider.context_window(model).await,
            Err(_) => {
                debug!(
                    primary = %self.primary.kind(),
                    primary_model = %self.primary.model(),
                    "Not querying primary context window because its circuit breaker is open"
                );
                None
            }
        };

        let Some(fallback) = self.fallback.as_ref() else {
            return primary_window;
        };

        let fallback_window = match self.health.check(fallback.dep) {
            Ok(()) => {
                fallback
                    .backend
                    .provider
                    .context_window(fallback.model())
                    .await
            }
            Err(_) => {
                debug!(
                    fallback = %fallback.kind(),
                    fallback_model = %fallback.model(),
                    "Not querying fallback context window because its circuit breaker is open"
                );
                None
            }
        };

        match (primary_window, fallback_window) {
            (Some(primary_window), Some(fallback_window)) => {
                Some(primary_window.min(fallback_window))
            }
            // The fallback cannot report a window: trust the primary's rather
            // than pinning every caller to the conservative default forever.
            (Some(primary_window), None) => Some(primary_window),
            // The primary cannot report a window: size for its assumed one,
            // but never beyond a fallback window that is known to be smaller.
            (None, Some(fallback_window)) => Some(
                self.primary
                    .backend
                    .provider
                    .assumed_context_window()
                    .min(fallback_window),
            ),
            (None, None) => None,
        }
    }

    /// The smaller of both backends' assumptions: [`Self::context_window`]
    /// returns `None` only when neither window is known, and either backend
    /// may serve the request — a cloud primary's large assumption must not
    /// overfeed a local fallback whose window could not be discovered.
    fn assumed_context_window(&self) -> usize {
        let primary = self.primary.backend.provider.assumed_context_window();
        self.fallback.as_ref().map_or(primary, |fallback| {
            primary.min(fallback.backend.provider.assumed_context_window())
        })
    }

    async fn init(&self) -> Result<()> {
        let primary = &self.primary;
        let Err(primary_error) = primary.init(&self.health).await else {
            return Ok(());
        };
        let Some(fallback) = &self.fallback else {
            return Err(primary_error);
        };
        warn!(
            primary = %primary.kind(),
            primary_model = %primary.model(),
            fallback = %fallback.kind(),
            fallback_model = %fallback.model(),
            "Primary LLM initialization failed; trying configured fallback"
        );
        fallback.init(&self.health).await.map_err(|fallback_error| {
            error!(
                primary = %primary.kind(),
                primary_model = %primary.model(),
                fallback = %fallback.kind(),
                fallback_model = %fallback.model(),
                "Primary and fallback LLM initialization both failed"
            );
            self.combined_error(fallback, &primary_error, &fallback_error)
        })
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use async_trait::async_trait;

    use super::{FallbackLlmProvider, LlmBackend};
    use crate::config::LlmProviderKind;
    use crate::error::{MemcanError, Result};
    use crate::health::{DependencyHealth, DependencyId, DependencyStatus};
    use crate::traits::{LlmMessage, LlmOptions, LlmProvider};

    #[derive(Clone, Copy)]
    enum MockResult {
        Success(&'static str),
        /// A content fault: the backend answered but the prompt yielded no
        /// usable text. Carries no availability signal.
        Failure(&'static str),
        /// An availability fault: the backend could not be reached.
        Unavailable(&'static str),
    }

    impl MockResult {
        fn into_result(self, context: &str) -> Result<String> {
            match self {
                MockResult::Success(text) => Ok(text.into()),
                MockResult::Failure(detail) => Err(MemcanError::LlmChat {
                    context: context.into(),
                    detail: detail.into(),
                }),
                MockResult::Unavailable(detail) => Err(MemcanError::LlmUnavailable {
                    context: context.into(),
                    detail: detail.into(),
                }),
            }
        }
    }

    struct MockLlmProvider {
        chat_results: Mutex<VecDeque<MockResult>>,
        init_result: MockResult,
        chat_calls: AtomicUsize,
        init_calls: AtomicUsize,
        context_window_calls: AtomicUsize,
        context_window: Option<usize>,
        assumed_context_window: usize,
    }

    impl MockLlmProvider {
        fn new(chat_results: impl IntoIterator<Item = MockResult>) -> Self {
            Self {
                chat_results: Mutex::new(chat_results.into_iter().collect()),
                init_result: MockResult::Success("initialized"),
                chat_calls: AtomicUsize::new(0),
                init_calls: AtomicUsize::new(0),
                context_window_calls: AtomicUsize::new(0),
                context_window: Some(4096),
                assumed_context_window: crate::pipeline::DEFAULT_CONTEXT_WINDOW,
            }
        }

        fn with_init(mut self, init_result: MockResult) -> Self {
            self.init_result = init_result;
            self
        }

        fn with_assumed_context_window(mut self, assumed: usize) -> Self {
            self.assumed_context_window = assumed;
            self
        }

        fn with_context_window(mut self, context_window: Option<usize>) -> Self {
            self.context_window = context_window;
            self
        }

        fn context_window_calls(&self) -> usize {
            self.context_window_calls.load(Ordering::SeqCst)
        }

        fn chat_calls(&self) -> usize {
            self.chat_calls.load(Ordering::SeqCst)
        }

        fn init_calls(&self) -> usize {
            self.init_calls.load(Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl LlmProvider for MockLlmProvider {
        async fn chat(
            &self,
            _model: &str,
            _messages: &[LlmMessage],
            _options: Option<LlmOptions>,
        ) -> Result<String> {
            self.chat_calls.fetch_add(1, Ordering::SeqCst);
            let result = self
                .chat_results
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or(MockResult::Success("default success"));
            result.into_result("mock provider")
        }

        async fn context_window(&self, _model: &str) -> Option<usize> {
            self.context_window_calls.fetch_add(1, Ordering::SeqCst);
            self.context_window
        }

        fn assumed_context_window(&self) -> usize {
            self.assumed_context_window
        }

        async fn init(&self) -> Result<()> {
            self.init_calls.fetch_add(1, Ordering::SeqCst);
            self.init_result.into_result("mock init").map(|_| ())
        }
    }

    fn wrapper(
        primary: Arc<MockLlmProvider>,
        fallback: Option<Arc<MockLlmProvider>>,
        health: Arc<DependencyHealth>,
    ) -> FallbackLlmProvider {
        let primary = LlmBackend {
            provider: primary,
            kind: LlmProviderKind::Ollama,
            model: "qwen3.5:9b".into(),
        };
        let fallback = fallback.map(|provider| LlmBackend {
            provider,
            kind: LlmProviderKind::Anthropic,
            model: "claude-haiku-5-5".into(),
        });
        FallbackLlmProvider::new(primary, fallback, health)
    }

    async fn chat(provider: &FallbackLlmProvider) -> Result<String> {
        provider.chat("qwen3.5:9b", &[], None).await
    }

    #[tokio::test]
    async fn tc1_primary_success_does_not_call_fallback() {
        let primary = Arc::new(MockLlmProvider::new([MockResult::Success("primary")]));
        let fallback = Arc::new(MockLlmProvider::new([MockResult::Success("fallback")]));
        let health = Arc::new(DependencyHealth::with_defaults());
        let provider = wrapper(primary.clone(), Some(fallback.clone()), health.clone());

        assert_eq!(chat(&provider).await.unwrap(), "primary");
        assert_eq!(primary.chat_calls(), 1);
        assert_eq!(fallback.chat_calls(), 0);
        assert_eq!(
            health.status()["llm_primary"].status,
            DependencyStatus::Healthy
        );
    }

    #[tokio::test]
    async fn context_window_reports_the_smaller_of_both_backends() {
        // chat() may dispatch to either backend, so the reported window must be
        // safe for both — not just the primary's larger one.
        let primary = Arc::new(MockLlmProvider::new([]).with_context_window(Some(32_768)));
        let fallback = Arc::new(MockLlmProvider::new([]).with_context_window(Some(8_192)));
        let health = Arc::new(DependencyHealth::with_defaults());
        let provider = wrapper(primary.clone(), Some(fallback.clone()), health);

        assert_eq!(provider.context_window("qwen3.5:9b").await, Some(8_192));
        assert_eq!(primary.context_window_calls(), 1);
        assert_eq!(fallback.context_window_calls(), 1);
    }

    #[tokio::test]
    async fn context_window_trusts_the_primary_when_the_fallback_cannot_report() {
        // A cloud API fallback can never report a window, so treating that as
        // "unknown" would pin every caller to the conservative default for the
        // whole process — the configuration this provider exists to serve.
        let primary = Arc::new(MockLlmProvider::new([]).with_context_window(Some(32_768)));
        let fallback = Arc::new(MockLlmProvider::new([]).with_context_window(None));
        let health = Arc::new(DependencyHealth::with_defaults());
        let provider = wrapper(primary, Some(fallback), health);

        assert_eq!(provider.context_window("qwen3.5:9b").await, Some(32_768));
    }

    #[tokio::test]
    async fn context_window_caps_the_primarys_assumption_at_the_fallbacks_window() {
        // A cloud primary cannot report its window and assumes a large one; a
        // known smaller fallback window must still bound the budget, or every
        // request served by the fallback would overflow it.
        let primary = Arc::new(
            MockLlmProvider::new([])
                .with_context_window(None)
                .with_assumed_context_window(crate::pipeline::CLOUD_DEFAULT_CONTEXT_WINDOW),
        );
        let fallback = Arc::new(MockLlmProvider::new([]).with_context_window(Some(8_192)));
        let health = Arc::new(DependencyHealth::with_defaults());
        let provider = wrapper(primary, Some(fallback), health);
        assert_eq!(provider.context_window("claude").await, Some(8_192));

        // A smaller assumption stays in force.
        let primary = Arc::new(MockLlmProvider::new([]).with_context_window(None));
        let fallback = Arc::new(MockLlmProvider::new([]).with_context_window(Some(8_192)));
        let health = Arc::new(DependencyHealth::with_defaults());
        let provider = wrapper(primary, Some(fallback), health);
        assert_eq!(
            provider.context_window("qwen3.5:9b").await,
            Some(crate::pipeline::DEFAULT_CONTEXT_WINDOW)
        );
    }

    #[tokio::test]
    async fn context_window_is_unknown_when_neither_backend_can_report() {
        let primary = Arc::new(MockLlmProvider::new([]).with_context_window(None));
        let fallback = Arc::new(MockLlmProvider::new([]).with_context_window(None));
        let health = Arc::new(DependencyHealth::with_defaults());
        let provider = wrapper(primary, Some(fallback), health);

        assert_eq!(provider.context_window("qwen3.5:9b").await, None);
    }

    #[tokio::test]
    async fn context_window_skips_a_backend_whose_breaker_is_open() {
        // A degraded backend hangs rather than refusing; querying it would block
        // a call whose result callers memoize for the process lifetime.
        let primary = Arc::new(MockLlmProvider::new([]).with_context_window(Some(32_768)));
        let fallback = Arc::new(MockLlmProvider::new([]).with_context_window(Some(8_192)));
        let health = Arc::new(DependencyHealth::with_defaults());
        let provider = wrapper(primary.clone(), Some(fallback.clone()), health.clone());

        health.report_failure(DependencyId::LlmPrimary, "primary is down");

        // The unqueried primary counts as its assumed window, capped by the
        // fallback's, which chat() will use while the primary is down.
        assert_eq!(
            provider.context_window("qwen3.5:9b").await,
            Some(crate::pipeline::DEFAULT_CONTEXT_WINDOW)
        );
        assert_eq!(
            primary.context_window_calls(),
            0,
            "must not query a backend the breaker already knows is down"
        );
    }

    #[tokio::test]
    async fn context_window_uses_the_primary_when_the_fallback_breaker_is_open() {
        // An open fallback breaker is indistinguishable from "cannot report" at
        // this layer, and chat() will not reach that fallback anyway — so the
        // primary's window is the one callers should size against.
        let primary = Arc::new(MockLlmProvider::new([]).with_context_window(Some(32_768)));
        let fallback = Arc::new(MockLlmProvider::new([]).with_context_window(Some(8_192)));
        let health = Arc::new(DependencyHealth::with_defaults());
        let provider = wrapper(primary, Some(fallback.clone()), health.clone());

        health.report_failure(DependencyId::LlmFallback, "fallback is down");

        assert_eq!(provider.context_window("qwen3.5:9b").await, Some(32_768));
        assert_eq!(
            fallback.context_window_calls(),
            0,
            "must not query a fallback the breaker already knows is down"
        );
    }

    #[tokio::test]
    async fn assumed_context_window_is_the_smaller_of_both_assumptions() {
        // A cloud primary with an Ollama fallback whose window could not be
        // discovered: neither reports, so callers size for the assumption —
        // which must not overfeed the local fallback 8x when it takes over.
        let cloud = || {
            Arc::new(
                MockLlmProvider::new([])
                    .with_context_window(None)
                    .with_assumed_context_window(crate::pipeline::CLOUD_DEFAULT_CONTEXT_WINDOW),
            )
        };
        let local = Arc::new(MockLlmProvider::new([]).with_context_window(None));
        let provider = wrapper(
            cloud(),
            Some(local),
            Arc::new(DependencyHealth::with_defaults()),
        );
        assert_eq!(provider.context_window("claude").await, None);
        assert_eq!(
            provider.assumed_context_window(),
            crate::pipeline::DEFAULT_CONTEXT_WINDOW
        );

        // Two cloud slots keep the cloud assumption; no fallback keeps the primary's.
        let provider = wrapper(
            cloud(),
            Some(cloud()),
            Arc::new(DependencyHealth::with_defaults()),
        );
        assert_eq!(
            provider.assumed_context_window(),
            crate::pipeline::CLOUD_DEFAULT_CONTEXT_WINDOW
        );
        let provider = wrapper(cloud(), None, Arc::new(DependencyHealth::with_defaults()));
        assert_eq!(
            provider.assumed_context_window(),
            crate::pipeline::CLOUD_DEFAULT_CONTEXT_WINDOW
        );
    }

    #[tokio::test]
    async fn context_window_without_fallback_delegates_to_primary() {
        let primary = Arc::new(MockLlmProvider::new([]).with_context_window(Some(32_768)));
        let health = Arc::new(DependencyHealth::with_defaults());
        let provider = wrapper(primary.clone(), None, health);

        assert_eq!(provider.context_window("qwen3.5:9b").await, Some(32_768));
        assert_eq!(primary.context_window_calls(), 1);
    }

    #[test]
    fn configured_fallback_is_included_in_health_status() {
        let primary = Arc::new(MockLlmProvider::new([]));
        let fallback = Arc::new(MockLlmProvider::new([]));
        let health = Arc::new(DependencyHealth::with_defaults());

        let _provider = wrapper(primary, Some(fallback), health.clone());

        let status = health.status();
        assert_eq!(status["llm_primary"].backend.as_deref(), Some("ollama"));
        assert_eq!(status["llm_fallback"].backend.as_deref(), Some("anthropic"));
        assert_eq!(
            status["llm_fallback"].model.as_deref(),
            Some("claude-haiku-5-5")
        );
    }

    #[tokio::test]
    async fn content_fault_is_not_shipped_to_the_fallback_provider() {
        // A prompt the primary cannot answer fails identically everywhere, so
        // re-sending it to a third-party backend would leak the content without
        // any prospect of success.
        let primary = Arc::new(MockLlmProvider::new([MockResult::Failure(
            "empty response from LLM",
        )]));
        let fallback = Arc::new(MockLlmProvider::new([MockResult::Success("fallback")]));
        let health = Arc::new(DependencyHealth::with_defaults());
        let provider = wrapper(primary.clone(), Some(fallback.clone()), health.clone());

        let error = chat(&provider).await.unwrap_err();
        assert!(matches!(
            error,
            MemcanError::LlmChat { ref detail, .. } if detail == "empty response from LLM"
        ));
        assert_eq!(primary.chat_calls(), 1);
        assert_eq!(
            fallback.chat_calls(),
            0,
            "content faults must never egress to the fallback provider"
        );
    }

    #[tokio::test]
    async fn content_fault_does_not_trip_the_primary_breaker() {
        // The prompt is at fault, not the backend; marking a serving backend
        // Down would corrupt the operator's health signal and arm egress for
        // every request in the recovery window.
        let primary = Arc::new(MockLlmProvider::new([
            MockResult::Failure("empty response from LLM"),
            MockResult::Success("primary still serving"),
        ]));
        let fallback = Arc::new(MockLlmProvider::new([MockResult::Success("fallback")]));
        let health = Arc::new(DependencyHealth::new(Duration::from_secs(60)));
        let provider = wrapper(primary, Some(fallback.clone()), health.clone());

        assert!(chat(&provider).await.is_err());
        assert_eq!(
            health.status()["llm_primary"].status,
            DependencyStatus::Healthy,
            "a content fault must leave the primary breaker closed"
        );

        // The breaker stayed closed, so the next request still reaches the primary.
        assert_eq!(chat(&provider).await.unwrap(), "primary still serving");
        assert_eq!(fallback.chat_calls(), 0);
    }

    #[tokio::test]
    async fn content_fault_from_the_fallback_does_not_trip_its_breaker() {
        let primary = Arc::new(MockLlmProvider::new([MockResult::Unavailable(
            "connection refused",
        )]));
        let fallback = Arc::new(MockLlmProvider::new([MockResult::Failure(
            "empty response from LLM",
        )]));
        let health = Arc::new(DependencyHealth::new(Duration::from_secs(60)));
        let provider = wrapper(primary, Some(fallback), health.clone());

        assert!(chat(&provider).await.is_err());
        assert_eq!(
            health.status()["llm_primary"].status,
            DependencyStatus::Down
        );
        assert_eq!(
            health.status()["llm_fallback"].status,
            DependencyStatus::Healthy,
            "the fallback answered; only the prompt failed"
        );
    }

    #[tokio::test]
    async fn tc2_primary_failure_calls_healthy_fallback() {
        let primary = Arc::new(MockLlmProvider::new([MockResult::Unavailable(
            "primary failed",
        )]));
        let fallback = Arc::new(MockLlmProvider::new([MockResult::Success("fallback")]));
        let health = Arc::new(DependencyHealth::with_defaults());
        let provider = wrapper(primary.clone(), Some(fallback.clone()), health.clone());

        assert_eq!(chat(&provider).await.unwrap(), "fallback");
        assert_eq!(primary.chat_calls(), 1);
        assert_eq!(fallback.chat_calls(), 1);
        assert_eq!(
            health.status()["llm_primary"].status,
            DependencyStatus::Down
        );
        assert_eq!(
            health.status()["llm_fallback"].status,
            DependencyStatus::Healthy
        );
    }

    #[tokio::test]
    async fn tc3_both_provider_failures_name_both_backends() {
        let primary = Arc::new(MockLlmProvider::new([MockResult::Unavailable(
            "primary failed",
        )]));
        let fallback = Arc::new(MockLlmProvider::new([MockResult::Unavailable(
            "fallback failed",
        )]));
        let health = Arc::new(DependencyHealth::with_defaults());
        let provider = wrapper(primary, Some(fallback), health.clone());

        let error = chat(&provider).await.unwrap_err().to_string();
        assert!(error.contains("ollama"), "{error}");
        assert!(error.contains("anthropic"), "{error}");
        assert_eq!(
            health.status()["llm_primary"].status,
            DependencyStatus::Down
        );
        assert_eq!(
            health.status()["llm_fallback"].status,
            DependencyStatus::Down
        );
    }

    #[tokio::test]
    async fn tc4_open_primary_breaker_skips_primary() {
        let primary = Arc::new(MockLlmProvider::new([MockResult::Success("primary")]));
        let fallback = Arc::new(MockLlmProvider::new([MockResult::Success("fallback")]));
        let health = Arc::new(DependencyHealth::new(Duration::from_secs(60)));
        health.report_failure(DependencyId::LlmPrimary, "known down");
        let provider = wrapper(primary.clone(), Some(fallback.clone()), health);

        assert_eq!(chat(&provider).await.unwrap(), "fallback");
        assert_eq!(primary.chat_calls(), 0);
        assert_eq!(fallback.chat_calls(), 1);
    }

    #[tokio::test]
    async fn tc5a_half_open_primary_probe_success_restores_health() {
        let primary = Arc::new(MockLlmProvider::new([MockResult::Success("recovered")]));
        let fallback = Arc::new(MockLlmProvider::new([MockResult::Success("fallback")]));
        let health = Arc::new(DependencyHealth::new(Duration::from_millis(5)));
        health.report_failure(DependencyId::LlmPrimary, "down");
        tokio::time::sleep(Duration::from_millis(10)).await;
        let provider = wrapper(primary.clone(), Some(fallback.clone()), health.clone());

        assert_eq!(chat(&provider).await.unwrap(), "recovered");
        assert_eq!(primary.chat_calls(), 1);
        assert_eq!(fallback.chat_calls(), 0);
        assert_eq!(
            health.status()["llm_primary"].status,
            DependencyStatus::Healthy
        );
    }

    #[tokio::test]
    async fn tc5b_half_open_primary_probe_failure_uses_fallback() {
        let primary = Arc::new(MockLlmProvider::new([MockResult::Unavailable(
            "still down",
        )]));
        let fallback = Arc::new(MockLlmProvider::new([MockResult::Success("fallback")]));
        let health = Arc::new(DependencyHealth::new(Duration::from_millis(5)));
        health.report_failure(DependencyId::LlmPrimary, "down");
        tokio::time::sleep(Duration::from_millis(10)).await;
        let provider = wrapper(primary, Some(fallback), health.clone());

        assert_eq!(chat(&provider).await.unwrap(), "fallback");
        assert_eq!(
            health.status()["llm_primary"].status,
            DependencyStatus::Down
        );
    }

    #[tokio::test]
    async fn tc6_no_fallback_primary_success_is_unchanged() {
        let primary = Arc::new(MockLlmProvider::new([MockResult::Success("primary")]));
        let health = Arc::new(DependencyHealth::with_defaults());
        let provider = wrapper(primary, None, health);

        assert_eq!(chat(&provider).await.unwrap(), "primary");
        assert_eq!(provider.context_window("qwen3.5:9b").await, Some(4096));
    }

    #[tokio::test]
    async fn tc7_no_fallback_returns_primary_error_unchanged() {
        let primary = Arc::new(MockLlmProvider::new([MockResult::Unavailable(
            "exact failure",
        )]));
        let health = Arc::new(DependencyHealth::with_defaults());
        let provider = wrapper(primary, None, health.clone());

        let error = chat(&provider).await.unwrap_err();
        assert!(matches!(
            error,
            MemcanError::LlmUnavailable { ref detail, .. } if detail == "exact failure"
        ));
        assert_eq!(
            health.status()["llm_primary"].status,
            DependencyStatus::Down
        );
    }

    #[tokio::test]
    async fn tc7b_no_fallback_content_fault_leaves_breaker_closed() {
        let primary = Arc::new(MockLlmProvider::new([MockResult::Failure("exact failure")]));
        let health = Arc::new(DependencyHealth::new(Duration::from_secs(60)));
        let provider = wrapper(primary, None, health.clone());

        let error = chat(&provider).await.unwrap_err();
        assert!(matches!(
            error,
            MemcanError::LlmChat { ref detail, .. } if detail == "exact failure"
        ));
        assert_eq!(
            health.status()["llm_primary"].status,
            DependencyStatus::Healthy
        );
    }

    #[tokio::test]
    async fn tc8_no_fallback_open_breaker_fails_without_primary_call() {
        let primary = Arc::new(MockLlmProvider::new([MockResult::Success("unused")]));
        let health = Arc::new(DependencyHealth::new(Duration::from_secs(60)));
        health.report_failure(DependencyId::LlmPrimary, "known down");
        let provider = wrapper(primary.clone(), None, health);

        let error = chat(&provider).await.unwrap_err();
        assert!(error.is_llm_error());
        assert!(error.to_string().contains("known down"));
        assert_eq!(primary.chat_calls(), 0);
    }

    #[tokio::test]
    async fn tc8b_both_open_breakers_skip_both_providers() {
        let primary = Arc::new(MockLlmProvider::new([MockResult::Success("unused")]));
        let fallback = Arc::new(MockLlmProvider::new([MockResult::Success("unused")]));
        let health = Arc::new(DependencyHealth::new(Duration::from_secs(60)));
        health.report_failure(DependencyId::LlmPrimary, "primary known down");
        health.report_failure(DependencyId::LlmFallback, "fallback known down");
        let provider = wrapper(primary.clone(), Some(fallback.clone()), health);

        let error = chat(&provider).await.unwrap_err();
        assert!(matches!(
            error,
            MemcanError::LlmChat {
                ref context,
                ref detail
            } if context.contains("ollama")
                && context.contains("anthropic")
                && detail.contains("primary known down")
                && detail.contains("fallback known down")
        ));
        assert_eq!(primary.chat_calls(), 0);
        assert_eq!(fallback.chat_calls(), 0);
    }

    #[tokio::test]
    async fn tc9_wrapper_drives_down_half_open_healthy_transition() {
        let primary = Arc::new(MockLlmProvider::new([
            MockResult::Unavailable("first failure"),
            MockResult::Success("recovered"),
        ]));
        let health = Arc::new(DependencyHealth::new(Duration::from_millis(5)));
        let provider = wrapper(primary, None, health.clone());

        assert!(chat(&provider).await.is_err());
        assert_eq!(
            health.status()["llm_primary"].status,
            DependencyStatus::Down
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
        assert_eq!(chat(&provider).await.unwrap(), "recovered");
        assert_eq!(
            health.status()["llm_primary"].status,
            DependencyStatus::Healthy
        );
    }

    #[tokio::test]
    async fn tc12_same_kind_fallback_has_its_own_breaker() {
        // Two Anthropic slots (different keys/models) are distinct backends:
        // an outage of the primary must leave the fallback serving.
        let primary = Arc::new(MockLlmProvider::new([MockResult::Unavailable(
            "primary key rate limited",
        )]));
        let fallback = Arc::new(MockLlmProvider::new([
            MockResult::Success("second key"),
            MockResult::Success("default success"),
            MockResult::Unavailable("second key rate limited"),
        ]));
        let health = Arc::new(DependencyHealth::new(Duration::from_secs(60)));
        let provider = FallbackLlmProvider::new(
            LlmBackend {
                provider: primary,
                kind: LlmProviderKind::Anthropic,
                model: "claude-haiku-5-5".into(),
            },
            Some(LlmBackend {
                provider: fallback.clone(),
                kind: LlmProviderKind::Anthropic,
                model: "claude-opus-5-5".into(),
            }),
            health.clone(),
        );

        assert_eq!(chat(&provider).await.unwrap(), "second key");
        assert_eq!(fallback.chat_calls(), 1);
        let status = health.status();
        assert_eq!(status["llm_primary"].status, DependencyStatus::Down);
        assert_eq!(status["llm_fallback"].status, DependencyStatus::Healthy);
        assert_eq!(chat(&provider).await.unwrap(), "default success");
        assert_eq!(fallback.chat_calls(), 2);

        // With both slots failing, the error tells the same-kind slots apart.
        let error = chat(&provider).await.unwrap_err().to_string();
        assert!(error.contains("claude-haiku-5-5"), "{error}");
        assert!(error.contains("claude-opus-5-5"), "{error}");
    }

    #[tokio::test]
    async fn tc16a_primary_init_failure_allows_healthy_fallback_startup() {
        let primary = Arc::new(
            MockLlmProvider::new([]).with_init(MockResult::Failure("primary init failed")),
        );
        let fallback = Arc::new(MockLlmProvider::new([MockResult::Success("fallback")]));
        let health = Arc::new(DependencyHealth::new(Duration::from_secs(60)));
        let provider = wrapper(primary.clone(), Some(fallback.clone()), health.clone());

        assert!(provider.init().await.is_ok());
        assert_eq!(primary.init_calls(), 1);
        assert_eq!(fallback.init_calls(), 1);
        assert_eq!(
            health.status()["llm_primary"].status,
            DependencyStatus::Down
        );
        assert!(health.check(DependencyId::LlmPrimary).is_err());

        assert_eq!(chat(&provider).await.unwrap(), "fallback");
        assert_eq!(primary.chat_calls(), 0);
        assert_eq!(fallback.chat_calls(), 1);
    }

    #[tokio::test]
    async fn tc16b_both_init_failures_return_error() {
        let primary = Arc::new(
            MockLlmProvider::new([]).with_init(MockResult::Failure("primary init failed")),
        );
        let fallback = Arc::new(
            MockLlmProvider::new([]).with_init(MockResult::Failure("fallback init failed")),
        );
        let health = Arc::new(DependencyHealth::with_defaults());
        let provider = wrapper(primary, Some(fallback), health.clone());

        assert!(provider.init().await.is_err());
        assert_eq!(
            health.status()["llm_primary"].status,
            DependencyStatus::Down
        );
        assert_eq!(
            health.status()["llm_fallback"].status,
            DependencyStatus::Down
        );
    }

    #[tokio::test]
    async fn tc16c_no_fallback_init_failure_is_unchanged() {
        let primary =
            Arc::new(MockLlmProvider::new([]).with_init(MockResult::Failure("exact init failure")));
        let provider = wrapper(primary, None, Arc::new(DependencyHealth::with_defaults()));

        let error = provider.init().await.unwrap_err();
        assert!(matches!(
            error,
            MemcanError::LlmChat { ref detail, .. } if detail == "exact init failure"
        ));
    }
}
