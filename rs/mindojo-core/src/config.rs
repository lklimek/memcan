use std::path::{Path, PathBuf};

use tracing::{debug, warn};

use crate::embed::{model_dims, resolve_model};
use crate::error::{MindojoError, Result};

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
    /// LLM model name for the genai crate. Prefix with provider, e.g.
    /// `"ollama::qwen3.5:4b"`, `"gpt-4o"`, `"claude-sonnet-4-20250514"`.
    pub llm_model: String,
    /// Fastembed model name, e.g. `"AllMiniLML6V2"`, `"BGESmallENV15"`.
    pub embed_model: String,
    /// Embedding vector dimensions (must match the chosen embed_model).
    pub embed_dims: usize,
    /// Ollama server URL, e.g. `"http://10.29.188.1:11434"`.
    /// Passed explicitly to the genai client since it does not read
    /// `OLLAMA_HOST` from the environment.
    pub ollama_host: Option<String>,
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
            .finish()
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            lancedb_path: "~/.local/share/mindojo/lancedb".into(),
            default_user_id: "global".into(),
            tech_stack: String::new(),
            distill_memories: true,
            log_file: "~/.claude/logs/mindojo-mcp.log".into(),
            llm_model: "ollama::qwen3.5:4b".into(),
            embed_model: "MultilingualE5Large".into(),
            embed_dims: 1024,
            ollama_host: None,
        }
    }
}

impl Settings {
    /// Load settings from .env files and environment variables.
    ///
    /// Search order for .env files:
    /// 1. Platform config dir: `~/.config/mindojo/.env`
    /// 2. Current working directory: `./.env`
    ///
    /// Environment variables override .env values.
    /// Load and validate settings. Returns an error if values are invalid.
    pub fn load() -> Result<Self> {
        // Load .env files (later loads do NOT override earlier ones in dotenvy,
        // but env vars always win). We load platform config first, then CWD.
        let mut loaded_files: Vec<PathBuf> = Vec::new();

        // 1. Platform config dir
        if let Some(config_dir) = dirs::config_dir() {
            let platform_env = config_dir.join("mindojo").join(".env");
            if platform_env.exists() && dotenvy::from_path(&platform_env).is_ok() {
                debug!("Loaded .env from {}", platform_env.display());
                loaded_files.push(platform_env);
            }
        }

        // 2. CWD/.env
        let cwd_env = Path::new(".env");
        if cwd_env.exists() {
            // dotenvy::from_path won't override already-set env vars by default,
            // but we want CWD to override platform config. Use from_path_override.
            if dotenvy::from_path_override(cwd_env).is_ok() {
                debug!("Loaded .env from CWD");
                loaded_files.push(cwd_env.to_path_buf());
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
        let log_file_raw = env_or("LOG_FILE", &defaults.log_file);
        let log_file = expand_tilde(&log_file_raw);
        let llm_model = env_or("LLM_MODEL", &defaults.llm_model);
        let embed_model = env_or("EMBED_MODEL", &defaults.embed_model);
        let embed_dims = env_or("EMBED_DIMS", &defaults.embed_dims.to_string())
            .parse::<usize>()
            .unwrap_or(defaults.embed_dims);
        let ollama_host = std::env::var("OLLAMA_HOST").ok().filter(|s| !s.is_empty());
        if let Some(ref host) = ollama_host {
            debug!(ollama_host = %host, "OLLAMA_HOST configured");
        }

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
        };
        settings.validate()?;
        Ok(settings)
    }

    /// Check invariants on loaded settings.
    fn validate(&self) -> Result<()> {
        // -- basic checks --
        if self.embed_dims == 0 {
            return Err(MindojoError::Config(
                "EMBED_DIMS must be greater than 0".into(),
            ));
        }
        if self.lancedb_path.is_empty() {
            return Err(MindojoError::Config(
                "LANCEDB_PATH must not be empty".into(),
            ));
        }

        // -- embed_model is a known fastembed model --
        let resolved = resolve_model(&self.embed_model).map_err(|_| {
            MindojoError::Config(format!(
                "EMBED_MODEL '{}' is not a known fastembed model. See embed.rs for supported models.",
                self.embed_model
            ))
        })?;

        // -- embed_dims matches the chosen model --
        if let Some(expected) = model_dims(&resolved)
            && expected != self.embed_dims
        {
            return Err(MindojoError::Config(format!(
                "EMBED_DIMS is {} but model '{}' produces {}-dimensional vectors",
                self.embed_dims, self.embed_model, expected
            )));
        }

        // -- llm_model format check (warn only) --
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
                MindojoError::Config(format!(
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

    #[test]
    fn test_expand_tilde() {
        let expanded = expand_tilde("~/.local/share/mindojo");
        assert!(!expanded.starts_with('~'), "tilde should be expanded");
        assert!(expanded.contains(".local/share/mindojo"));
    }

    #[test]
    fn test_expand_tilde_no_tilde() {
        assert_eq!(expand_tilde("/absolute/path"), "/absolute/path");
    }

    #[test]
    fn test_defaults() {
        let d = Settings::default();
        assert_eq!(d.llm_model, "ollama::qwen3.5:4b");
        assert_eq!(d.embed_model, "MultilingualE5Large");
        assert_eq!(d.embed_dims, 1024);
        assert!(d.distill_memories);
    }

    #[test]
    fn test_validate_zero_embed_dims() {
        let s = Settings {
            embed_dims: 0,
            ..Settings::default()
        };
        assert!(s.validate().is_err());
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
    fn test_validate_unknown_embed_model() {
        let s = Settings {
            embed_model: "NonexistentModel".into(),
            ..Settings::default()
        };
        let err = s.validate().unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("not a known fastembed model"),
            "expected 'not a known fastembed model' in: {msg}"
        );
    }

    #[test]
    fn test_validate_mismatched_embed_dims() {
        let s = Settings {
            embed_model: "MultilingualE5Large".into(),
            embed_dims: 384,
            ..Settings::default()
        };
        let err = s.validate().unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("1024"),
            "expected '1024' in error message: {msg}"
        );
    }
}
