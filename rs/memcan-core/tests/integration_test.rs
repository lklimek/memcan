//! Integration tests using mock providers and temp LanceDB directory.
//!
//! These tests use simple in-memory trait implementations instead of
//! HTTP mocking, since we no longer have an HTTP-based Ollama client.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use memcan_core::error::{MemcanError, Result};
use memcan_core::lancedb_store::LanceDbStore;
use memcan_core::pipeline::{MEMORIES_TABLE, Pipeline, PipelineStep};
use memcan_core::traits::{
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

    async fn context_window(&self, _model: &str) -> Option<usize> {
        None
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
        Err(MemcanError::LlmChat {
            context: "mock server error".into(),
            detail: "HTTP 500 Internal Server Error".into(),
        })
    }

    async fn context_window(&self, _model: &str) -> Option<usize> {
        None
    }
}

/// Mock embedding provider that always fails.
struct FailingEmbedder;

#[async_trait]
impl EmbeddingProvider for FailingEmbedder {
    async fn embed(&self, _texts: &[String]) -> Result<Vec<Vec<f32>>> {
        Err(MemcanError::Embedding {
            context: "mock embed failure".into(),
            detail: "simulated error".into(),
        })
    }

    fn dimensions(&self) -> usize {
        TEST_DIMS
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

    async fn context_window(&self, _model: &str) -> Option<usize> {
        None
    }
}

/// Helper: open a LanceDbStore in a tempdir, returning both so the tempdir lives long enough.
async fn temp_store() -> (tempfile::TempDir, LanceDbStore) {
    let tmp = tempdir().expect("failed to create tempdir");
    let path = tmp.path().to_str().expect("tempdir path not utf8");
    let store = LanceDbStore::open(path).await.expect("open lancedb");
    (tmp, store)
}

/// Helper: create a Pipeline with the given mocks.
fn make_pipeline(
    store: Arc<dyn VectorStore>,
    embedder: Arc<dyn EmbeddingProvider>,
    llm: Arc<dyn LlmProvider>,
    distill: bool,
) -> Pipeline {
    Pipeline::new(store, embedder, llm, "test-llm", MEMORIES_TABLE, distill)
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
        r#"{"facts": ["Rust is great for systems programming"]}"#.to_string(),
        r#"{"events": [{"type": "ADD", "data": "Rust is great for systems programming"}]}"#
            .to_string(),
    ]);

    let (_tmp, store) = temp_store().await;
    store
        .ensure_table(MEMORIES_TABLE, TEST_DIMS)
        .await
        .expect("create table");

    let pipeline = make_pipeline(
        Arc::new(store) as Arc<dyn VectorStore>,
        Arc::new(embedder),
        Arc::new(llm),
        true,
    );
    let progress = pipeline.progress();

    let metadata = serde_json::json!({});
    let result = pipeline
        .add_memory(
            "Rust is great for systems programming",
            "test-user",
            &metadata,
        )
        .await
        .expect("add_memory should succeed");
    pipeline.complete();

    let facts = result.facts.expect("should have facts when distilling");
    assert_eq!(facts.len(), 1);
    assert_eq!(facts[0], "Rust is great for systems programming");

    let p = progress.lock().unwrap();
    assert_eq!(p.step, PipelineStep::Completed);
    assert!(p.warnings.is_empty());
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
        r#"{"facts": ["Test fact"], "events": [{"type": "ADD", "data": "Test fact"}]}"#.to_string(),
        r#"{"events": [{"type": "ADD", "data": "Test fact"}]}"#.to_string(),
        r#"{"facts": ["Test fact 2"], "events": [{"type": "ADD", "data": "Test fact 2"}]}"#
            .to_string(),
        r#"{"events": [{"type": "ADD", "data": "Test fact 2"}]}"#.to_string(),
    ]);

    let (_tmp, store) = temp_store().await;
    store
        .ensure_table(MEMORIES_TABLE, TEST_DIMS)
        .await
        .expect("create table");

    let store: Arc<dyn VectorStore> = Arc::new(store);
    let embedder: Arc<dyn EmbeddingProvider> = Arc::new(embedder);
    let llm: Arc<dyn LlmProvider> = Arc::new(llm);
    let metadata = serde_json::json!({});

    let pipeline1 = make_pipeline(
        Arc::clone(&store),
        Arc::clone(&embedder),
        Arc::clone(&llm),
        true,
    );
    pipeline1
        .add_memory("Rust has zero-cost abstractions", "test-user", &metadata)
        .await
        .expect("first add");
    pipeline1.complete();

    let pipeline2 = make_pipeline(
        Arc::clone(&store),
        Arc::clone(&embedder),
        Arc::clone(&llm),
        true,
    );
    pipeline2
        .add_memory(
            "Rust provides zero-cost abstractions for safe systems programming",
            "test-user",
            &metadata,
        )
        .await
        .expect("second add");
    pipeline2.complete();

    let count = store.count(MEMORIES_TABLE, None).await.expect("count");
    assert!(
        count >= 2,
        "Expected at least 2 memories (both ADDed by mock), got {count}"
    );
}

