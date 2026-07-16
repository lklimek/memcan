use std::path::{Path, PathBuf};
use std::str::FromStr;

use tracing::{debug, warn};

use crate::embed::{model_dims, resolve_model};
use crate::error::{MemcanError, Result};

/// Runtime-selectable LLM backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LlmProviderKind {
    Ollama,
    OpenRouter,
}

impl FromStr for LlmProviderKind {
    type Err = MemcanError;

    fn from_str(value: &str) -> Result<Self> {
        match value.to_ascii_lowercase().as_str() {
            "ollama" => Ok(Self::Ollama),
            "openrouter" => Ok(Self::OpenRouter),
            _ => Err(MemcanError::Config(format!(
                "unknown LLM provider '{value}'; expected 'ollama' or 'openrouter'"
            ))),
        }
    }
}

impl std::fmt::Display for LlmProviderKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Ollama => f.write_str("ollama"),
            Self::OpenRouter => f.write_str("openrouter"),
        }
    }
}

/// Expand a leading `~` to the user's home directory.
fn expand_tilde(path: &str) -> String {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest).to_string_lossy().into_owned();
        }
    } else if path == "~"
        && let Some(home) = dirs::home_dir()
    {
        return home.to_string_lossy().into_owned();
    }
    path.to_string()
}

/// Application settings loaded from .env files and environment variables.
#[derive(Clone)]
pub struct Settings {
    pub lancedb_path: String,
    pub default_user_id: String,
    pub tech_stack: String,
    pub distill_memories: bool,
    pub log_file: String,
    /// Ollama model name, e.g. `"qwen3.5:9b"`.
    /// The legacy `"ollama::"` prefix is accepted for compatibility.
    pub llm_model: String,
    /// Primary LLM backend.
    pub llm_provider: LlmProviderKind,
    /// Optional LLM backend used when the primary is unavailable.
    pub llm_fallback_provider: Option<LlmProviderKind>,
    /// Bearer key for OpenRouter.
    pub openrouter_api_key: Option<String>,
    /// OpenRouter model slug, e.g. `"openai/gpt-4o-mini"`.
    pub openrouter_model: String,
    /// OpenRouter-compatible API base URL.
    pub openrouter_base_url: String,
    /// Fastembed model name, e.g. `"AllMiniLML6V2"`, `"BGESmallENV15"`.
    pub embed_model: String,
    /// Embedding vector dimensions (derived automatically from embed_model).
    pub embed_dims: usize,
    /// Ollama server URL, e.g. `"http://10.29.188.1:11434"`.
    /// Passed explicitly to the LLM client.
    pub ollama_host: Option<String>,
    /// Bearer token for Ollama endpoint auth. When set, every Ollama
    /// request sends `Authorization: Bearer <key>`.
    pub ollama_api_key: Option<String>,
    /// Server listen address, e.g. `"127.0.0.1:8191"`.
    pub listen: String,
    /// Bearer token for MemCan server auth.
    pub api_key: Option<String>,
    /// MemCan server URL for thin clients, e.g. `"http://localhost:8190"`.
    pub url: String,
    /// Run full compaction (compact fragments + prune versions) on every table
    /// at startup, before serving. The startup window is single-writer, which is
    /// the safe time to compact.
    pub compact_on_startup: bool,
    /// Auto-compact a table once it reaches this many data fragments. `0`
    /// disables auto-compaction (startup compaction is gated separately).
    pub compact_fragment_threshold: usize,
}

