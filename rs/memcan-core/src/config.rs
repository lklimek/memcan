use std::path::{Path, PathBuf};

use tracing::{debug, warn};

use crate::embed::{model_dims, resolve_model};
use crate::error::{MemcanError, Result};

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
    /// LLM model name. With the default `ollama-rs-llm` feature, this is a
    /// bare Ollama model name like `"qwen3.5:9b"`. With `genai-llm`, prefix
    /// with provider, e.g. `"ollama::qwen3.5:9b"`, `"gpt-4o"`.
    pub llm_model: String,
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
    /// Shared username for the read-only tasks web UI.
    pub webui_username: Option<String>,
    /// Shared password for the read-only tasks web UI.
    pub webui_password: Option<String>,
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
            .field("embed_model", &self.embed_model)
            .field("embed_dims", &self.embed_dims)
            .field("ollama_host", &self.ollama_host)
            .field(
                "ollama_api_key",
                &self.ollama_api_key.as_ref().map(|_| "***"),
            )
            .field("listen", &self.listen)
            .field("api_key", &self.api_key.as_ref().map(|_| "***"))
            .field("webui_username", &self.webui_username)
            .field(
                "webui_password",
                &self.webui_password.as_ref().map(|_| "***"),
            )
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
            embed_model: "MultilingualE5Large".into(),
            embed_dims: 1024,
            ollama_host: None,
            ollama_api_key: None,
            listen: "127.0.0.1:8191".into(),
            api_key: None,
            webui_username: None,
            webui_password: None,
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
        let llm_model = env_or("LLM_MODEL", &defaults.llm_model);
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
        let webui_username = std::env::var("MEMCAN_WEBUI_USERNAME")
            .ok()
            .filter(|s| !s.is_empty());
        let webui_password = std::env::var("MEMCAN_WEBUI_PASSWORD")
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
            embed_model,
            embed_dims,
            ollama_host,
            ollama_api_key,
            listen,
            api_key,
            webui_username,
            webui_password,
            url,
            compact_on_startup,
            compact_fragment_threshold,
        };
        settings.validate()?;
        Ok(settings)
    }

    /// Check invariants on loaded settings.
    fn validate(&self) -> Result<()> {
        if self.lancedb_path.is_empty() {
            return Err(MemcanError::Config("LANCEDB_PATH must not be empty".into()));
        }

        // -- llm_model format check (warn only, genai-llm needs provider prefix) --
        #[cfg(all(feature = "genai-llm", not(feature = "ollama-rs-llm")))]
        if !self.llm_model.contains("::") {
            warn!(
                "LLM_MODEL '{}' is missing a provider prefix (e.g. 'ollama::model-name')",
                self.llm_model
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

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    struct EnvGuard(Vec<(&'static str, Option<String>)>);

    impl EnvGuard {
        fn set(values: &[(&'static str, Option<&str>)]) -> Self {
            let originals = values
                .iter()
                .map(|(key, _)| (*key, std::env::var(key).ok()))
                .collect();
            // SAFETY: these tests are serialized, and every variable is restored on drop.
            unsafe {
                for (key, value) in values {
                    match value {
                        Some(value) => std::env::set_var(key, value),
                        None => std::env::remove_var(key),
                    }
                }
            }
            Self(originals)
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            // SAFETY: these tests are serialized, and this restores their prior environment.
            unsafe {
                for (key, value) in self.0.drain(..) {
                    match value {
                        Some(value) => std::env::set_var(key, value),
                        None => std::env::remove_var(key),
                    }
                }
            }
        }
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

    #[test]
    #[serial]
    fn test_webui_credentials_load_set_unset_and_empty_values() {
        let temp = tempfile::tempdir().unwrap();
        let temp_path = temp.path().to_str().unwrap();
        let _environment = EnvGuard::set(&[
            ("HOME", Some(temp_path)),
            ("XDG_CONFIG_HOME", Some(temp_path)),
            ("XDG_DATA_HOME", Some(temp_path)),
            ("MEMCAN_WEBUI_USERNAME", Some("operator")),
            ("MEMCAN_WEBUI_PASSWORD", Some("correct horse")),
        ]);

        let settings = Settings::load().unwrap();
        assert_eq!(settings.webui_username.as_deref(), Some("operator"));
        assert_eq!(settings.webui_password.as_deref(), Some("correct horse"));

        // SAFETY: this serialized test owns and later restores these variables.
        unsafe {
            std::env::set_var("MEMCAN_WEBUI_USERNAME", "");
            std::env::set_var("MEMCAN_WEBUI_PASSWORD", "still-set");
        }
        let settings = Settings::load().unwrap();
        assert!(settings.webui_username.is_none());
        assert_eq!(settings.webui_password.as_deref(), Some("still-set"));

        // SAFETY: this serialized test owns and later restores these variables.
        unsafe {
            std::env::set_var("MEMCAN_WEBUI_USERNAME", "operator");
            std::env::remove_var("MEMCAN_WEBUI_PASSWORD");
        }
        let settings = Settings::load().unwrap();
        assert_eq!(settings.webui_username.as_deref(), Some("operator"));
        assert!(settings.webui_password.is_none());
    }

    #[test]
    #[serial]
    fn test_webui_password_is_masked_in_settings_debug() {
        let temp = tempfile::tempdir().unwrap();
        let temp_path = temp.path().to_str().unwrap();
        let _environment = EnvGuard::set(&[
            ("HOME", Some(temp_path)),
            ("XDG_CONFIG_HOME", Some(temp_path)),
            ("XDG_DATA_HOME", Some(temp_path)),
            ("MEMCAN_WEBUI_USERNAME", Some("operator")),
            ("MEMCAN_WEBUI_PASSWORD", Some("never-print-me")),
        ]);

        let settings = Settings::load().unwrap();

        let debug = format!("{settings:?}");
        assert!(debug.contains("operator"));
        assert!(debug.contains("***"));
        assert!(!debug.contains("never-print-me"));
    }
}