// -- Test 6: config load -----------------------------------------------------

#[test]
fn test_config_load() {
    let settings = memcan_core::config::Settings::default();
    assert_eq!(settings.default_user_id, "global");
    assert!(settings.distill_memories);
    assert_eq!(settings.embed_dims, 1024);
    assert_eq!(settings.llm_model, "ollama::qwen3.5:4b");
    assert_eq!(settings.embed_model, "MultilingualE5Large");

    let loaded = memcan_core::config::Settings::load().expect("load should succeed");
    assert!(!loaded.llm_model.is_empty());
}

// -- Test 7: LanceDB CRUD ---------------------------------------------------

#[tokio::test]
async fn test_lancedb_crud() {
    let (_tmp, store) = temp_store().await;

    let table_name = "test-crud";

    store
        .ensure_table(table_name, TEST_DIMS)
        .await
        .expect("create table");

    store
        .ensure_table(table_name, TEST_DIMS)
        .await
        .expect("create table again");

    let count = store.count(table_name, None).await.expect("count");
    assert_eq!(count, 0, "Table should be empty initially");

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

    let count = store.count(table_name, None).await.expect("count");
    assert_eq!(count, 2, "Should have 2 records");

    let results = store
        .search(table_name, &test_vector(), None, 10, 0)
        .await
        .expect("search");
    assert!(!results.is_empty(), "Search should return results");
    assert_eq!(results[0].id, "id-1");

    let got = store
        .get(table_name, &["id-2".to_string()])
        .await
        .expect("get");
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].id, "id-2");

    let updated_point = VectorPoint {
        id: "id-1".into(),
        vector: test_vector(),
        payload: serde_json::json!({"data": "updated first record", "user_id": "u1"}),
    };
    store
        .upsert(table_name, &[updated_point])
        .await
        .expect("upsert update");

    let count = store.count(table_name, None).await.expect("count");
    assert_eq!(count, 2, "Upsert should replace, not duplicate");

    let got = store
        .get(table_name, &["id-1".to_string()])
        .await
        .expect("get updated");
    assert_eq!(got.len(), 1);
    assert_eq!(
        got[0].payload["data"].as_str().unwrap(),
        "updated first record"
    );

    store
        .delete(table_name, &["id-1".to_string()])
        .await
        .expect("delete");
    let count = store.count(table_name, None).await.expect("count");
    assert_eq!(count, 1, "Should have 1 record after delete");

    let got = store
        .get(table_name, &["id-1".to_string()])
        .await
        .expect("get deleted");
    assert!(got.is_empty(), "Deleted record should not be found");

    let all = store
        .scroll(table_name, None, 100, 0)
        .await
        .expect("scroll");
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].id, "id-2");
}