impl std::fmt::Debug for Settings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Settings")
            .field("lancedb_path", &self.lancedb_path)
            .field("default_user_id", &self.default_user_id)
            .field("tech_stack", &self.tech_stack)
            .field("distill_memories", &self.distill_memories)
            .field("log_file", &self.log_file)
            .field("llm_model", &self.llm_model)
            .field("llm_provider", &self.llm_provider)
            .field("llm_fallback_provider", &self.llm_fallback_provider)
            .field(
                "openrouter_api_key",
                &self.openrouter_api_key.as_ref().map(|_| "***"),
            )
            .field("openrouter_model", &self.openrouter_model)
            .field("openrouter_base_url", &self.openrouter_base_url)
            .field("embed_model", &self.embed_model)
            .field("embed_dims", &self.embed_dims)
            .field("ollama_host", &self.ollama_host)
            .field(
                "ollama_api_key",
                &self.ollama_api_key.as_ref().map(|_| "***"),
            )
            .field("listen", &self.listen)
            .field("api_key", &self.api_key.as_ref().map(|_| "***"))
            .field("url", &self.url)
            .field("compact_on_startup", &self.compact_on_startup)
            .field(
                "compact_fragment_threshold",
                &self.compact_fragment_threshold,
            )
            .finish()
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            lancedb_path: "~/.local/share/memcan/lancedb".into(),
            default_user_id: "global".into(),
            tech_stack: String::new(),
            distill_memories: true,
            log_file: "~/.claude/logs/memcan-mcp.log".into(),
            llm_model: "gemma4:26b-a4b-it-qat".into(),
            llm_provider: LlmProviderKind::Ollama,
            llm_fallback_provider: None,
            openrouter_api_key: None,
            openrouter_model: String::new(),
            openrouter_base_url: "https://openrouter.ai/api/v1".into(),
            embed_model: "MultilingualE5Large".into(),
            embed_dims: 1024,
            ollama_host: None,
            ollama_api_key: None,
            listen: "127.0.0.1:8191".into(),
            api_key: None,
            url: "http://localhost:8190".into(),
            compact_on_startup: true,
            compact_fragment_threshold: 64,
        }
    }
}

impl Settings {
    /// Load settings from .env files and environment variables.
    ///
    /// Precedence (highest to lowest):
    /// 1. Explicit environment variables (already set before load)
    /// 2. CWD `./.env`
    /// 3. Platform config dir `~/.config/memcan/.env`
    ///
    /// Returns an error if values are invalid.
    pub fn load() -> Result<Self> {
        // Load .env files. dotenvy::from_path sets vars only if not already present,
        // so load order determines priority: CWD > config dir > explicit env vars win over both.
        let mut loaded_files: Vec<PathBuf> = Vec::new();

        // 1. CWD/.env (higher priority)
        let cwd_env = Path::new(".env");
        if cwd_env.exists() && dotenvy::from_path(cwd_env).is_ok() {
            debug!("Loaded .env from CWD");
            loaded_files.push(cwd_env.to_path_buf());
        }

        // 2. Platform config dir (fallback)
        if let Some(config_dir) = dirs::config_dir() {
            let platform_env = config_dir.join("memcan").join(".env");
            if platform_env.exists() && dotenvy::from_path(&platform_env).is_ok() {
                debug!("Loaded .env from {}", platform_env.display());
                loaded_files.push(platform_env);
            }
        }

        let defaults = Settings::default();

        let lancedb_path_raw = env_or("LANCEDB_PATH", &defaults.lancedb_path);
        let lancedb_path = expand_tilde(&lancedb_path_raw);
        let default_user_id = env_or("DEFAULT_USER_ID", &defaults.default_user_id);
        let tech_stack = env_or("TECH_STACK", &defaults.tech_stack);
        let distill_memories = env_or("DISTILL_MEMORIES", "true")
            .parse::<bool>()
            .unwrap_or(true);
        let log_file_raw = std::env::var("MEMCAN_LOG_FILE")
            .or_else(|_| std::env::var("LOG_FILE"))
            .unwrap_or_else(|_| defaults.log_file.clone());
        let log_file = expand_tilde(&log_file_raw);
        let llm_model_env = std::env::var("LLM_MODEL").ok();
        let llm_model = llm_model_env
            .clone()
            .unwrap_or_else(|| defaults.llm_model.clone());
        let llm_provider = env_or("LLM_PROVIDER", &defaults.llm_provider.to_string()).parse()?;
        let llm_fallback_provider = std::env::var("LLM_FALLBACK_PROVIDER")
            .ok()
            .filter(|value| !value.is_empty())
            .map(|value| value.parse())
            .transpose()?;
        let openrouter_api_key = std::env::var("OPENROUTER_API_KEY")
            .ok()
            .filter(|value| !value.is_empty());
        if openrouter_api_key.is_some() {
            debug!("OPENROUTER_API_KEY configured");
        }
        let openrouter_model = env_or("OPENROUTER_MODEL", &defaults.openrouter_model);
        let openrouter_base_url = env_or("OPENROUTER_BASE_URL", &defaults.openrouter_base_url);
        let embed_model = env_or("EMBED_MODEL", &defaults.embed_model);
        let resolved_embed = resolve_model(&embed_model).map_err(|_| {
            MemcanError::Config(format!(
                "EMBED_MODEL '{embed_model}' is not a known fastembed model"
            ))
        })?;
        let embed_dims = model_dims(&resolved_embed).ok_or_else(|| {
            MemcanError::Config(format!(
                "No known dimensions for embed model '{embed_model}' — add it to model_dims()"
            ))
        })?;
        let ollama_host = std::env::var("OLLAMA_HOST").ok().filter(|s| !s.is_empty());
        if let Some(ref host) = ollama_host {
            debug!(ollama_host = %host, "OLLAMA_HOST configured");
        }
        let ollama_api_key = std::env::var("OLLAMA_API_KEY")
            .ok()
            .filter(|s| !s.is_empty());
        if ollama_api_key.is_some() {
            debug!("OLLAMA_API_KEY configured");
        }

        let listen = env_or("MEMCAN_LISTEN", &defaults.listen);
        let api_key = std::env::var("MEMCAN_API_KEY")
            .ok()
            .filter(|s| !s.is_empty());
        let url = env_or("MEMCAN_URL", &defaults.url);
        let compact_on_startup = env_or(
            "COMPACT_ON_STARTUP",
            &defaults.compact_on_startup.to_string(),
        )
        .parse::<bool>()
        .unwrap_or(defaults.compact_on_startup);
        let compact_fragment_threshold = env_or(
            "COMPACT_FRAGMENT_THRESHOLD",
            &defaults.compact_fragment_threshold.to_string(),
        )
        .parse::<usize>()
        .unwrap_or(defaults.compact_fragment_threshold);

        let settings = Settings {
            lancedb_path,
            default_user_id,
            tech_stack,
            distill_memories,
            log_file,
            llm_model,
            llm_provider,
            llm_fallback_provider,
            openrouter_api_key,
            openrouter_model,
            openrouter_base_url,
            embed_model,
            embed_dims,
            ollama_host,
            ollama_api_key,
            listen,
            api_key,
            url,
            compact_on_startup,
            compact_fragment_threshold,
        };
        settings.validate()?;
        if llm_model_is_ignored(
            settings.llm_provider,
            llm_model_env.as_deref(),
            &defaults.llm_model,
        ) {
            warn!(
                llm_model = %settings.llm_model,
                openrouter_model = %settings.openrouter_model,
                "LLM_MODEL is ignored while LLM_PROVIDER=openrouter; set OPENROUTER_MODEL to choose the OpenRouter model"
            );
        }
        Ok(settings)
    }

