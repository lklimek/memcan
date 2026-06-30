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
//! - `prompt_tokens`      — input tokens reported by the provider (when available)
//! - `completion_tokens`  — output tokens reported by the provider (when available)
//! - `total_tokens`       — sum of prompt + completion (only when both are present)
//!
//! When the provider returns only one counter, that field is logged without the
//! missing one (and without `total_tokens`). When neither is available, a single
//! debug line notes that counts are unavailable.

/// Computed telemetry values from provider-reported token counts.
///
/// This is a pure, allocation-free enum — no I/O, no side effects — so it is
/// independently unit-testable separate from the tracing machinery.
#[derive(Debug, PartialEq)]
pub(crate) enum TelemetryResult {
    /// Both prompt and completion counts are known; `total` is their sum.
    Full {
        prompt: u64,
        completion: u64,
        total: u64,
    },
    /// Only the prompt (input) count is known.
    PromptOnly { prompt: u64 },
    /// Only the completion (output) count is known.
    CompletionOnly { completion: u64 },
    /// Provider returned no token counts.
    Unavailable,
}

/// Classify raw provider counts into a [`TelemetryResult`].
///
/// This is the pure arithmetic/branching core of the telemetry pipeline.
/// `emit` calls this and dispatches tracing; tests can call this directly.
pub(crate) fn compute_telemetry(
    prompt_tokens: Option<u64>,
    completion_tokens: Option<u64>,
) -> TelemetryResult {
    match (prompt_tokens, completion_tokens) {
        (Some(p), Some(c)) => TelemetryResult::Full {
            prompt: p,
            completion: c,
            total: p + c,
        },
        (Some(p), None) => TelemetryResult::PromptOnly { prompt: p },
        (None, Some(c)) => TelemetryResult::CompletionOnly { completion: c },
        (None, None) => TelemetryResult::Unavailable,
    }
}

/// Emit a structured per-call token-usage line at `DEBUG` level.
///
/// `prompt_tokens` and `completion_tokens` are provider-reported values.
/// Pass `None` when the provider did not return a metric.
/// Partial counts (one `Some`, one `None`) are logged with whatever fields are
/// present — the missing field is omitted rather than collapsing to "unavailable".
pub(crate) fn emit(
    op: &str,
    model: &str,
    prompt_tokens: Option<u64>,
    completion_tokens: Option<u64>,
) {
    match compute_telemetry(prompt_tokens, completion_tokens) {
        TelemetryResult::Full {
            prompt,
            completion,
            total,
        } => {
            tracing::debug!(
                target: "memcan::llm::telemetry",
                op,
                model,
                prompt_tokens = prompt,
                completion_tokens = completion,
                total_tokens = total,
                "LLM token usage"
            );
        }
        TelemetryResult::PromptOnly { prompt } => {
            tracing::debug!(
                target: "memcan::llm::telemetry",
                op,
                model,
                prompt_tokens = prompt,
                "LLM token usage (completion count unavailable)"
            );
        }
        TelemetryResult::CompletionOnly { completion } => {
            tracing::debug!(
                target: "memcan::llm::telemetry",
                op,
                model,
                completion_tokens = completion,
                "LLM token usage (prompt count unavailable)"
            );
        }
        TelemetryResult::Unavailable => {
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

    // ── compute_telemetry contract tests (pure, no tracing needed) ────────────

    #[test]
    fn test_full_counts_and_total_equals_sum() {
        let result = compute_telemetry(Some(100), Some(50));
        assert_eq!(
            result,
            TelemetryResult::Full {
                prompt: 100,
                completion: 50,
                total: 150
            }
        );
        // Explicitly assert the arithmetic contract the module doc promises.
        if let TelemetryResult::Full {
            prompt,
            completion,
            total,
        } = result
        {
            assert_eq!(
                total,
                prompt + completion,
                "total_tokens must == prompt + completion"
            );
        }
    }

    #[test]
    fn test_total_equals_sum_for_large_values() {
        // Guard against integer overflow in the total computation path.
        let p: u64 = 1_000_000;
        let c: u64 = 500_000;
        let result = compute_telemetry(Some(p), Some(c));
        assert_eq!(
            result,
            TelemetryResult::Full {
                prompt: p,
                completion: c,
                total: p + c
            }
        );
    }

    #[test]
    fn test_zero_counts_are_full_not_unavailable() {
        // Zero is a valid token count — not the same as missing.
        assert_eq!(
            compute_telemetry(Some(0), Some(0)),
            TelemetryResult::Full {
                prompt: 0,
                completion: 0,
                total: 0
            }
        );
    }

    #[test]
    fn test_prompt_only() {
        assert_eq!(
            compute_telemetry(Some(42), None),
            TelemetryResult::PromptOnly { prompt: 42 }
        );
    }

    #[test]
    fn test_completion_only() {
        assert_eq!(
            compute_telemetry(None, Some(99)),
            TelemetryResult::CompletionOnly { completion: 99 }
        );
    }

    #[test]
    fn test_both_none_is_unavailable() {
        assert_eq!(compute_telemetry(None, None), TelemetryResult::Unavailable);
    }

    // ── emit smoke tests — assert no panic for all variants ──────────────────
    // (tracing output capture would require a dev-dep; the arithmetic contract
    // above is tested through compute_telemetry which emit delegates to.)

    #[test]
    fn test_emit_full_does_not_panic() {
        emit("fact_extraction", "qwen3.5:9b", Some(100), Some(50));
    }

    #[test]
    fn test_emit_prompt_only_does_not_panic() {
        emit("dedup", "qwen3.5:9b", Some(200), None);
    }

    #[test]
    fn test_emit_completion_only_does_not_panic() {
        emit("code_description", "qwen3.5:9b", None, Some(30));
    }

    #[test]
    fn test_emit_unavailable_does_not_panic() {
        emit("standards_metadata", "qwen3.5:9b", None, None);
    }
}
