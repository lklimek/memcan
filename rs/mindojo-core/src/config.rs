use std::path::{Path, PathBuf};

use tracing::debug;

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
#[derive(Debug, Clone)]
pub struct Settings {
    pub ollama_url: String,
    pub ollama_api_key: String,
    pub lancedb_path: String,
    pub default_user_id: String,
    pub tech_stack: String,
    pub distill_memories: bool,
    pub log_file: String,
    pub llm_model: String,
    pub embed_model: String,
    pub embed_dims: usize,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            ollama_url: "http://localhost:11434".into(),
            ollama_api_key: String::new(),
            lancedb_path: "~/.local/share/mindojo/lancedb".into(),
            default_user_id: "global".into(),
            tech_stack: String::new(),
            distill_memories: true,
            log_file: "~/.claude/logs/mindojo-mcp.log".into(),
            llm_model: "qwen3.5:4b".into(),
            embed_model: "qwen3-embedding:4b".into(),
            embed_dims: 2560,
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
    pub fn load() -> Self {
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

        let ollama_url = env_or("OLLAMA_URL", &defaults.ollama_url);
        let ollama_api_key = env_or("OLLAMA_API_KEY", &defaults.ollama_api_key);
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

        Settings {
            ollama_url,
            ollama_api_key,
            lancedb_path,
            default_user_id,
            tech_stack,
            distill_memories,
            log_file,
            llm_model,
            embed_model,
            embed_dims,
        }
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
        assert_eq!(d.ollama_url, "http://localhost:11434");
        assert_eq!(d.embed_dims, 2560);
        assert!(d.distill_memories);
    }
}
