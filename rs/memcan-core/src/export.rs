//! Collection export to JSONL format.
//!
//! Paginated scroll over any collection, serializing records to JSONL
//! (text + metadata, no vectors).

use serde::{Deserialize, Serialize};

use crate::error::{MemcanError, Result};
use crate::pipeline::{CODE_TABLE, MEMORIES_TABLE, STANDARDS_TABLE};
use crate::todo::TODOS_TABLE;
use crate::traits::VectorStore;

/// Map a user-facing collection name to its LanceDB table name.
pub fn collection_to_table(collection: &str) -> Result<&'static str> {
    match collection {
        "memories" => Ok(MEMORIES_TABLE),
        "standards" => Ok(STANDARDS_TABLE),
        "code" => Ok(CODE_TABLE),
        "todos" => Ok(TODOS_TABLE),
        _ => Err(MemcanError::Other(format!(
            "unknown collection: {collection}"
        ))),
    }
}

/// Map a LanceDB table name back to the user-facing collection name.
pub fn table_to_collection(table: &str) -> &'static str {
    match table {
        MEMORIES_TABLE => "memories",
        STANDARDS_TABLE => "standards",
        CODE_TABLE => "code",
        TODOS_TABLE => "todos",
        _ => "unknown",
    }
}

/// A single JSONL record envelope (no vectors).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportRecord {
    pub _collection: String,
    pub id: String,
    #[serde(flatten)]
    pub payload: serde_json::Map<String, serde_json::Value>,
}

/// Export statistics.
pub struct ExportStats {
    pub count: usize,
}

/// Export a collection page-by-page via `VectorStore::scroll`.
///
/// Calls `sink` for each record. Returns total count.
pub async fn export_collection(
    store: &dyn VectorStore,
    table: &str,
    filter: Option<&str>,
    limit: usize,
    sink: &mut dyn FnMut(ExportRecord) -> Result<()>,
) -> Result<ExportStats> {
    let collection = table_to_collection(table).to_string();
    let mut offset = 0usize;
    let mut count = 0usize;

    loop {
        let batch = store.scroll(table, filter, limit, offset).await?;
        if batch.is_empty() {
            break;
        }

        for result in &batch {
            let mut payload = match &result.payload {
                serde_json::Value::Object(map) => map.clone(),
                _ => serde_json::Map::new(),
            };
            payload.remove("vector");

            let record = ExportRecord {
                _collection: collection.clone(),
                id: result.id.clone(),
                payload,
            };
            sink(record)?;
            count += 1;
        }

        offset += batch.len();
        if batch.len() < limit {
            break;
        }
    }

    Ok(ExportStats { count })
}

/// Serialize an `ExportRecord` to a JSONL line.
pub fn record_to_jsonl(record: &ExportRecord) -> Result<String> {
    serde_json::to_string(record).map_err(|e| MemcanError::Json {
        context: "serializing export record".into(),
        source: e,
    })
}

