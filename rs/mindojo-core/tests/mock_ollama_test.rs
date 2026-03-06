//! Integration tests using a mock Ollama server and temp LanceDB directory.

use mindojo_core::lancedb_store::LanceDbStore;
use mindojo_core::ollama::OllamaClient;
use mindojo_core::pipeline::{MEMORIES_TABLE, do_add_memory};
use mindojo_core::traits::{EmbeddingProvider, LlmMessage, LlmProvider, VectorPoint, VectorStore};
use mockito::{Matcher, Server};
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

/// Helper: open a LanceDbStore in a tempdir, returning both so the tempdir lives long enough.
async fn temp_store() -> (tempfile::TempDir, LanceDbStore) {
    let tmp = tempdir().expect("failed to create tempdir");
    let path = tmp.path().to_str().expect("tempdir path not utf8");
    let store = LanceDbStore::open(path).await.expect("open lancedb");
    (tmp, store)
}

// -- Test 1: embed mock ------------------------------------------------------

#[tokio::test]
async fn test_embed_mock() {
    let mut server = Server::new_async().await;

    let mock = server
        .mock("POST", "/api/embed")
        .match_body(Matcher::PartialJsonString(
            r#"{"model":"test-embed"}"#.into(),
        ))
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(
            serde_json::json!({
                "model": "test-embed",
                "embeddings": [[0.1, 0.2, 0.3, 0.4]],
                "total_duration": 1000000
            })
            .to_string(),
        )
        .create_async()
        .await;

    let client = OllamaClient::new(server.url().as_str(), None, "test-embed", TEST_DIMS);

    let result = client
        .embed(&["Hello world".to_string()])
        .await
        .expect("embed should succeed");

    assert_eq!(result.len(), 1);
    assert_eq!(result[0].len(), TEST_DIMS);
    assert!((result[0][0] - 0.1).abs() < f32::EPSILON);
    assert_eq!(client.dimensions(), TEST_DIMS);

    mock.assert_async().await;
}

// -- Test 2: chat mock -------------------------------------------------------

#[tokio::test]
async fn test_chat_mock() {
    let mut server = Server::new_async().await;

    let mock = server
        .mock("POST", "/api/chat")
        .match_body(Matcher::PartialJsonString(r#"{"model":"test-llm"}"#.into()))
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(
            serde_json::json!({
                "model": "test-llm",
                "message": {"role": "assistant", "content": "Hello from mock!"},
                "done": true
            })
            .to_string(),
        )
        .create_async()
        .await;

    let client = OllamaClient::new(server.url().as_str(), None, "test-embed", TEST_DIMS);

    let messages = vec![LlmMessage {
        role: "user".into(),
        content: "Say hello".into(),
    }];

    let result = client
        .chat("test-llm", &messages, None)
        .await
        .expect("chat should succeed");

    assert_eq!(result, "Hello from mock!");

    mock.assert_async().await;
}

// -- Test 3: full memory pipeline mock ----------------------------------------

#[tokio::test]
async fn test_memory_pipeline_mock() {
    let mut server = Server::new_async().await;

    // Mock /api/embed -- will be called multiple times.
    let embed_mock = server
        .mock("POST", "/api/embed")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(
            serde_json::json!({
                "model": "test-embed",
                "embeddings": [[0.1, 0.2, 0.3, 0.4]],
                "total_duration": 1000000
            })
            .to_string(),
        )
        .expect_at_least(1)
        .create_async()
        .await;

    // Mock /api/chat -- mockito matches mocks in creation order.
    // Create fact extraction first (matches first call), then dedup (matches second).
    let fact_mock = server
        .mock("POST", "/api/chat")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(
            serde_json::json!({
                "model": "test-llm",
                "message": {
                    "role": "assistant",
                    "content": r#"{"facts": ["Rust is great for systems programming"]}"#
                },
                "done": true
            })
            .to_string(),
        )
        .expect(1)
        .create_async()
        .await;

    let dedup_mock = server
        .mock("POST", "/api/chat")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(
            serde_json::json!({
                "model": "test-llm",
                "message": {
                    "role": "assistant",
                    "content": r#"{"events": [{"type": "ADD", "data": "Rust is great for systems programming"}]}"#
                },
                "done": true
            })
            .to_string(),
        )
        .expect_at_least(1)
        .create_async()
        .await;

    // Set up LanceDB in a temp directory.
    let (_tmp, store) = temp_store().await;
    store
        .ensure_table(MEMORIES_TABLE, TEST_DIMS)
        .await
        .expect("create table");

    let client = OllamaClient::new(server.url().as_str(), None, "test-embed", TEST_DIMS);

    let metadata = serde_json::json!({});
    let result = do_add_memory(
        "Rust is great for systems programming",
        "test-user",
        &metadata,
        true, // distill
        MEMORIES_TABLE,
        &store,
        &client,
        &client,
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

    fact_mock.assert_async().await;
    dedup_mock.assert_async().await;
    embed_mock.assert_async().await;
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
        .search(MEMORIES_TABLE, &test_vector(), None, 10)
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
    let mut server = Server::new_async().await;

    // Mock embed to return consistent vectors.
    server
        .mock("POST", "/api/embed")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(
            serde_json::json!({
                "model": "test-embed",
                "embeddings": [[0.1, 0.2, 0.3, 0.4]],
                "total_duration": 1000000
            })
            .to_string(),
        )
        .expect_at_least(1)
        .create_async()
        .await;

    // Both fact extraction and dedup calls return consistent responses.
    // Mock responds to all /api/chat with a JSON containing both facts and events.
    // The pipeline parses what it expects from each call.
    // Since mockito matches in creation order: fact extraction first, then dedup.
    // But here both calls share the same mock, so we use a single response that
    // parses as both FactsResponse and UpdateResponse.
    server
        .mock("POST", "/api/chat")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(
            serde_json::json!({
                "model": "test-llm",
                "message": {
                    "role": "assistant",
                    "content": r#"{"facts": ["Test fact"], "events": [{"type": "ADD", "data": "Test fact"}]}"#
                },
                "done": true
            })
            .to_string(),
        )
        .expect_at_least(1)
        .create_async()
        .await;

    let (_tmp, store) = temp_store().await;
    store
        .ensure_table(MEMORIES_TABLE, TEST_DIMS)
        .await
        .expect("create table");

    let client = OllamaClient::new(server.url().as_str(), None, "test-embed", TEST_DIMS);
    let metadata = serde_json::json!({});

    // First memory.
    do_add_memory(
        "Rust has zero-cost abstractions",
        "test-user",
        &metadata,
        true,
        MEMORIES_TABLE,
        &store,
        &client,
        &client,
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
        &client,
        &client,
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
    assert_eq!(settings.ollama_url, "http://localhost:11434");
    assert_eq!(settings.default_user_id, "global");
    assert!(settings.distill_memories);
    assert_eq!(settings.embed_dims, 2560);
    assert_eq!(settings.llm_model, "qwen3.5:4b");
    assert_eq!(settings.embed_model, "qwen3-embedding:4b");

    // Test that Settings::load() doesn't panic.
    let loaded = mindojo_core::config::Settings::load();
    // ollama_url should be either the default or overridden by env.
    assert!(!loaded.ollama_url.is_empty());
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
        .search(table_name, &test_vector(), None, 10)
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
    let all = store.scroll(table_name, None, 100).await.expect("scroll");
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].id, "id-2");
}
