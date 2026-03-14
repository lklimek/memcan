//! Memcan-specific [`TableSchema`] implementation.
//!
//! Defines the filterable columns used by memcan's memories, standards, code,
//! and todos tables. External consumers (e.g. penny) provide their own
//! [`TableSchema`] implementations with different column sets.

use arrow_schema::{DataType, Field};

use crate::traits::TableSchema;

/// The standard memcan table schema with filterable metadata columns.
///
/// All memcan tables (memories, standards, code, todos) share this schema.
/// The extra columns are extracted from the JSON payload at upsert time.
pub struct MemcanTableSchema;

impl MemcanTableSchema {
    /// Column names used for payload-field extraction.
    const COLUMNS: &[&str] = &[
        "user_id",
        "project",
        "standard_type",
        "standard_id",
        "tech_stack",
        "file_path",
        "content_hash",
        "status",
        "priority",
    ];
}

impl TableSchema for MemcanTableSchema {
    fn extra_fields(&self) -> Vec<Field> {
        Self::COLUMNS
            .iter()
            .map(|name| Field::new(*name, DataType::Utf8, true))
            .collect()
    }

    fn extract_columns(&self, payload: &serde_json::Value) -> Vec<Option<String>> {
        Self::COLUMNS
            .iter()
            .map(|key| payload.get(*key).and_then(|v| v.as_str()).map(String::from))
            .collect()
    }

    fn migration_columns(&self) -> Vec<String> {
        vec!["status".into(), "priority".into()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extra_fields_count() {
        let schema = MemcanTableSchema;
        assert_eq!(schema.extra_fields().len(), 9);
    }

    #[test]
    fn test_extract_columns_full() {
        let schema = MemcanTableSchema;
        let payload = serde_json::json!({
            "user_id": "alice",
            "project": "memcan",
            "standard_type": "security",
            "standard_id": "owasp",
            "tech_stack": "rust",
            "file_path": "src/main.rs",
            "content_hash": "abc123",
            "status": "pending",
            "priority": "high",
        });
        let cols = schema.extract_columns(&payload);
        assert_eq!(cols.len(), 9);
        assert_eq!(cols[0].as_deref(), Some("alice"));
        assert_eq!(cols[1].as_deref(), Some("memcan"));
        assert_eq!(cols[7].as_deref(), Some("pending"));
        assert_eq!(cols[8].as_deref(), Some("high"));
    }

    #[test]
    fn test_extract_columns_missing() {
        let schema = MemcanTableSchema;
        let payload = serde_json::json!({"data": "hello"});
        let cols = schema.extract_columns(&payload);
        assert_eq!(cols.len(), 9);
        assert!(cols.iter().all(|c| c.is_none()));
    }

    #[test]
    fn test_migration_columns() {
        let schema = MemcanTableSchema;
        let cols = schema.migration_columns();
        assert_eq!(cols, vec!["status", "priority"]);
    }
}
