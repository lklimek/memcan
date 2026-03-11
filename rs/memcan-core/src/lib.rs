//! Core library for MemCan persistent memory.
//!
//! Provides storage-agnostic abstractions for vector search, LLM chat, and
//! embedding via the traits in [`traits`]. The [`pipeline`] module implements
//! the fact-extraction and deduplication workflow. [`lancedb_store`] is the
//! default [`traits::VectorStore`] backend.
//!
//! Key types:
//! - [`traits::VectorStore`] / [`lancedb_store::LanceDbStore`] -- vector DB
//! - [`traits::EmbeddingProvider`] / [`embed`] -- text embeddings
//! - [`traits::LlmProvider`] -- LLM chat (see `llm_ollama_rs` or `llm` modules)
//! - [`pipeline::Pipeline`] -- end-to-end memory storage pipeline
//! - [`config::Settings`] -- runtime configuration

pub mod config;
pub mod embed;
pub mod error;
pub mod health;
pub mod indexing;
pub mod init;
pub mod lancedb_store;
#[cfg(feature = "genai-llm")]
pub mod llm;
#[cfg(feature = "ollama-rs-llm")]
pub mod llm_ollama_rs;
pub mod ollama;
pub mod pipeline;
pub mod prompts;
pub mod search;
pub mod traits;

#[cfg(not(any(feature = "ollama-rs-llm", feature = "genai-llm")))]
compile_error!("At least one LLM feature must be enabled: ollama-rs-llm or genai-llm");
