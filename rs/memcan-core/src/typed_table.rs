//! Strongly-typed table handle for LanceDB.
//!
//! [`TypedTable`] wraps a [`lancedb::Table`] together with its
//! [`TableSchema`], providing per-table operations without re-specifying
//! the schema on every call.  Obtain one via
//! [`LanceDbStore::typed_table()`](crate::lancedb_store::LanceDbStore::typed_table).

use arrow_array::RecordBatch;
use futures::TryStreamExt;
use lancedb::query::{ExecutableQuery, QueryBase, Select};

use crate::error::VectorStoreError;
use crate::lancedb_store::LanceDbStore;
use crate::traits::{SearchResult, TableSchema, VectorPoint};

type Result<T> = std::result::Result<T, VectorStoreError>;

/// A handle to a single LanceDB table, bound to a specific [`TableSchema`].
///
/// All per-table operations (upsert, search, delete, count, scroll) live here.
/// Create one via [`LanceDbStore::typed_table()`].
pub struct TypedTable<S: TableSchema> {
    name: String,
    inner: lancedb::Table,
    schema: S,
    dims: usize,
}

impl<S: TableSchema> TypedTable<S> {
    /// Create a new `TypedTable` (called by `LanceDbStore::typed_table()`).
    pub(crate) fn new(name: String, inner: lancedb::Table, schema: S, dims: usize) -> Self {
        Self {
            name,
            inner,
            schema,
            dims,
        }
    }

    /// Table name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Vector dimensionality.
    pub fn dims(&self) -> usize {
        self.dims
    }

    /// Insert or update points. Existing IDs are overwritten.
    pub async fn upsert(&self, points: &[VectorPoint]) -> Result<()> {
        if points.is_empty() {
            return Ok(());
        }

        let filter = crate::lancedb_store::build_id_filter(points.iter().map(|p| p.id.as_str()));
        if let Err(e) = self.inner.delete(&filter).await {
            tracing::warn!(table = %self.name, error = %e, "pre-upsert delete failed, duplicates may occur");
        }

        let batch =
            LanceDbStore::points_to_batch(points, self.dims, &self.schema).map_err(|e| {
                VectorStoreError::SchemaMismatch {
                    table: self.name.clone(),
                    detail: e.to_string(),
                }
            })?;
        let schema = batch.schema();
        let batches = arrow_array::RecordBatchIterator::new(vec![Ok(batch)], schema);
        self.inner
            .add(Box::new(batches))
            .execute()
            .await
            .map_err(|e| self.classify_error(e))?;
        Ok(())
    }