    /// Check invariants on loaded settings.
    fn validate(&self) -> Result<()> {
        if self.lancedb_path.is_empty() {
            return Err(MemcanError::Config("LANCEDB_PATH must not be empty".into()));
        }

        let openrouter_participates = self.llm_provider == LlmProviderKind::OpenRouter
            || self.llm_fallback_provider == Some(LlmProviderKind::OpenRouter);
        if openrouter_participates {
            if self.openrouter_api_key.is_none() {
                return Err(MemcanError::Config(
                    "OPENROUTER_API_KEY is required when OpenRouter is configured".into(),
                ));
            }
            if self.openrouter_model.is_empty() {
                return Err(MemcanError::Config(
                    "OPENROUTER_MODEL is required when OpenRouter is configured".into(),
                ));
            }

            let openrouter_url =
                reqwest::Url::parse(&self.openrouter_base_url).map_err(|error| {
                    MemcanError::Config(format!("OPENROUTER_BASE_URL is invalid: {error}"))
                })?;
            let loopback = matches!(
                openrouter_url.host_str(),
                Some("localhost" | "127.0.0.1" | "::1" | "[::1]")
            );
            if openrouter_url.scheme() != "https" && !loopback {
                return Err(MemcanError::Config(
                    "OPENROUTER_BASE_URL must use HTTPS when configured with a non-loopback host"
                        .into(),
                ));
            }
        }
        if self.llm_fallback_provider == Some(self.llm_provider) {
            warn!(
                provider = %self.llm_provider,
                "LLM fallback provider matches the primary provider"
            );
        }

        // -- log_file parent directory (warn only, don't create) --
        let log_path = Path::new(&self.log_file);
        if let Some(parent) = log_path.parent()
            && !parent.as_os_str().is_empty()
            && !parent.exists()
        {
            warn!(
                "LOG_FILE directory '{}' does not exist. It will be created on first use.",
                parent.display()
            );
        }

        Ok(())
    }

