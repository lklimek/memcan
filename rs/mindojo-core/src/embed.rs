//! In-process embedding provider using [`fastembed`] (ONNX Runtime).
//!
//! Replaces the old Ollama-based embeddings with a zero-dependency local model.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use fastembed::{EmbeddingModel, TextEmbedding, TextInitOptions};
use tracing::info;

use crate::config::Settings;
use crate::error::{MindojoError, Result};
use crate::traits::EmbeddingProvider;

/// Wraps a [`TextEmbedding`] model for in-process embedding.
///
/// `TextEmbedding` is `!Send` (ONNX session), so we hold it behind a `Mutex`
/// and run inference on a blocking thread via `tokio::task::spawn_blocking`.
pub struct FastEmbedProvider {
    model: Arc<Mutex<TextEmbedding>>,
    dims: usize,
}

impl std::fmt::Debug for FastEmbedProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FastEmbedProvider")
            .field("dims", &self.dims)
            .finish()
    }
}

impl FastEmbedProvider {
    /// Create a new provider using a built-in fastembed model.
    pub fn new(model_name: EmbeddingModel, dims: usize) -> Result<Self> {
        let opts = TextInitOptions::new(model_name).with_show_download_progress(true);
        let model = TextEmbedding::try_new(opts).map_err(|e| MindojoError::Embedding {
            context: "failed to initialise fastembed model".into(),
            detail: e.to_string(),
        })?;
        info!(dims, "FastEmbed model loaded");
        Ok(Self {
            model: Arc::new(Mutex::new(model)),
            dims,
        })
    }

    /// Create from application [`Settings`].
    ///
    /// Uses [`Settings::embed_model`] to resolve the fastembed model variant.
    pub fn from_settings(settings: &Settings) -> Result<Self> {
        let model_name = resolve_model(&settings.embed_model)?;
        Self::new(model_name, settings.embed_dims)
    }
}

#[async_trait]
impl EmbeddingProvider for FastEmbedProvider {
    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        let docs: Vec<String> = texts.to_vec();
        let model = Arc::clone(&self.model);
        tokio::task::spawn_blocking(move || {
            let mut guard = model.lock().map_err(|e| MindojoError::Embedding {
                context: "model lock poisoned".into(),
                detail: e.to_string(),
            })?;
            guard
                .embed(docs, None)
                .map_err(|e| MindojoError::Embedding {
                    context: "fastembed embed failed".into(),
                    detail: e.to_string(),
                })
        })
        .await
        .map_err(|e| MindojoError::Embedding {
            context: "spawn_blocking panicked".into(),
            detail: e.to_string(),
        })?
    }

    fn dimensions(&self) -> usize {
        self.dims
    }
}

/// Map a user-facing model name string to a fastembed [`EmbeddingModel`] variant.
///
/// Accepts both fastembed enum names (e.g. `"AllMiniLML6V2"`) and common
/// short-hand names (e.g. `"all-MiniLM-L6-v2"`).
pub fn resolve_model(name: &str) -> Result<EmbeddingModel> {
    // Canonical lowered match
    let lower = name.to_lowercase().replace(['-', '_', '.'], "");
    let model = match lower.as_str() {
        // -- all-MiniLM-L6-v2 (default, very fast, 384 dims)
        "allminilml6v2" => EmbeddingModel::AllMiniLML6V2,
        "allminilml6v2q" => EmbeddingModel::AllMiniLML6V2Q,
        // -- all-MiniLM-L12-v2
        "allminilml12v2" => EmbeddingModel::AllMiniLML12V2,
        "allminilml12v2q" => EmbeddingModel::AllMiniLML12V2Q,
        // -- BGE small/base/large EN v1.5
        "bgesmallenv15" => EmbeddingModel::BGESmallENV15,
        "bgesmallenv15q" => EmbeddingModel::BGESmallENV15Q,
        "bgebaseenv15" => EmbeddingModel::BGEBaseENV15,
        "bgebaseenv15q" => EmbeddingModel::BGEBaseENV15Q,
        "bgelargeenv15" => EmbeddingModel::BGELargeENV15,
        "bgelargeenv15q" => EmbeddingModel::BGELargeENV15Q,
        // -- Nomic
        "nomicembedtextv1" => EmbeddingModel::NomicEmbedTextV1,
        "nomicembedtextv15" => EmbeddingModel::NomicEmbedTextV15,
        "nomicembedtextv15q" => EmbeddingModel::NomicEmbedTextV15Q,
        // -- Multilingual E5
        "multilinguale5small" => EmbeddingModel::MultilingualE5Small,
        "multilinguale5base" => EmbeddingModel::MultilingualE5Base,
        "multilinguale5large" => EmbeddingModel::MultilingualE5Large,
        // -- Snowflake Arctic
        "snowflakearcticembedl" => EmbeddingModel::SnowflakeArcticEmbedL,
        "snowflakearcticembedlq" => EmbeddingModel::SnowflakeArcticEmbedLQ,
        "snowflakearcticembedm" => EmbeddingModel::SnowflakeArcticEmbedM,
        "snowflakearcticembedmq" => EmbeddingModel::SnowflakeArcticEmbedMQ,
        "snowflakearcticembedmlong" => EmbeddingModel::SnowflakeArcticEmbedMLong,
        "snowflakearcticembedslong" | "snowflakearcticembedmlongq" => {
            EmbeddingModel::SnowflakeArcticEmbedMLongQ
        }
        "snowflakearcticembeds" => EmbeddingModel::SnowflakeArcticEmbedS,
        "snowflakearcticembedsq" => EmbeddingModel::SnowflakeArcticEmbedSQ,
        "snowflakearcticembedxs" => EmbeddingModel::SnowflakeArcticEmbedXS,
        "snowflakearcticembedxsq" => EmbeddingModel::SnowflakeArcticEmbedXSQ,
        // -- BGE-M3
        "bgem3" => EmbeddingModel::BGEM3,
        // -- MxbaiEmbedLargeV1
        "mxbaiembedlargev1" => EmbeddingModel::MxbaiEmbedLargeV1,
        "mxbaiembedlargev1q" => EmbeddingModel::MxbaiEmbedLargeV1Q,
        // -- GTE
        "gtebaseenv15" => EmbeddingModel::GTEBaseENV15,
        "gtebaseenv15q" => EmbeddingModel::GTEBaseENV15Q,
        "gtelargeenv15" => EmbeddingModel::GTELargeENV15,
        "gtelargeenv15q" => EmbeddingModel::GTELargeENV15Q,
        // -- Jina
        "jinaembeddingsv2basecode" => EmbeddingModel::JinaEmbeddingsV2BaseCode,
        "jinaembeddingsv2baseen" => EmbeddingModel::JinaEmbeddingsV2BaseEN,
        _ => {
            return Err(MindojoError::Other(format!(
                "Unknown fastembed model: '{name}'. Use one of: AllMiniLML6V2, BGESmallENV15, NomicEmbedTextV15, etc."
            )));
        }
    };
    Ok(model)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolve_model_canonical() {
        assert!(matches!(
            resolve_model("AllMiniLML6V2").unwrap(),
            EmbeddingModel::AllMiniLML6V2
        ));
    }

    #[test]
    fn test_resolve_model_lowered() {
        assert!(matches!(
            resolve_model("bge-small-en-v1.5").unwrap(),
            EmbeddingModel::BGESmallENV15
        ));
    }

    #[test]
    fn test_resolve_model_unknown() {
        assert!(resolve_model("nonexistent-model").is_err());
    }
}
