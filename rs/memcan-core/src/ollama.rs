//! Ollama model management utilities.

use std::borrow::Cow;

/// Strip the `"ollama::"` provider prefix from a model name if present.
///
/// The Ollama API rejects any model name containing `"::"`.
pub fn strip_ollama_prefix(name: &str) -> &str {
    name.strip_prefix("ollama::").unwrap_or(name)
}

/// Namespace a model name to the Ollama provider, unless it already carries a
/// `"<provider>::"` namespace.
///
/// genai infers its adapter from the model name and only recognises Ollama by
/// an `"ollama::"` namespace or by falling through its provider heuristics, so
/// bare names that collide with another provider (`"command-r7b"` -> Cohere,
/// `"glm4:9b"` -> ZAI) are routed off-host. Namespacing pins the adapter.
///
/// An existing namespace is preserved so an explicit `"openai::gpt-4o"` is
/// never mangled into `"ollama::openai::gpt-4o"`.
///
/// ```
/// # use memcan_core::ollama::ensure_ollama_prefix;
/// assert_eq!(ensure_ollama_prefix("command-r7b"), "ollama::command-r7b");
/// assert_eq!(ensure_ollama_prefix("ollama::qwen3.5:9b"), "ollama::qwen3.5:9b");
/// ```
pub fn ensure_ollama_prefix(name: &str) -> Cow<'_, str> {
    if name.contains("::") {
        Cow::Borrowed(name)
    } else {
        Cow::Owned(format!("ollama::{name}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_strip_ollama_prefix_with_prefix() {
        assert_eq!(strip_ollama_prefix("ollama::qwen3.5:9b"), "qwen3.5:9b");
    }

    #[test]
    fn test_strip_ollama_prefix_without_prefix() {
        assert_eq!(strip_ollama_prefix("gpt-4o"), "gpt-4o");
    }

    #[test]
    fn test_strip_ollama_prefix_empty() {
        assert_eq!(strip_ollama_prefix(""), "");
    }

    #[test]
    fn test_strip_ollama_prefix_partial() {
        assert_eq!(strip_ollama_prefix("ollama:model"), "ollama:model");
    }

    #[test]
    fn ensure_ollama_prefix_namespaces_bare_names() {
        assert_eq!(ensure_ollama_prefix("qwen3.5:9b"), "ollama::qwen3.5:9b");
        assert_eq!(
            ensure_ollama_prefix("gemma4:26b-a4b-it-qat"),
            "ollama::gemma4:26b-a4b-it-qat"
        );
        // Bare names that genai's heuristics would route to another provider.
        assert_eq!(ensure_ollama_prefix("command-r7b"), "ollama::command-r7b");
        assert_eq!(ensure_ollama_prefix("glm4:9b"), "ollama::glm4:9b");
    }

    #[test]
    fn ensure_ollama_prefix_keeps_existing_namespace() {
        assert_eq!(
            ensure_ollama_prefix("ollama::qwen3.5:9b"),
            "ollama::qwen3.5:9b"
        );
        assert_eq!(ensure_ollama_prefix("openai::gpt-4o"), "openai::gpt-4o");
    }

    #[test]
    fn ensure_ollama_prefix_borrows_when_unchanged() {
        assert!(matches!(
            ensure_ollama_prefix("ollama::qwen3.5:9b"),
            Cow::Borrowed(_)
        ));
    }

    #[test]
    fn ensure_then_strip_round_trips_bare_names() {
        for name in ["qwen3.5:9b", "command-r7b", ""] {
            assert_eq!(strip_ollama_prefix(&ensure_ollama_prefix(name)), name);
        }
    }
}
