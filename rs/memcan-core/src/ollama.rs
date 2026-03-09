//! Ollama model management utilities.

use tracing::{info, warn};

use crate::config::Settings;

/// Strip the `"ollama::"` provider prefix from a model name if present.
///
/// The Ollama API rejects any model name containing `"::"`.
pub fn strip_ollama_prefix(name: &str) -> &str {
    name.strip_prefix("ollama::").unwrap_or(name)
}

const NOTHINK_SUFFIX: &str = "-memcan-nothink";
const NOTHINK_SYSTEM: &str =
    "/no_think\nAlways respond with valid JSON only. No markdown, no commentary.";

#[deprecated(since = "0.32.0", note = "ollama-rs handles think:false natively")]
/// Ensure a nothink model variant exists on the Ollama server.
///
/// Derives `{base_model}-memcan-nothink`, checks if it exists via
/// `POST /api/show`, and creates it via `POST /api/create` if missing.
/// Returns the nothink model name with `ollama::` prefix restored.
///
/// Falls back gracefully on failure (logs warning, returns original model).
pub async fn ensure_nothink_model(settings: &Settings) -> String {
    let original = &settings.llm_model;

    if !original.to_lowercase().contains("ollama") {
        return original.clone();
    }

    let base_model = strip_ollama_prefix(original);
    let nothink_name = format!("{base_model}{NOTHINK_SUFFIX}");

    let host = settings
        .ollama_host
        .as_deref()
        .unwrap_or("http://localhost:11434");
    let host = host.trim_end_matches('/');

    let client = reqwest::Client::new();

    let mut show_req = client
        .post(format!("{host}/api/show"))
        .json(&serde_json::json!({"name": &nothink_name}));
    if let Some(ref key) = settings.ollama_api_key {
        show_req = show_req.header("Authorization", format!("Bearer {key}"));
    }

    match show_req.send().await {
        Ok(resp) if resp.status().is_success() => {
            info!(model = %nothink_name, "nothink model already exists");
            return format!("ollama::{nothink_name}");
        }
        Ok(_) => {
            info!(model = %nothink_name, "nothink model not found, creating");
        }
        Err(e) => {
            warn!(error = %e, "failed to check nothink model, using original");
            return original.clone();
        }
    }

    let mut create_req = client
        .post(format!("{host}/api/create"))
        .json(&serde_json::json!({
            "model": &nothink_name,
            "from": base_model,
            "system": NOTHINK_SYSTEM,
        }));
    if let Some(ref key) = settings.ollama_api_key {
        create_req = create_req.header("Authorization", format!("Bearer {key}"));
    }

    match create_req.send().await {
        Ok(resp) if resp.status().is_success() => {
            info!(model = %nothink_name, "nothink model created");
            format!("ollama::{nothink_name}")
        }
        Ok(resp) => {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            warn!(
                status = %status,
                body = %body,
                "failed to create nothink model, using original"
            );
            original.clone()
        }
        Err(e) => {
            warn!(error = %e, "failed to create nothink model, using original");
            original.clone()
        }
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
    fn test_nothink_suffix() {
        assert_eq!(NOTHINK_SUFFIX, "-memcan-nothink");
    }

    #[test]
    fn test_nothink_name_derivation() {
        let base = strip_ollama_prefix("ollama::qwen3.5:9b");
        let nothink = format!("{base}{NOTHINK_SUFFIX}");
        assert_eq!(nothink, "qwen3.5:9b-memcan-nothink");
    }

    #[tokio::test]
    #[allow(deprecated)]
    async fn test_non_ollama_model_passthrough() {
        let settings = Settings {
            llm_model: "gpt-4o".into(),
            ..Settings::default()
        };
        let result = ensure_nothink_model(&settings).await;
        assert_eq!(result, "gpt-4o");
    }
}