// -- Test 8: LLM error falls back to raw store via Pipeline ------------------

#[tokio::test]
async fn test_pipeline_falls_back_on_llm_error() {
    let embedder = MockEmbedder::new();
    let llm = FailingLlm;

    let (_tmp, store) = temp_store().await;
    store
        .ensure_table(MEMORIES_TABLE, TEST_DIMS)
        .await
        .expect("create table");

    let pipeline = make_pipeline(
        Arc::new(store) as _,
        Arc::new(embedder),
        Arc::new(llm),
        true,
    );
    let progress = pipeline.progress();

    let metadata = serde_json::json!({});
    let result = pipeline
        .add_memory("important lesson learned", "test-user", &metadata)
        .await;

    assert!(result.is_ok(), "pipeline should fall back, not fail");
    let add_result = result.unwrap();
    assert!(
        add_result.facts.is_none(),
        "should return None (no extracted facts)"
    );

    pipeline.complete();
    let p = progress.lock().unwrap();
    assert_eq!(p.step, PipelineStep::CompletedDegraded);
    assert!(
        !p.warnings.is_empty(),
        "LLM failure should produce a warning"
    );
    assert!(
        p.warnings[0].contains("fact extraction failed"),
        "warning should mention fact extraction failure, got: {}",
        p.warnings[0]
    );
}

// -- Test 9: malformed JSON from LLM ----------------------------------------

#[tokio::test]
async fn test_malformed_llm_json_handled_gracefully() {
    let llm = MalformedJsonLlm;
    let embedder = MockEmbedder::new();

    let (_tmp, store) = temp_store().await;
    store
        .ensure_table(MEMORIES_TABLE, TEST_DIMS)
        .await
        .expect("create table");

    let pipeline = make_pipeline(
        Arc::new(store) as _,
        Arc::new(embedder),
        Arc::new(llm),
        true,
    );
    let progress = pipeline.progress();
    let metadata = serde_json::json!({});

    let result = pipeline
        .add_memory("some memory content", "test-user", &metadata)
        .await
        .expect("pipeline should succeed with fallback");
    pipeline.complete();

    assert!(result.facts.is_none(), "no facts when JSON is unparseable");
    let p = progress.lock().unwrap();
    assert_eq!(p.step, PipelineStep::CompletedDegraded);
    assert!(
        !p.warnings.is_empty(),
        "unparseable JSON should produce a warning"
    );
    assert!(
        p.warnings[0].contains("unparseable"),
        "warning should mention unparseable, got: {}",
        p.warnings[0]
    );
}

// -- Test 10: non-LLM errors propagate through Pipeline ----------------------

#[tokio::test]
async fn test_non_llm_error_propagates() {
    let embedder = FailingEmbedder;
    let llm = FailingLlm;

    let (_tmp, store) = temp_store().await;
    store
        .ensure_table(MEMORIES_TABLE, TEST_DIMS)
        .await
        .expect("create table");

    let pipeline = make_pipeline(
        Arc::new(store) as _,
        Arc::new(embedder),
        Arc::new(llm),
        true,
    );
    let metadata = serde_json::json!({});

    let result = pipeline
        .add_memory("some content", "test-user", &metadata)
        .await;

    assert!(result.is_err(), "embedding error should propagate");
    assert!(
        !result.unwrap_err().is_llm_error(),
        "should not be an LLM error"
    );
}

// -- Test 11: distill=false has no warnings ----------------------------------

