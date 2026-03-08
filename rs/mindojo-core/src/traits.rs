use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::error::Result;

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
    ///
    /// Idempotent: calling on an existing table is a no-op.
    async fn ensure_table(&self, name: &str, dims: usize) -> Result<()>;

    /// Insert or update points. Existing IDs are overwritten.
    async fn upsert(&self, table: &str, points: &[VectorPoint]) -> Result<()>;

    /// Nearest-neighbor search returning up to `limit` results.
    ///
    /// Optionally filters results with a SQL WHERE clause.
    async fn search(
        &self,
        table: &str,
        vector: &[f32],
        filter: Option<&str>,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<SearchResult>>;

    /// List records with optional SQL filter (no vector search).
    async fn scroll(
        &self,
        table: &str,
        filter: Option<&str>,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<SearchResult>>;

    /// Count records matching an optional SQL filter.
    async fn count(&self, table: &str, filter: Option<&str>) -> Result<usize>;

    /// Delete records by their IDs.
    async fn delete(&self, table: &str, ids: &[String]) -> Result<()>;

    /// Delete all records matching a SQL filter. Returns number deleted.
    async fn delete_by_filter(&self, table: &str, filter: &str) -> Result<usize>;

    /// Retrieve specific records by their IDs.
    async fn get(&self, table: &str, ids: &[String]) -> Result<Vec<SearchResult>>;
}

/// Abstraction over an embedding model provider.
#[async_trait]
pub trait EmbeddingProvider: Send + Sync {
    /// Embed one or more texts into dense float vectors.
    ///
    /// Returns one vector per input text, all with the same dimensionality.
    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>>;

    /// Return the dimensionality of vectors produced by [`Self::embed`].
    fn dimensions(&self) -> usize;
}

/// Abstraction over an LLM chat provider.
#[async_trait]
pub trait LlmProvider: Send + Sync {
    /// Send a chat conversation and return the assistant's reply text.
    ///
    /// `model` selects the backend model (format is provider-specific).
    async fn chat(
        &self,
        model: &str,
        messages: &[LlmMessage],
        options: Option<LlmOptions>,
    ) -> Result<String>;
}

/// Role of a participant in an LLM conversation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
}

impl std::fmt::Display for Role {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::System => f.write_str("system"),
            Self::User => f.write_str("user"),
            Self::Assistant => f.write_str("assistant"),
        }
    }
}

/// A single message in an LLM conversation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmMessage {
    pub role: Role,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_role_serde_roundtrip() {
        let msg = LlmMessage {
            role: Role::System,
            content: "hello".into(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains(r#""role":"system""#));
        let parsed: LlmMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.role, Role::System);
    }

    #[test]
    fn test_role_display() {
        assert_eq!(Role::System.to_string(), "system");
        assert_eq!(Role::User.to_string(), "user");
        assert_eq!(Role::Assistant.to_string(), "assistant");
    }
}
