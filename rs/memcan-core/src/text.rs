//! Text utility helpers shared across `pipeline` and `search`.

use std::borrow::Cow;

/// Truncate `s` to at most `max` Unicode scalar values (characters), appending
/// `suffix` when a cut is made.
///
/// Properties:
/// - **Char-boundary safe** — never byte-slices; no panic on multibyte input.
/// - **Single-pass** — `char_indices().nth(max)` locates the cut in one scan.
/// - **Allocation-free on the happy path** — when `s` fits within `max` the
///   original string slice is returned as `Cow::Borrowed` without any heap
///   allocation; only a truncated result causes allocation.
///
/// # Examples
/// ```
/// use std::borrow::Cow;
/// use memcan_core::text::truncate_with;
///
/// // Fits — zero allocation, original slice borrowed.
/// assert!(matches!(truncate_with("hello", 10, "…"), Cow::Borrowed("hello")));
///
/// // Truncated — suffix appended.
/// let t = truncate_with("hello world", 5, "…");
/// assert_eq!(&*t, "hello…");
///
/// // Multibyte safe.
/// let s = "ł日".repeat(50); // 100 chars
/// let t = truncate_with(&s, 80, "");
/// assert_eq!(t.chars().count(), 80);
/// ```
pub fn truncate_with<'a>(s: &'a str, max: usize, suffix: &str) -> Cow<'a, str> {
    match s.char_indices().nth(max) {
        None => Cow::Borrowed(s),
        Some((byte_idx, _)) => {
            let mut out = String::with_capacity(byte_idx + suffix.len());
            out.push_str(&s[..byte_idx]);
            out.push_str(suffix);
            Cow::Owned(out)
        }
    }
}

