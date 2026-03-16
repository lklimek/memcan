//! Tests for the export_collection MCP tool behavior.
//!
//! QA-004: The server's export_collection tool duplicates core export logic
//! instead of delegating to `memcan_core::export::export_collection`.
//! This test documents the structural concern -- the tool inlines its own
//! scroll + record construction loop rather than calling the core function.
//!
//! QA-006: The export_collection and server export CLI both accept a raw SQL
//! filter string that is passed directly to LanceDB `only_if()`. The MCP tool
//! does not sanitize this input, unlike other tools (search_memories, etc.)
//! which use `sanitize_eq` / `sanitize_like`.

// These are documentation tests that verify the behavior concern exists.
// They don't test runtime behavior because the MemcanService requires
// full infrastructure setup. See serve.rs unit tests for param tests.
