use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// A single result returned from a vector store search or retrieval.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResult {
    pub id: String,
    pub score: f32,
    pub payload: serde_json::Value,
}

/// A point to upsert into a vector store.
#[derive(Debug, Clone)]
pub struct VectorPoint {
    pub id: String,
    pub vector: Vec<f32>,
    pub payload: serde_json::Value,
}

/// Abstraction over a vector database (LanceDB, Qdrant, etc.).
#[async_trait]
pub trait VectorStore: Send + Sync {
    /// Ensure a table (collection) exists with the given dimensionality.
    async fn ensure_table(&self, name: &str, dims: usize) -> anyhow::Result<()>;

    /// Insert or update points. Existing IDs are overwritten.
    async fn upsert(&self, table: &str, points: &[VectorPoint]) -> anyhow::Result<()>;

    /// Nearest-neighbor search with optional SQL WHERE filter.
    async fn search(
        &self,
        table: &str,
        vector: &[f32],
        filter: Option<&str>,
        limit: usize,
    ) -> anyhow::Result<Vec<SearchResult>>;

    /// List records with optional filter (no vector search).
    async fn scroll(
        &self,
        table: &str,
        filter: Option<&str>,
        limit: usize,
    ) -> anyhow::Result<Vec<SearchResult>>;

    /// Count records with optional filter.
    async fn count(&self, table: &str, filter: Option<&str>) -> anyhow::Result<usize>;

    /// Delete records by ID.
    async fn delete(&self, table: &str, ids: &[String]) -> anyhow::Result<()>;

    /// Delete records matching a SQL filter. Returns number deleted.
    async fn delete_by_filter(&self, table: &str, filter: &str) -> anyhow::Result<usize>;

    /// Retrieve specific records by their IDs.
    async fn get(&self, table: &str, ids: &[String]) -> anyhow::Result<Vec<SearchResult>>;
}

/// Abstraction over an embedding model provider.
#[async_trait]
pub trait EmbeddingProvider: Send + Sync {
    /// Embed one or more texts into vectors.
    async fn embed(&self, texts: &[String]) -> anyhow::Result<Vec<Vec<f32>>>;

    /// Return the dimensionality of the embeddings.
    fn dimensions(&self) -> usize;
}

/// Abstraction over an LLM chat provider.
#[async_trait]
pub trait LlmProvider: Send + Sync {
    /// Send a chat request and return the assistant's reply text.
    async fn chat(
        &self,
        model: &str,
        messages: &[LlmMessage],
        options: Option<LlmOptions>,
    ) -> anyhow::Result<String>;
}

/// A single message in an LLM conversation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmMessage {
    pub role: String,
    pub content: String,
}

/// Options for LLM chat requests.
#[derive(Debug, Clone, Default)]
pub struct LlmOptions {
    /// Request JSON-formatted output.
    pub format_json: bool,
    /// Sampling temperature.
    pub temperature: Option<f32>,
    /// Maximum tokens in the response.
    pub max_tokens: Option<u32>,
    /// Enable thinking/reasoning mode.
    pub think: Option<bool>,
}
