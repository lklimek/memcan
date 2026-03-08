//! Integration tests using mock providers and temp LanceDB directory.
//!
//! These tests use simple in-memory trait implementations instead of
//! HTTP mocking, since we no longer have an HTTP-based Ollama client.

use std::sync::Mutex;

use async_trait::async_trait;
use mindojo_core::error::{MindojoError, Result};
use mindojo_core::lancedb_store::LanceDbStore;
use mindojo_core::pipeline::{MEMORIES_TABLE, do_add_memory, extract_facts};
use mindojo_core::traits::{
    EmbeddingProvider, LlmMessage, LlmOptions, LlmProvider, Role, VectorPoint, VectorStore,
};
use tempfile::tempdir;

/// Embedding dimension used throughout tests.
const TEST_DIMS: usize = 4;

/// A fixed 4-dimensional test embedding vector.
fn test_vector() -> Vec<f32> {
    vec![0.1, 0.2, 0.3, 0.4]
}

/// A second vector that is slightly different for dedup testing.
fn test_vector_2() -> Vec<f32> {
    vec![0.11, 0.21, 0.31, 0.41]
}

/// Mock embedding provider that returns a fixed vector.
struct MockEmbedder {
    dims: usize,
    vector: Vec<f32>,
}

impl MockEmbedder {
    fn new() -> Self {
        Self {
            dims: TEST_DIMS,
            vector: test_vector(),
        }
    }
}

#[async_trait]
impl EmbeddingProvider for MockEmbedder {
    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        Ok(texts.iter().map(|_| self.vector.clone()).collect())
    }

    fn dimensions(&self) -> usize {
        self.dims
    }
}

/// Mock LLM provider with configurable responses.
struct MockLlm {
    responses: Mutex<Vec<String>>,
}

impl MockLlm {
    fn new(responses: Vec<String>) -> Self {
        Self {
            responses: Mutex::new(responses),
        }
    }
}

