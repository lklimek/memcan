//! LanceDB implementation of [`VectorStore`].

use std::sync::Arc;

use arrow_array::{
    Array, ArrayRef, FixedSizeListArray, Float32Array, LargeStringArray, RecordBatch,
    RecordBatchIterator, StringArray,
};
use arrow_schema::{DataType, Field, Schema};
use async_trait::async_trait;
use futures::TryStreamExt;
use lancedb::query::{ExecutableQuery, QueryBase, Select};
use lancedb::{Connection, Table, connect};
use tokio::sync::Mutex;
use tracing::{debug, warn};

use crate::error::{MemcanError, Result, ResultExt};
use crate::traits::{SearchResult, VectorPoint, VectorStore};

/// LanceDB-backed vector store.
///
/// Data is stored in a local directory. Each "table" in the trait maps to a
/// LanceDB table with a fixed schema containing filterable columns extracted
/// from the JSON payload alongside the full JSON payload itself.
pub struct LanceDbStore {
    conn: Connection,
    /// Guards table-creation to avoid races.
    create_lock: Mutex<()>,
}

impl std::fmt::Debug for LanceDbStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LanceDbStore").finish()
    }
}

impl LanceDbStore {
    /// Open (or create) a LanceDB database at the given path.
    pub async fn open(path: &str) -> Result<Self> {
        std::fs::create_dir_all(path)
            .with_context(|| format!("cannot create LanceDB directory: {path}"))?;
        let conn = connect(path)
            .execute()
            .await
            .with_context(|| format!("cannot open LanceDB at {path}"))?;
        Ok(Self {
            conn,
            create_lock: Mutex::new(()),
        })
    }

    /// Build the Arrow schema for a table with the given vector dimensionality.
    ///
    /// The schema includes filterable columns extracted from the JSON payload
    /// so that LanceDB SQL WHERE filters can reference them directly.
    fn table_schema(dims: usize) -> Arc<Schema> {
        Arc::new(Schema::new(vec![
            Field::new("id", DataType::Utf8, false),
            Field::new(
                "vector",
                DataType::FixedSizeList(
                    Arc::new(Field::new("item", DataType::Float32, true)),
                    dims as i32,
                ),
                false,
            ),
            Field::new("payload", DataType::LargeUtf8, false),
            Field::new("user_id", DataType::Utf8, true),
            Field::new("project", DataType::Utf8, true),
            Field::new("standard_type", DataType::Utf8, true),
            Field::new("standard_id", DataType::Utf8, true),
            Field::new("tech_stack", DataType::Utf8, true),
            Field::new("file_path", DataType::Utf8, true),
            Field::new("content_hash", DataType::Utf8, true),
        ]))
    }