/// Strip a surrounding markdown code fence from `s`, returning the inner body.
///
/// Some LLMs wrap their answer in a ` ```json … ``` ` (or bare ` ``` … ``` `)
/// fence even when asked for raw JSON output. This removes a single leading
/// fence line — including an optional language tag such as `json` — and the
/// matching trailing fence, so the body can be parsed directly. Input that is
/// not fenced is returned trimmed and otherwise unchanged (no-op passthrough).
///
/// The returned slice always borrows from `s`; no allocation is performed.
///
/// # Examples
/// ```
/// use memcan_core::text::strip_code_fence;
///
/// assert_eq!(strip_code_fence("```json\n{\"a\":1}\n```"), "{\"a\":1}");
/// assert_eq!(strip_code_fence("```\n{\"a\":1}\n```"), "{\"a\":1}");
/// assert_eq!(strip_code_fence("{\"a\":1}"), "{\"a\":1}");
/// ```
pub fn strip_code_fence(s: &str) -> &str {
    let trimmed = s.trim();
    let Some(after_open) = trimmed.strip_prefix("```") else {
        return trimmed;
    };
    // Discard an optional language tag (a contiguous alphanumeric run right
    // after the opening fence, e.g. `json`) — regardless of whether the body
    // starts on the same line (`` ```json {...}\n``` ``) or the next one
    // (`` ```json\n{...}\n``` ``). Splitting on the first newline instead
    // would wrongly swallow a same-line body into the "tag" when the fence
    // only closes on a later line.
    let body = after_open.trim_start_matches(|c: char| c.is_ascii_alphanumeric());
    let body = body.trim();
    body.strip_suffix("```").map_or(body, str::trim)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Fits within cap — must return Borrowed (no allocation).
    #[test]
    fn test_fits_returns_borrowed() {
        let s = "hello";
        assert!(
            matches!(truncate_with(s, 10, "…"), Cow::Borrowed(_)),
            "value within cap must be Borrowed"
        );
    }

    // Exact boundary — 5 chars, cap=5 → Borrowed.
    #[test]
    fn test_exact_boundary_not_truncated() {
        let s = "hello";
        let result = truncate_with(s, 5, "…");
        assert_eq!(&*result, "hello", "exactly-at-cap must be unchanged");
        assert!(!result.ends_with('…'));
    }

    // One over cap — 6 chars, cap=5 → truncated + suffix.
    #[test]
    fn test_one_over_cap_truncated() {
        let result = truncate_with("hello!", 5, "…");
        assert_eq!(&*result, "hello…");
        assert!(matches!(result, Cow::Owned(_)));
    }

    // Empty suffix → plain truncation, no trailing chars.
    #[test]
    fn test_empty_suffix() {
        let result = truncate_with("hello world", 5, "");
        assert_eq!(&*result, "hello");
    }

    // Multi-char ASCII suffix ("...").
    #[test]
    fn test_ascii_dot_suffix() {
        let result = truncate_with("abcde", 3, "...");
        assert_eq!(&*result, "abc...");
    }

    // Multibyte chars (Polish + CJK) — no byte-slicing panic.
    #[test]
    fn test_multibyte_no_panic() {
        let s = "ł日".repeat(50); // 100 chars, 150 bytes
        assert!(s.len() > 80, "must exceed 80 bytes to exercise the guard");
        let result = truncate_with(&s, 80, "");
        assert_eq!(result.chars().count(), 80, "must cap at 80 chars");
        // Validity: re-iterating must not panic.
        let _ = result.chars().count();
    }

    // Exactly at multibyte cap — no truncation.
    #[test]
    fn test_multibyte_at_cap_unchanged() {
        let s = "ł日".repeat(10); // 20 chars
        let result = truncate_with(&s, 20, "…");
        assert_eq!(result.chars().count(), 20);
        assert!(!result.ends_with('…'));
    }

    // Single-char ellipsis suffix on long multibyte input.
    #[test]
    fn test_multibyte_with_ellipsis() {
        let s = "ł日".repeat(300); // 600 chars
        let result = truncate_with(&s, 500, "…");
        assert_eq!(result.chars().count(), 501); // 500 data + 1 ellipsis
        assert!(result.ends_with('…'));
    }

    // Empty string — no truncation.
    #[test]
    fn test_empty_string() {
        let result = truncate_with("", 10, "…");
        assert_eq!(&*result, "");
        assert!(matches!(result, Cow::Borrowed(_)));
    }

    // max=0 — everything is truncated (only suffix remains).
    #[test]
    fn test_zero_max() {
        let result = truncate_with("hello", 0, "…");
        assert_eq!(&*result, "…");
    }

    // --- strip_code_fence ---------------------------------------------------

    // Fenced with a `json` language tag — the observed Ollama quirk.
    #[test]
    fn test_fence_with_lang_tag() {
        let input = "```json\n{\"facts\": [\"x\"]}\n```";
        assert_eq!(strip_code_fence(input), "{\"facts\": [\"x\"]}");
    }

    // Fenced without a language tag.
    #[test]
    fn test_fence_without_lang_tag() {
        let input = "```\n{\"facts\": [\"x\"]}\n```";
        assert_eq!(strip_code_fence(input), "{\"facts\": [\"x\"]}");
    }

    // Already-bare JSON — no-op passthrough.
    #[test]
    fn test_fence_bare_passthrough() {
        let input = "{\"facts\": [\"x\"]}";
        assert_eq!(strip_code_fence(input), "{\"facts\": [\"x\"]}");
    }

    // Leading/trailing whitespace around the fence is stripped.
    #[test]
    fn test_fence_surrounding_whitespace() {
        let input = "  \n ```json\n{\"facts\": []}\n```  \n ";
        assert_eq!(strip_code_fence(input), "{\"facts\": []}");
    }

    // Non-`json` language tag (e.g. uppercase) is also discarded.
    #[test]
    fn test_fence_other_lang_tag() {
        assert_eq!(strip_code_fence("```JSON\n{\"a\":1}\n```"), "{\"a\":1}");
    }

    // Multiline (pretty-printed) body keeps its internal newlines intact.
    #[test]
    fn test_fence_multiline_body_preserved() {
        let input = "```json\n{\n  \"a\": 1\n}\n```";
        assert_eq!(strip_code_fence(input), "{\n  \"a\": 1\n}");
    }

    // Single-line fence with a language tag and no body newline.
    #[test]
    fn test_fence_single_line_with_tag() {
        assert_eq!(strip_code_fence("```json {\"a\":1} ```"), "{\"a\":1}");
    }

    // Mixed format: body starts on the same line as the opening tag, but the
    // closing fence is on a later line. Regression test — this previously
    // returned "" because the whole first line (tag + body) was mistaken for
    // the language tag.
    #[test]
    fn test_fence_body_starts_on_tag_line_closes_on_next() {
        assert_eq!(strip_code_fence("```json {\"a\":1}\n```"), "{\"a\":1}");
    }

    // Bare (unfenced) text with surrounding whitespace is only trimmed.
    #[test]
    fn test_fence_bare_with_whitespace() {
        assert_eq!(strip_code_fence("  {\"a\":1}\n"), "{\"a\":1}");
    }

    // Empty input — trimmed to empty, no panic.
    #[test]
    fn test_fence_empty() {
        assert_eq!(strip_code_fence("   "), "");
    }
}
