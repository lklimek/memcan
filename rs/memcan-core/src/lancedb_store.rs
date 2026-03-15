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
use lancedb::table::NewColumnTransform;
use lancedb::{Connection, Table, connect};
use tokio::sync::Mutex;
use tracing::{debug, warn};

use lancedb::table::OptimizeAction;

use crate::error::{MemcanError, Result, ResultExt, VectorStoreError};
use crate::traits::{SearchResult, TableSchema, VectorPoint, VectorStore};

// Re-import for typed_table() factory method.
use crate::typed_table::TypedTable;

/// Classify a raw LanceDB error, detecting stale handles and missing tables.
pub(crate) fn classify_lancedb_error(table_name: &str, err: lancedb::Error) -> VectorStoreError {
    let msg = err.to_string();
    if msg.contains("not found") || msg.contains("does not exist") {
        VectorStoreError::StaleHandle {
            table: table_name.to_string(),
            reason: msg,
        }
    } else {
        VectorStoreError::Store(err)
    }
}

/// Build a SQL `id IN (...)` filter with single-quote escaping.
pub(crate) fn build_id_filter<'a>(ids: impl Iterator<Item = &'a str>) -> String {
    let escaped: Vec<String> = ids
        .map(|id| format!("'{}'", id.replace('\'', "''")))
        .collect();
    format!("id IN ({})", escaped.join(", "))
}

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

    /// Build the Arrow schema for a table with the given vector dimensionality
    /// and extra filterable columns from the provided [`TableSchema`].
    fn build_arrow_schema(
        dims: usize,
        table_schema: &dyn TableSchema,
    ) -> std::result::Result<Arc<Schema>, VectorStoreError> {
        let dims_i32 = i32::try_from(dims).map_err(|_| VectorStoreError::SchemaMismatch {
            table: "<schema>".into(),
            detail: format!("dims {dims} exceeds i32::MAX"),
        })?;
        let mut fields = vec![
            Field::new("id", DataType::Utf8, false),
            Field::new(
                "vector",
                DataType::FixedSizeList(
                    Arc::new(Field::new("item", DataType::Float32, true)),
                    dims_i32,
                ),
                false,
            ),
            Field::new("payload", DataType::LargeUtf8, false),
        ];
        fields.extend(table_schema.extra_fields());
        Ok(Arc::new(Schema::new(fields)))
    }

    /// Convert VectorPoints into a RecordBatch for upsert.
    pub(crate) fn points_to_batch(
        points: &[VectorPoint],
        dims: usize,
        table_schema: &dyn TableSchema,
    ) -> Result<RecordBatch> {
        let schema = Self::build_arrow_schema(dims, table_schema)
            .map_err(|e| MemcanError::Config(e.to_string()))?;
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
        let dims_i32 = i32::try_from(dims)
            .map_err(|_| MemcanError::Config(format!("dims {dims} exceeds i32::MAX")))?;
        let values = Float32Array::from(floats);
        let field = Arc::new(Field::new("item", DataType::Float32, true));
        let vector_array = FixedSizeListArray::new(field, dims_i32, Arc::new(values), None);

        // payload (LargeUtf8)
        let payloads: Vec<String> = points
            .iter()
            .map(|p| serde_json::to_string(&p.payload).unwrap_or_else(|_| "{}".into()))
            .collect();
        let payload_refs: Vec<&str> = payloads.iter().map(|s| s.as_str()).collect();
        let payload_array = LargeStringArray::from(payload_refs);

        let mut arrays: Vec<ArrayRef> = vec![
            Arc::new(id_array),
            Arc::new(vector_array),
            Arc::new(payload_array),
        ];

        // Extra filterable columns from TableSchema
        let extra_fields = table_schema.extra_fields();
        if !extra_fields.is_empty() {
            let mut columns: Vec<Vec<Option<String>>> = Vec::with_capacity(points.len());
            for p in points {
                let cols = table_schema.extract_columns(&p.payload);
                if cols.len() != extra_fields.len() {
                    return Err(MemcanError::Config(format!(
                        "extract_columns returned {} values but extra_fields has {} for point '{}'",
                        cols.len(),
                        extra_fields.len(),
                        p.id,
                    )));
                }
                columns.push(cols);
            }

            for col_idx in 0..extra_fields.len() {
                let col_values: Vec<Option<&str>> = columns
                    .iter()
                    .map(|row| row.get(col_idx).and_then(|v| v.as_deref()))
                    .collect();
                arrays.push(Arc::new(StringArray::from(col_values)) as ArrayRef);
            }
        }

        let batch = RecordBatch::try_new(schema, arrays)?;
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

    /// Configure Lance's built-in auto-cleanup hook on a table.
    ///
    /// Runs on every commit: prunes versions older than 1 day every 20 commits,
    /// keeping at least 5 recent versions.
    async fn ensure_auto_cleanup(table: &Table) {
        let Some(native) = table.as_native() else {
            warn!("table is not a NativeTable; skipping auto-cleanup config");
            return;
        };
        if let Err(e) = native
            .update_config([
                ("lance.auto_cleanup.interval".to_string(), "20".to_string()),
                (
                    "lance.auto_cleanup.older_than".to_string(),
                    "1d".to_string(),
                ),
                (
                    "lance.auto_cleanup.retain_versions".to_string(),
                    "5".to_string(),
                ),
            ])
            .await
        {
            warn!("failed to set auto-cleanup config: {e}");
        } else {
            tracing::debug!(
                table = table.name(),
                "lance auto-cleanup configured: interval=20, older_than=1d, retain=5"
            );
        }
    }

    /// Prune old versions from a single table (one-time cleanup for existing backlog).
    pub async fn compact_table(&self, table_name: &str) -> Result<()> {
        let table = self
            .conn
            .open_table(table_name)
            .execute()
            .await
            .with_context(|| format!("failed to open table {table_name} for compaction"))?;
        table
            .optimize(OptimizeAction::Prune {
                older_than: Some(chrono::Duration::days(1)),
                delete_unverified: Some(false),
                error_if_tagged_old_versions: Some(false),
            })
            .await
            .with_context(|| format!("failed to prune table {table_name}"))?;
        Ok(())
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
    pub(crate) fn batch_to_results(batch: &RecordBatch, has_distance: bool) -> Vec<SearchResult> {
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

    /// Open (or create) a typed table handle.
    ///
    /// Ensures the table exists with the correct schema, then returns a
    /// [`TypedTable`] bound to the given [`TableSchema`].
    pub async fn typed_table<S: TableSchema>(
        &self,
        name: &str,
        schema: S,
        dims: usize,
    ) -> std::result::Result<TypedTable<S>, VectorStoreError> {
        self.ensure_table(name, dims, &schema).await.map_err(|e| {
            VectorStoreError::SchemaMismatch {
                table: name.to_string(),
                detail: e.to_string(),
            }
        })?;
        let table = self
            .conn
            .open_table(name)
            .execute()
            .await
            .map_err(|e| classify_lancedb_error(name, e))?;
        Ok(TypedTable::new(name.into(), table, schema, dims))
    }
}

#[async_trait]
impl VectorStore for LanceDbStore {
    async fn ensure_table(
        &self,
        name: &str,
        dims: usize,
        table_schema: &dyn TableSchema,
    ) -> Result<()> {
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

            // Migrate: add columns listed in migration_columns() if missing.
            let schema = tbl.schema().await?;
            let field_names: Vec<&str> =
                schema.fields().iter().map(|f| f.name().as_str()).collect();
            let missing: Vec<(String, String)> = table_schema
                .migration_columns()
                .into_iter()
                .filter(|col| !field_names.contains(&col.as_str()))
                .map(|col| (col, "CAST(NULL AS STRING)".to_string()))
                .collect();
            if !missing.is_empty() {
                let col_names: Vec<&str> = missing.iter().map(|(n, _)| n.as_str()).collect();
                warn!(
                    table = name,
                    columns = ?col_names,
                    "migrating table: adding missing columns"
                );
                tbl.add_columns(NewColumnTransform::SqlExpressions(missing), None)
                    .await
                    .with_context(|| format!("failed to add missing columns to table {name}"))?;
            }

            Self::ensure_auto_cleanup(&tbl).await;
            return Ok(());
        }
        debug!(name, dims, "creating table");
        let schema = Self::build_arrow_schema(dims, table_schema)
            .map_err(|e| MemcanError::Config(e.to_string()))?;
        let batches = RecordBatchIterator::new(vec![], schema.clone());
        let tbl = self
            .conn
            .create_table(name, Box::new(batches))
            .execute()
            .await
            .with_context(|| format!("failed to create table {name}"))?;
        Self::ensure_auto_cleanup(&tbl).await;
        Ok(())
    }

    /// **Deprecated:** use [`TypedTable`] via [`typed_table()`](Self::typed_table).
    async fn upsert(
        &self,
        table: &str,
        points: &[VectorPoint],
        table_schema: &dyn TableSchema,
    ) -> Result<()> {
        if points.is_empty() {
            return Ok(());
        }
        let tbl = self.open_table(table).await?;
        let dims = self.infer_dims(&tbl).await?;

        let filter = build_id_filter(points.iter().map(|p| p.id.as_str()));
        let _ = tbl.delete(&filter).await; // ignore error if no rows match

        let batch = Self::points_to_batch(points, dims, table_schema)?;
        let schema = batch.schema();
        let batches = RecordBatchIterator::new(vec![Ok(batch)], schema);
        tbl.add(Box::new(batches)).execute().await?;
        Ok(())
    }

    /// **Deprecated:** use [`TypedTable`] via [`typed_table()`](Self::typed_table).
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

    /// **Deprecated:** use [`TypedTable`] via [`typed_table()`](Self::typed_table).
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

    /// **Deprecated:** use [`TypedTable`] via [`typed_table()`](Self::typed_table).
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

    /// **Deprecated:** use [`TypedTable`] via [`typed_table()`](Self::typed_table).
    async fn delete(&self, table: &str, ids: &[String]) -> Result<()> {
        if ids.is_empty() {
            return Ok(());
        }
        let tbl = self.open_table(table).await?;
        let filter = build_id_filter(ids.iter().map(|id| id.as_str()));
        tbl.delete(&filter).await?;
        Ok(())
    }

    /// **Deprecated:** use [`TypedTable`] via [`typed_table()`](Self::typed_table).
    async fn delete_by_filter(&self, table: &str, filter: &str) -> Result<usize> {
        let before = self.count(table, Some(filter)).await?;
        if before > 0 {
            let tbl = self.open_table(table).await?;
            tbl.delete(filter).await?;
        }
        Ok(before)
    }

    /// **Deprecated:** use [`TypedTable`] via [`typed_table()`](Self::typed_table).
    async fn get(&self, table: &str, ids: &[String]) -> Result<Vec<SearchResult>> {
        if ids.is_empty() {
            return Ok(vec![]);
        }
        let filter = build_id_filter(ids.iter().map(|id| id.as_str()));
        self.scroll(table, Some(&filter), ids.len(), 0).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::MemcanTableSchema;
    use crate::traits::MinimalTableSchema;

    #[test]
    fn test_schema_creation() {
        let ts = MemcanTableSchema;
        let schema = LanceDbStore::build_arrow_schema(768, &ts).unwrap();
        assert_eq!(schema.fields().len(), 12);
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
        assert_eq!(schema.field(10).name(), "status");
        assert_eq!(schema.field(11).name(), "priority");
        match schema.field(1).data_type() {
            DataType::FixedSizeList(_, size) => assert_eq!(*size, 768),
            other => panic!("unexpected type: {:?}", other),
        }
    }

    #[test]
    fn test_minimal_schema_creation() {
        let ts = MinimalTableSchema;
        let schema = LanceDbStore::build_arrow_schema(384, &ts).unwrap();
        assert_eq!(schema.fields().len(), 3);
        assert_eq!(schema.field(0).name(), "id");
        assert_eq!(schema.field(1).name(), "vector");
        assert_eq!(schema.field(2).name(), "payload");
    }

    #[test]
    fn test_points_to_batch() {
        let dims = 4;
        let ts = MemcanTableSchema;
        let points = vec![VectorPoint {
            id: "test-1".into(),
            vector: vec![1.0, 2.0, 3.0, 4.0],
            payload: serde_json::json!({"user_id": "bob", "data": "hello"}),
        }];
        let batch = LanceDbStore::points_to_batch(&points, dims, &ts).unwrap();
        assert_eq!(batch.num_rows(), 1);
        assert_eq!(batch.num_columns(), 12);
    }

    #[test]
    fn test_points_to_batch_minimal() {
        let dims = 4;
        let ts = MinimalTableSchema;
        let points = vec![VectorPoint {
            id: "test-1".into(),
            vector: vec![1.0, 2.0, 3.0, 4.0],
            payload: serde_json::json!({"key": "value"}),
        }];
        let batch = LanceDbStore::points_to_batch(&points, dims, &ts).unwrap();
        assert_eq!(batch.num_rows(), 1);
        assert_eq!(batch.num_columns(), 3);
    }

    #[tokio::test]
    async fn test_ensure_table_dimension_mismatch() {
        let tmp = tempfile::tempdir().expect("create tempdir");
        let path = tmp.path().to_str().expect("tempdir path");
        let store = LanceDbStore::open(path).await.expect("open lancedb");
        let ts = MinimalTableSchema;

        store
            .ensure_table("test-dims", 384, &ts)
            .await
            .expect("create table with 384 dims");

        let err = store
            .ensure_table("test-dims", 1024, &ts)
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
        let ts = MinimalTableSchema;
        let points = vec![VectorPoint {
            id: "test-1".into(),
            vector: vec![1.0, 2.0, 3.0], // only 3, expected 4
            payload: serde_json::json!({}),
        }];
        let result = LanceDbStore::points_to_batch(&points, dims, &ts);
        assert!(result.is_err());
    }

    #[test]
    fn test_points_to_batch_extracts_status_and_priority() {
        let dims = 4;
        let ts = MemcanTableSchema;
        let points = vec![VectorPoint {
            id: "todo-1".into(),
            vector: vec![1.0, 2.0, 3.0, 4.0],
            payload: serde_json::json!({
                "status": "pending",
                "priority": "high",
                "project": "test",
            }),
        }];
        let batch = LanceDbStore::points_to_batch(&points, dims, &ts).unwrap();
        let status_col = batch
            .column_by_name("status")
            .and_then(|c| c.as_any().downcast_ref::<StringArray>())
            .expect("status column");
        assert_eq!(status_col.value(0), "pending");
        let priority_col = batch
            .column_by_name("priority")
            .and_then(|c| c.as_any().downcast_ref::<StringArray>())
            .expect("priority column");
        assert_eq!(priority_col.value(0), "high");
    }

    #[tokio::test]
    async fn test_ensure_table_migrates_missing_columns() {
        let tmp = tempfile::tempdir().expect("create tempdir");
        let path = tmp.path().to_str().expect("tempdir path");
        let dims: usize = 4;

        // Create a table with the OLD schema (without status/priority).
        let old_schema = Arc::new(Schema::new(vec![
            Field::new("id", DataType::Utf8, false),
            Field::new(
                "vector",
                DataType::FixedSizeList(Arc::new(Field::new("item", DataType::Float32, true)), 4),
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
        ]));

        let conn = connect(path).execute().await.expect("connect");
        let batches = RecordBatchIterator::new(vec![], old_schema.clone());
        conn.create_table("migrate_test", Box::new(batches))
            .execute()
            .await
            .expect("create old-schema table");

        // Now open via LanceDbStore and ensure_table should migrate.
        let store = LanceDbStore::open(path).await.expect("open");
        let ts = MemcanTableSchema;
        store
            .ensure_table("migrate_test", dims, &ts)
            .await
            .expect("ensure_table should migrate");

        // Verify columns were added.
        let tbl = store.open_table("migrate_test").await.expect("open table");
        let schema = tbl.schema().await.expect("schema");
        let names: Vec<&str> = schema.fields().iter().map(|f| f.name().as_str()).collect();
        assert!(
            names.contains(&"status"),
            "status column missing after migration"
        );
        assert!(
            names.contains(&"priority"),
            "priority column missing after migration"
        );
    }
}