    /// Extract a string field from a JSON value, returning `None` if absent or non-string.
    fn json_opt_str<'a>(payload: &'a serde_json::Value, key: &str) -> Option<&'a str> {
        payload.get(key).and_then(|v| v.as_str())
    }

    /// Convert VectorPoints into a RecordBatch for upsert.
    fn points_to_batch(points: &[VectorPoint], dims: usize) -> Result<RecordBatch> {
        let schema = Self::table_schema(dims);
        let n = points.len();

        // id
        let ids: Vec<&str> = points.iter().map(|p| p.id.as_str()).collect();
        let id_array = StringArray::from(ids);

        // vector
        let mut floats: Vec<f32> = Vec::with_capacity(n * dims);
        for p in points {
            if p.vector.len() != dims {
                return Err(MemcanError::DimensionMismatch {
                    expected: dims,
                    actual: p.vector.len(),
                });
            }
            floats.extend_from_slice(&p.vector);
        }
        let values = Float32Array::from(floats);
        let field = Arc::new(Field::new("item", DataType::Float32, true));
        let vector_array = FixedSizeListArray::new(field, dims as i32, Arc::new(values), None);

        // payload (LargeUtf8)
        let payloads: Vec<String> = points
            .iter()
            .map(|p| serde_json::to_string(&p.payload).unwrap_or_else(|_| "{}".into()))
            .collect();
        let payload_refs: Vec<&str> = payloads.iter().map(|s| s.as_str()).collect();
        let payload_array = LargeStringArray::from(payload_refs);

        // Filterable columns extracted from payload
        let user_ids: Vec<Option<&str>> = points
            .iter()
            .map(|p| Self::json_opt_str(&p.payload, "user_id"))
            .collect();
        let projects: Vec<Option<&str>> = points
            .iter()
            .map(|p| Self::json_opt_str(&p.payload, "project"))
            .collect();
        let standard_types: Vec<Option<&str>> = points
            .iter()
            .map(|p| Self::json_opt_str(&p.payload, "standard_type"))
            .collect();
        let standard_ids: Vec<Option<&str>> = points
            .iter()
            .map(|p| Self::json_opt_str(&p.payload, "standard_id"))
            .collect();
        let tech_stacks: Vec<Option<&str>> = points
            .iter()
            .map(|p| Self::json_opt_str(&p.payload, "tech_stack"))
            .collect();
        let file_paths: Vec<Option<&str>> = points
            .iter()
            .map(|p| Self::json_opt_str(&p.payload, "file_path"))
            .collect();
        let content_hashes: Vec<Option<&str>> = points
            .iter()
            .map(|p| Self::json_opt_str(&p.payload, "content_hash"))
            .collect();

        let batch = RecordBatch::try_new(
            schema,
            vec![
                Arc::new(id_array) as ArrayRef,
                Arc::new(vector_array) as ArrayRef,
                Arc::new(payload_array) as ArrayRef,
                Arc::new(StringArray::from(user_ids)) as ArrayRef,
                Arc::new(StringArray::from(projects)) as ArrayRef,
                Arc::new(StringArray::from(standard_types)) as ArrayRef,
                Arc::new(StringArray::from(standard_ids)) as ArrayRef,
                Arc::new(StringArray::from(tech_stacks)) as ArrayRef,
                Arc::new(StringArray::from(file_paths)) as ArrayRef,
                Arc::new(StringArray::from(content_hashes)) as ArrayRef,
            ],
        )?;
        Ok(batch)
    }

    /// Open an existing table by name.
    async fn open_table(&self, name: &str) -> Result<Table> {
        self.conn
            .open_table(name)
            .execute()
            .await
            .with_context(|| format!("failed to open table {name}"))
    }

    /// Infer vector dimensionality from an existing table's schema.
    async fn infer_dims(&self, table: &Table) -> Result<usize> {
        let schema = table.schema().await?;
        for field in schema.fields() {
            if field.name() == "vector"
                && let DataType::FixedSizeList(_, size) = field.data_type()
            {
                return Ok(*size as usize);
            }
        }
        Err(MemcanError::SchemaDimensions)
    }

    /// Extract SearchResults from a RecordBatch with optional distance column.
    fn batch_to_results(batch: &RecordBatch, has_distance: bool) -> Vec<SearchResult> {
        let id_col = batch
            .column_by_name("id")
            .and_then(|c| c.as_any().downcast_ref::<StringArray>());
        if id_col.is_none() {
            warn!(
                column = "id",
                expected = "StringArray",
                "column downcast failed, returning empty results"
            );
            return Vec::new();
        }

        let payload_col = batch
            .column_by_name("payload")
            .and_then(|c| c.as_any().downcast_ref::<LargeStringArray>());
        if payload_col.is_none() {
            warn!(
                column = "payload",
                expected = "LargeStringArray",
                "column downcast failed, returning empty results"
            );
            return Vec::new();
        }

        let dist_col = if has_distance {
            let col = batch
                .column_by_name("_distance")
                .and_then(|c| c.as_any().downcast_ref::<Float32Array>());
            if col.is_none() {
                warn!(
                    column = "_distance",
                    expected = "Float32Array",
                    "distance column downcast failed, scores will be 0"
                );
            }
            col
        } else {
            None
        };

        let id_col = id_col.unwrap();
        let payload_col = payload_col.unwrap();

        let n = batch.num_rows();
        let mut results = Vec::with_capacity(n);
        for i in 0..n {
            let id = id_col.value(i).to_string();
            let payload_str = payload_col.value(i).to_string();
            let payload: serde_json::Value = match serde_json::from_str(&payload_str) {
                Ok(v) => v,
                Err(e) => {
                    warn!(row = i, error = %e, "failed to parse payload JSON, using empty object");
                    serde_json::Value::Object(Default::default())
                }
            };
            let score = dist_col.map(|c| 1.0 / (1.0 + c.value(i))).unwrap_or(0.0);
            results.push(SearchResult { id, score, payload });
        }
        results
    }

    /// List all table names in the database.
    pub async fn table_names(&self) -> Result<Vec<String>> {
        self.conn
            .table_names()
            .execute()
            .await
            .context("failed to list table names")
    }
}

