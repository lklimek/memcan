//! JSONL import with re-embedding.
//!
//! Accepts parsed [`ExportRecord`]s, embeds the `data` field, and upserts
//! into the vector store. No LLM fact extraction, no dedup.

use std::collections::HashMap;

use tracing::{info, warn};

use crate::error::Result;
use crate::export::{ExportRecord, collection_to_table};
use crate::traits::{EmbeddingProvider, TableSchema, VectorPoint, VectorStore};

/// Result of a batch import operation.
pub struct ImportResult {
    pub imported: usize,
    pub skipped: usize,
    pub errors: Vec<String>,
}

const EMBED_BATCH_SIZE: usize = 50;

/// Import records: group by collection, embed the `data` field, upsert.
///
/// Skips LLM fact extraction and dedup entirely.
pub async fn import_records(
    records: Vec<ExportRecord>,
    store: &dyn VectorStore,
    embedder: &dyn EmbeddingProvider,
    schema: &dyn TableSchema,
    embed_dims: usize,
) -> Result<ImportResult> {
    let mut grouped: HashMap<String, Vec<ExportRecord>> = HashMap::new();
    for record in records {
        grouped
            .entry(record._collection.clone())
            .or_default()
            .push(record);
    }

    let mut imported = 0usize;
    let mut skipped = 0usize;
    let mut errors = Vec::new();

    for (collection, group) in &grouped {
        let table = match collection_to_table(collection) {
            Ok(t) => t,
            Err(e) => {
                errors.push(format!("unknown collection '{collection}': {e}"));
                skipped += group.len();
                continue;
            }
        };

        store.ensure_table(table, embed_dims, schema).await?;

        for chunk in group.chunks(EMBED_BATCH_SIZE) {
            let mut texts = Vec::with_capacity(chunk.len());
            let mut valid_records = Vec::with_capacity(chunk.len());

            for record in chunk {
                match record.payload.get("data").and_then(|v| v.as_str()) {
                    Some(data) => {
                        texts.push(data.to_string());
                        valid_records.push(record);
                    }
                    None => {
                        errors.push(format!("record {} missing 'data' field", record.id));
                        skipped += 1;
                    }
                }
            }

            if texts.is_empty() {
                continue;
            }

            let vectors = match embedder.embed(&texts).await {
                Ok(v) => v,
                Err(e) => {
                    let msg = format!("embedding batch failed for {collection}: {e}");
                    warn!("{msg}");
                    errors.push(msg);
                    skipped += valid_records.len();
                    continue;
                }
            };

            let points: Vec<VectorPoint> = valid_records
                .iter()
                .zip(vectors)
                .map(|(record, vector)| {
                    let mut payload = record.payload.clone();
                    payload.remove("_collection");
                    VectorPoint {
                        id: record.id.clone(),
                        vector,
                        payload: serde_json::Value::Object(payload),
                    }
                })
                .collect();

            let count = points.len();
            store.upsert(table, &points, schema).await?;
            imported += count;
        }
    }

    info!(imported, skipped, errors = errors.len(), "Import complete");
    Ok(ImportResult {
        imported,
        skipped,
        errors,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::traits::{SearchResult, VectorPoint};
    use async_trait::async_trait;
    use std::sync::Mutex;

    struct MockEmbedder {
        dims: usize,
    }

    #[async_trait]
    impl EmbeddingProvider for MockEmbedder {
        async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
            Ok(texts.iter().map(|_| vec![0.1; self.dims]).collect())
        }
        fn dimensions(&self) -> usize {
            self.dims
        }
    }

    struct MockStore {
        upserted: Mutex<Vec<(String, Vec<VectorPoint>)>>,
        ensured: Mutex<Vec<String>>,
    }

    impl MockStore {
        fn new() -> Self {
            Self {
                upserted: Mutex::new(Vec::new()),
                ensured: Mutex::new(Vec::new()),
            }
        }

        fn upserted_count(&self) -> usize {
            self.upserted
                .lock()
                .unwrap()
                .iter()
                .map(|(_, pts)| pts.len())
                .sum()
        }

        fn upserted_tables(&self) -> Vec<String> {
            self.upserted
                .lock()
                .unwrap()
                .iter()
                .map(|(t, _)| t.clone())
                .collect()
        }
    }

    #[async_trait]
    impl VectorStore for MockStore {
        async fn ensure_table(
            &self,
            name: &str,
            _dims: usize,
            _schema: &dyn TableSchema,
        ) -> Result<()> {
            self.ensured.lock().unwrap().push(name.to_string());
            Ok(())
        }
        async fn upsert(
            &self,
            table: &str,
            points: &[VectorPoint],
            _schema: &dyn TableSchema,
        ) -> Result<()> {
            self.upserted
                .lock()
                .unwrap()
                .push((table.to_string(), points.to_vec()));
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

    fn make_record(collection: &str, id: &str, data: &str) -> ExportRecord {
        let mut payload = serde_json::Map::new();
        payload.insert("data".into(), serde_json::Value::String(data.into()));
        payload.insert(
            "collection".into(),
            serde_json::Value::String(collection.into()),
        );
        ExportRecord {
            _collection: collection.into(),
            id: id.into(),
            payload,
        }
    }

    #[tokio::test]
    async fn test_import_empty() {
        let store = MockStore::new();
        let embedder = MockEmbedder { dims: 3 };
        let schema = crate::traits::MinimalTableSchema;

        let result = import_records(vec![], &store, &embedder, &schema, 3)
            .await
            .unwrap();

        assert_eq!(result.imported, 0);
        assert_eq!(result.skipped, 0);
        assert!(result.errors.is_empty());
    }

    #[tokio::test]
    async fn test_import_single_collection() {
        let store = MockStore::new();
        let embedder = MockEmbedder { dims: 3 };
        let schema = crate::traits::MinimalTableSchema;

        let records = vec![
            make_record("memories", "m1", "lesson one"),
            make_record("memories", "m2", "lesson two"),
        ];

        let result = import_records(records, &store, &embedder, &schema, 3)
            .await
            .unwrap();

        assert_eq!(result.imported, 2);
        assert_eq!(result.skipped, 0);
        assert!(result.errors.is_empty());
        assert_eq!(store.upserted_count(), 2);
    }

    #[tokio::test]
    async fn test_import_multiple_collections() {
        let store = MockStore::new();
        let embedder = MockEmbedder { dims: 3 };
        let schema = crate::traits::MinimalTableSchema;

        let records = vec![
            make_record("memories", "m1", "memory"),
            make_record("code", "c1", "fn main() {}"),
        ];

        let result = import_records(records, &store, &embedder, &schema, 3)
            .await
            .unwrap();

        assert_eq!(result.imported, 2);
        let tables = store.upserted_tables();
        assert!(tables.contains(&"memcan_memories".to_string()));
        assert!(tables.contains(&"memcan_code".to_string()));
    }

    #[tokio::test]
    async fn test_import_missing_data_field() {
        let store = MockStore::new();
        let embedder = MockEmbedder { dims: 3 };
        let schema = crate::traits::MinimalTableSchema;

        let mut payload = serde_json::Map::new();
        payload.insert("user_id".into(), serde_json::Value::String("global".into()));
        let records = vec![ExportRecord {
            _collection: "memories".into(),
            id: "m1".into(),
            payload,
        }];

        let result = import_records(records, &store, &embedder, &schema, 3)
            .await
            .unwrap();

        assert_eq!(result.imported, 0);
        assert_eq!(result.skipped, 1);
        assert_eq!(result.errors.len(), 1);
        assert!(result.errors[0].contains("missing 'data' field"));
    }

    #[tokio::test]
    async fn test_import_unknown_collection() {
        let store = MockStore::new();
        let embedder = MockEmbedder { dims: 3 };
        let schema = crate::traits::MinimalTableSchema;

        let records = vec![make_record("nonexistent", "x1", "data")];

        let result = import_records(records, &store, &embedder, &schema, 3)
            .await
            .unwrap();

        assert_eq!(result.imported, 0);
        assert_eq!(result.skipped, 1);
        assert!(!result.errors.is_empty());
    }

    #[tokio::test]
    async fn test_import_strips_collection_from_payload() {
        let store = MockStore::new();
        let embedder = MockEmbedder { dims: 3 };
        let schema = crate::traits::MinimalTableSchema;

        let mut payload = serde_json::Map::new();
        payload.insert("data".into(), serde_json::Value::String("hello".into()));
        payload.insert(
            "_collection".into(),
            serde_json::Value::String("memories".into()),
        );
        let records = vec![ExportRecord {
            _collection: "memories".into(),
            id: "m1".into(),
            payload,
        }];

        let result = import_records(records, &store, &embedder, &schema, 3)
            .await
            .unwrap();

        assert_eq!(result.imported, 1);
        let upserted = store.upserted.lock().unwrap();
        let point_payload = &upserted[0].1[0].payload;
        assert!(point_payload.get("_collection").is_none());
    }

    #[tokio::test]
    async fn test_import_ensures_table() {
        let store = MockStore::new();
        let embedder = MockEmbedder { dims: 3 };
        let schema = crate::traits::MinimalTableSchema;

        let records = vec![make_record("memories", "m1", "data")];

        import_records(records, &store, &embedder, &schema, 3)
            .await
            .unwrap();

        let ensured = store.ensured.lock().unwrap();
        assert!(ensured.contains(&"memcan_memories".to_string()));
    }
}