/// Parse a JSONL line into an `ExportRecord`.
pub fn jsonl_to_record(line: &str) -> Result<ExportRecord> {
    serde_json::from_str(line).map_err(|e| MemcanError::Json {
        context: "parsing JSONL line".into(),
        source: e,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::traits::{SearchResult, TableSchema, VectorPoint};
    use async_trait::async_trait;
    use std::sync::Mutex;

    struct MockStore {
        pages: Mutex<Vec<Vec<SearchResult>>>,
    }

    impl MockStore {
        fn new(pages: Vec<Vec<SearchResult>>) -> Self {
            Self {
                pages: Mutex::new(pages),
            }
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
            _table: &str,
            _vector: &[f32],
            _filter: Option<&str>,
            _limit: usize,
            _offset: usize,
        ) -> Result<Vec<SearchResult>> {
            Ok(vec![])
        }
        async fn scroll(
            &self,
            _table: &str,
            _filter: Option<&str>,
            _limit: usize,
            _offset: usize,
        ) -> Result<Vec<SearchResult>> {
            let mut pages = self.pages.lock().unwrap();
            if pages.is_empty() {
                Ok(vec![])
            } else {
                Ok(pages.remove(0))
            }
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

    #[test]
    fn test_collection_to_table() {
        assert_eq!(collection_to_table("memories").unwrap(), MEMORIES_TABLE);
        assert_eq!(collection_to_table("standards").unwrap(), STANDARDS_TABLE);
        assert_eq!(collection_to_table("code").unwrap(), CODE_TABLE);
        assert_eq!(collection_to_table("todos").unwrap(), TODOS_TABLE);
        assert!(collection_to_table("nonexistent").is_err());
    }

    #[test]
    fn test_table_to_collection() {
        assert_eq!(table_to_collection(MEMORIES_TABLE), "memories");
        assert_eq!(table_to_collection(STANDARDS_TABLE), "standards");
        assert_eq!(table_to_collection(CODE_TABLE), "code");
        assert_eq!(table_to_collection(TODOS_TABLE), "todos");
        assert_eq!(table_to_collection("random_table"), "unknown");
    }

    #[test]
    fn test_record_to_jsonl_roundtrip() {
        let mut payload = serde_json::Map::new();
        payload.insert(
            "data".into(),
            serde_json::Value::String("hello world".into()),
        );
        payload.insert("user_id".into(), serde_json::Value::String("global".into()));

        let record = ExportRecord {
            _collection: "memories".into(),
            id: "abc-123".into(),
            payload,
        };

        let line = record_to_jsonl(&record).unwrap();
        let parsed = jsonl_to_record(&line).unwrap();

        assert_eq!(parsed._collection, "memories");
        assert_eq!(parsed.id, "abc-123");
        assert_eq!(
            parsed.payload.get("data").unwrap().as_str().unwrap(),
            "hello world"
        );
        assert_eq!(
            parsed.payload.get("user_id").unwrap().as_str().unwrap(),
            "global"
        );
    }

    #[test]
    fn test_jsonl_to_record_invalid() {
        assert!(jsonl_to_record("not valid json").is_err());
    }

    #[test]
    fn test_record_flattened_serialization() {
        let mut payload = serde_json::Map::new();
        payload.insert("data".into(), serde_json::Value::String("test".into()));

        let record = ExportRecord {
            _collection: "memories".into(),
            id: "id-1".into(),
            payload,
        };

        let json: serde_json::Value =
            serde_json::from_str(&record_to_jsonl(&record).unwrap()).unwrap();
        assert_eq!(json["_collection"], "memories");
        assert_eq!(json["id"], "id-1");
        assert_eq!(json["data"], "test");
    }

    #[tokio::test]
    async fn test_export_collection_empty() {
        let store = MockStore::new(vec![]);
        let mut records = Vec::new();
        let stats = export_collection(&store, MEMORIES_TABLE, None, 100, &mut |r| {
            records.push(r);
            Ok(())
        })
        .await
        .unwrap();

        assert_eq!(stats.count, 0);
        assert!(records.is_empty());
    }

    #[tokio::test]
    async fn test_export_collection_single_page() {
        let results = vec![SearchResult {
            id: "r1".into(),
            score: 0.0,
            payload: serde_json::json!({"data": "hello", "user_id": "global"}),
        }];
        let store = MockStore::new(vec![results]);
        let mut records = Vec::new();
        let stats = export_collection(&store, MEMORIES_TABLE, None, 100, &mut |r| {
            records.push(r);
            Ok(())
        })
        .await
        .unwrap();

        assert_eq!(stats.count, 1);
        assert_eq!(records[0]._collection, "memories");
        assert_eq!(records[0].id, "r1");
        assert_eq!(
            records[0].payload.get("data").unwrap().as_str().unwrap(),
            "hello"
        );
    }

    #[tokio::test]
    async fn test_export_collection_multiple_pages() {
        let page1 = vec![
            SearchResult {
                id: "r1".into(),
                score: 0.0,
                payload: serde_json::json!({"data": "first"}),
            },
            SearchResult {
                id: "r2".into(),
                score: 0.0,
                payload: serde_json::json!({"data": "second"}),
            },
        ];
        let page2 = vec![SearchResult {
            id: "r3".into(),
            score: 0.0,
            payload: serde_json::json!({"data": "third"}),
        }];
        let store = MockStore::new(vec![page1, page2]);
        let mut records = Vec::new();
        let stats = export_collection(&store, MEMORIES_TABLE, None, 2, &mut |r| {
            records.push(r);
            Ok(())
        })
        .await
        .unwrap();

        assert_eq!(stats.count, 3);
        assert_eq!(records.len(), 3);
    }

    #[tokio::test]
    async fn test_export_strips_vector_from_payload() {
        let results = vec![SearchResult {
            id: "r1".into(),
            score: 0.0,
            payload: serde_json::json!({"data": "hi", "vector": [0.1, 0.2]}),
        }];
        let store = MockStore::new(vec![results]);
        let mut records = Vec::new();
        export_collection(&store, CODE_TABLE, None, 100, &mut |r| {
            records.push(r);
            Ok(())
        })
        .await
        .unwrap();

        assert!(!records[0].payload.contains_key("vector"));
        assert_eq!(records[0]._collection, "code");
    }
}
