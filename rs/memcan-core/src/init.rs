//! Shared initialization for MemCan binaries.
//!
//! Deduplicates the `Settings::load() -> embedder -> store` bootstrap that
//! every binary repeats.

use std::sync::Arc;

use crate::config::{LlmProviderKind, Settings};
use crate::embed::FastEmbedProvider;
use crate::error::Result;
use crate::health::{DependencyHealth, DependencyId};
use crate::lancedb_store::LanceDbStore;
use crate::llm_fallback::FallbackLlmProvider;
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
/// Returns the provider (as a trait object) and the resolved model name.
pub fn create_llm_provider(
    settings: &Settings,
    health: Arc<DependencyHealth>,
) -> Result<(Arc<dyn LlmProvider>, String)> {
    let (primary, primary_model, primary_dep) = build_backend(settings.llm_provider, settings)?;
    let fallback = match settings.llm_fallback_provider {
        Some(kind) if kind == settings.llm_provider => {
            tracing::warn!(
                provider = %kind,
                "Ignoring configured LLM fallback because it matches the primary backend"
            );
            None
        }
        Some(kind) => Some(build_backend(kind, settings)?),
        None => None,
    };
    let provider = FallbackLlmProvider::new(primary, primary_dep, fallback, health);
    Ok((Arc::new(provider), primary_model))
}

fn build_backend(
    kind: LlmProviderKind,
    settings: &Settings,
) -> Result<(Arc<dyn LlmProvider>, String, DependencyId)> {
    #[cfg(not(any(feature = "ollama-rs-llm", feature = "genai-llm")))]
    let _ = settings;

    match kind {
        LlmProviderKind::Ollama => {
            #[cfg(feature = "ollama-rs-llm")]
            {
                let provider = crate::llm_ollama_rs::OllamaRsLlmProvider::from_settings(settings);
                let model = provider.default_model().to_string();
                Ok((Arc::new(provider), model, DependencyId::Ollama))
            }
            #[cfg(all(feature = "genai-llm", not(feature = "ollama-rs-llm")))]
            {
                let provider = crate::llm::GenaiLlmProvider::from_settings(settings);
                // genai selects its adapter from the model name; namespacing
                // pins it to Ollama, which the operator selected explicitly.
                let model =
                    crate::ollama::ensure_ollama_prefix(provider.default_model()).into_owned();
                Ok((Arc::new(provider), model, DependencyId::Ollama))
            }
            #[cfg(not(any(feature = "ollama-rs-llm", feature = "genai-llm")))]
            {
                Err(crate::error::MemcanError::Config(
                    "Ollama requires an LLM provider feature".into(),
                ))
            }
        }
        LlmProviderKind::OpenRouter => {
            #[cfg(feature = "genai-llm")]
            {
                let provider = crate::llm::GenaiLlmProvider::from_openrouter_settings(settings);
                Ok((
                    Arc::new(provider),
                    settings.openrouter_model.clone(),
                    DependencyId::OpenRouter,
                ))
            }
            #[cfg(not(feature = "genai-llm"))]
            {
                Err(crate::error::MemcanError::Config(
                    "OpenRouter requires the genai-llm feature".into(),
                ))
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
        let openrouter_settings = Settings {
            llm_provider: LlmProviderKind::OpenRouter,
            openrouter_api_key: Some("test-key".into()),
            openrouter_model: "openai/gpt-4o-mini".into(),
            ..Settings::default()
        };
        let (_, openrouter_model) =
            create_llm_provider(&openrouter_settings, Arc::clone(&health)).unwrap();
        assert_eq!(openrouter_model, "openai/gpt-4o-mini");

        let ollama_settings = Settings {
            llm_provider: LlmProviderKind::Ollama,
            llm_model: "ollama::qwen3.5:9b".into(),
            ..Settings::default()
        };
        let (_, ollama_model) = create_llm_provider(&ollama_settings, health).unwrap();
        assert_eq!(ollama_model, "qwen3.5:9b");
    }

    #[cfg(not(feature = "genai-llm"))]
    #[test]
    fn openrouter_requires_genai_feature_when_disabled() {
        let settings = Settings {
            llm_provider: LlmProviderKind::OpenRouter,
            openrouter_api_key: Some("test-key".into()),
            openrouter_model: "openai/gpt-4o-mini".into(),
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
            llm_model: "ollama::qwen3.5:9b".into(),
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
            Settings::default().llm_model.as_str(),
            "command-r7b",
            "glm4:9b",
        ] {
            let settings = Settings {
                llm_provider: LlmProviderKind::Ollama,
                llm_model: raw.into(),
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
