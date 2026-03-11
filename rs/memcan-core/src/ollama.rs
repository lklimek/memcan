//! Ollama model management utilities.

/// Strip the `"ollama::"` provider prefix from a model name if present.
///
/// The Ollama API rejects any model name containing `"::"`.
pub fn strip_ollama_prefix(name: &str) -> &str {
    name.strip_prefix("ollama::").unwrap_or(name)
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
}
