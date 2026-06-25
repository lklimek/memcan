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
}
