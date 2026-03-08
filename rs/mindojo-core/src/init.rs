//! Shared initialization for MindOJO binaries.
//!
//! Deduplicates the `Settings::load() -> embedder -> store` bootstrap that
//! every binary repeats.

use crate::config::Settings;
use crate::embed::FastEmbedProvider;
use crate::error::Result;
use crate::lancedb_store::LanceDbStore;

/// Common runtime context for MindOJO binaries.
pub struct MindojoContext {
    pub settings: Settings,
    pub embedder: FastEmbedProvider,
    pub store: LanceDbStore,
}

impl MindojoContext {
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
