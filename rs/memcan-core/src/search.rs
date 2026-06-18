//! Unified search across all collections (memories, standards, code).
//!
//! Embeds the query once, searches each requested collection in parallel,
//! and merges results by score descending.

use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::error::Result;
use crate::pipeline::{CODE_TABLE, MEMORIES_TABLE, STANDARDS_TABLE};
use crate::query::{resolve_user_id, sanitize_eq, sanitize_like};
use crate::todo::TODOS_TABLE;
use crate::traits::{EmbeddingProvider, SearchResult, VectorStore};

const DEFAULT_LIMIT: u32 = 5;
const MAX_LIMIT: u32 = 100;
/// Maximum characters returned in `data`; content beyond this is replaced with an ellipsis.
const MAX_DATA_LEN: usize = 500;

const ALL_COLLECTIONS: &[&str] = &["memories", "standards", "code", "todos"];

/// Parameters for a unified cross-collection search.
#[derive(Debug, Clone, Deserialize)]
pub struct UnifiedSearchParams {
    pub query: String,
    /// Which collections to search. None = all.
    pub collections: Option<Vec<String>>,
    pub project: Option<String>,
    pub user_id: Option<String>,
    /// Total result limit across all collections (merged by relevance score). Default 5, max 100.
    pub limit: Option<u32>,
    // Standards filters
    pub standard_type: Option<String>,
    pub standard_id: Option<String>,
    // Code filters
    pub tech_stack: Option<String>,
    pub file_path: Option<String>,
}

/// A single result from the unified search.
#[derive(Debug, Clone, Serialize)]
pub struct UnifiedSearchResult {
    pub collection: String,
    pub id: String,
    pub score: f32,
    pub data: String,
    pub metadata: serde_json::Value,
}

fn table_for_collection(name: &str) -> Option<&'static str> {
    match name {
        "memories" => Some(MEMORIES_TABLE),
        "standards" => Some(STANDARDS_TABLE),
        "code" => Some(CODE_TABLE),
        "todos" => Some(TODOS_TABLE),
        _ => None,
    }
}

fn build_todos_filter(params: &UnifiedSearchParams) -> Option<String> {
    params.project.as_ref().map(|p| {
        let safe = sanitize_eq(p);
        format!("project = '{safe}'")
    })
}

fn build_memories_filter(params: &UnifiedSearchParams, default_user_id: &str) -> String {
    let uid = resolve_user_id(&params.project, &params.user_id, default_user_id);
    let safe = sanitize_eq(&uid);
    format!("user_id = '{safe}'")
}

fn build_standards_filter(params: &UnifiedSearchParams) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    if let Some(ref v) = params.standard_type {
        let safe = sanitize_eq(v);
        parts.push(format!("standard_type = '{safe}'"));
    }
    if let Some(ref v) = params.standard_id {
        let safe = sanitize_eq(v);
        parts.push(format!("standard_id = '{safe}'"));
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" AND "))
    }
}

fn build_code_filter(params: &UnifiedSearchParams) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    if let Some(ref p) = params.project {
        let safe = sanitize_eq(p);
        parts.push(format!("project = '{safe}'"));
    }
    if let Some(ref ts) = params.tech_stack {
        let safe = sanitize_eq(ts);
        parts.push(format!("tech_stack = '{safe}'"));
    }
    if let Some(ref fp) = params.file_path {
        let safe = sanitize_like(fp);
        parts.push(format!("file_path LIKE '%{safe}%'"));
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" AND "))
    }
}

fn to_unified(collection: &str, results: Vec<SearchResult>) -> Vec<UnifiedSearchResult> {
    results
        .into_iter()
        .map(|r| {
            let raw = r
                .payload
                .get("data")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            // Char-safe truncation: multilingual content must not be byte-sliced.
            // Single pass: char_indices().nth(MAX_DATA_LEN) locates the cut point without counting all chars first.
            let data = match raw.char_indices().nth(MAX_DATA_LEN) {
                Some((idx, _)) => {
                    let mut s = raw[..idx].to_string();
                    s.push('…');
                    s
                }
                None => raw,
            };
            // Strip the "data" key from metadata to avoid duplicating it alongside the top-level field.
            let mut metadata = r.payload;
            if let Some(obj) = metadata.as_object_mut() {
                obj.remove("data");
            }
            UnifiedSearchResult {
                collection: collection.to_string(),
                id: r.id,
                score: r.score,
                data,
                metadata,
            }
        })
        .collect()
}

