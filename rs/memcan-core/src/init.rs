//! Shared initialization for MemCan binaries.
//!
//! Deduplicates the `Settings::load() -> embedder -> store` bootstrap that
//! every binary repeats.

use std::sync::Arc;

use crate::config::Settings;
use crate::embed::FastEmbedProvider;
use crate::error::Result;
use crate::lancedb_store::LanceDbStore;
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
        let store = LanceDbStore::open(&settings.lancedb_path).await?;
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

/// Create the default LLM provider from settings.
///
/// Returns the provider (as a trait object) and the resolved model name.
/// When `ollama-rs-llm` is enabled (default), the model name is prefix-stripped.
/// When `genai-llm` is enabled instead, the model name is passed through as-is.
pub fn create_llm_provider(settings: &Settings) -> (Arc<dyn LlmProvider>, String) {
    #[cfg(feature = "ollama-rs-llm")]
    {
        let provider = crate::llm_ollama_rs::OllamaRsLlmProvider::from_settings(settings);
        let model = provider.default_model().to_string();
        (Arc::new(provider), model)
    }
    #[cfg(all(feature = "genai-llm", not(feature = "ollama-rs-llm")))]
    {
        let provider = crate::llm::GenaiLlmProvider::from_settings(settings);
        let model = provider.default_model().to_string();
        (Arc::new(provider), model)
    }
}