#[async_trait]
impl VectorStore for LanceDbStore {
    async fn ensure_table(&self, name: &str, dims: usize) -> Result<()> {
        let _guard = self.create_lock.lock().await;
        let names = self.conn.table_names().execute().await?;
        if names.contains(&name.to_string()) {
            debug!(name, "table already exists");
            let tbl = self.open_table(name).await?;
            let existing_dims = self.infer_dims(&tbl).await?;
            if existing_dims != dims {
                return Err(MemcanError::Config(format!(
                    "Table '{name}' has {existing_dims}-dimensional vectors but the configured \
                     embedding model produces {dims}-dimensional vectors. \
                     This usually means the embedding model changed. \
                     Migration is not yet supported — back up and recreate the table, \
                     or revert EMBED_MODEL to match the existing data."
                )));
            }
            return Ok(());
        }
        debug!(name, dims, "creating table");
        let schema = Self::table_schema(dims);
        let batches = RecordBatchIterator::new(vec![], schema.clone());
        self.conn
            .create_table(name, Box::new(batches))
            .execute()
            .await
            .with_context(|| format!("failed to create table {name}"))?;
        Ok(())
    }

    async fn upsert(&self, table: &str, points: &[VectorPoint]) -> Result<()> {
        if points.is_empty() {
            return Ok(());
        }
        let tbl = self.open_table(table).await?;
        let dims = self.infer_dims(&tbl).await?;

        // Delete existing rows with matching IDs (upsert = delete + add).
        let id_list: Vec<String> = points
            .iter()
            .map(|p| format!("'{}'", p.id.replace('\'', "''")))
            .collect();
        let filter = format!("id IN ({})", id_list.join(", "));
        let _ = tbl.delete(&filter).await; // ignore error if no rows match

        let batch = Self::points_to_batch(points, dims)?;
        let schema = batch.schema();
        let batches = RecordBatchIterator::new(vec![Ok(batch)], schema);
        tbl.add(Box::new(batches)).execute().await?;
        Ok(())
    }

