//! Shared initialization for MemCan binaries.
//!
//! Deduplicates the `Settings::load() -> embedder -> store` bootstrap that
//! every binary repeats.

use std::sync::Arc;

use crate::config::{LlmProviderKind, LlmSlot, Settings};
use crate::embed::FastEmbedProvider;
use crate::error::Result;
use crate::health::DependencyHealth;
use crate::lancedb_store::LanceDbStore;
use crate::llm_fallback::{FallbackLlmProvider, LlmBackend};
use crate::traits::LlmProvider;

/// Common runtime context for MemCan binaries.
pub struct MemcanContext {
    pub settings: Settings,
    pub embedder: FastEmbedProvider,
    pub store: LanceDbStore,
}

impl MemcanContext {
    /// Load settings, create embedder, and open the vector store.
    pub async fn init() -> Result<Self> {
        let settings = Settings::load()?;
        settings.ensure_log_dir()?;
        let embedder = FastEmbedProvider::from_settings(&settings)?;
        let store = LanceDbStore::open(&settings.lancedb_path)
            .await?
            .with_auto_compaction(settings.compact_fragment_threshold);
        Ok(Self {
            settings,
            embedder,
            store,
        })
    }

    /// Load settings and create embedder only (no store).
    ///
    /// Useful for commands like `--download-model` that only need the
    /// embedding model, not the full vector store.
    pub fn init_settings_and_embedder() -> Result<(Settings, FastEmbedProvider)> {
        let settings = Settings::load()?;
        let embedder = FastEmbedProvider::from_settings(&settings)?;
        Ok((settings, embedder))
    }
}

/// Create the configured primary LLM provider and optional fallback.
///
/// Returns the provider (as a trait object) and the primary slot's model name.
pub fn create_llm_provider(
    settings: &Settings,
    health: Arc<DependencyHealth>,
) -> Result<(Arc<dyn LlmProvider>, String)> {
    let primary = build_backend(&settings.primary_slot()?)?;
    let fallback = settings
        .fallback_slot()?
        .map(|slot| build_backend(&slot))
        .transpose()?;
    let primary_model = primary.model.clone();
    let provider = FallbackLlmProvider::new(primary, fallback, health);
    Ok((Arc::new(provider), primary_model))
}

/// Build one slot's backend and log what it resolved to.
///
/// The log line is the operator's only confirmation of which API a slot
/// actually talks to; a misconfigured cloud backend otherwise surfaces only as
/// quiet fallback traffic. The API key is never logged.
fn build_backend(slot: &LlmSlot) -> Result<LlmBackend> {
    let backend = build_backend_inner(slot)?;
    tracing::info!(
        role = %slot.role,
        backend = %slot.provider,
        model = %backend.model,
        base_url = %crate::config::redact_url(
            slot.base_url
                .as_deref()
                .unwrap_or(crate::ollama::DEFAULT_OLLAMA_HOST)
        ),
        "LLM backend configured"
    );
    Ok(backend)
}

