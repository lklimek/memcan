//! Integration tests for export/import roundtrip correctness.
//!
//! These tests verify requirements from the feature spec:
//! - JSONL roundtrip preserves all data
//! - `_collection` field stripped before upsert
//! - `id` field survives roundtrip even if payload contains `id` key
//! - ExportRecord with serde(flatten) does not lose struct-level `id`

use memcan_core::export::{ExportRecord, jsonl_to_record, record_to_jsonl};

/// QA-001: If the payload map contains an `id` key (same name as the struct field),
/// serde flatten causes duplicate JSON keys in serialized output. On deserialization,
/// serde rejects the duplicate field with an error.
///
/// This means: if any LanceDB payload ever contains an `id` field, export->import
/// roundtrip will fail with a deserialization error, causing data loss.
///
/// Expected behavior: export should strip conflicting keys from payload, or the
/// struct should not use `#[serde(flatten)]`.
#[test]
fn roundtrip_fails_when_payload_has_id_key() {
    let mut payload = serde_json::Map::new();
    payload.insert(
        "data".into(),
        serde_json::Value::String("test memory".into()),
    );
    // Simulate a payload that also contains an "id" field
    payload.insert(
        "id".into(),
        serde_json::Value::String("payload-id-conflict".into()),
    );

    let record = ExportRecord {
        _collection: "memories".into(),
        id: "struct-id".into(),
        payload,
    };

    let line = record_to_jsonl(&record).unwrap();
    // Deserialization fails due to duplicate `id` key from serde(flatten)
    let result = jsonl_to_record(&line);
    assert!(
        result.is_err(),
        "serde(flatten) produces duplicate `id` keys causing deserialization failure"
    );
    let err_msg = format!("{}", result.unwrap_err());
    assert!(
        err_msg.contains("duplicate field"),
        "error should mention duplicate field, got: {err_msg}"
    );
}

/// QA-002: Same issue as QA-001 but for `_collection` key.
/// If payload contains `_collection`, serde flatten produces duplicate keys.
#[test]
fn roundtrip_fails_when_payload_has_collection_key() {
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

    let line = record_to_jsonl(&record).unwrap();
    let result = jsonl_to_record(&line);
    assert!(
        result.is_err(),
        "serde(flatten) produces duplicate `_collection` keys causing deserialization failure"
    );
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

/// QA-005: Import error counting -- the server returns `errors` as an array,
/// not a number. The CLI uses `as_u64()` which returns None for arrays.
#[test]
fn import_response_errors_field_is_array_not_number() {
    // Simulate what the server _import_records tool returns
    let server_response = serde_json::json!({
        "imported": 5,
        "skipped": 2,
        "errors": ["line 3: missing data field", "unknown collection 'foo'"],
    });

    // The CLI does: parsed.get("errors").and_then(|v| v.as_u64()).unwrap_or(0)
    let errors_as_u64 = server_response
        .get("errors")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);

    // This always returns 0 because "errors" is an array, not a number
    assert_eq!(
        errors_as_u64, 0,
        "as_u64() on an array returns None, so errors are always counted as 0"
    );

    // The correct approach: count the array length
    let errors_count = server_response
        .get("errors")
        .and_then(|v| v.as_array())
        .map(|a| a.len() as u64)
        .unwrap_or(0);
    assert_eq!(errors_count, 2, "correct error count should be 2");
}
