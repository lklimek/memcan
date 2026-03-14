use arrow_schema::Field;
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

/// Defines the Arrow schema and column extraction logic for a LanceDB table.
///
/// Every LanceDB table has three mandatory columns (`id`, `vector`, `payload`).
/// A `TableSchema` adds zero or more *filterable* columns whose values are
/// extracted from the JSON payload at upsert time so that LanceDB SQL WHERE
/// filters can reference them directly.
///
/// Implementations live in the consumer crate (memcan-server provides
/// `MemcanTableSchema`; penny will provide its own).
pub trait TableSchema: Send + Sync {
    /// Extra Arrow fields beyond the mandatory `id`, `vector`, `payload`.
    fn extra_fields(&self) -> Vec<Field>;

    /// Extract filterable column values from one payload, in the same order as
    /// [`extra_fields`](Self::extra_fields). Each entry becomes one Arrow
    /// `StringArray` cell (nullable).
    fn extract_columns(&self, payload: &serde_json::Value) -> Vec<Option<String>>;

    /// Column names that should be auto-added (as nullable STRING) when
    /// opening a table whose schema is missing them. Used for online
    /// migration of older tables.
    fn migration_columns(&self) -> Vec<String> {
        vec![]
    }
}

/// A [`TableSchema`] with no extra columns beyond `id`, `vector`, `payload`.
///
/// Suitable for consumers that only need basic vector search without
/// filterable metadata columns.
pub struct MinimalTableSchema;

impl TableSchema for MinimalTableSchema {
    fn extra_fields(&self) -> Vec<Field> {
        vec![]
    }

    fn extract_columns(&self, _payload: &serde_json::Value) -> Vec<Option<String>> {
        vec![]
    }
}

/// Abstraction over a vector database (LanceDB, Qdrant, etc.).
#[async_trait]
pub trait VectorStore: Send + Sync {
    /// Ensure a table (collection) exists with the given dimensionality.
    ///
    /// The `schema` parameter defines extra filterable columns and migration
    /// logic. Pass [`MinimalTableSchema`] for a bare `id + vector + payload`
    /// table, or a domain-specific implementation for richer filtering.
    ///
    /// Idempotent: calling on an existing table is a no-op.
    async fn ensure_table(&self, name: &str, dims: usize, schema: &dyn TableSchema) -> Result<()>;

    /// Insert or update points. Existing IDs are overwritten.
    ///
    /// The `schema` parameter must match the one used in
    /// [`ensure_table`](Self::ensure_table).
    async fn upsert(
        &self,
        table: &str,
        points: &[VectorPoint],
        schema: &dyn TableSchema,
    ) -> Result<()>;

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

    /// Query the model's context window size in tokens.
    /// Returns None if the provider doesn't support this query.
    async fn context_window(&self, model: &str) -> Option<usize>;

    /// Provider-specific initialization (e.g. model availability check).
    /// Default is a no-op.
    async fn init(&self) -> Result<()> {
        Ok(())
    }
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

    #[test]
    fn test_minimal_table_schema() {
        let schema = MinimalTableSchema;
        assert!(schema.extra_fields().is_empty());
        let payload = serde_json::json!({"key": "value"});
        assert!(schema.extract_columns(&payload).is_empty());
        assert!(schema.migration_columns().is_empty());
    }
}
