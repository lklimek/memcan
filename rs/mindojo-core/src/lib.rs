//! Core library for MindOJO persistent memory.
//!
//! Provides storage-agnostic abstractions for vector search, LLM chat, and
//! embedding via the traits in [`traits`]. The [`pipeline`] module implements
//! the fact-extraction and deduplication workflow. [`lancedb_store`] is the
//! default [`traits::VectorStore`] backend.
//!
//! Key types:
//! - [`traits::VectorStore`] / [`lancedb_store::LanceDbStore`] -- vector DB
//! - [`traits::EmbeddingProvider`] / [`embed`] -- text embeddings
//! - [`traits::LlmProvider`] / [`llm`] -- LLM chat
//! - [`pipeline::do_add_memory`] -- end-to-end memory storage
//! - [`config::Settings`] -- runtime configuration

pub mod config;
pub mod embed;
pub mod error;
pub mod lancedb_store;
pub mod llm;
pub mod pipeline;
pub mod prompts;
pub mod traits;