fn build_backend_inner(slot: &LlmSlot) -> Result<LlmBackend> {
    let kind = slot.provider;
    match kind {
        LlmProviderKind::Ollama => {
            #[cfg(feature = "ollama-rs-llm")]
            {
                let provider = crate::llm_ollama_rs::OllamaRsLlmProvider::from_slot(slot);
                let model = provider.default_model().to_string();
                Ok(LlmBackend {
                    provider: Arc::new(provider),
                    kind,
                    model,
                })
            }
            #[cfg(all(feature = "genai-llm", not(feature = "ollama-rs-llm")))]
            {
                let provider = crate::llm::GenaiLlmProvider::from_ollama_slot(slot);
                // genai selects its adapter from the model name; namespacing
                // pins it to Ollama, which the operator selected explicitly.
                let model =
                    crate::ollama::ensure_ollama_prefix(provider.default_model()).into_owned();
                Ok(LlmBackend {
                    provider: Arc::new(provider),
                    kind,
                    model,
                })
            }
            #[cfg(not(any(feature = "ollama-rs-llm", feature = "genai-llm")))]
            {
                Err(crate::error::MemcanError::Config(
                    "Ollama requires an LLM provider feature".into(),
                ))
            }
        }
        LlmProviderKind::Anthropic
        | LlmProviderKind::OpenAi
        | LlmProviderKind::OpenRouter
        | LlmProviderKind::OpenAiCompatible => {
            #[cfg(feature = "genai-llm")]
            {
                let provider = crate::llm::GenaiLlmProvider::from_cloud_slot(slot)?;
                let model = provider.default_model().to_string();
                Ok(LlmBackend {
                    provider: Arc::new(provider),
                    kind,
                    model,
                })
            }
            #[cfg(not(feature = "genai-llm"))]
            {
                Err(crate::error::MemcanError::Config(format!(
                    "the '{kind}' LLM backend requires the genai-llm feature"
                )))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(any(feature = "ollama-rs-llm", not(feature = "genai-llm")))]
    use crate::config::LlmProviderKind;
    use crate::health::DependencyHealth;

    #[cfg(all(feature = "ollama-rs-llm", feature = "genai-llm"))]
    #[test]
    fn tc15_provider_construction_returns_primary_model() {
        let health = Arc::new(DependencyHealth::with_defaults());
        for kind in [
            LlmProviderKind::Anthropic,
            LlmProviderKind::OpenAi,
            LlmProviderKind::OpenRouter,
        ] {
            let cloud_settings = Settings {
                llm_provider: kind,
                llm_model: Some("claude-haiku-5-5".into()),
                llm_api_key: Some("test-key".into()),
                ..Settings::default()
            };
            let (_, cloud_model) =
                create_llm_provider(&cloud_settings, Arc::clone(&health)).unwrap();
            assert_eq!(cloud_model, "claude-haiku-5-5", "{kind}");
        }

        let ollama_settings = Settings {
            llm_provider: LlmProviderKind::Ollama,
            llm_model: Some("ollama::qwen3.5:9b".into()),
            ..Settings::default()
        };
        let (_, ollama_model) = create_llm_provider(&ollama_settings, health).unwrap();
        assert_eq!(ollama_model, "qwen3.5:9b");
    }

    #[cfg(all(feature = "ollama-rs-llm", feature = "genai-llm"))]
    #[test]
    fn same_kind_slots_are_wired_and_described_separately() {
        let health = Arc::new(DependencyHealth::with_defaults());
        let settings = Settings {
            llm_provider: LlmProviderKind::Anthropic,
            llm_model: Some("claude-haiku-5-5".into()),
            llm_api_key: Some("primary-key".into()),
            llm_fallback_provider: Some(LlmProviderKind::Anthropic),
            llm_fallback_model: Some("claude-opus-5-5".into()),
            llm_fallback_api_key: Some("fallback-key".into()),
            ..Settings::default()
        };

        let (_, model) = create_llm_provider(&settings, Arc::clone(&health)).unwrap();

        assert_eq!(model, "claude-haiku-5-5");
        let status = health.status();
        assert_eq!(status["llm_primary"].backend.as_deref(), Some("anthropic"));
        assert_eq!(
            status["llm_primary"].model.as_deref(),
            Some("claude-haiku-5-5")
        );
        assert_eq!(status["llm_fallback"].backend.as_deref(), Some("anthropic"));
        assert_eq!(
            status["llm_fallback"].model.as_deref(),
            Some("claude-opus-5-5")
        );
    }

    #[cfg(feature = "ollama-rs-llm")]
    #[test]
    fn invalid_fallback_slot_fails_construction_naming_the_variable() {
        let settings = Settings {
            llm_fallback_provider: Some(LlmProviderKind::Anthropic),
            llm_fallback_model: Some("claude-haiku-5-5".into()),
            ..Settings::default()
        };

        let error = create_llm_provider(&settings, Arc::new(DependencyHealth::with_defaults()))
            .err()
            .unwrap()
            .to_string();
        assert!(error.contains("LLM_FALLBACK_API_KEY"), "{error}");
    }

    #[cfg(not(feature = "genai-llm"))]
    #[test]
    fn openrouter_requires_genai_feature_when_disabled() {
        let settings = Settings {
            llm_provider: LlmProviderKind::OpenRouter,
            llm_model: Some("openai/gpt-4o-mini".into()),
            llm_api_key: Some("test-key".into()),
            ..Settings::default()
        };
        let error = create_llm_provider(&settings, Arc::new(DependencyHealth::with_defaults()))
            .err()
            .unwrap();
        assert!(error.to_string().contains("genai-llm"));
    }

    #[cfg(not(any(feature = "ollama-rs-llm", feature = "genai-llm")))]
    #[test]
    fn ollama_requires_llm_feature_when_disabled() {
        let error = create_llm_provider(
            &Settings::default(),
            Arc::new(DependencyHealth::with_defaults()),
        )
        .err()
        .unwrap();

        assert!(error.to_string().contains("LLM provider feature"));
    }

    #[cfg(all(feature = "genai-llm", not(feature = "ollama-rs-llm")))]
    #[test]
    fn genai_only_ollama_backend_is_available() {
        let settings = Settings {
            llm_model: Some("ollama::qwen3.5:9b".into()),
            ..Settings::default()
        };

        let (_, model) =
            create_llm_provider(&settings, Arc::new(DependencyHealth::with_defaults())).unwrap();

        assert_eq!(model, "ollama::qwen3.5:9b");
    }

    /// genai picks its adapter from the model name, so an unprefixed name can
    /// route away from Ollama (`command-r7b` -> Cohere) even though the
    /// operator selected `LLM_PROVIDER=ollama`.
    #[cfg(all(feature = "genai-llm", not(feature = "ollama-rs-llm")))]
    #[test]
    fn genai_only_ollama_backend_normalizes_unprefixed_model() {
        for raw in [
            crate::config::DEFAULT_OLLAMA_MODEL,
            "command-r7b",
            "glm4:9b",
        ] {
            let settings = Settings {
                llm_provider: LlmProviderKind::Ollama,
                llm_model: Some(raw.into()),
                ..Settings::default()
            };

            let (_, model) =
                create_llm_provider(&settings, Arc::new(DependencyHealth::with_defaults()))
                    .unwrap();

            assert_eq!(
                model,
                format!("ollama::{raw}"),
                "unprefixed model '{raw}' must resolve to the Ollama adapter"
            );
        }
    }
}
