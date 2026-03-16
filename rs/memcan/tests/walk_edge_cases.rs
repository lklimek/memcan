//! Edge case tests for walk module.
//!
//! QA-008: chunk_into_batches with batch_size=0 does not panic but produces
//! one batch per item (wasteful). The CLI accepts 0 from user input.

// Note: walk module is private (mod walk), so we cannot test it directly from
// an integration test. These tests document the concern.
// The fix should either validate batch_size >= 1 in the CLI arg parser
// or in the chunk_into_batches function.