    async fn search(
        &self,
        table: &str,
        vector: &[f32],
        filter: Option<&str>,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<SearchResult>> {
        let tbl = self.open_table(table).await?;
        let mut query = tbl
            .vector_search(vector)
            .context("vector search init failed")?;
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
            .context("vector search execute failed")?;

        let all: Vec<RecordBatch> = batches.try_collect().await?;
        let mut results = Vec::new();
        for batch in &all {
            results.extend(Self::batch_to_results(batch, true));
        }
        Ok(results)
    }

    async fn scroll(
        &self,
        table: &str,
        filter: Option<&str>,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<SearchResult>> {
        let tbl = self.open_table(table).await?;
        let mut query = tbl.query();
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
            .context("scroll execute failed")?;

        let all: Vec<RecordBatch> = batches.try_collect().await?;
        let mut results = Vec::new();
        for batch in &all {
            results.extend(Self::batch_to_results(batch, false));
        }
        Ok(results)
    }

    async fn count(&self, table: &str, filter: Option<&str>) -> Result<usize> {
        let tbl = self.open_table(table).await?;
        let mut query = tbl.query();
        if let Some(f) = filter {
            query = query.only_if(f);
        }
        let batches = query
            .select(Select::columns(&["id"]))
            .execute()
            .await
            .context("count execute failed")?;

        let all: Vec<RecordBatch> = batches.try_collect().await?;
        let total: usize = all.iter().map(|b| b.num_rows()).sum();
        Ok(total)
    }

    async fn delete(&self, table: &str, ids: &[String]) -> Result<()> {
        if ids.is_empty() {
            return Ok(());
        }
        let tbl = self.open_table(table).await?;
        let id_list: Vec<String> = ids
            .iter()
            .map(|id| format!("'{}'", id.replace('\'', "''")))
            .collect();
        let filter = format!("id IN ({})", id_list.join(", "));
        tbl.delete(&filter).await?;
        Ok(())
    }

    async fn delete_by_filter(&self, table: &str, filter: &str) -> Result<usize> {
        let before = self.count(table, Some(filter)).await?;
        if before > 0 {
            let tbl = self.open_table(table).await?;
            tbl.delete(filter).await?;
        }
        Ok(before)
    }

    async fn get(&self, table: &str, ids: &[String]) -> Result<Vec<SearchResult>> {
        if ids.is_empty() {
            return Ok(vec![]);
        }
        let id_list: Vec<String> = ids
            .iter()
            .map(|id| format!("'{}'", id.replace('\'', "''")))
            .collect();
        let filter = format!("id IN ({})", id_list.join(", "));
        self.scroll(table, Some(&filter), ids.len(), 0).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_schema_creation() {
        let schema = LanceDbStore::table_schema(768);
        assert_eq!(schema.fields().len(), 10);
        assert_eq!(schema.field(0).name(), "id");
        assert_eq!(schema.field(1).name(), "vector");
        assert_eq!(schema.field(2).name(), "payload");
        assert_eq!(*schema.field(2).data_type(), DataType::LargeUtf8);
        assert_eq!(schema.field(3).name(), "user_id");
        assert_eq!(schema.field(4).name(), "project");
        assert_eq!(schema.field(5).name(), "standard_type");
        assert_eq!(schema.field(6).name(), "standard_id");
        assert_eq!(schema.field(7).name(), "tech_stack");
        assert_eq!(schema.field(8).name(), "file_path");
        assert_eq!(schema.field(9).name(), "content_hash");
        match schema.field(1).data_type() {
            DataType::FixedSizeList(_, size) => assert_eq!(*size, 768),
            other => panic!("unexpected type: {:?}", other),
        }
    }

    #[test]
    fn test_json_opt_str() {
        let v = serde_json::json!({"user_id": "alice", "count": 42});
        assert_eq!(LanceDbStore::json_opt_str(&v, "user_id"), Some("alice"));
        assert_eq!(LanceDbStore::json_opt_str(&v, "missing"), None);
        assert_eq!(LanceDbStore::json_opt_str(&v, "count"), None);
    }

    #[test]
    fn test_points_to_batch() {
        let dims = 4;
        let points = vec![VectorPoint {
            id: "test-1".into(),
            vector: vec![1.0, 2.0, 3.0, 4.0],
            payload: serde_json::json!({"user_id": "bob", "data": "hello"}),
        }];
        let batch = LanceDbStore::points_to_batch(&points, dims).unwrap();
        assert_eq!(batch.num_rows(), 1);
        assert_eq!(batch.num_columns(), 10);
    }

    #[tokio::test]
    async fn test_ensure_table_dimension_mismatch() {
        let tmp = tempfile::tempdir().expect("create tempdir");
        let path = tmp.path().to_str().expect("tempdir path");
        let store = LanceDbStore::open(path).await.expect("open lancedb");

        store
            .ensure_table("test-dims", 384)
            .await
            .expect("create table with 384 dims");

        let err = store
            .ensure_table("test-dims", 1024)
            .await
            .expect_err("should fail with dimension mismatch");

        let msg = err.to_string();
        assert!(
            msg.contains("dimensional vectors"),
            "error should mention dimensional vectors, got: {msg}"
        );
        assert!(msg.contains("384"), "error should mention 384, got: {msg}");
        assert!(
            msg.contains("1024"),
            "error should mention 1024, got: {msg}"
        );
    }

    #[test]
    fn test_points_to_batch_dimension_mismatch() {
        let dims = 4;
        let points = vec![VectorPoint {
            id: "test-1".into(),
            vector: vec![1.0, 2.0, 3.0], // only 3, expected 4
            payload: serde_json::json!({}),
        }];
        let result = LanceDbStore::points_to_batch(&points, dims);
        assert!(result.is_err());
    }
}
