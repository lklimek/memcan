use std::sync::Arc;

use async_trait::async_trait;
use tracing::{debug, error, warn};

use crate::error::{MemcanError, Result};
use crate::health::{DependencyHealth, DependencyId};
use crate::traits::{LlmMessage, LlmOptions, LlmProvider};

/// LLM provider that dispatches to a circuit-breaker-aware fallback.
pub struct FallbackLlmProvider {
    primary: Arc<dyn LlmProvider>,
    primary_dep: DependencyId,
    fallback: Option<FallbackTarget>,
    health: Arc<DependencyHealth>,
}

struct FallbackTarget {
    provider: Arc<dyn LlmProvider>,
    model: String,
    dep: DependencyId,
}

impl FallbackLlmProvider {
    pub(crate) fn new(
        primary: Arc<dyn LlmProvider>,
        primary_dep: DependencyId,
        fallback: Option<(Arc<dyn LlmProvider>, String, DependencyId)>,
        health: Arc<DependencyHealth>,
    ) -> Self {
        let fallback = fallback.and_then(|(provider, model, dep)| {
            if dep == primary_dep {
                warn!(
                    provider = %primary_dep,
                    "Ignoring LLM fallback because it matches the primary backend"
                );
                None
            } else {
                Some(FallbackTarget {
                    provider,
                    model,
                    dep,
                })
            }
        });
        health.mark_configured(primary_dep);
        if let Some(fallback) = &fallback {
            health.mark_configured(fallback.dep);
        }

        Self {
            primary,
            primary_dep,
            fallback,
            health,
        }
    }

    fn combined_error(
        &self,
        fallback: &FallbackTarget,
        primary_error: &MemcanError,
        fallback_error: &MemcanError,
    ) -> MemcanError {
        MemcanError::LlmChat {
            context: format!(
                "both LLM backends '{}' and '{}' failed",
                self.primary_dep, fallback.dep
            ),
            detail: format!("primary: {primary_error}; fallback: {fallback_error}"),
        }
    }
}