    /// Nearest-neighbor search returning up to `limit` results.
    pub async fn search(
        &self,
        vector: &[f32],
        filter: Option<&str>,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<SearchResult>> {
        let mut query = self
            .inner
            .vector_search(vector)
            .map_err(|e| self.classify_error(e))?;
        query = query.limit(limit);
        if offset > 0 {
            query = query.offset(offset);
        }
        if let Some(f) = filter {
            query = query.only_if(f);
        }
        let batches = query
            .select(Select::columns(&["id", "payload", "_distance"]))
            .execute()
            .await
            .map_err(|e| self.classify_error(e))?;

        let all: Vec<RecordBatch> = batches
            .try_collect()
            .await
            .map_err(|e| self.classify_error(e))?;
        let mut results = Vec::new();
        for batch in &all {
            results.extend(LanceDbStore::batch_to_results(batch, true));
        }
        Ok(results)
    }

    /// Delete records matching a SQL filter expression.
    pub async fn delete(&self, filter: &str) -> Result<()> {
        self.inner
            .delete(filter)
            .await
            .map_err(|e| self.classify_error(e))?;
        Ok(())
    }

    /// Count records matching an optional SQL filter.
    pub async fn count(&self, filter: Option<&str>) -> Result<usize> {
        let mut query = self.inner.query();
        if let Some(f) = filter {
            query = query.only_if(f);
        }
        let batches = query
            .select(Select::columns(&["id"]))
            .execute()
            .await
            .map_err(|e| self.classify_error(e))?;

        let all: Vec<RecordBatch> = batches
            .try_collect()
            .await
            .map_err(|e| self.classify_error(e))?;
        Ok(all.iter().map(|b| b.num_rows()).sum())
    }

    /// List records with optional SQL filter (no vector search).
    pub async fn scroll(
        &self,
        filter: Option<&str>,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<SearchResult>> {
        let mut query = self.inner.query();
        if let Some(f) = filter {
            query = query.only_if(f);
        }
        query = query.limit(limit);
        if offset > 0 {
            query = query.offset(offset);
        }
        let batches = query
            .select(Select::columns(&["id", "payload"]))
            .execute()
            .await
            .map_err(|e| self.classify_error(e))?;

        let all: Vec<RecordBatch> = batches
            .try_collect()
            .await
            .map_err(|e| self.classify_error(e))?;
        let mut results = Vec::new();
        for batch in &all {
            results.extend(LanceDbStore::batch_to_results(batch, false));
        }
        Ok(results)
    }

    /// Retrieve specific records by their IDs.
    pub async fn get(&self, ids: &[String]) -> Result<Vec<SearchResult>> {
        if ids.is_empty() {
            return Ok(vec![]);
        }
        let filter = crate::lancedb_store::build_id_filter(ids.iter().map(|id| id.as_str()));
        self.scroll(Some(&filter), ids.len(), 0).await
    }

    /// Delete records by their IDs.
    pub async fn delete_by_ids(&self, ids: &[String]) -> Result<()> {
        if ids.is_empty() {
            return Ok(());
        }
        let filter = crate::lancedb_store::build_id_filter(ids.iter().map(|id| id.as_str()));
        self.delete(&filter).await
    }

    /// Classify a raw LanceDB error, detecting stale handles.
    fn classify_error(&self, err: lancedb::Error) -> VectorStoreError {
        crate::lancedb_store::classify_lancedb_error(&self.name, err)
    }
}

impl<S: TableSchema + Clone> Clone for TypedTable<S> {
    fn clone(&self) -> Self {
        Self {
            name: self.name.clone(),
            inner: self.inner.clone(),
            schema: self.schema.clone(),
            dims: self.dims,
        }
    }
}

impl<S: TableSchema> std::fmt::Debug for TypedTable<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TypedTable")
            .field("name", &self.name)
            .field("dims", &self.dims)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lancedb_store::LanceDbStore;
    use crate::traits::MinimalTableSchema;

    #[tokio::test]
    async fn test_typed_table_upsert_and_count() {
        let tmp = tempfile::tempdir().expect("create tempdir");
        let path = tmp.path().to_str().expect("tempdir path");
        let store = LanceDbStore::open(path).await.expect("open lancedb");

        let table = store
            .typed_table("test_upsert", MinimalTableSchema, 4)
            .await
            .expect("typed_table");
        assert_eq!(table.name(), "test_upsert");
        assert_eq!(table.dims(), 4);

        let points = vec![
            VectorPoint {
                id: "a".into(),
                vector: vec![1.0, 0.0, 0.0, 0.0],
                payload: serde_json::json!({"key": "alpha"}),
            },
            VectorPoint {
                id: "b".into(),
                vector: vec![0.0, 1.0, 0.0, 0.0],
                payload: serde_json::json!({"key": "beta"}),
            },
        ];
        table.upsert(&points).await.expect("upsert");

        let count = table.count(None).await.expect("count");
        assert_eq!(count, 2);

        let count_filtered = table.count(Some("id = 'a'")).await.expect("count filtered");
        assert_eq!(count_filtered, 1);
    }

    #[tokio::test]
    async fn test_typed_table_search() {
        let tmp = tempfile::tempdir().expect("create tempdir");
        let path = tmp.path().to_str().expect("tempdir path");
        let store = LanceDbStore::open(path).await.expect("open lancedb");

        let table = store
            .typed_table("test_search", MinimalTableSchema, 4)
            .await
            .expect("typed_table");

        let points = vec![
            VectorPoint {
                id: "x".into(),
                vector: vec![1.0, 0.0, 0.0, 0.0],
                payload: serde_json::json!({"val": "x"}),
            },
            VectorPoint {
                id: "y".into(),
                vector: vec![0.0, 1.0, 0.0, 0.0],
                payload: serde_json::json!({"val": "y"}),
            },
        ];
        table.upsert(&points).await.expect("upsert");

        let results = table
            .search(&[1.0, 0.0, 0.0, 0.0], None, 10, 0)
            .await
            .expect("search");
        assert!(!results.is_empty());
        assert_eq!(results[0].id, "x");
        assert!(results[0].score > 0.0, "search score should be positive");
        assert_eq!(results[0].payload["val"], "x");
    }

    #[tokio::test]
    async fn test_typed_table_delete() {
        let tmp = tempfile::tempdir().expect("create tempdir");
        let path = tmp.path().to_str().expect("tempdir path");
        let store = LanceDbStore::open(path).await.expect("open lancedb");

        let table = store
            .typed_table("test_delete", MinimalTableSchema, 4)
            .await
            .expect("typed_table");

        let points = vec![VectorPoint {
            id: "d1".into(),
            vector: vec![1.0, 0.0, 0.0, 0.0],
            payload: serde_json::json!({}),
        }];
        table.upsert(&points).await.expect("upsert");
        assert_eq!(table.count(None).await.expect("count"), 1);

        table.delete("id = 'd1'").await.expect("delete");
        assert_eq!(table.count(None).await.expect("count after delete"), 0);
    }

    #[tokio::test]
    async fn test_typed_table_scroll() {
        let tmp = tempfile::tempdir().expect("create tempdir");
        let path = tmp.path().to_str().expect("tempdir path");
        let store = LanceDbStore::open(path).await.expect("open lancedb");

        let table = store
            .typed_table("test_scroll", MinimalTableSchema, 4)
            .await
            .expect("typed_table");

        let points: Vec<VectorPoint> = (0..5)
            .map(|i| VectorPoint {
                id: format!("s{i}"),
                vector: vec![i as f32, 0.0, 0.0, 0.0],
                payload: serde_json::json!({"idx": i}),
            })
            .collect();
        table.upsert(&points).await.expect("upsert");

        let page = table.scroll(None, 3, 0).await.expect("scroll page 1");
        assert_eq!(page.len(), 3);

        let page2 = table.scroll(None, 3, 3).await.expect("scroll page 2");
        assert_eq!(page2.len(), 2);
    }

    #[tokio::test]
    async fn test_typed_table_upsert_empty() {
        let tmp = tempfile::tempdir().expect("create tempdir");
        let path = tmp.path().to_str().expect("tempdir path");
        let store = LanceDbStore::open(path).await.expect("open lancedb");

        let table = store
            .typed_table("test_empty", MinimalTableSchema, 4)
            .await
            .expect("typed_table");

        table.upsert(&[]).await.expect("upsert empty should be ok");
        assert_eq!(table.count(None).await.expect("count"), 0);
    }

    #[tokio::test]
    async fn test_typed_table_debug() {
        let tmp = tempfile::tempdir().expect("create tempdir");
        let path = tmp.path().to_str().expect("tempdir path");
        let store = LanceDbStore::open(path).await.expect("open lancedb");

        let table = store
            .typed_table("test_debug", MinimalTableSchema, 4)
            .await
            .expect("typed_table");

        let dbg = format!("{:?}", table);
        assert!(dbg.contains("test_debug"));
        assert!(dbg.contains("4"));
    }
}