/// Search across multiple collections in parallel, merging results by score.
pub async fn unified_search(
    params: &UnifiedSearchParams,
    store: &dyn VectorStore,
    embedder: &dyn EmbeddingProvider,
    default_user_id: &str,
) -> Result<Vec<UnifiedSearchResult>> {
    let limit = params.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT) as usize;

    let collections: Vec<&str> = match &params.collections {
        Some(cols) => cols
            .iter()
            .filter_map(|c| {
                let c = c.as_str();
                if ALL_COLLECTIONS.contains(&c) {
                    Some(c)
                } else {
                    warn!(collection = c, "Unknown collection, skipping");
                    None
                }
            })
            .collect(),
        None => ALL_COLLECTIONS.to_vec(),
    };

    if collections.is_empty() {
        return Ok(vec![]);
    }

    let vectors = embedder.embed(std::slice::from_ref(&params.query)).await?;
    let vector = &vectors[0];

    let search_memories = collections.contains(&"memories");
    let search_standards = collections.contains(&"standards");
    let search_code = collections.contains(&"code");
    let search_todos = collections.contains(&"todos");

    let mem_filter = if search_memories {
        Some(build_memories_filter(params, default_user_id))
    } else {
        None
    };
    let std_filter = if search_standards {
        build_standards_filter(params)
    } else {
        None
    };
    let code_filter = if search_code {
        build_code_filter(params)
    } else {
        None
    };
    let todos_filter = if search_todos {
        build_todos_filter(params)
    } else {
        None
    };

    let mem_fut = async {
        if !search_memories {
            return Vec::new();
        }
        let table = table_for_collection("memories").unwrap();
        match store
            .search(table, vector, mem_filter.as_deref(), limit, 0)
            .await
        {
            Ok(r) => to_unified("memories", r),
            Err(e) => {
                warn!(collection = "memories", error = %e, "collection search failed");
                Vec::new()
            }
        }
    };

    let std_fut = async {
        if !search_standards {
            return Vec::new();
        }
        let table = table_for_collection("standards").unwrap();
        match store
            .search(table, vector, std_filter.as_deref(), limit, 0)
            .await
        {
            Ok(r) => to_unified("standards", r),
            Err(e) => {
                warn!(collection = "standards", error = %e, "collection search failed");
                Vec::new()
            }
        }
    };

    let code_fut = async {
        if !search_code {
            return Vec::new();
        }
        let table = table_for_collection("code").unwrap();
        match store
            .search(table, vector, code_filter.as_deref(), limit, 0)
            .await
        {
            Ok(r) => to_unified("code", r),
            Err(e) => {
                warn!(collection = "code", error = %e, "collection search failed");
                Vec::new()
            }
        }
    };

    let todos_fut = async {
        if !search_todos {
            return Vec::new();
        }
        let table = table_for_collection("todos").unwrap();
        match store
            .search(table, vector, todos_filter.as_deref(), limit, 0)
            .await
        {
            Ok(r) => to_unified("todos", r),
            Err(e) => {
                warn!(collection = "todos", error = %e, "collection search failed");
                Vec::new()
            }
        }
    };

    let (mem_results, std_results, code_results, todos_results) =
        tokio::join!(mem_fut, std_fut, code_fut, todos_fut);

    let mut all: Vec<UnifiedSearchResult> = Vec::new();
    all.extend(mem_results);
    all.extend(std_results);
    all.extend(code_results);
    all.extend(todos_results);

    all.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    // Global cap: keep only the top-`limit` results across all collections.
    all.truncate(limit);
    Ok(all)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::traits::{SearchResult, TableSchema, VectorPoint};
    use async_trait::async_trait;
    use std::sync::Mutex;

    struct MockEmbedder;

    #[async_trait]
    impl EmbeddingProvider for MockEmbedder {
        async fn embed(&self, _texts: &[String]) -> Result<Vec<Vec<f32>>> {
            Ok(vec![vec![0.1, 0.2, 0.3]])
        }
        fn dimensions(&self) -> usize {
            3
        }
    }

    struct MockStore {
        tables: Mutex<std::collections::HashMap<String, Vec<SearchResult>>>,
    }

    impl MockStore {
        fn new() -> Self {
            Self {
                tables: Mutex::new(std::collections::HashMap::new()),
            }
        }

        fn add_results(&self, table: &str, results: Vec<SearchResult>) {
            self.tables
                .lock()
                .unwrap()
                .insert(table.to_string(), results);
        }
    }

    #[async_trait]
    impl VectorStore for MockStore {
        async fn ensure_table(
            &self,
            _name: &str,
            _dims: usize,
            _schema: &dyn TableSchema,
        ) -> Result<()> {
            Ok(())
        }
        async fn upsert(
            &self,
            _table: &str,
            _points: &[VectorPoint],
            _schema: &dyn TableSchema,
        ) -> Result<()> {
            Ok(())
        }
        async fn search(
            &self,
            table: &str,
            _vector: &[f32],
            _filter: Option<&str>,
            limit: usize,
            _offset: usize,
        ) -> Result<Vec<SearchResult>> {
            let tables = self.tables.lock().unwrap();
            Ok(tables
                .get(table)
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .take(limit)
                .collect())
        }
        async fn scroll(
            &self,
            _table: &str,
            _filter: Option<&str>,
            _limit: usize,
            _offset: usize,
        ) -> Result<Vec<SearchResult>> {
            Ok(vec![])
        }
        async fn count(&self, _table: &str, _filter: Option<&str>) -> Result<usize> {
            Ok(0)
        }
        async fn delete(&self, _table: &str, _ids: &[String]) -> Result<()> {
            Ok(())
        }
        async fn delete_by_filter(&self, _table: &str, _filter: &str) -> Result<usize> {
            Ok(0)
        }
        async fn get(&self, _table: &str, _ids: &[String]) -> Result<Vec<SearchResult>> {
            Ok(vec![])
        }
    }

    fn make_result(id: &str, score: f32, data: &str) -> SearchResult {
        SearchResult {
            id: id.to_string(),
            score,
            payload: serde_json::json!({"data": data, "user_id": "global"}),
        }
    }

    #[tokio::test]
    async fn test_unified_search_all_collections() {
        let store = MockStore::new();
        store.add_results(
            MEMORIES_TABLE,
            vec![make_result("m1", 0.9, "memory about rust")],
        );
        store.add_results(
            STANDARDS_TABLE,
            vec![make_result("s1", 0.8, "OWASP standard")],
        );
        store.add_results(CODE_TABLE, vec![make_result("c1", 0.7, "fn main() {}")]);
        store.add_results(TODOS_TABLE, vec![make_result("t1", 0.6, "fix the tests")]);

        let params = UnifiedSearchParams {
            query: "rust".to_string(),
            collections: None,
            project: None,
            user_id: None,
            limit: None,
            standard_type: None,
            standard_id: None,
            tech_stack: None,
            file_path: None,
        };

        let results = unified_search(&params, &store, &MockEmbedder, "global")
            .await
            .unwrap();

        assert_eq!(results.len(), 4);
        assert_eq!(results[0].collection, "memories");
        assert_eq!(results[0].score, 0.9);
        assert_eq!(results[1].collection, "standards");
        assert_eq!(results[2].collection, "code");
        assert_eq!(results[3].collection, "todos");
    }

    #[tokio::test]
    async fn test_unified_search_specific_collections() {
        let store = MockStore::new();
        store.add_results(MEMORIES_TABLE, vec![make_result("m1", 0.9, "memory")]);
        store.add_results(CODE_TABLE, vec![make_result("c1", 0.7, "code")]);

        let params = UnifiedSearchParams {
            query: "test".to_string(),
            collections: Some(vec!["code".to_string()]),
            project: None,
            user_id: None,
            limit: None,
            standard_type: None,
            standard_id: None,
            tech_stack: None,
            file_path: None,
        };

        let results = unified_search(&params, &store, &MockEmbedder, "global")
            .await
            .unwrap();

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].collection, "code");
    }

    #[tokio::test]
    async fn test_unified_search_empty_tables() {
        let store = MockStore::new();

        let params = UnifiedSearchParams {
            query: "anything".to_string(),
            collections: None,
            project: None,
            user_id: None,
            limit: None,
            standard_type: None,
            standard_id: None,
            tech_stack: None,
            file_path: None,
        };

        let results = unified_search(&params, &store, &MockEmbedder, "global")
            .await
            .unwrap();

        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn test_unified_search_score_ordering() {
        let store = MockStore::new();
        store.add_results(
            MEMORIES_TABLE,
            vec![make_result("m1", 0.5, "low score memory")],
        );
        store.add_results(CODE_TABLE, vec![make_result("c1", 0.95, "high score code")]);

        let params = UnifiedSearchParams {
            query: "test".to_string(),
            collections: None,
            project: None,
            user_id: None,
            limit: None,
            standard_type: None,
            standard_id: None,
            tech_stack: None,
            file_path: None,
        };

        let results = unified_search(&params, &store, &MockEmbedder, "global")
            .await
            .unwrap();

        assert!(results[0].score >= results[1].score);
        assert_eq!(results[0].collection, "code");
    }

    #[tokio::test]
    async fn test_unified_search_unknown_collection_ignored() {
        let store = MockStore::new();

        let params = UnifiedSearchParams {
            query: "test".to_string(),
            collections: Some(vec!["nonexistent".to_string()]),
            project: None,
            user_id: None,
            limit: None,
            standard_type: None,
            standard_id: None,
            tech_stack: None,
            file_path: None,
        };

        let results = unified_search(&params, &store, &MockEmbedder, "global")
            .await
            .unwrap();

        assert!(results.is_empty());
    }

    #[test]
    fn test_build_memories_filter_with_project() {
        let params = UnifiedSearchParams {
            query: String::new(),
            collections: None,
            project: Some("myproj".into()),
            user_id: None,
            limit: None,
            standard_type: None,
            standard_id: None,
            tech_stack: None,
            file_path: None,
        };
        let filter = build_memories_filter(&params, "global");
        assert_eq!(filter, "user_id = 'project:myproj'");
    }

    #[test]
    fn test_build_memories_filter_with_user_id() {
        let params = UnifiedSearchParams {
            query: String::new(),
            collections: None,
            project: None,
            user_id: Some("alice".into()),
            limit: None,
            standard_type: None,
            standard_id: None,
            tech_stack: None,
            file_path: None,
        };
        let filter = build_memories_filter(&params, "global");
        assert_eq!(filter, "user_id = 'alice'");
    }

    #[test]
    fn test_build_standards_filter() {
        let params = UnifiedSearchParams {
            query: String::new(),
            collections: None,
            project: None,
            user_id: None,
            limit: None,
            standard_type: Some("security".into()),
            standard_id: Some("owasp".into()),
            tech_stack: None,
            file_path: None,
        };
        let filter = build_standards_filter(&params).unwrap();
        assert!(filter.contains("standard_type = 'security'"));
        assert!(filter.contains("standard_id = 'owasp'"));
    }

    #[test]
    fn test_build_code_filter() {
        let params = UnifiedSearchParams {
            query: String::new(),
            collections: None,
            project: Some("memcan".into()),
            user_id: None,
            limit: None,
            standard_type: None,
            standard_id: None,
            tech_stack: Some("rust".into()),
            file_path: Some("main.rs".into()),
        };
        let filter = build_code_filter(&params).unwrap();
        assert!(filter.contains("project = 'memcan'"));
        assert!(filter.contains("tech_stack = 'rust'"));
        assert!(filter.contains("file_path LIKE '%main.rs%'"));
    }

    #[test]
    fn test_table_for_collection() {
        assert_eq!(table_for_collection("memories"), Some(MEMORIES_TABLE));
        assert_eq!(table_for_collection("standards"), Some(STANDARDS_TABLE));
        assert_eq!(table_for_collection("code"), Some(CODE_TABLE));
        assert_eq!(table_for_collection("todos"), Some(TODOS_TABLE));
        assert_eq!(table_for_collection("unknown"), None);
    }

    #[test]
    fn test_build_todos_filter_with_project() {
        let params = UnifiedSearchParams {
            query: String::new(),
            collections: None,
            project: Some("myproj".into()),
            user_id: None,
            limit: None,
            standard_type: None,
            standard_id: None,
            tech_stack: None,
            file_path: None,
        };
        let filter = build_todos_filter(&params).unwrap();
        assert_eq!(filter, "project = 'myproj'");
    }

    #[test]
    fn test_build_todos_filter_no_project() {
        let params = UnifiedSearchParams {
            query: String::new(),
            collections: None,
            project: None,
            user_id: None,
            limit: None,
            standard_type: None,
            standard_id: None,
            tech_stack: None,
            file_path: None,
        };
        assert!(build_todos_filter(&params).is_none());
    }

    // T1: metadata must not contain the "data" key; top-level data must be populated.
    #[test]
    fn test_to_unified_metadata_no_data_key() {
        let results = vec![SearchResult {
            id: "id1".to_string(),
            score: 0.8,
            payload: serde_json::json!({
                "data": "hello world",
                "user_id": "global",
                "project": "myproj"
            }),
        }];
        let unified = to_unified("memories", results);
        assert_eq!(unified.len(), 1);
        assert_eq!(unified[0].data, "hello world");
        let meta = unified[0].metadata.as_object().unwrap();
        assert!(
            !meta.contains_key("data"),
            "metadata must not contain 'data' key"
        );
        assert!(meta.contains_key("user_id"), "user_id must be preserved");
    }

    // T2: long data with multibyte chars is capped to exactly 501 chars (500 + ellipsis); no panic.
    #[test]
    fn test_to_unified_caps_long_data_multibyte() {
        // "ł日" = 2 chars, repeat 300 → 600 chars total (well above 500-char cap)
        let long_data: String = "ł日".repeat(300);
        assert_eq!(long_data.chars().count(), 600);
        let results = vec![SearchResult {
            id: "id2".to_string(),
            score: 0.5,
            payload: serde_json::json!({ "data": long_data }),
        }];
        let unified = to_unified("code", results);
        let char_count = unified[0].data.chars().count();
        // exactly 500 data chars + 1 ellipsis char = 501
        assert_eq!(
            char_count, 501,
            "data should be exactly 501 chars, got {char_count}"
        );
        assert!(
            unified[0].data.ends_with('…'),
            "truncated data should end with ellipsis"
        );
    }

    // Boundary: data of exactly 500 chars is returned unchanged (no ellipsis, no truncation).
    #[test]
    fn test_to_unified_data_exactly_at_cap_unchanged() {
        let at_cap: String = "a".repeat(MAX_DATA_LEN);
        assert_eq!(at_cap.chars().count(), 500);
        let results = vec![SearchResult {
            id: "boundary1".to_string(),
            score: 0.5,
            payload: serde_json::json!({ "data": at_cap }),
        }];
        let unified = to_unified("memories", results);
        assert_eq!(
            unified[0].data.chars().count(),
            500,
            "exactly-500-char data must not be truncated"
        );
        assert!(
            !unified[0].data.ends_with('…'),
            "no ellipsis at exactly the cap"
        );
    }

    // Boundary: data of exactly 501 chars is truncated to 500 + ellipsis (guards > vs >=).
    #[test]
    fn test_to_unified_data_one_over_cap_truncated() {
        let one_over: String = "b".repeat(MAX_DATA_LEN + 1);
        assert_eq!(one_over.chars().count(), 501);
        let results = vec![SearchResult {
            id: "boundary2".to_string(),
            score: 0.5,
            payload: serde_json::json!({ "data": one_over }),
        }];
        let unified = to_unified("memories", results);
        assert_eq!(
            unified[0].data.chars().count(),
            501,
            "501-char input should yield 500 chars + ellipsis = 501"
        );
        assert!(unified[0].data.ends_with('…'), "must end with ellipsis");
    }

    // Missing key: payload with no "data" key yields empty string, no panic, metadata intact.
    #[test]
    fn test_to_unified_missing_data_key() {
        let results = vec![SearchResult {
            id: "nodatakey".to_string(),
            score: 0.6,
            payload: serde_json::json!({ "user_id": "alice", "project": "memcan" }),
        }];
        let unified = to_unified("memories", results);
        assert_eq!(unified[0].data, "", "missing data key yields empty string");
        let meta = unified[0].metadata.as_object().unwrap();
        assert!(meta.contains_key("user_id"), "other keys preserved");
        assert!(meta.contains_key("project"), "other keys preserved");
    }

    // T3: global top-N — total results across collections are capped to limit.
    #[tokio::test]
    async fn test_unified_search_global_top_n_limit() {
        let store = MockStore::new();
        store.add_results(
            MEMORIES_TABLE,
            vec![
                make_result("m1", 0.9, "memory 1"),
                make_result("m2", 0.7, "memory 2"),
                make_result("m3", 0.5, "memory 3"),
            ],
        );
        store.add_results(
            CODE_TABLE,
            vec![
                make_result("c1", 0.85, "code 1"),
                make_result("c2", 0.65, "code 2"),
                make_result("c3", 0.45, "code 3"),
            ],
        );

        let params = UnifiedSearchParams {
            query: "test".to_string(),
            collections: Some(vec!["memories".to_string(), "code".to_string()]),
            project: None,
            user_id: None,
            limit: Some(3),
            standard_type: None,
            standard_id: None,
            tech_stack: None,
            file_path: None,
        };

        let results = unified_search(&params, &store, &MockEmbedder, "global")
            .await
            .unwrap();

        assert_eq!(
            results.len(),
            3,
            "global cap: expected 3, got {}",
            results.len()
        );
        // sorted by score desc
        assert!(results[0].score >= results[1].score);
        assert!(results[1].score >= results[2].score);
        // top-3 by score: m1(0.9), c1(0.85), m2(0.7)
        assert_eq!(results[0].score, 0.9);
        assert_eq!(results[1].score, 0.85);
        assert_eq!(results[2].score, 0.7);
    }

    // T4: data shorter than the cap is returned unchanged (no ellipsis appended).
    #[test]
    fn test_to_unified_short_data_unchanged() {
        let short = "Hello ł日 world"; // multibyte, well under 500 chars
        let results = vec![SearchResult {
            id: "id4".to_string(),
            score: 0.5,
            payload: serde_json::json!({ "data": short }),
        }];
        let unified = to_unified("memories", results);
        assert_eq!(
            unified[0].data, short,
            "short data should be returned unchanged"
        );
        assert!(!unified[0].data.ends_with('…'), "no ellipsis on short data");
    }

    // T5: only "data" is stripped from metadata; all other keys survive.
    #[test]
    fn test_to_unified_preserves_other_metadata_keys() {
        let results = vec![SearchResult {
            id: "id5".to_string(),
            score: 0.7,
            payload: serde_json::json!({
                "data": "some data",
                "user_id": "alice",
                "project": "memcan",
                "type": "snippet",
            }),
        }];
        let unified = to_unified("code", results);
        let meta = unified[0].metadata.as_object().unwrap();
        assert!(!meta.contains_key("data"), "data key must be stripped");
        assert!(meta.contains_key("user_id"), "user_id must survive");
        assert!(meta.contains_key("project"), "project must survive");
        assert!(meta.contains_key("type"), "type must survive");
    }
}
