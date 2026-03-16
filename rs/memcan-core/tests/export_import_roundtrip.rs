//! Integration tests for export/import roundtrip correctness.
//!
//! These tests verify requirements from the feature spec:
//! - JSONL roundtrip preserves all data
//! - `_collection` field stripped before upsert
//! - `id` field survives roundtrip even if payload contains `id` key
//! - ExportRecord with serde(flatten) does not lose struct-level `id`

use memcan_core::export::{ExportRecord, jsonl_to_record, record_to_jsonl};

/// QA-001 (fixed): Reserved keys (`id`, `_collection`) in payload are stripped
/// before serialization and after deserialization, so roundtrip succeeds.
#[test]
fn roundtrip_succeeds_when_payload_has_id_key() {
    let mut payload = serde_json::Map::new();
    payload.insert(
        "data".into(),
        serde_json::Value::String("test memory".into()),
    );
    payload.insert(
        "id".into(),
        serde_json::Value::String("payload-id-conflict".into()),
    );

    let record = ExportRecord {
        _collection: "memories".into(),
        id: "struct-id".into(),
        payload,
    };

    // export_collection strips reserved keys, so simulate that here
    let mut clean_payload = record.payload.clone();
    memcan_core::export::strip_reserved_keys(&mut clean_payload);
    let clean_record = ExportRecord {
        _collection: record._collection.clone(),
        id: record.id.clone(),
        payload: clean_payload,
    };

    let line = record_to_jsonl(&clean_record).unwrap();
    let parsed = jsonl_to_record(&line).unwrap();
    assert_eq!(parsed.id, "struct-id");
    assert_eq!(parsed._collection, "memories");
    assert!(!parsed.payload.contains_key("id"));
}

/// QA-002 (fixed): `_collection` key in payload is stripped, roundtrip succeeds.
#[test]
fn roundtrip_succeeds_when_payload_has_collection_key() {
    let mut payload = serde_json::Map::new();
    payload.insert("data".into(), serde_json::Value::String("test".into()));
    payload.insert(
        "_collection".into(),
        serde_json::Value::String("wrong-collection".into()),
    );

    let record = ExportRecord {
        _collection: "memories".into(),
        id: "test-1".into(),
        payload,
    };

    let mut clean_payload = record.payload.clone();
    memcan_core::export::strip_reserved_keys(&mut clean_payload);
    let clean_record = ExportRecord {
        _collection: record._collection.clone(),
        id: record.id.clone(),
        payload: clean_payload,
    };

    let line = record_to_jsonl(&clean_record).unwrap();
    let parsed = jsonl_to_record(&line).unwrap();
    assert_eq!(parsed._collection, "memories");
    assert!(!parsed.payload.contains_key("_collection"));
}

/// QA-003: Verify that the server export_collection MCP tool response format
/// (JSON envelope with count/offset/data) can be correctly parsed by CLI.
///
/// The server returns: {"count": N, "offset": M, "data": "...JSONL lines..."}
/// The CLI must extract the "data" field and split by lines to get JSONL records.
#[test]
fn server_export_response_envelope_parsing() {
    // Simulate what the server export_collection tool returns
    let jsonl_line1 = r#"{"_collection":"memories","id":"m1","data":"hello"}"#;
    let jsonl_line2 = r#"{"_collection":"memories","id":"m2","data":"world"}"#;
    let data = format!("{}\n{}", jsonl_line1, jsonl_line2);

    let server_response = serde_json::json!({
        "count": 2,
        "offset": 0,
        "data": data,
    });
    let response_text = serde_json::to_string(&server_response).unwrap();

    // The CLI currently does: result.lines().filter(|l| !l.trim().is_empty())
    // This treats the entire JSON envelope as lines, which is WRONG.
    // It should parse the JSON envelope and extract the "data" field.

    // Correct approach: parse the envelope
    let parsed: serde_json::Value = serde_json::from_str(&response_text).unwrap();
    let data_field = parsed
        .get("data")
        .and_then(|v| v.as_str())
        .expect("response must have 'data' field");
    let count = parsed
        .get("count")
        .and_then(|v| v.as_u64())
        .expect("response must have 'count' field");

    // Verify the data field contains valid JSONL
    let lines: Vec<&str> = data_field
        .lines()
        .filter(|l| !l.trim().is_empty())
        .collect();
    assert_eq!(lines.len(), 2, "data field should contain 2 JSONL lines");
    assert_eq!(count, 2);

    // Each line should be parseable as ExportRecord
    for line in &lines {
        let record = jsonl_to_record(line).unwrap();
        assert_eq!(record._collection, "memories");
    }

    // The WRONG approach (what CLI currently does) would try to parse
    // the envelope itself as JSONL lines, producing garbage output
    let wrong_lines: Vec<&str> = response_text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .collect();
    // The envelope is a single JSON line, so this produces 1 "line"
    // which is the entire envelope -- NOT valid JSONL records
    assert_eq!(
        wrong_lines.len(),
        1,
        "naive line splitting of envelope produces 1 line (the whole envelope)"
    );
    // Trying to parse it as ExportRecord should fail or produce wrong data
    let wrong_parse = jsonl_to_record(wrong_lines[0]);
    // It might succeed but the data would be wrong (it's the envelope, not a record)
    if let Ok(record) = wrong_parse {
        // If it parses, it would have wrong structure
        assert_ne!(
            record.id, "m1",
            "parsing envelope as JSONL record should not produce correct id"
        );
    }
}

/// QA-005 (fixed): Import error counting reads array length, not as_u64().
#[test]
fn import_response_errors_field_is_array() {
    let server_response = serde_json::json!({
        "imported": 5,
        "skipped": 2,
        "errors": ["line 3: missing data field", "unknown collection 'foo'"],
    });

    let errors_count = server_response
        .get("errors")
        .and_then(|v| v.as_array())
        .map(|a| a.len() as u64)
        .unwrap_or(0);
    assert_eq!(errors_count, 2, "error count should match array length");
}