#[tokio::test]
async fn test_pipeline_no_distill_no_warnings() {
    let embedder = MockEmbedder::new();
    let llm = FailingLlm; // should not be called

    let (_tmp, store) = temp_store().await;
    store
        .ensure_table(MEMORIES_TABLE, TEST_DIMS)
        .await
        .expect("create table");

    let pipeline = make_pipeline(
        Arc::new(store) as _,
        Arc::new(embedder),
        Arc::new(llm),
        false,
    );
    let progress = pipeline.progress();
    let metadata = serde_json::json!({});

    let result = pipeline
        .add_memory("raw memory content", "test-user", &metadata)
        .await
        .expect("pipeline should succeed");
    pipeline.complete();

    assert!(result.facts.is_none(), "no facts when distill=false");
    let p = progress.lock().unwrap();
    assert_eq!(p.step, PipelineStep::Completed);
    assert!(
        p.warnings.is_empty(),
        "distill=false should have no warnings"
    );
}

// -- Test 12: dedup LLM failure produces warning -----------------------------

/// Mock LLM where first call (extraction) succeeds but second call (dedup) fails.
struct ExtractOkDedupFailLlm;

#[async_trait]
impl LlmProvider for ExtractOkDedupFailLlm {
    async fn chat(
        &self,
        _model: &str,
        messages: &[LlmMessage],
        _options: Option<LlmOptions>,
    ) -> Result<String> {
        let has_system = messages.iter().any(|m| m.role == Role::System);
        if has_system {
            Ok(r#"{"facts": ["extracted fact"]}"#.to_string())
        } else {
            Err(MemcanError::LlmChat {
                context: "dedup".into(),
                detail: "simulated dedup failure".into(),
            })
        }
    }

    async fn context_window(&self, _model: &str) -> Option<usize> {
        None
    }
}

#[tokio::test]
async fn test_pipeline_dedup_failure_warning() {
    let embedder = MockEmbedder::new();
    let llm = ExtractOkDedupFailLlm;

    let (_tmp, store) = temp_store().await;
    store
        .ensure_table(MEMORIES_TABLE, TEST_DIMS)
        .await
        .expect("create table");

    let pipeline = make_pipeline(
        Arc::new(store) as _,
        Arc::new(embedder),
        Arc::new(llm),
        true,
    );
    let progress = pipeline.progress();
    let metadata = serde_json::json!({});

    let result = pipeline
        .add_memory("some memory content", "test-user", &metadata)
        .await
        .expect("pipeline should succeed with dedup fallback");
    pipeline.complete();

    assert!(result.facts.is_some(), "facts should be present");
    let p = progress.lock().unwrap();
    assert_eq!(p.step, PipelineStep::CompletedDegraded);
    assert!(
        !p.warnings.is_empty(),
        "dedup failure should produce a warning"
    );
    assert!(
        p.warnings[0].contains("Dedup LLM failed"),
        "warning should mention dedup failure, got: {}",
        p.warnings[0]
    );
}

// -- Test 13: clean pipeline path has no warnings ----------------------------

#[tokio::test]
async fn test_pipeline_clean_path_no_warnings() {
    let embedder = MockEmbedder::new();
    let llm = MockLlm::new(vec![
        r#"{"facts": ["clean fact"]}"#.to_string(),
        r#"{"events": [{"type": "ADD", "data": "clean fact"}]}"#.to_string(),
    ]);

    let (_tmp, store) = temp_store().await;
    store
        .ensure_table(MEMORIES_TABLE, TEST_DIMS)
        .await
        .expect("create table");

    let pipeline = make_pipeline(
        Arc::new(store) as _,
        Arc::new(embedder),
        Arc::new(llm),
        true,
    );
    let progress = pipeline.progress();
    let metadata = serde_json::json!({});

    let result = pipeline
        .add_memory("clean memory content", "test-user", &metadata)
        .await
        .expect("pipeline should succeed");
    pipeline.complete();

    let facts = result.facts.expect("should have facts");
    assert_eq!(facts, vec!["clean fact"]);
    let p = progress.lock().unwrap();
    assert_eq!(p.step, PipelineStep::Completed);
    assert!(
        p.warnings.is_empty(),
        "clean path should produce no warnings"
    );
}

// -- Test 14: progress shows final state after successful distill path --------

#[tokio::test]
async fn test_progress_final_state_distill() {
    let embedder = MockEmbedder::new();
    let llm = MockLlm::new(vec![
        r#"{"facts": ["tracked fact"]}"#.to_string(),
        r#"{"events": [{"type": "ADD", "data": "tracked fact"}]}"#.to_string(),
    ]);

    let (_tmp, store) = temp_store().await;
    store
        .ensure_table(MEMORIES_TABLE, TEST_DIMS)
        .await
        .expect("create table");

    let pipeline = make_pipeline(
        Arc::new(store) as _,
        Arc::new(embedder),
        Arc::new(llm),
        true,
    );
    let progress = pipeline.progress();
    let metadata = serde_json::json!({});

    pipeline
        .add_memory("tracked memory content", "test-user", &metadata)
        .await
        .expect("pipeline should succeed");
    pipeline.complete();

    let p = progress.lock().unwrap();
    assert_eq!(p.step, PipelineStep::Completed);
    assert!(p.warnings.is_empty());
    assert!(p.completed_at.is_some());
    assert!(p.error.is_none());
}

// -- Test 15: progress shows failed state on error ---------------------------

#[tokio::test]
async fn test_progress_failed_state() {
    let embedder = FailingEmbedder;
    let llm = FailingLlm;

    let (_tmp, store) = temp_store().await;
    store
        .ensure_table(MEMORIES_TABLE, TEST_DIMS)
        .await
        .expect("create table");

    let pipeline = make_pipeline(
        Arc::new(store) as _,
        Arc::new(embedder),
        Arc::new(llm),
        true,
    );
    let progress = pipeline.progress();
    let metadata = serde_json::json!({});

    let result = pipeline
        .add_memory("will fail", "test-user", &metadata)
        .await;
    assert!(result.is_err());
    pipeline.fail(result.unwrap_err());

    let p = progress.lock().unwrap();
    assert_eq!(p.step, PipelineStep::Failed);
    assert!(p.error.is_some());
    assert!(p.completed_at.is_some());
}

// -- Test 16: progress shows degraded state on LLM fallback ------------------

#[tokio::test]
async fn test_progress_degraded_on_llm_fallback() {
    let embedder = MockEmbedder::new();
    let llm = FailingLlm;

    let (_tmp, store) = temp_store().await;
    store
        .ensure_table(MEMORIES_TABLE, TEST_DIMS)
        .await
        .expect("create table");

    let pipeline = make_pipeline(
        Arc::new(store) as _,
        Arc::new(embedder),
        Arc::new(llm),
        true,
    );
    let progress = pipeline.progress();
    let metadata = serde_json::json!({});

    pipeline
        .add_memory("fallback memory content", "test-user", &metadata)
        .await
        .expect("pipeline should fall back");
    pipeline.complete();

    let p = progress.lock().unwrap();
    assert_eq!(p.step, PipelineStep::CompletedDegraded);
    assert!(!p.warnings.is_empty());
    assert!(p.completed_at.is_some());
}

// -- Test 17: no-distill path progress shows completed -----------------------

#[tokio::test]
async fn test_progress_no_distill() {
    let embedder = MockEmbedder::new();
    let llm = FailingLlm; // should not be called

    let (_tmp, store) = temp_store().await;
    store
        .ensure_table(MEMORIES_TABLE, TEST_DIMS)
        .await
        .expect("create table");

    let pipeline = make_pipeline(
        Arc::new(store) as _,
        Arc::new(embedder),
        Arc::new(llm),
        false,
    );
    let progress = pipeline.progress();
    let metadata = serde_json::json!({});

    pipeline
        .add_memory("raw content", "test-user", &metadata)
        .await
        .expect("pipeline should succeed");
    pipeline.complete();

    let p = progress.lock().unwrap();
    assert_eq!(p.step, PipelineStep::Completed);
    assert!(p.warnings.is_empty());
    assert!(p.completed_at.is_some());
}