#[async_trait]
impl LlmProvider for MockLlm {
    async fn chat(
        &self,
        _model: &str,
        _messages: &[LlmMessage],
        _options: Option<LlmOptions>,
    ) -> Result<String> {
        let mut responses = self.responses.lock().unwrap();
        if responses.is_empty() {
            Ok(r#"{"facts": [], "events": [{"type": "NONE"}]}"#.to_string())
        } else {
            Ok(responses.remove(0))
        }
    }
}

/// Mock LLM that always returns an error (simulates 500 / connection failure).
struct FailingLlm;

#[async_trait]
impl LlmProvider for FailingLlm {
    async fn chat(
        &self,
        _model: &str,
        _messages: &[LlmMessage],
        _options: Option<LlmOptions>,
    ) -> Result<String> {
        Err(MindojoError::LlmChat {
            context: "mock server error".into(),
            detail: "HTTP 500 Internal Server Error".into(),
        })
    }
}

/// Mock LLM that returns malformed JSON (simulates garbled response).
struct MalformedJsonLlm;

#[async_trait]
impl LlmProvider for MalformedJsonLlm {
    async fn chat(
        &self,
        _model: &str,
        _messages: &[LlmMessage],
        _options: Option<LlmOptions>,
    ) -> Result<String> {
        Ok("this is not valid json at all {{{".to_string())
    }
}

/// Helper: open a LanceDbStore in a tempdir, returning both so the tempdir lives long enough.
async fn temp_store() -> (tempfile::TempDir, LanceDbStore) {
    let tmp = tempdir().expect("failed to create tempdir");
    let path = tmp.path().to_str().expect("tempdir path not utf8");
    let store = LanceDbStore::open(path).await.expect("open lancedb");
    (tmp, store)
}

// -- Test 1: mock embed -------------------------------------------------------

#[tokio::test]
async fn test_embed_mock() {
    let embedder = MockEmbedder::new();
    let result = embedder
        .embed(&["Hello world".to_string()])
        .await
        .expect("embed should succeed");

    assert_eq!(result.len(), 1);
    assert_eq!(result[0].len(), TEST_DIMS);
    assert!((result[0][0] - 0.1).abs() < f32::EPSILON);
    assert_eq!(embedder.dimensions(), TEST_DIMS);
}

// -- Test 2: mock chat --------------------------------------------------------

#[tokio::test]
async fn test_chat_mock() {
    let llm = MockLlm::new(vec!["Hello from mock!".to_string()]);
    let messages = vec![LlmMessage {
        role: Role::User,
        content: "Say hello".into(),
    }];

    let result = llm
        .chat("test-llm", &messages, None)
        .await
        .expect("chat should succeed");

    assert_eq!(result, "Hello from mock!");
}

// -- Test 3: full memory pipeline mock ----------------------------------------

#[tokio::test]
async fn test_memory_pipeline_mock() {
    let embedder = MockEmbedder::new();
    let llm = MockLlm::new(vec![
        // Fact extraction response
        r#"{"facts": ["Rust is great for systems programming"]}"#.to_string(),
        // Dedup response
        r#"{"events": [{"type": "ADD", "data": "Rust is great for systems programming"}]}"#
            .to_string(),
    ]);

    let (_tmp, store) = temp_store().await;
    store
        .ensure_table(MEMORIES_TABLE, TEST_DIMS)
        .await
        .expect("create table");

    let metadata = serde_json::json!({});
    let result = do_add_memory(
        "Rust is great for systems programming",
        "test-user",
        &metadata,
        true, // distill
        MEMORIES_TABLE,
        &store,
        &embedder,
        &llm,
        "test-llm",
        None,
    )
    .await
    .expect("do_add_memory should succeed");

    // Should return extracted facts.
    let facts = result.expect("should have facts when distilling");
    assert_eq!(facts.len(), 1);
    assert_eq!(facts[0], "Rust is great for systems programming");

    // Verify data was stored in LanceDB.
    let count = store
        .count(MEMORIES_TABLE, None)
        .await
        .expect("count should work");
    assert!(count >= 1, "Expected at least 1 stored memory, got {count}");
}

// -- Test 4: store and search ------------------------------------------------

#[tokio::test]
async fn test_store_and_search() {
    let (_tmp, store) = temp_store().await;
    store
        .ensure_table(MEMORIES_TABLE, TEST_DIMS)
        .await
        .expect("create table");

    let payload = serde_json::json!({
        "data": "LanceDB is a vector database",
        "user_id": "test-user",
        "hash": "abc123",
        "created_at": "2026-01-01T00:00:00Z",
        "updated_at": null
    });

    store
        .upsert(
            MEMORIES_TABLE,
            &[VectorPoint {
                id: "mem-001".into(),
                vector: test_vector(),
                payload: payload.clone(),
            }],
        )
        .await
        .expect("upsert");

    // Search with the same vector should find the memory.
    let results = store
        .search(MEMORIES_TABLE, &test_vector(), None, 10, 0)
        .await
        .expect("search");

    assert!(!results.is_empty(), "Search should return results");
    assert_eq!(results[0].id, "mem-001");
    assert_eq!(
        results[0].payload["data"].as_str().unwrap(),
        "LanceDB is a vector database"
    );
    assert!(results[0].score > 0.0, "Score should be positive");
}

// -- Test 5: dedup (similar memories) ----------------------------------------

#[tokio::test]
async fn test_dedup() {
    let embedder = MockEmbedder::new();
    let llm = MockLlm::new(vec![
        // First memory: extraction + dedup
        r#"{"facts": ["Test fact"], "events": [{"type": "ADD", "data": "Test fact"}]}"#.to_string(),
        r#"{"events": [{"type": "ADD", "data": "Test fact"}]}"#.to_string(),
        // Second memory: extraction + dedup
        r#"{"facts": ["Test fact 2"], "events": [{"type": "ADD", "data": "Test fact 2"}]}"#
            .to_string(),
        r#"{"events": [{"type": "ADD", "data": "Test fact 2"}]}"#.to_string(),
    ]);

    let (_tmp, store) = temp_store().await;
    store
        .ensure_table(MEMORIES_TABLE, TEST_DIMS)
        .await
        .expect("create table");

    let metadata = serde_json::json!({});

    // First memory.
    do_add_memory(
        "Rust has zero-cost abstractions",
        "test-user",
        &metadata,
        true,
        MEMORIES_TABLE,
        &store,
        &embedder,
        &llm,
        "test-llm",
        None,
    )
    .await
    .expect("first add");

    // Second memory (similar content).
    do_add_memory(
        "Rust provides zero-cost abstractions for safe systems programming",
        "test-user",
        &metadata,
        true,
        MEMORIES_TABLE,
        &store,
        &embedder,
        &llm,
        "test-llm",
        None,
    )
    .await
    .expect("second add");

    // Both were added (since our mock always returns ADD).
    let count = store.count(MEMORIES_TABLE, None).await.expect("count");
    assert!(
        count >= 2,
        "Expected at least 2 memories (both ADDed by mock), got {count}"
    );
}

// -- Test 6: config load -----------------------------------------------------

#[test]
fn test_config_load() {
    // Verify settings can be loaded and have sane defaults.
    let settings = mindojo_core::config::Settings::default();
    assert_eq!(settings.default_user_id, "global");
    assert!(settings.distill_memories);
    assert_eq!(settings.embed_dims, 1024);
    assert_eq!(settings.llm_model, "ollama::qwen3.5:4b");
    assert_eq!(settings.embed_model, "MultilingualE5Large");

    // Test that Settings::load() doesn't panic.
    let loaded = mindojo_core::config::Settings::load().expect("load should succeed");
    assert!(!loaded.llm_model.is_empty());
}

// -- Test 7: LanceDB CRUD ---------------------------------------------------

#[tokio::test]
async fn test_lancedb_crud() {
    let (_tmp, store) = temp_store().await;

    let table_name = "test-crud";

    // Create table.
    store
        .ensure_table(table_name, TEST_DIMS)
        .await
        .expect("create table");

    // Creating the same table again should be idempotent.
    store
        .ensure_table(table_name, TEST_DIMS)
        .await
        .expect("create table again");

    // Count should be 0 initially.
    let count = store.count(table_name, None).await.expect("count");
    assert_eq!(count, 0, "Table should be empty initially");

    // Upsert two points.
    let points = vec![
        VectorPoint {
            id: "id-1".into(),
            vector: test_vector(),
            payload: serde_json::json!({"data": "first record", "user_id": "u1"}),
        },
        VectorPoint {
            id: "id-2".into(),
            vector: test_vector_2(),
            payload: serde_json::json!({"data": "second record", "user_id": "u2"}),
        },
    ];
    store.upsert(table_name, &points).await.expect("upsert");

    // Count should be 2.
    let count = store.count(table_name, None).await.expect("count");
    assert_eq!(count, 2, "Should have 2 records");

    // Search should return results.
    let results = store
        .search(table_name, &test_vector(), None, 10, 0)
        .await
        .expect("search");
    assert!(!results.is_empty(), "Search should return results");
    // The closest result should be id-1 (same vector).
    assert_eq!(results[0].id, "id-1");

    // Get by ID.
    let got = store
        .get(table_name, &["id-2".to_string()])
        .await
        .expect("get");
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].id, "id-2");

    // Upsert same ID with updated payload (idempotent update).
    let updated_point = VectorPoint {
        id: "id-1".into(),
        vector: test_vector(),
        payload: serde_json::json!({"data": "updated first record", "user_id": "u1"}),
    };
    store
        .upsert(table_name, &[updated_point])
        .await
        .expect("upsert update");

    // Count should still be 2 (upsert replaced, not appended).
    let count = store.count(table_name, None).await.expect("count");
    assert_eq!(count, 2, "Upsert should replace, not duplicate");

    // Verify the update took effect.
    let got = store
        .get(table_name, &["id-1".to_string()])
        .await
        .expect("get updated");
    assert_eq!(got.len(), 1);
    assert_eq!(
        got[0].payload["data"].as_str().unwrap(),
        "updated first record"
    );

    // Delete by ID.
    store
        .delete(table_name, &["id-1".to_string()])
        .await
        .expect("delete");
    let count = store.count(table_name, None).await.expect("count");
    assert_eq!(count, 1, "Should have 1 record after delete");

    // Get deleted ID should return empty.
    let got = store
        .get(table_name, &["id-1".to_string()])
        .await
        .expect("get deleted");
    assert!(got.is_empty(), "Deleted record should not be found");

    // Scroll (list all).
    let all = store
        .scroll(table_name, None, 100, 0)
        .await
        .expect("scroll");
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].id, "id-2");
}

// -- Test 8: LLM error propagation ------------------------------------------

#[tokio::test]
async fn test_llm_error_propagates() {
    let llm = FailingLlm;
    let result = extract_facts("some content", &llm, "test-model", None).await;
    // extract_facts swallows LLM errors and returns Ok(None)
    assert!(result.is_ok());
    assert!(
        result.unwrap().is_none(),
        "LLM failure should yield None (graceful fallback)"
    );
}

// -- Test 9: malformed JSON from LLM ----------------------------------------

#[tokio::test]
async fn test_malformed_llm_json_handled_gracefully() {
    let llm = MalformedJsonLlm;
    let result = extract_facts("some content", &llm, "test-model", None).await;
    // Malformed JSON should be caught and return Ok(None).
    assert!(result.is_ok());
    assert!(
        result.unwrap().is_none(),
        "malformed JSON should yield None (graceful fallback)"
    );
}