    /// Create the log file's parent directory if it doesn't exist.
    /// Call this after validation, before starting the server.
    pub fn ensure_log_dir(&self) -> Result<()> {
        let log_path = Path::new(&self.log_file);
        if let Some(parent) = log_path.parent()
            && !parent.as_os_str().is_empty()
            && !parent.exists()
        {
            std::fs::create_dir_all(parent).map_err(|err| {
                MemcanError::Config(format!(
                    "LOG_FILE directory '{}' could not be created: {}",
                    parent.display(),
                    err
                ))
            })?;
        }
        Ok(())
    }
}

/// Read an environment variable, falling back to a default.
fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

/// Report whether a configured `LLM_MODEL` has no effect.
///
/// `LLM_MODEL` names the Ollama model; the OpenRouter backend reads
/// `OPENROUTER_MODEL` instead. Only an explicit, non-default `llm_model_env`
/// is worth reporting — an unset or default-valued variable carries no
/// operator intent to contradict.
fn llm_model_is_ignored(
    provider: LlmProviderKind,
    llm_model_env: Option<&str>,
    default_model: &str,
) -> bool {
    provider == LlmProviderKind::OpenRouter
        && llm_model_env.is_some_and(|model| !model.is_empty() && model != default_model)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tc10_openrouter_configuration_requires_key_and_model() {
        let missing_key = Settings {
            llm_provider: LlmProviderKind::OpenRouter,
            openrouter_model: "openai/gpt-4o-mini".into(),
            ..Settings::default()
        };
        let error = missing_key.validate().unwrap_err().to_string();
        assert!(error.contains("OPENROUTER_API_KEY"));

        let missing_model = Settings {
            llm_provider: LlmProviderKind::OpenRouter,
            openrouter_api_key: Some("test-key".into()),
            ..Settings::default()
        };
        let error = missing_model.validate().unwrap_err().to_string();
        assert!(error.contains("OPENROUTER_MODEL"));

        let configured = Settings {
            llm_provider: LlmProviderKind::OpenRouter,
            openrouter_api_key: Some("test-key".into()),
            openrouter_model: "openai/gpt-4o-mini".into(),
            ..Settings::default()
        };
        assert!(configured.validate().is_ok());

        let fallback = Settings {
            llm_fallback_provider: Some(LlmProviderKind::OpenRouter),
            openrouter_api_key: Some("test-key".into()),
            openrouter_model: "openai/gpt-4o-mini".into(),
            ..Settings::default()
        };
        assert!(fallback.validate().is_ok());

        let fallback_missing_key = Settings {
            llm_fallback_provider: Some(LlmProviderKind::OpenRouter),
            openrouter_model: "openai/gpt-4o-mini".into(),
            ..Settings::default()
        };
        assert!(
            fallback_missing_key
                .validate()
                .unwrap_err()
                .to_string()
                .contains("OPENROUTER_API_KEY")
        );

        let fallback_missing_model = Settings {
            llm_fallback_provider: Some(LlmProviderKind::OpenRouter),
            openrouter_api_key: Some("test-key".into()),
            ..Settings::default()
        };
        assert!(
            fallback_missing_model
                .validate()
                .unwrap_err()
                .to_string()
                .contains("OPENROUTER_MODEL")
        );
    }

    #[test]
    fn unused_openrouter_base_url_is_not_validated() {
        let settings = Settings {
            openrouter_base_url: "not-a-url".into(),
            ..Settings::default()
        };

        assert!(settings.validate().is_ok());
    }

    #[test]
    fn openrouter_http_requires_a_loopback_host() {
        for base_url in [
            "http://localhost:8080/v1",
            "http://127.0.0.1:8080/v1",
            "http://[::1]:8080/v1",
        ] {
            let loopback = Settings {
                llm_provider: LlmProviderKind::OpenRouter,
                openrouter_api_key: Some("test-key".into()),
                openrouter_model: "openai/gpt-4o-mini".into(),
                openrouter_base_url: base_url.into(),
                ..Settings::default()
            };
            assert!(
                loopback.validate().is_ok(),
                "loopback URL should be accepted: {base_url}"
            );
        }

        let remote = Settings {
            llm_provider: LlmProviderKind::OpenRouter,
            openrouter_api_key: Some("test-key".into()),
            openrouter_model: "openai/gpt-4o-mini".into(),
            openrouter_base_url: "http://openrouter.example/v1".into(),
            ..Settings::default()
        };
        let error = remote.validate().unwrap_err().to_string();
        assert!(error.contains("OPENROUTER_BASE_URL"));
        assert!(error.contains("HTTPS"));
    }

    #[test]
    fn tc11_provider_kind_parsing_is_case_insensitive() {
        assert_eq!(
            "ollama".parse::<LlmProviderKind>().unwrap(),
            LlmProviderKind::Ollama
        );
        assert_eq!(
            "OPENROUTER".parse::<LlmProviderKind>().unwrap(),
            LlmProviderKind::OpenRouter
        );
        assert!("unknown".parse::<LlmProviderKind>().is_err());

        let configured = Settings {
            llm_provider: "ollama".parse().unwrap(),
            llm_fallback_provider: Some("openrouter".parse().unwrap()),
            openrouter_api_key: Some("test-key".into()),
            openrouter_model: "openai/gpt-4o-mini".into(),
            ..Settings::default()
        };
        assert_eq!(configured.llm_provider, LlmProviderKind::Ollama);
        assert_eq!(
            configured.llm_fallback_provider,
            Some(LlmProviderKind::OpenRouter)
        );
        assert_eq!(Settings::default().llm_fallback_provider, None);
    }

    #[test]
    fn tc14_settings_debug_redacts_openrouter_api_key() {
        let settings = Settings {
            openrouter_api_key: Some("raw-openrouter-secret".into()),
            ..Settings::default()
        };
        let debug = format!("{settings:?}");
        assert!(!debug.contains("raw-openrouter-secret"));
        assert!(debug.contains("openrouter_api_key: Some(\"***\")"));
    }

    #[test]
    fn llm_model_is_ignored_only_for_explicit_non_default_under_openrouter() {
        let default = Settings::default().llm_model;

        assert!(
            llm_model_is_ignored(LlmProviderKind::OpenRouter, Some("qwen3.5:9b"), &default),
            "an explicit non-default LLM_MODEL is dead config under OpenRouter"
        );

        // No operator intent to contradict: stay quiet rather than warn on every boot.
        assert!(!llm_model_is_ignored(
            LlmProviderKind::OpenRouter,
            None,
            &default
        ));
        assert!(!llm_model_is_ignored(
            LlmProviderKind::OpenRouter,
            Some(&default),
            &default
        ));
        assert!(!llm_model_is_ignored(
            LlmProviderKind::OpenRouter,
            Some(""),
            &default
        ));

        // Ollama primary consumes LLM_MODEL, including as OpenRouter's fallback peer.
        assert!(!llm_model_is_ignored(
            LlmProviderKind::Ollama,
            Some("qwen3.5:9b"),
            &default
        ));
    }

    #[test]
    fn test_expand_tilde() {
        let expanded = expand_tilde("~/.local/share/memcan");
        assert!(!expanded.starts_with('~'), "tilde should be expanded");
        assert!(expanded.contains(".local/share/memcan"));
    }

    #[test]
    fn test_expand_tilde_no_tilde() {
        assert_eq!(expand_tilde("/absolute/path"), "/absolute/path");
    }

    #[test]
    fn test_defaults() {
        let d = Settings::default();
        assert_eq!(d.llm_model, "gemma4:26b-a4b-it-qat");
        assert_eq!(d.embed_model, "MultilingualE5Large");
        assert_eq!(d.embed_dims, 1024);
        assert!(d.distill_memories);
        assert!(d.compact_on_startup);
        assert_eq!(d.compact_fragment_threshold, 64);
    }

    #[test]
    fn test_validate_empty_lancedb_path() {
        let s = Settings {
            lancedb_path: String::new(),
            ..Settings::default()
        };
        assert!(s.validate().is_err());
    }

    #[test]
    fn test_validate_defaults_ok() {
        Settings::default().validate().unwrap();
    }
}