impl std::fmt::Debug for FallbackLlmProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FallbackLlmProvider")
            .field("primary_dep", &self.primary_dep)
            .field(
                "fallback_dep",
                &self.fallback.as_ref().map(|fallback| fallback.dep),
            )
            .field(
                "fallback_model",
                &self.fallback.as_ref().map(|fallback| &fallback.model),
            )
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl LlmProvider for FallbackLlmProvider {
    async fn chat(
        &self,
        model: &str,
        messages: &[LlmMessage],
        options: Option<LlmOptions>,
    ) -> Result<String> {
        let primary_error = match self.health.check(self.primary_dep) {
            Ok(()) => match self.primary.chat(model, messages, options.clone()).await {
                Ok(response) => {
                    self.health.report_success(self.primary_dep);
                    return Ok(response);
                }
                Err(primary_error) => {
                    self.health
                        .report_failure(self.primary_dep, &primary_error.to_string());
                    if self.fallback.is_none() {
                        return Err(primary_error);
                    }
                    warn!(
                        primary = %self.primary_dep,
                        "Primary LLM request failed; attempting configured fallback"
                    );
                    primary_error
                }
            },
            Err(primary_error) => {
                if self.fallback.is_none() {
                    return Err(MemcanError::LlmChat {
                        context: format!("LLM backend '{}' is unavailable", self.primary_dep),
                        detail: primary_error.to_string(),
                    });
                }
                debug!(
                    primary = %self.primary_dep,
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
                primary = %self.primary_dep,
                fallback = %fallback.dep,
                "Both LLM circuit breakers are open"
            );
            return Err(self.combined_error(fallback, &primary_error, &fallback_error));
        }

        match fallback
            .provider
            .chat(&fallback.model, messages, options)
            .await
        {
            Ok(response) => {
                self.health.report_success(fallback.dep);
                warn!(
                    primary = %self.primary_dep,
                    fallback = %fallback.dep,
                    "Fallback LLM request succeeded after primary became unavailable"
                );
                Ok(response)
            }
            Err(fallback_error) => {
                self.health
                    .report_failure(fallback.dep, &fallback_error.to_string());
                error!(
                    primary = %self.primary_dep,
                    fallback = %fallback.dep,
                    "Primary and fallback LLM requests both failed"
                );
                Err(self.combined_error(fallback, &primary_error, &fallback_error))
            }
        }
    }

    /// Report a context window that is safe for **either** backend.
    ///
    /// [`chat`](Self::chat) dispatches to the primary or the fallback depending
    /// on circuit-breaker state, and each backend runs its own model with its
    /// own window. Reporting the primary's window would let callers size a
    /// prompt the fallback cannot accept, so this returns the minimum of the
    /// two. A backend whose breaker is already open is not queried at all —
    /// a degraded (hung rather than refusing) backend would otherwise block
    /// this call, and callers memoize the result for the process lifetime.
    ///
    /// Returns `None` whenever either backend's window is unknown, so callers
    /// apply their own conservative default rather than trust a half-known
    /// budget.
    async fn context_window(&self, model: &str) -> Option<usize> {
        let primary_window = match self.health.check(self.primary_dep) {
            Ok(()) => self.primary.context_window(model).await,
            Err(_) => {
                debug!(
                    primary = %self.primary_dep,
                    "Not querying primary context window because its circuit breaker is open"
                );
                None
            }
        };

        let Some(fallback) = self.fallback.as_ref() else {
            return primary_window;
        };

        let fallback_window = match self.health.check(fallback.dep) {
            Ok(()) => fallback.provider.context_window(&fallback.model).await,
            Err(_) => {
                debug!(
                    fallback = %fallback.dep,
                    "Not querying fallback context window because its circuit breaker is open"
                );
                None
            }
        };

        match (primary_window, fallback_window) {
            (Some(primary_window), Some(fallback_window)) => {
                Some(primary_window.min(fallback_window))
            }
            _ => None,
        }
    }

    async fn init(&self) -> Result<()> {
        match self.primary.init().await {
            Ok(()) => {
                self.health.report_success(self.primary_dep);
                Ok(())
            }
            Err(primary_error) => {
                self.health
                    .report_failure(self.primary_dep, &primary_error.to_string());
                let Some(fallback) = &self.fallback else {
                    return Err(primary_error);
                };
                warn!(
                    primary = %self.primary_dep,
                    fallback = %fallback.dep,
                    "Primary LLM initialization failed; trying configured fallback"
                );
                match fallback.provider.init().await {
                    Ok(()) => {
                        self.health.report_success(fallback.dep);
                        Ok(())
                    }
                    Err(fallback_error) => {
                        self.health
                            .report_failure(fallback.dep, &fallback_error.to_string());
                        error!(
                            primary = %self.primary_dep,
                            fallback = %fallback.dep,
                            "Primary and fallback LLM initialization both failed"
                        );
                        Err(self.combined_error(fallback, &primary_error, &fallback_error))
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use async_trait::async_trait;

    use super::FallbackLlmProvider;
    use crate::error::{MemcanError, Result};
    use crate::health::{DependencyHealth, DependencyId, DependencyStatus};
    use crate::traits::{LlmMessage, LlmOptions, LlmProvider};

    #[derive(Clone, Copy)]
    enum MockResult {
        Success(&'static str),
        Failure(&'static str),
    }

    struct MockLlmProvider {
        chat_results: Mutex<VecDeque<MockResult>>,
        init_result: MockResult,
        chat_calls: AtomicUsize,
        init_calls: AtomicUsize,
        context_window_calls: AtomicUsize,
        context_window: Option<usize>,
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
            }
        }

        fn with_init(mut self, init_result: MockResult) -> Self {
            self.init_result = init_result;
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
            match result {
                MockResult::Success(text) => Ok(text.into()),
                MockResult::Failure(detail) => Err(MemcanError::LlmChat {
                    context: "mock provider".into(),
                    detail: detail.into(),
                }),
            }
        }

        async fn context_window(&self, _model: &str) -> Option<usize> {
            self.context_window_calls.fetch_add(1, Ordering::SeqCst);
            self.context_window
        }

        async fn init(&self) -> Result<()> {
            self.init_calls.fetch_add(1, Ordering::SeqCst);
            match self.init_result {
                MockResult::Success(_) => Ok(()),
                MockResult::Failure(detail) => Err(MemcanError::LlmChat {
                    context: "mock init".into(),
                    detail: detail.into(),
                }),
            }
        }
    }

    fn wrapper(
        primary: Arc<MockLlmProvider>,
        fallback: Option<Arc<MockLlmProvider>>,
        health: Arc<DependencyHealth>,
    ) -> FallbackLlmProvider {
        let primary_provider: Arc<dyn LlmProvider> = primary;
        let fallback = fallback.map(|provider| {
            let provider: Arc<dyn LlmProvider> = provider;
            (
                provider,
                "openai/gpt-4o-mini".into(),
                DependencyId::OpenRouter,
            )
        });
        FallbackLlmProvider::new(primary_provider, DependencyId::Ollama, fallback, health)
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
        assert_eq!(health.status()["ollama"].status, DependencyStatus::Healthy);
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
    async fn context_window_is_unknown_when_either_backend_cannot_report() {
        // A half-known budget is worse than none: callers have a conservative
        // default, but cannot know the unreported backend is smaller.
        let primary = Arc::new(MockLlmProvider::new([]).with_context_window(Some(32_768)));
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

        health.report_failure(DependencyId::Ollama, "primary is down");

        assert_eq!(provider.context_window("qwen3.5:9b").await, None);
        assert_eq!(
            primary.context_window_calls(),
            0,
            "must not query a backend the breaker already knows is down"
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
    fn configured_openrouter_is_included_in_health_status() {
        let primary = Arc::new(MockLlmProvider::new([]));
        let fallback = Arc::new(MockLlmProvider::new([]));
        let health = Arc::new(DependencyHealth::with_defaults());

        let _provider = wrapper(primary, Some(fallback), health.clone());

        assert!(health.status().contains_key("openrouter"));
    }

    #[tokio::test]
    async fn tc2_primary_failure_calls_healthy_fallback() {
        let primary = Arc::new(MockLlmProvider::new([MockResult::Failure(
            "primary failed",
        )]));
        let fallback = Arc::new(MockLlmProvider::new([MockResult::Success("fallback")]));
        let health = Arc::new(DependencyHealth::with_defaults());
        let provider = wrapper(primary.clone(), Some(fallback.clone()), health.clone());

        assert_eq!(chat(&provider).await.unwrap(), "fallback");
        assert_eq!(primary.chat_calls(), 1);
        assert_eq!(fallback.chat_calls(), 1);
        assert_eq!(health.status()["ollama"].status, DependencyStatus::Down);
        assert_eq!(
            health.status()["openrouter"].status,
            DependencyStatus::Healthy
        );
    }

    #[tokio::test]
    async fn tc3_both_provider_failures_name_both_backends() {
        let primary = Arc::new(MockLlmProvider::new([MockResult::Failure(
            "primary failed",
        )]));
        let fallback = Arc::new(MockLlmProvider::new([MockResult::Failure(
            "fallback failed",
        )]));
        let health = Arc::new(DependencyHealth::with_defaults());
        let provider = wrapper(primary, Some(fallback), health.clone());

        let error = chat(&provider).await.unwrap_err().to_string();
        assert!(error.contains("ollama"));
        assert!(error.contains("openrouter"));
        assert_eq!(health.status()["ollama"].status, DependencyStatus::Down);
        assert_eq!(health.status()["openrouter"].status, DependencyStatus::Down);
    }

    #[tokio::test]
    async fn tc4_open_primary_breaker_skips_primary() {
        let primary = Arc::new(MockLlmProvider::new([MockResult::Success("primary")]));
        let fallback = Arc::new(MockLlmProvider::new([MockResult::Success("fallback")]));
        let health = Arc::new(DependencyHealth::new(Duration::from_secs(60)));
        health.report_failure(DependencyId::Ollama, "known down");
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
        health.report_failure(DependencyId::Ollama, "down");
        tokio::time::sleep(Duration::from_millis(10)).await;
        let provider = wrapper(primary.clone(), Some(fallback.clone()), health.clone());

        assert_eq!(chat(&provider).await.unwrap(), "recovered");
        assert_eq!(primary.chat_calls(), 1);
        assert_eq!(fallback.chat_calls(), 0);
        assert_eq!(health.status()["ollama"].status, DependencyStatus::Healthy);
    }

    #[tokio::test]
    async fn tc5b_half_open_primary_probe_failure_uses_fallback() {
        let primary = Arc::new(MockLlmProvider::new([MockResult::Failure("still down")]));
        let fallback = Arc::new(MockLlmProvider::new([MockResult::Success("fallback")]));
        let health = Arc::new(DependencyHealth::new(Duration::from_millis(5)));
        health.report_failure(DependencyId::Ollama, "down");
        tokio::time::sleep(Duration::from_millis(10)).await;
        let provider = wrapper(primary, Some(fallback), health.clone());

        assert_eq!(chat(&provider).await.unwrap(), "fallback");
        assert_eq!(health.status()["ollama"].status, DependencyStatus::Down);
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
        let primary = Arc::new(MockLlmProvider::new([MockResult::Failure("exact failure")]));
        let health = Arc::new(DependencyHealth::with_defaults());
        let provider = wrapper(primary, None, health.clone());

        let error = chat(&provider).await.unwrap_err();
        assert!(matches!(
            error,
            MemcanError::LlmChat { ref detail, .. } if detail == "exact failure"
        ));
        assert_eq!(health.status()["ollama"].status, DependencyStatus::Down);
    }

    #[tokio::test]
    async fn tc8_no_fallback_open_breaker_fails_without_primary_call() {
        let primary = Arc::new(MockLlmProvider::new([MockResult::Success("unused")]));
        let health = Arc::new(DependencyHealth::new(Duration::from_secs(60)));
        health.report_failure(DependencyId::Ollama, "known down");
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
        health.report_failure(DependencyId::Ollama, "primary known down");
        health.report_failure(DependencyId::OpenRouter, "fallback known down");
        let provider = wrapper(primary.clone(), Some(fallback.clone()), health);

        let error = chat(&provider).await.unwrap_err();
        assert!(matches!(
            error,
            MemcanError::LlmChat {
                ref context,
                ref detail
            } if context.contains("ollama")
                && context.contains("openrouter")
                && detail.contains("primary known down")
                && detail.contains("fallback known down")
        ));
        assert_eq!(primary.chat_calls(), 0);
        assert_eq!(fallback.chat_calls(), 0);
    }

    #[tokio::test]
    async fn tc9_wrapper_drives_down_half_open_healthy_transition() {
        let primary = Arc::new(MockLlmProvider::new([
            MockResult::Failure("first failure"),
            MockResult::Success("recovered"),
        ]));
        let health = Arc::new(DependencyHealth::new(Duration::from_millis(5)));
        let provider = wrapper(primary, None, health.clone());

        assert!(chat(&provider).await.is_err());
        assert_eq!(health.status()["ollama"].status, DependencyStatus::Down);
        tokio::time::sleep(Duration::from_millis(10)).await;
        assert_eq!(chat(&provider).await.unwrap(), "recovered");
        assert_eq!(health.status()["ollama"].status, DependencyStatus::Healthy);
    }

    #[tokio::test]
    async fn tc12_equal_fallback_dependency_is_neutralized() {
        let primary = Arc::new(MockLlmProvider::new([MockResult::Success("primary")]));
        let duplicate = Arc::new(MockLlmProvider::new([MockResult::Success("duplicate")]));
        let primary_provider: Arc<dyn LlmProvider> = primary;
        let duplicate_provider: Arc<dyn LlmProvider> = duplicate.clone();
        let provider = FallbackLlmProvider::new(
            primary_provider,
            DependencyId::Ollama,
            Some((
                duplicate_provider,
                "other-ollama-model".into(),
                DependencyId::Ollama,
            )),
            Arc::new(DependencyHealth::with_defaults()),
        );

        assert_eq!(chat(&provider).await.unwrap(), "primary");
        assert_eq!(duplicate.chat_calls(), 0);
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
        assert_eq!(health.status()["ollama"].status, DependencyStatus::Down);
        assert!(health.check(DependencyId::Ollama).is_err());

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
        assert_eq!(health.status()["ollama"].status, DependencyStatus::Down);
        assert_eq!(health.status()["openrouter"].status, DependencyStatus::Down);
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
