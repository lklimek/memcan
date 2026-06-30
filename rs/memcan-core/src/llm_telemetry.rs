//! Structured per-call LLM token telemetry.
//!
//! Both LLM provider backends (ollama-rs and genai) call [`emit`] after a
//! successful chat request. All telemetry is emitted at `DEBUG` level under
//! the target `"memcan::llm::telemetry"`, so operators can enable it
//! independently:
//!
//! ```text
//! RUST_LOG=memcan::llm::telemetry=debug
//! ```
//!
//! Each log record carries structured fields:
//! - `op`                 — operation label (e.g. `"fact_extraction"`, `"code_description"`)
//! - `model`              — model name as sent to the provider
//! - `prompt_tokens`      — input tokens reported by the provider
//! - `completion_tokens`  — output tokens reported by the provider
//! - `total_tokens`       — sum of prompt + completion tokens
//!
//! When the provider does not return token counts (e.g. network error recovery
//! path, provider misconfiguration), a single debug line is emitted noting that
//! counts are unavailable. No fallback estimation is performed.

/// Emit a structured per-call token-usage line at `DEBUG` level.
///
/// `prompt_tokens` and `completion_tokens` are provider-reported values.
/// Pass `None` when the provider did not return a metric.
pub(crate) fn emit(
    op: &str,
    model: &str,
    prompt_tokens: Option<u64>,
    completion_tokens: Option<u64>,
) {
    match (prompt_tokens, completion_tokens) {
        (Some(p), Some(c)) => {
            tracing::debug!(
                target: "memcan::llm::telemetry",
                op,
                model,
                prompt_tokens = p,
                completion_tokens = c,
                total_tokens = p + c,
                "LLM token usage"
            );
        }
        _ => {
            tracing::debug!(
                target: "memcan::llm::telemetry",
                op,
                model,
                "LLM token usage unavailable (provider did not report counts)"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verify `emit` does not panic for any combination of Some/None inputs.
    #[test]
    fn test_emit_does_not_panic_with_counts() {
        emit("fact_extraction", "qwen3.5:9b", Some(100), Some(50));
    }

    #[test]
    fn test_emit_does_not_panic_without_counts() {
        emit("dedup", "qwen3.5:9b", None, None);
    }

    #[test]
    fn test_emit_does_not_panic_partial_counts() {
        emit("code_description", "qwen3.5:9b", Some(200), None);
        emit("standards_metadata", "qwen3.5:9b", None, Some(30));
    }
}
