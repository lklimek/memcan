//! Ollama model management utilities.

use tracing::info;

use crate::config::Settings;

/// Strip the `"ollama::"` provider prefix from a model name if present.
///
/// The Ollama API rejects any model name containing `"::"`.
pub fn strip_ollama_prefix(name: &str) -> &str {
    name.strip_prefix("ollama::").unwrap_or(name)
}

/// Ensure the configured LLM model exists on the Ollama server, pulling it if
/// absent. Non-Ollama providers are silently skipped.
///
/// Called at startup from [`crate::init::MemcanContext::init`] so that a
/// missing model causes an immediate, loud failure instead of silent fallback
/// to raw storage.
#[cfg(feature = "ollama-rs-llm")]
pub async fn ensure_model(settings: &Settings) -> crate::error::Result<()> {
    use ollama_rs::Ollama;

    use crate::error::MemcanError;
    use crate::llm_ollama_rs::parse_host_port;

    if !settings.llm_model.to_lowercase().contains("ollama") {
        return Ok(());
    }

    let model_name = strip_ollama_prefix(&settings.llm_model);

    let raw_host = settings
        .ollama_host
        .as_deref()
        .unwrap_or("http://localhost:11434");
    let (host, port) = parse_host_port(raw_host);

    let client = if let Some(ref api_key) = settings.ollama_api_key {
        match reqwest::header::HeaderValue::from_str(&format!("Bearer {api_key}")) {
            Ok(val) => {
                let mut headers = reqwest::header::HeaderMap::new();
                headers.insert(reqwest::header::AUTHORIZATION, val);
                Ollama::new_with_request_headers(host, port, headers)
            }
            Err(_) => Ollama::new(host, port),
        }
    } else {
        Ollama::new(host, port)
    };

    match client.show_model_info(model_name.to_string()).await {
        Ok(_) => {
            info!(model = %model_name, "LLM model available");
            return Ok(());
        }
        Err(_) => {
            info!(model = %model_name, "LLM model not found locally, pulling");
        }
    }

    client
        .pull_model(model_name.to_string(), false)
        .await
        .map_err(|e| {
            MemcanError::Other(format!("failed to pull Ollama model '{model_name}': {e}"))
        })?;

    info!(model = %model_name, "LLM model pulled successfully");
    Ok(())
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

    #[cfg(feature = "ollama-rs-llm")]
    #[tokio::test]
    async fn test_ensure_model_non_ollama_skips() {
        let settings = Settings {
            llm_model: "gpt-4o".into(),
            ..Settings::default()
        };
        let result = ensure_model(&settings).await;
        assert!(result.is_ok());
    }
}
