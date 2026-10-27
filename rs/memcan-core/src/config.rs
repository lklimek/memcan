use std::path::{Path, PathBuf};
use std::str::FromStr;

use tracing::{debug, warn};

use crate::embed::{model_dims, resolve_model};
use crate::error::{MemcanError, Result};

/// Runtime-selectable LLM backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LlmProviderKind {
    /// Ollama server, configured by the slot's `*_MODEL` / `*_BASE_URL`
    /// (default `OLLAMA_HOST`) / `*_API_KEY` (default `OLLAMA_API_KEY`).
    Ollama,
    /// Native Anthropic Messages API.
    Anthropic,
    /// OpenAI Chat Completions API.
    OpenAi,
    /// OpenRouter through its OpenAI-compatible API.
    OpenRouter,
    /// Any OpenAI-compatible API; the slot's `*_BASE_URL` is required.
    OpenAiCompatible,
}

/// Accepted `LLM_PROVIDER` / `LLM_FALLBACK_PROVIDER` values, for error messages.
const ACCEPTED_PROVIDERS: &str = "'ollama', 'anthropic' (alias 'claude'), 'openai', 'openrouter', \
     'openai-compatible' (alias 'openai-compat')";

impl LlmProviderKind {
    /// Base URL used when the slot's `*_BASE_URL` is unset.
    ///
    /// `None` for Ollama (configured by `OLLAMA_HOST`) and for the generic
    /// OpenAI-compatible backend, which has no meaningful default.
    pub fn default_base_url(self) -> Option<&'static str> {
        match self {
            Self::Anthropic => Some("https://api.anthropic.com/v1/"),
            Self::OpenAi => Some("https://api.openai.com/v1"),
            Self::OpenRouter => Some("https://openrouter.ai/api/v1"),
            Self::Ollama | Self::OpenAiCompatible => None,
        }
    }
}

impl FromStr for LlmProviderKind {
    type Err = MemcanError;

    fn from_str(value: &str) -> Result<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "ollama" => Ok(Self::Ollama),
            "anthropic" | "claude" => Ok(Self::Anthropic),
            "openai" => Ok(Self::OpenAi),
            "openrouter" => Ok(Self::OpenRouter),
            "openai-compatible" | "openai-compat" => Ok(Self::OpenAiCompatible),
            _ => Err(MemcanError::Config(format!(
                "unknown LLM provider '{value}'; expected one of {ACCEPTED_PROVIDERS}"
            ))),
        }
    }
}

impl std::fmt::Display for LlmProviderKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Ollama => "ollama",
            Self::Anthropic => "anthropic",
            Self::OpenAi => "openai",
            Self::OpenRouter => "openrouter",
            Self::OpenAiCompatible => "openai-compatible",
        })
    }
}

/// Ollama model used when an Ollama slot sets no model.
pub const DEFAULT_OLLAMA_MODEL: &str = "lklimek/gemma4-text:26b-a4b-it-qat";

/// One of the two LLM backend slots.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LlmRole {
    /// Serves every request while it is available (`LLM_*`).
    Primary,
    /// Serves requests while the primary is unavailable (`LLM_FALLBACK_*`).
    Fallback,
}

/// Per-slot setting, for naming its environment variable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SlotVar {
    Provider,
    Model,
    ApiKey,
    BaseUrl,
}

impl LlmRole {
    /// Environment variable holding `var` for this slot.
    fn var(self, var: SlotVar) -> &'static str {
        match (self, var) {
            (Self::Primary, SlotVar::Provider) => "LLM_PROVIDER",
            (Self::Primary, SlotVar::Model) => "LLM_MODEL",
            (Self::Primary, SlotVar::ApiKey) => "LLM_API_KEY",
            (Self::Primary, SlotVar::BaseUrl) => "LLM_BASE_URL",
            (Self::Fallback, SlotVar::Provider) => "LLM_FALLBACK_PROVIDER",
            (Self::Fallback, SlotVar::Model) => "LLM_FALLBACK_MODEL",
            (Self::Fallback, SlotVar::ApiKey) => "LLM_FALLBACK_API_KEY",
            (Self::Fallback, SlotVar::BaseUrl) => "LLM_FALLBACK_BASE_URL",
        }
    }
}

impl SlotVar {
    /// The deprecated `OPENROUTER_*` variable that used to supply this setting.
    fn legacy_name(self) -> Option<&'static str> {
        match self {
            Self::Provider => None,
            Self::Model => Some("OPENROUTER_MODEL"),
            Self::ApiKey => Some("OPENROUTER_API_KEY"),
            Self::BaseUrl => Some("OPENROUTER_BASE_URL"),
        }
    }
}

/// Which slot settings came from deprecated `OPENROUTER_*` variables, so
/// configuration errors name the variable the operator actually set.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LegacyVarOrigins {
    /// Slot the legacy variables were mapped onto.
    role: Option<LlmRole>,
    /// Settings of that slot whose value a legacy variable supplied.
    supplied: Vec<SlotVar>,
}

impl LegacyVarOrigins {
    /// Variable that supplied the (set) value of `var` in `role`.
    fn supplied_by(&self, role: LlmRole, var: SlotVar) -> &'static str {
        match var.legacy_name() {
            Some(legacy) if self.role == Some(role) && self.supplied.contains(&var) => legacy,
            _ => role.var(var),
        }
    }

    /// Variable(s) that could supply the missing value of `var` in `role`.
    fn missing(&self, role: LlmRole, var: SlotVar) -> String {
        match var.legacy_name() {
            Some(legacy) if self.role == Some(role) => {
                format!("{} (or the deprecated {legacy})", role.var(var))
            }
            _ => role.var(var).to_string(),
        }
    }
}

impl std::fmt::Display for LlmRole {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Primary => "primary",
            Self::Fallback => "fallback",
        })
    }
}

/// A fully resolved LLM backend slot, with every default applied.
#[derive(Clone, PartialEq, Eq)]
pub struct LlmSlot {
    pub role: LlmRole,
    pub provider: LlmProviderKind,
    /// Model id sent to the backend.
    pub model: String,
    /// `x-api-key` for Anthropic, bearer token otherwise. Always set for a
    /// cloud backend; for Ollama, defaults to `OLLAMA_API_KEY`.
    pub api_key: Option<String>,
    /// Endpoint. Always set for a cloud backend; for Ollama, defaults to
    /// `OLLAMA_HOST`, and `None` leaves the client default.
    pub base_url: Option<String>,
}

impl std::fmt::Debug for LlmSlot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LlmSlot")
            .field("role", &self.role)
            .field("provider", &self.provider)
            .field("model", &self.model)
            .field("api_key", &self.api_key.as_ref().map(|_| "***"))
            .field("base_url", &self.base_url.as_deref().map(redact_url))
            .finish()
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
    /// Primary LLM backend (`LLM_PROVIDER`).
    pub llm_provider: LlmProviderKind,
    /// Primary model (`LLM_MODEL`). `None` selects [`DEFAULT_OLLAMA_MODEL`]
    /// for Ollama and is a configuration error for a cloud backend. The
    /// legacy `"ollama::"` prefix is accepted for Ollama.
    pub llm_model: Option<String>,
    /// Primary API key (`LLM_API_KEY`); Ollama falls back to `OLLAMA_API_KEY`.
    pub llm_api_key: Option<String>,
    /// Primary base URL (`LLM_BASE_URL`); `None` selects the backend default
    /// (`OLLAMA_HOST` for Ollama).
    pub llm_base_url: Option<String>,
    /// Optional fallback backend (`LLM_FALLBACK_PROVIDER`), used while the
    /// primary is unavailable.
    pub llm_fallback_provider: Option<LlmProviderKind>,
    /// Fallback model (`LLM_FALLBACK_MODEL`), defaulted like `llm_model`.
    pub llm_fallback_model: Option<String>,
    /// Fallback API key (`LLM_FALLBACK_API_KEY`), defaulted like `llm_api_key`.
    pub llm_fallback_api_key: Option<String>,
    /// Fallback base URL (`LLM_FALLBACK_BASE_URL`), defaulted like `llm_base_url`.
    pub llm_fallback_base_url: Option<String>,
    /// Slot settings supplied by deprecated `OPENROUTER_*` variables.
    pub llm_legacy_origins: LegacyVarOrigins,
    /// Fastembed model name, e.g. `"AllMiniLML6V2"`, `"BGESmallENV15"`.
    pub embed_model: String,
    /// Embedding vector dimensions (derived automatically from embed_model).
    pub embed_dims: usize,
    /// Ollama server URL, e.g. `"http://10.29.188.1:11434"`; the default base
    /// URL of every Ollama slot.
    pub ollama_host: Option<String>,
    /// Bearer token for Ollama endpoint auth; the default API key of every
    /// Ollama slot, sent as `Authorization: Bearer <key>`.
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
    /// Maximum idle time, in seconds, while receiving an incoming HTTP request
    /// body on `/mcp`. The clock resets on every received chunk, so it bounds a
    /// stalled connection without penalizing a large body that keeps arriving.
    /// `0` disables the bound.
    pub mcp_body_read_timeout_secs: u64,
}

impl std::fmt::Debug for Settings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Settings")
            .field("lancedb_path", &self.lancedb_path)
            .field("default_user_id", &self.default_user_id)
            .field("tech_stack", &self.tech_stack)
            .field("distill_memories", &self.distill_memories)
            .field("log_file", &self.log_file)
            .field("llm_provider", &self.llm_provider)
            .field("llm_model", &self.llm_model)
            .field("llm_api_key", &self.llm_api_key.as_ref().map(|_| "***"))
            .field(
                "llm_base_url",
                &self.llm_base_url.as_deref().map(redact_url),
            )
            .field("llm_fallback_provider", &self.llm_fallback_provider)
            .field("llm_fallback_model", &self.llm_fallback_model)
            .field(
                "llm_fallback_api_key",
                &self.llm_fallback_api_key.as_ref().map(|_| "***"),
            )
            .field(
                "llm_fallback_base_url",
                &self.llm_fallback_base_url.as_deref().map(redact_url),
            )
            .field("llm_legacy_origins", &self.llm_legacy_origins)
            .field("embed_model", &self.embed_model)
            .field("embed_dims", &self.embed_dims)
            .field("ollama_host", &self.ollama_host.as_deref().map(redact_url))
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
            .field(
                "mcp_body_read_timeout_secs",
                &self.mcp_body_read_timeout_secs,
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
            llm_provider: LlmProviderKind::Ollama,
            llm_model: None,
            llm_api_key: None,
            llm_base_url: None,
            llm_fallback_provider: None,
            llm_fallback_model: None,
            llm_fallback_api_key: None,
            llm_fallback_base_url: None,
            llm_legacy_origins: LegacyVarOrigins::default(),
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
            mcp_body_read_timeout_secs: 30,
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
        let llm_provider = env_non_blank("LLM_PROVIDER")
            .map(|value| value.parse())
            .transpose()?
            .unwrap_or(defaults.llm_provider);
        let mut llm_fallback_provider = env_non_blank("LLM_FALLBACK_PROVIDER")
            .map(|value| value.parse())
            .transpose()?;
        let mut primary = SlotEnv::read(LlmRole::Primary);
        let mut fallback = SlotEnv::read(LlmRole::Fallback);
        let legacy = LegacyOpenRouterEnv::read();
        let legacy_set = !legacy.is_empty();
        let legacy_report = apply_legacy_openrouter_env(
            llm_provider,
            &mut primary,
            llm_fallback_provider,
            &mut fallback,
            legacy,
        );
        legacy_report.log(legacy_set);
        if legacy_report.drop_fallback {
            llm_fallback_provider = None;
        }
        let llm_legacy_origins = LegacyVarOrigins {
            role: legacy_report.role,
            supplied: legacy_report.supplied,
        };
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
            debug!(ollama_host = %redact_url(host), "OLLAMA_HOST configured");
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
        let mcp_body_read_timeout_secs = env_u64_or_warn(
            "MEMCAN_BODY_READ_TIMEOUT",
            defaults.mcp_body_read_timeout_secs,
        );

        let settings = Settings {
            lancedb_path,
            default_user_id,
            tech_stack,
            distill_memories,
            log_file,
            llm_provider,
            llm_model: primary.model,
            llm_api_key: primary.api_key,
            llm_base_url: primary.base_url,
            llm_fallback_provider,
            llm_fallback_model: fallback.model,
            llm_fallback_api_key: fallback.api_key,
            llm_fallback_base_url: fallback.base_url,
            llm_legacy_origins,
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
            mcp_body_read_timeout_secs,
        };
        settings.validate()?;
        Ok(settings)
    }

    /// Check invariants on loaded settings.
    fn validate(&self) -> Result<()> {
        if self.lancedb_path.is_empty() {
            return Err(MemcanError::Config("LANCEDB_PATH must not be empty".into()));
        }

        let primary = self.primary_slot()?;
        let fallback = self.fallback_slot()?;
        for slot in std::iter::once(&primary).chain(fallback.as_ref()) {
            if breaks_json_mode(slot) {
                warn!(
                    role = %slot.role,
                    backend = %slot.provider,
                    "The {} LLM slot sends Anthropic's OpenAI-compatible endpoint \
                     `response_format: json_object`, which it has been observed to reject with \
                     a 400 (vendor docs say it is ignored), failing every JSON-mode call; set \
                     {}=anthropic for the native Messages API",
                    slot.role,
                    slot.role.var(SlotVar::Provider)
                );
            }
        }
        if fallback.is_some_and(|fallback| {
            fallback.provider == primary.provider
                && fallback.model == primary.model
                && fallback.base_url == primary.base_url
                && fallback.api_key == primary.api_key
        }) {
            warn!(
                provider = %primary.provider,
                model = %primary.model,
                "The LLM fallback slot duplicates the primary; it can only absorb transient faults"
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

    /// The resolved primary LLM slot.
    ///
    /// Fails with an error naming the exact missing or invalid variable when
    /// a cloud backend lacks its model, API key, or base URL.
    pub fn primary_slot(&self) -> Result<LlmSlot> {
        self.resolve_slot(LlmRole::Primary, self.llm_provider)
    }

    /// The resolved fallback LLM slot, if one is configured.
    pub fn fallback_slot(&self) -> Result<Option<LlmSlot>> {
        self.llm_fallback_provider
            .map(|provider| self.resolve_slot(LlmRole::Fallback, provider))
            .transpose()
    }

    /// Raw `(model, api_key, base_url)` of a slot, blank values dropped.
    fn slot_values(&self, role: LlmRole) -> (Option<&str>, Option<&str>, Option<&str>) {
        match role {
            LlmRole::Primary => (
                non_blank(&self.llm_model),
                non_blank(&self.llm_api_key),
                non_blank(&self.llm_base_url),
            ),
            LlmRole::Fallback => (
                non_blank(&self.llm_fallback_model),
                non_blank(&self.llm_fallback_api_key),
                non_blank(&self.llm_fallback_base_url),
            ),
        }
    }

    fn resolve_ollama_slot(&self, role: LlmRole) -> LlmSlot {
        let (model, api_key, base_url) = self.slot_values(role);
        LlmSlot {
            role,
            provider: LlmProviderKind::Ollama,
            model: model.unwrap_or(DEFAULT_OLLAMA_MODEL).to_string(),
            api_key: api_key
                .or(non_blank(&self.ollama_api_key))
                .map(str::to_string),
            base_url: base_url
                .or(non_blank(&self.ollama_host))
                .map(str::to_string),
        }
    }

    fn resolve_slot(&self, role: LlmRole, provider: LlmProviderKind) -> Result<LlmSlot> {
        if provider == LlmProviderKind::Ollama {
            return Ok(self.resolve_ollama_slot(role));
        }
        let (model, api_key, base_url) = self.slot_values(role);
        // A blank value counts as missing: a whitespace key would otherwise
        // reach the HTTP client as an empty credential and 401 on every
        // request instead of failing at startup.
        let origins = &self.llm_legacy_origins;
        let required = |var: SlotVar| {
            MemcanError::Config(format!(
                "{} is required when {}={provider}",
                origins.missing(role, var),
                role.var(SlotVar::Provider)
            ))
        };
        let api_key = api_key.ok_or_else(|| required(SlotVar::ApiKey))?;
        let model = model.ok_or_else(|| required(SlotVar::Model))?;
        let base_url = base_url
            .or(provider.default_base_url())
            .ok_or_else(|| required(SlotVar::BaseUrl))?;
        require_https_or_loopback(origins.supplied_by(role, SlotVar::BaseUrl), base_url)?;
        Ok(LlmSlot {
            role,
            provider,
            model: model.to_string(),
            api_key: Some(api_key.to_string()),
            base_url: Some(base_url.to_string()),
        })
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

/// Read an unsigned integer setting from the environment, warning on garbage.
///
/// A mistyped value falls back to `default` like any other setting, but says so:
/// a timeout that quietly reverts to its default is the same class of silent
/// failure the timeout itself guards against.
fn env_u64_or_warn(key: &str, default: u64) -> u64 {
    let Ok(raw) = std::env::var(key) else {
        return default;
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return default;
    }
    trimmed.parse::<u64>().unwrap_or_else(|_| {
        warn!("{key}='{raw}' is not a non-negative integer; falling back to {default}");
        default
    })
}

/// Read an environment variable, trimmed, treating a blank value as unset.
///
/// Docker Compose passes unset variables through as empty strings, which must
/// not shadow a default.
fn env_non_blank(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// Trimmed view of an optional setting, `None` when blank.
fn non_blank(value: &Option<String>) -> Option<&str> {
    value
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

/// Render a URL for logs and errors without credentials: userinfo, query,
/// and fragment are dropped, since gateways sometimes carry a token there.
pub(crate) fn redact_url(url: &str) -> String {
    match reqwest::Url::parse(url) {
        Ok(mut parsed) => {
            // Both only fail for URLs that cannot carry userinfo at all.
            let _ = parsed.set_username("");
            let _ = parsed.set_password(None);
            parsed.set_query(None);
            parsed.set_fragment(None);
            parsed.to_string()
        }
        Err(_) => "<unparsable URL>".to_string(),
    }
}

/// `true` when an OpenAI-adapter slot targets Anthropic's OpenAI-compatible
/// endpoint, which has been observed to 400 on `response_format: json_object`.
fn breaks_json_mode(slot: &LlmSlot) -> bool {
    let openai_adapter = matches!(
        slot.provider,
        LlmProviderKind::OpenAi | LlmProviderKind::OpenRouter | LlmProviderKind::OpenAiCompatible
    );
    openai_adapter
        && slot
            .base_url
            .as_deref()
            .and_then(|url| reqwest::Url::parse(url).ok())
            .is_some_and(|url| {
                url.host_str()
                    .is_some_and(|host| host.eq_ignore_ascii_case("api.anthropic.com"))
            })
}

/// Reject a cloud base URL that is unparsable, or plain HTTP to a remote host.
///
/// The URL itself is not echoed: an unparsable value may still hold a secret.
fn require_https_or_loopback(var: &str, base_url: &str) -> Result<()> {
    let parsed = reqwest::Url::parse(base_url)
        .map_err(|error| MemcanError::Config(format!("{var} is not a valid URL: {error}")))?;
    let loopback = matches!(
        parsed.host_str(),
        Some("localhost" | "127.0.0.1" | "::1" | "[::1]")
    );
    if parsed.scheme() != "https" && !loopback {
        return Err(MemcanError::Config(format!(
            "{var} must use HTTPS when configured with a non-loopback host"
        )));
    }
    Ok(())
}

/// One slot's `MODEL` / `API_KEY` / `BASE_URL` variables, as read.
#[derive(Debug, Default)]
struct SlotEnv {
    model: Option<String>,
    api_key: Option<String>,
    base_url: Option<String>,
}

impl SlotEnv {
    fn read(role: LlmRole) -> Self {
        Self {
            model: env_non_blank(role.var(SlotVar::Model)),
            api_key: env_non_blank(role.var(SlotVar::ApiKey)),
            base_url: env_non_blank(role.var(SlotVar::BaseUrl)),
        }
    }
}

/// The deprecated single-cloud `OPENROUTER_*` variables, as read.
#[derive(Debug, Default)]
struct LegacyOpenRouterEnv {
    api_key: Option<String>,
    model: Option<String>,
    base_url: Option<String>,
}

impl LegacyOpenRouterEnv {
    fn read() -> Self {
        Self {
            api_key: env_non_blank("OPENROUTER_API_KEY"),
            model: env_non_blank("OPENROUTER_MODEL"),
            base_url: env_non_blank("OPENROUTER_BASE_URL"),
        }
    }

    fn is_empty(&self) -> bool {
        self.api_key.is_none() && self.model.is_none() && self.base_url.is_none()
    }
}

/// How a primary-slot clash between `LLM_MODEL` and `OPENROUTER_MODEL` was
/// settled, reported by `load()` as a warning naming where each value went.
#[derive(Debug, Clone, PartialEq, Eq)]
enum LegacyModelNote {
    /// Pre-slot `.env`: `LLM_MODEL` kept its old meaning (the Ollama model)
    /// and became the Ollama fallback's model.
    LlmModelMovedToFallback { cloud: String, ollama: String },
    /// Pre-slot `.env` without an Ollama fallback: `LLM_MODEL` kept its old
    /// meaning and has no slot to configure.
    LlmModelIgnored { cloud: String, ignored: String },
    /// The file also uses per-slot settings, so `LLM_MODEL` is the primary
    /// model and the stale `OPENROUTER_MODEL` is ignored.
    LegacyModelIgnored { primary: String, ignored: String },
}

/// What [`apply_legacy_openrouter_env`] did, for the startup warnings.
#[derive(Debug, Default, PartialEq, Eq)]
struct LegacyReport {
    /// Slot the legacy values were mapped onto; `None` when none reads them.
    role: Option<LlmRole>,
    /// Legacy variables ignored because the slot variable is set.
    shadowed: Vec<&'static str>,
    /// Settings of the mapped slot whose value a legacy variable supplied.
    supplied: Vec<SlotVar>,
    model_note: Option<LegacyModelNote>,
    /// Pre-slot file naming the legacy backend for both slots: the fallback
    /// duplicates the primary and is dropped, as it was before slots.
    drop_fallback: bool,
}

impl LegacyReport {
    fn log(&self, legacy_set: bool) {
        if !legacy_set {
            return;
        }
        if self.drop_fallback {
            warn!(
                "Ignoring configured LLM fallback because it matches the primary backend \
                 (deprecated OPENROUTER_* settings configure a single slot)"
            );
        }
        let Some(role) = self.role else {
            warn!(
                "Deprecated OPENROUTER_* settings are ignored: no LLM slot uses the \
                 'openrouter' or 'openai-compatible' backend"
            );
            return;
        };
        warn!(
            slot = %role,
            shadowed = %self.shadowed.join(", "),
            "Deprecated OPENROUTER_* settings are mapped onto the {role} LLM slot; rename them \
             to {}, {} and {} (a set slot variable wins over its OPENROUTER_* name)",
            role.var(SlotVar::ApiKey),
            role.var(SlotVar::Model),
            role.var(SlotVar::BaseUrl),
        );
        match &self.model_note {
            Some(LegacyModelNote::LlmModelMovedToFallback { cloud, ollama }) => warn!(
                "Legacy mode: OPENROUTER_MODEL='{cloud}' is the primary model and \
                 LLM_MODEL='{ollama}' keeps its pre-slot meaning as the Ollama fallback's model; \
                 set LLM_FALLBACK_MODEL to make LLM_MODEL the primary model"
            ),
            Some(LegacyModelNote::LlmModelIgnored { cloud, ignored }) => warn!(
                "Legacy mode: OPENROUTER_MODEL='{cloud}' is the primary model and \
                 LLM_MODEL='{ignored}' (pre-slot meaning: the Ollama model) is ignored; remove \
                 OPENROUTER_MODEL to make LLM_MODEL the primary model"
            ),
            Some(LegacyModelNote::LegacyModelIgnored { primary, ignored }) => warn!(
                "LLM_MODEL='{primary}' is the primary model; OPENROUTER_MODEL='{ignored}' is \
                 ignored because per-slot settings are in use"
            ),
            None => {}
        }
    }
}

/// Map deprecated `OPENROUTER_*` settings onto the slot they used to configure.
///
/// Before per-slot settings, the single cloud backend (`openrouter`, often
/// aimed at another OpenAI-compatible API through `OPENROUTER_BASE_URL`) read
/// `OPENROUTER_*`, while `LLM_MODEL` always named the Ollama model. The values
/// land on the first `openrouter` / `openai-compatible` slot, where a set slot
/// variable wins.
///
/// `LLM_MODEL` is the one ambiguous variable: on a primary legacy slot with
/// `OPENROUTER_MODEL` also set, it is either a pre-slot Ollama model or a
/// per-slot primary model. Any setting the pre-slot format did not have — a
/// non-`openrouter` primary, `LLM_API_KEY` / `LLM_BASE_URL`, an
/// `LLM_FALLBACK_*` value, or a fallback other than `ollama` / `openrouter` —
/// proves the per-slot reading, so `LLM_MODEL` stays the primary model.
/// Otherwise the pre-slot reading holds: `LLM_MODEL` becomes the Ollama
/// fallback's model, or is ignored without an Ollama fallback.
fn apply_legacy_openrouter_env(
    primary_kind: LlmProviderKind,
    primary: &mut SlotEnv,
    fallback_kind: Option<LlmProviderKind>,
    fallback: &mut SlotEnv,
    legacy: LegacyOpenRouterEnv,
) -> LegacyReport {
    let mut report = LegacyReport::default();
    if legacy.is_empty() {
        return report;
    }
    let reads_legacy = |kind: LlmProviderKind| {
        matches!(
            kind,
            LlmProviderKind::OpenRouter | LlmProviderKind::OpenAiCompatible
        )
    };
    let role = if reads_legacy(primary_kind) {
        LlmRole::Primary
    } else if fallback_kind.is_some_and(reads_legacy) {
        LlmRole::Fallback
    } else {
        return report;
    };
    report.role = Some(role);
    report.drop_fallback = role == LlmRole::Primary
        && fallback_kind == Some(primary_kind)
        && fallback.model.is_none()
        && fallback.api_key.is_none()
        && fallback.base_url.is_none();

    let per_slot_format = primary_kind != LlmProviderKind::OpenRouter
        || primary.api_key.is_some()
        || primary.base_url.is_some()
        || fallback.model.is_some()
        || fallback.api_key.is_some()
        || fallback.base_url.is_some()
        || !matches!(
            fallback_kind,
            None | Some(LlmProviderKind::Ollama | LlmProviderKind::OpenRouter)
        );

    let slot = match role {
        LlmRole::Primary => {
            if let Some(cloud) = legacy.model {
                match primary.model.take() {
                    None => {
                        report.supplied.push(SlotVar::Model);
                        primary.model = Some(cloud);
                    }
                    Some(model) if model == cloud => primary.model = Some(model),
                    Some(model) if per_slot_format => {
                        report.model_note = Some(LegacyModelNote::LegacyModelIgnored {
                            primary: model.clone(),
                            ignored: cloud,
                        });
                        primary.model = Some(model);
                    }
                    Some(ollama) => {
                        report.supplied.push(SlotVar::Model);
                        primary.model = Some(cloud.clone());
                        if fallback_kind == Some(LlmProviderKind::Ollama) {
                            report.model_note = Some(LegacyModelNote::LlmModelMovedToFallback {
                                cloud,
                                ollama: ollama.clone(),
                            });
                            fallback.model = Some(ollama);
                        } else if crate::ollama::strip_ollama_prefix(&ollama)
                            != DEFAULT_OLLAMA_MODEL
                        {
                            // The old Compose file injected the default model;
                            // that carries no operator intent worth a warning.
                            report.model_note = Some(LegacyModelNote::LlmModelIgnored {
                                cloud,
                                ignored: ollama,
                            });
                        }
                    }
                }
            }
            primary
        }
        LlmRole::Fallback => {
            match (&fallback.model, legacy.model) {
                (None, Some(model)) => {
                    report.supplied.push(SlotVar::Model);
                    fallback.model = Some(model);
                }
                (None, None) => {}
                (Some(_), Some(_)) => report.shadowed.push("OPENROUTER_MODEL"),
                (Some(_), None) => {}
            }
            fallback
        }
    };
    match (&slot.api_key, legacy.api_key) {
        (None, Some(key)) => {
            report.supplied.push(SlotVar::ApiKey);
            slot.api_key = Some(key);
        }
        (None, None) => {}
        (Some(_), Some(_)) => report.shadowed.push("OPENROUTER_API_KEY"),
        (Some(_), None) => {}
    }
    match (&slot.base_url, legacy.base_url) {
        (None, Some(url)) => {
            report.supplied.push(SlotVar::BaseUrl);
            slot.base_url = Some(url);
        }
        (None, None) => {}
        (Some(_), Some(_)) => report.shadowed.push("OPENROUTER_BASE_URL"),
        (Some(_), None) => {}
    }
    report
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

    fn cloud_primary(kind: LlmProviderKind) -> Settings {
        Settings {
            llm_provider: kind,
            llm_model: Some("claude-haiku-5-5".into()),
            llm_api_key: Some("primary-key".into()),
            ..Settings::default()
        }
    }

    fn with_cloud_fallback(settings: Settings, kind: LlmProviderKind) -> Settings {
        Settings {
            llm_fallback_provider: Some(kind),
            llm_fallback_model: Some("gpt-4o-mini".into()),
            llm_fallback_api_key: Some("fallback-key".into()),
            ..settings
        }
    }

    /// Isolate `Settings::load()` from the host's `.env` files and LLM env.
    fn isolated_llm_env(temp_path: &str, overrides: &[(&'static str, Option<&str>)]) -> EnvGuard {
        let mut values: Vec<(&'static str, Option<&str>)> = vec![
            ("HOME", Some(temp_path)),
            ("XDG_CONFIG_HOME", Some(temp_path)),
            ("XDG_DATA_HOME", Some(temp_path)),
            ("LLM_PROVIDER", None),
            ("LLM_MODEL", None),
            ("LLM_API_KEY", None),
            ("LLM_BASE_URL", None),
            ("LLM_FALLBACK_PROVIDER", None),
            ("LLM_FALLBACK_MODEL", None),
            ("LLM_FALLBACK_API_KEY", None),
            ("LLM_FALLBACK_BASE_URL", None),
            ("OPENROUTER_API_KEY", None),
            ("OPENROUTER_MODEL", None),
            ("OPENROUTER_BASE_URL", None),
            ("OLLAMA_HOST", None),
            ("OLLAMA_API_KEY", None),
        ];
        for (key, value) in overrides {
            match values.iter_mut().find(|(existing, _)| existing == key) {
                Some(slot) => slot.1 = *value,
                None => values.push((key, *value)),
            }
        }
        EnvGuard::set(&values)
    }

    fn load_with(overrides: &[(&'static str, Option<&str>)]) -> Result<Settings> {
        let temp = tempfile::tempdir().unwrap();
        let _environment = isolated_llm_env(temp.path().to_str().unwrap(), overrides);
        Settings::load()
    }

    #[test]
    fn ollama_slot_defaults_to_the_ollama_model_host_and_key() {
        let settings = Settings {
            ollama_host: Some("http://gpu:11434".into()),
            ollama_api_key: Some("ollama-key".into()),
            ..Settings::default()
        };

        let primary = settings.primary_slot().unwrap();
        assert_eq!(primary.role, LlmRole::Primary);
        assert_eq!(primary.provider, LlmProviderKind::Ollama);
        assert_eq!(primary.model, DEFAULT_OLLAMA_MODEL);
        assert_eq!(primary.api_key.as_deref(), Some("ollama-key"));
        assert_eq!(primary.base_url.as_deref(), Some("http://gpu:11434"));
        assert_eq!(settings.fallback_slot().unwrap(), None);

        // Slot values win over the Ollama-wide defaults.
        let overridden = Settings {
            llm_model: Some("qwen3.5:9b".into()),
            llm_api_key: Some("slot-key".into()),
            llm_base_url: Some("http://other:11434".into()),
            ..settings
        };
        let primary = overridden.primary_slot().unwrap();
        assert_eq!(primary.model, "qwen3.5:9b");
        assert_eq!(primary.api_key.as_deref(), Some("slot-key"));
        assert_eq!(primary.base_url.as_deref(), Some("http://other:11434"));
    }

    #[test]
    fn cloud_slots_resolve_alias_default_base_urls() {
        for (kind, url) in [
            (LlmProviderKind::Anthropic, "https://api.anthropic.com/v1/"),
            (LlmProviderKind::OpenAi, "https://api.openai.com/v1"),
            (LlmProviderKind::OpenRouter, "https://openrouter.ai/api/v1"),
        ] {
            let primary = cloud_primary(kind).primary_slot().unwrap();
            assert_eq!(primary.base_url.as_deref(), Some(url), "{kind}");
            assert_eq!(primary.api_key.as_deref(), Some("primary-key"));
            assert_eq!(primary.model, "claude-haiku-5-5");
        }

        let overridden = Settings {
            llm_base_url: Some("https://gateway.example/v1".into()),
            ..cloud_primary(LlmProviderKind::Anthropic)
        };
        assert_eq!(
            overridden.primary_slot().unwrap().base_url.as_deref(),
            Some("https://gateway.example/v1")
        );
    }

    #[test]
    fn missing_cloud_settings_name_the_exact_variable() {
        let primary = cloud_primary(LlmProviderKind::Anthropic);
        for (settings, var) in [
            (
                Settings {
                    llm_api_key: None,
                    ..primary.clone()
                },
                "LLM_API_KEY",
            ),
            (
                Settings {
                    llm_api_key: Some("   ".into()),
                    ..primary.clone()
                },
                "LLM_API_KEY",
            ),
            (
                Settings {
                    llm_model: None,
                    ..primary.clone()
                },
                "LLM_MODEL",
            ),
            (
                Settings {
                    llm_fallback_api_key: None,
                    ..with_cloud_fallback(primary.clone(), LlmProviderKind::OpenAi)
                },
                "LLM_FALLBACK_API_KEY",
            ),
            (
                Settings {
                    llm_fallback_model: Some(String::new()),
                    ..with_cloud_fallback(primary.clone(), LlmProviderKind::OpenAi)
                },
                "LLM_FALLBACK_MODEL",
            ),
            (
                with_cloud_fallback(primary.clone(), LlmProviderKind::OpenAiCompatible),
                "LLM_FALLBACK_BASE_URL",
            ),
            (
                cloud_primary(LlmProviderKind::OpenAiCompatible),
                "LLM_BASE_URL",
            ),
        ] {
            let error = settings.validate().unwrap_err().to_string();
            assert!(error.contains(var), "expected {var}: {error}");
        }
    }

    #[test]
    fn any_slot_combination_is_valid() {
        use LlmProviderKind::{Anthropic, Ollama, OpenAi, OpenRouter};
        for (primary, fallback) in [
            (Anthropic, Anthropic),
            (Anthropic, OpenAi),
            (OpenRouter, Anthropic),
            (Anthropic, Ollama),
        ] {
            let settings = with_cloud_fallback(cloud_primary(primary), fallback);
            assert!(
                settings.validate().is_ok(),
                "{primary} + {fallback} should validate"
            );
        }

        let ollama_twice = Settings {
            llm_fallback_provider: Some(Ollama),
            llm_fallback_base_url: Some("http://second-gpu:11434".into()),
            ..Settings::default()
        };
        assert!(ollama_twice.validate().is_ok());
    }

    #[test]
    fn same_backend_kind_twice_keeps_separate_credentials() {
        let settings = Settings {
            llm_fallback_model: Some("claude-opus-5-5".into()),
            ..with_cloud_fallback(
                cloud_primary(LlmProviderKind::Anthropic),
                LlmProviderKind::Anthropic,
            )
        };

        let primary = settings.primary_slot().unwrap();
        let fallback = settings.fallback_slot().unwrap().unwrap();
        assert_eq!(fallback.role, LlmRole::Fallback);
        assert_eq!(primary.api_key.as_deref(), Some("primary-key"));
        assert_eq!(fallback.api_key.as_deref(), Some("fallback-key"));
        assert_eq!(fallback.model, "claude-opus-5-5");
        assert_eq!(primary.base_url, fallback.base_url);
    }

    #[test]
    fn unused_base_url_is_not_validated() {
        let settings = Settings {
            llm_fallback_base_url: Some("not-a-url".into()),
            ..Settings::default()
        };

        assert!(settings.validate().is_ok());
    }

    #[test]
    fn cloud_base_url_http_requires_a_loopback_host() {
        for base_url in [
            "http://localhost:8080/v1",
            "http://127.0.0.1:8080/v1",
            "http://[::1]:8080/v1",
        ] {
            let loopback = Settings {
                llm_base_url: Some(base_url.into()),
                ..cloud_primary(LlmProviderKind::OpenRouter)
            };
            assert!(
                loopback.validate().is_ok(),
                "loopback URL should be accepted: {base_url}"
            );
        }

        let remote = Settings {
            llm_fallback_base_url: Some("http://gateway.example/v1".into()),
            ..with_cloud_fallback(Settings::default(), LlmProviderKind::OpenAiCompatible)
        };
        let error = remote.validate().unwrap_err().to_string();
        assert!(error.contains("LLM_FALLBACK_BASE_URL"), "{error}");
        assert!(error.contains("HTTPS"), "{error}");

        // Ollama keeps plain-HTTP LAN hosts working.
        let ollama = Settings {
            llm_base_url: Some("http://gpu.lan:11434".into()),
            ..Settings::default()
        };
        assert!(ollama.validate().is_ok());
    }

    #[test]
    fn provider_aliases_parse_case_insensitively() {
        for (raw, expected) in [
            ("ollama", LlmProviderKind::Ollama),
            ("OLLAMA", LlmProviderKind::Ollama),
            ("anthropic", LlmProviderKind::Anthropic),
            ("Claude", LlmProviderKind::Anthropic),
            ("openai", LlmProviderKind::OpenAi),
            ("OpenAI", LlmProviderKind::OpenAi),
            ("OPENROUTER", LlmProviderKind::OpenRouter),
            ("openai-compatible", LlmProviderKind::OpenAiCompatible),
            ("OpenAI-Compat", LlmProviderKind::OpenAiCompatible),
            (" anthropic ", LlmProviderKind::Anthropic),
        ] {
            assert_eq!(
                raw.parse::<LlmProviderKind>().unwrap(),
                expected,
                "{raw:?} should parse"
            );
        }
    }

    #[test]
    fn display_round_trips_through_parsing() {
        for kind in [
            LlmProviderKind::Ollama,
            LlmProviderKind::Anthropic,
            LlmProviderKind::OpenAi,
            LlmProviderKind::OpenRouter,
            LlmProviderKind::OpenAiCompatible,
        ] {
            assert_eq!(kind.to_string().parse::<LlmProviderKind>().unwrap(), kind);
        }
    }

    #[test]
    fn unknown_provider_error_lists_every_accepted_value() {
        let error = "gemini".parse::<LlmProviderKind>().unwrap_err().to_string();
        for accepted in [
            "'gemini'",
            "ollama",
            "anthropic",
            "claude",
            "openai",
            "openrouter",
            "openai-compatible",
            "openai-compat",
        ] {
            assert!(error.contains(accepted), "missing {accepted}: {error}");
        }
    }

    #[test]
    fn debug_redacts_every_api_key() {
        let settings = Settings {
            llm_api_key: Some("raw-primary-secret".into()),
            llm_fallback_api_key: Some("raw-fallback-secret".into()),
            ..Settings::default()
        };
        let debug = format!("{settings:?}");
        assert!(!debug.contains("raw-primary-secret"));
        assert!(!debug.contains("raw-fallback-secret"));
        assert!(debug.contains("llm_api_key: Some(\"***\")"));
        assert!(debug.contains("llm_fallback_api_key: Some(\"***\")"));

        let slot = format!(
            "{:?}",
            cloud_primary(LlmProviderKind::Anthropic).primary_slot()
        );
        assert!(!slot.contains("primary-key"), "{slot}");
    }

    #[test]
    #[serial]
    fn slot_env_vars_load() {
        let settings = load_with(&[
            ("LLM_PROVIDER", Some("Claude")),
            ("LLM_MODEL", Some("claude-haiku-5-5")),
            ("LLM_API_KEY", Some("  primary-key  ")),
            ("LLM_FALLBACK_PROVIDER", Some("openai")),
            ("LLM_FALLBACK_MODEL", Some("gpt-4o-mini")),
            ("LLM_FALLBACK_API_KEY", Some("fallback-key")),
            ("LLM_FALLBACK_BASE_URL", Some("https://gateway.example/v1")),
        ])
        .unwrap();

        let primary = settings.primary_slot().unwrap();
        assert_eq!(primary.provider, LlmProviderKind::Anthropic);
        assert_eq!(primary.model, "claude-haiku-5-5");
        assert_eq!(primary.api_key.as_deref(), Some("primary-key"));
        assert_eq!(
            primary.base_url.as_deref(),
            Some("https://api.anthropic.com/v1/")
        );
        let fallback = settings.fallback_slot().unwrap().unwrap();
        assert_eq!(fallback.provider, LlmProviderKind::OpenAi);
        assert_eq!(fallback.model, "gpt-4o-mini");
        assert_eq!(fallback.api_key.as_deref(), Some("fallback-key"));
        assert_eq!(
            fallback.base_url.as_deref(),
            Some("https://gateway.example/v1")
        );
    }

    #[test]
    #[serial]
    fn blank_env_values_count_as_unset() {
        // Docker Compose passes unset variables as empty strings.
        let settings = load_with(&[
            ("LLM_PROVIDER", Some("")),
            ("LLM_MODEL", Some("")),
            ("LLM_BASE_URL", Some("  ")),
            ("LLM_FALLBACK_PROVIDER", Some("")),
            ("OPENROUTER_BASE_URL", Some("")),
        ])
        .unwrap();

        assert_eq!(settings.llm_provider, LlmProviderKind::Ollama);
        assert_eq!(settings.llm_model, None);
        assert_eq!(settings.llm_base_url, None);
        assert_eq!(settings.llm_fallback_provider, None);
        assert_eq!(settings.primary_slot().unwrap().model, DEFAULT_OLLAMA_MODEL);
    }

    #[test]
    #[serial]
    fn cloud_primary_without_a_model_fails_load_naming_the_variable() {
        // No Ollama model name may leak into a cloud slot as its default.
        let error = load_with(&[
            ("LLM_PROVIDER", Some("anthropic")),
            ("LLM_API_KEY", Some("key")),
        ])
        .unwrap_err()
        .to_string();
        assert!(error.contains("LLM_MODEL"), "{error}");
    }

    #[test]
    #[serial]
    fn unknown_provider_env_value_fails_load() {
        let error = load_with(&[("LLM_FALLBACK_PROVIDER", Some("anthropik"))])
            .unwrap_err()
            .to_string();
        assert!(error.contains("anthropik"), "{error}");
    }

    #[test]
    #[serial]
    fn legacy_openrouter_primary_with_ollama_fallback_keeps_old_semantics() {
        // The exact pre-slot `.env`: LLM_MODEL named the Ollama model and
        // OPENROUTER_MODEL the cloud one.
        let settings = load_with(&[
            ("LLM_PROVIDER", Some("openrouter")),
            ("LLM_FALLBACK_PROVIDER", Some("ollama")),
            ("LLM_MODEL", Some("qwen3.5:9b")),
            ("OPENROUTER_API_KEY", Some("legacy-key")),
            ("OPENROUTER_MODEL", Some("claude-haiku-5-5")),
            ("OPENROUTER_BASE_URL", Some("https://gateway.example/v1")),
            ("OLLAMA_HOST", Some("http://gpu:11434")),
        ])
        .unwrap();

        let primary = settings.primary_slot().unwrap();
        assert_eq!(primary.provider, LlmProviderKind::OpenRouter);
        assert_eq!(primary.model, "claude-haiku-5-5");
        assert_eq!(primary.api_key.as_deref(), Some("legacy-key"));
        assert_eq!(
            primary.base_url.as_deref(),
            Some("https://gateway.example/v1")
        );
        let fallback = settings.fallback_slot().unwrap().unwrap();
        assert_eq!(fallback.provider, LlmProviderKind::Ollama);
        assert_eq!(fallback.model, "qwen3.5:9b");
        assert_eq!(fallback.base_url.as_deref(), Some("http://gpu:11434"));
    }

    #[test]
    #[serial]
    fn legacy_ollama_primary_with_openrouter_fallback_keeps_old_semantics() {
        let settings = load_with(&[
            ("LLM_PROVIDER", Some("ollama")),
            ("LLM_FALLBACK_PROVIDER", Some("openrouter")),
            ("LLM_MODEL", Some("qwen3.5:9b")),
            ("OPENROUTER_API_KEY", Some("legacy-key")),
            ("OPENROUTER_MODEL", Some("openai/gpt-4o-mini")),
        ])
        .unwrap();

        let primary = settings.primary_slot().unwrap();
        assert_eq!(primary.provider, LlmProviderKind::Ollama);
        assert_eq!(primary.model, "qwen3.5:9b");
        let fallback = settings.fallback_slot().unwrap().unwrap();
        assert_eq!(fallback.model, "openai/gpt-4o-mini");
        assert_eq!(fallback.api_key.as_deref(), Some("legacy-key"));
        assert_eq!(
            fallback.base_url.as_deref(),
            Some("https://openrouter.ai/api/v1")
        );
    }

    #[test]
    #[serial]
    fn legacy_openrouter_primary_alone_ignores_the_ollama_llm_model() {
        let settings = load_with(&[
            ("LLM_PROVIDER", Some("openrouter")),
            ("LLM_MODEL", Some("qwen3.5:9b")),
            ("OPENROUTER_API_KEY", Some("legacy-key")),
            ("OPENROUTER_MODEL", Some("openai/gpt-4o-mini")),
        ])
        .unwrap();

        assert_eq!(settings.primary_slot().unwrap().model, "openai/gpt-4o-mini");
        assert_eq!(settings.llm_fallback_provider, None);
    }

    #[test]
    #[serial]
    fn slot_vars_win_over_legacy_ones() {
        let settings = load_with(&[
            ("LLM_PROVIDER", Some("ollama")),
            ("LLM_FALLBACK_PROVIDER", Some("openai-compatible")),
            ("LLM_FALLBACK_MODEL", Some("new-model")),
            ("LLM_FALLBACK_API_KEY", Some("new-key")),
            ("LLM_FALLBACK_BASE_URL", Some("https://new.example/v1")),
            ("OPENROUTER_API_KEY", Some("legacy-key")),
            ("OPENROUTER_MODEL", Some("legacy-model")),
            ("OPENROUTER_BASE_URL", Some("https://legacy.example/v1")),
        ])
        .unwrap();

        let fallback = settings.fallback_slot().unwrap().unwrap();
        assert_eq!(fallback.model, "new-model");
        assert_eq!(fallback.api_key.as_deref(), Some("new-key"));
        assert_eq!(fallback.base_url.as_deref(), Some("https://new.example/v1"));

        // LLM_FALLBACK_MODEL names the Ollama model, so LLM_MODEL is
        // unambiguously the primary model and the stale OPENROUTER_MODEL loses.
        let settings = load_with(&[
            ("LLM_PROVIDER", Some("openrouter")),
            ("LLM_FALLBACK_PROVIDER", Some("ollama")),
            ("LLM_MODEL", Some("anthropic/claude-haiku-5-5")),
            ("LLM_FALLBACK_MODEL", Some("glm4:9b")),
            ("OPENROUTER_API_KEY", Some("legacy-key")),
            ("OPENROUTER_MODEL", Some("openai/gpt-4o-mini")),
        ])
        .unwrap();
        assert_eq!(
            settings.primary_slot().unwrap().model,
            "anthropic/claude-haiku-5-5"
        );
        assert_eq!(settings.fallback_slot().unwrap().unwrap().model, "glm4:9b");
    }

    #[test]
    #[serial]
    fn legacy_vars_never_configure_a_non_legacy_backend() {
        // Only `openrouter` / `openai-compatible` slots ever read OPENROUTER_*;
        // an Anthropic slot must not inherit an OpenRouter key.
        let error = load_with(&[
            ("LLM_PROVIDER", Some("anthropic")),
            ("LLM_MODEL", Some("claude-haiku-5-5")),
            ("OPENROUTER_API_KEY", Some("legacy-key")),
        ])
        .unwrap_err()
        .to_string();
        assert!(error.contains("LLM_API_KEY"), "{error}");
    }

    fn legacy(model: Option<&str>, api_key: Option<&str>) -> LegacyOpenRouterEnv {
        LegacyOpenRouterEnv {
            api_key: api_key.map(str::to_string),
            model: model.map(str::to_string),
            base_url: None,
        }
    }

    fn slot_env(model: Option<&str>) -> SlotEnv {
        SlotEnv {
            model: model.map(str::to_string),
            ..SlotEnv::default()
        }
    }

    #[test]
    fn legacy_model_clash_in_a_pre_slot_file_moves_llm_model_to_the_ollama_fallback() {
        let mut primary = slot_env(Some("qwen3.5:9b"));
        let mut fallback = SlotEnv::default();

        let report = apply_legacy_openrouter_env(
            LlmProviderKind::OpenRouter,
            &mut primary,
            Some(LlmProviderKind::Ollama),
            &mut fallback,
            legacy(Some("openai/gpt-4o-mini"), Some("key")),
        );

        assert_eq!(primary.model.as_deref(), Some("openai/gpt-4o-mini"));
        assert_eq!(fallback.model.as_deref(), Some("qwen3.5:9b"));
        assert_eq!(report.role, Some(LlmRole::Primary));
        assert_eq!(
            report.model_note,
            Some(LegacyModelNote::LlmModelMovedToFallback {
                cloud: "openai/gpt-4o-mini".into(),
                ollama: "qwen3.5:9b".into(),
            })
        );
    }

    #[test]
    fn legacy_model_clash_without_an_ollama_fallback_reports_the_ignored_llm_model() {
        let mut primary = slot_env(Some("qwen3.5:9b"));
        let mut fallback = SlotEnv::default();

        let report = apply_legacy_openrouter_env(
            LlmProviderKind::OpenRouter,
            &mut primary,
            None,
            &mut fallback,
            legacy(Some("openai/gpt-4o-mini"), Some("key")),
        );

        assert_eq!(primary.model.as_deref(), Some("openai/gpt-4o-mini"));
        assert_eq!(fallback.model, None);
        assert_eq!(
            report.model_note,
            Some(LegacyModelNote::LlmModelIgnored {
                cloud: "openai/gpt-4o-mini".into(),
                ignored: "qwen3.5:9b".into(),
            })
        );

        // The old Compose file injected the default model: nothing to report.
        let mut primary = slot_env(Some(&format!("ollama::{DEFAULT_OLLAMA_MODEL}")));
        let report = apply_legacy_openrouter_env(
            LlmProviderKind::OpenRouter,
            &mut primary,
            None,
            &mut SlotEnv::default(),
            legacy(Some("openai/gpt-4o-mini"), Some("key")),
        );
        assert_eq!(report.model_note, None);
    }

    #[test]
    fn any_per_slot_setting_makes_llm_model_the_primary_model() {
        // Each of these settings did not exist before slots, so a file using
        // it is per-slot and LLM_MODEL must keep its per-slot meaning.
        let cases: [(LlmProviderKind, SlotEnv, Option<LlmProviderKind>, SlotEnv); 5] = [
            (
                LlmProviderKind::OpenAiCompatible,
                slot_env(Some("claude-haiku-5-5")),
                None,
                SlotEnv::default(),
            ),
            (
                LlmProviderKind::OpenRouter,
                SlotEnv {
                    api_key: Some("new-key".into()),
                    ..slot_env(Some("claude-haiku-5-5"))
                },
                None,
                SlotEnv::default(),
            ),
            (
                LlmProviderKind::OpenRouter,
                SlotEnv {
                    base_url: Some("https://gateway.example/v1".into()),
                    ..slot_env(Some("claude-haiku-5-5"))
                },
                None,
                SlotEnv::default(),
            ),
            (
                LlmProviderKind::OpenRouter,
                slot_env(Some("claude-haiku-5-5")),
                Some(LlmProviderKind::Ollama),
                slot_env(Some("glm4:9b")),
            ),
            (
                LlmProviderKind::OpenRouter,
                slot_env(Some("claude-haiku-5-5")),
                Some(LlmProviderKind::Anthropic),
                SlotEnv::default(),
            ),
        ];
        for (primary_kind, mut primary, fallback_kind, mut fallback) in cases {
            let fallback_model = fallback.model.clone();
            let report = apply_legacy_openrouter_env(
                primary_kind,
                &mut primary,
                fallback_kind,
                &mut fallback,
                legacy(Some("openai/gpt-4o-mini"), Some("legacy-key")),
            );

            assert_eq!(
                primary.model.as_deref(),
                Some("claude-haiku-5-5"),
                "{primary_kind} / {fallback_kind:?}"
            );
            assert_eq!(fallback.model, fallback_model);
            assert_eq!(
                report.model_note,
                Some(LegacyModelNote::LegacyModelIgnored {
                    primary: "claude-haiku-5-5".into(),
                    ignored: "openai/gpt-4o-mini".into(),
                })
            );
        }
    }

    #[test]
    fn shadowed_legacy_vars_are_reported() {
        let mut fallback = SlotEnv {
            model: Some("new-model".into()),
            api_key: Some("new-key".into()),
            base_url: None,
        };
        let report = apply_legacy_openrouter_env(
            LlmProviderKind::Ollama,
            &mut SlotEnv::default(),
            Some(LlmProviderKind::OpenRouter),
            &mut fallback,
            LegacyOpenRouterEnv {
                api_key: Some("legacy-key".into()),
                model: Some("legacy-model".into()),
                base_url: Some("https://legacy.example/v1".into()),
            },
        );

        assert_eq!(report.role, Some(LlmRole::Fallback));
        assert_eq!(report.shadowed, ["OPENROUTER_MODEL", "OPENROUTER_API_KEY"]);
        assert_eq!(fallback.model.as_deref(), Some("new-model"));
        assert_eq!(
            fallback.base_url.as_deref(),
            Some("https://legacy.example/v1")
        );
    }

    #[test]
    fn json_mode_breakage_is_detected_for_openai_adapters_aimed_at_anthropic() {
        let slot = |provider, base_url: &str| LlmSlot {
            role: LlmRole::Primary,
            provider,
            model: "claude-haiku-5-5".into(),
            api_key: Some("key".into()),
            base_url: Some(base_url.into()),
        };
        for provider in [
            LlmProviderKind::OpenRouter,
            LlmProviderKind::OpenAiCompatible,
            LlmProviderKind::OpenAi,
        ] {
            assert!(
                breaks_json_mode(&slot(provider, "https://api.anthropic.com/v1")),
                "{provider}"
            );
            assert!(breaks_json_mode(&slot(
                provider,
                "https://API.Anthropic.com/v1/"
            )));
            assert!(!breaks_json_mode(&slot(
                provider,
                "https://openrouter.ai/api/v1"
            )));
        }
        // The native backend is the fix, not the problem.
        assert!(!breaks_json_mode(&slot(
            LlmProviderKind::Anthropic,
            "https://api.anthropic.com/v1/"
        )));
    }

    #[test]
    fn redact_url_strips_credentials_query_and_fragment() {
        assert_eq!(
            redact_url("https://user:token@gw.example:8443/v1?api_key=secret#frag"),
            "https://gw.example:8443/v1"
        );
        assert_eq!(
            redact_url("https://api.anthropic.com/v1/"),
            "https://api.anthropic.com/v1/"
        );
        assert_eq!(redact_url("not a url secret"), "<unparsable URL>");
    }

    #[test]
    fn invalid_base_url_error_does_not_echo_the_value() {
        let settings = Settings {
            llm_base_url: Some("htp//secret-token".into()),
            ..cloud_primary(LlmProviderKind::OpenAiCompatible)
        };
        let error = settings.validate().unwrap_err().to_string();
        assert!(error.contains("LLM_BASE_URL"), "{error}");
        assert!(!error.contains("secret-token"), "{error}");
    }

    #[test]
    #[serial]
    fn legacy_openrouter_in_both_slots_drops_the_duplicate_fallback() {
        // A pre-slot `.env` naming openrouter twice booted with the primary
        // alone; it must keep booting instead of demanding fallback settings.
        let settings = load_with(&[
            ("LLM_PROVIDER", Some("openrouter")),
            ("LLM_FALLBACK_PROVIDER", Some("openrouter")),
            ("OPENROUTER_API_KEY", Some("k")),
            ("OPENROUTER_MODEL", Some("m")),
        ])
        .unwrap();

        assert_eq!(settings.llm_fallback_provider, None);
        assert_eq!(settings.fallback_slot().unwrap(), None);
        let primary = settings.primary_slot().unwrap();
        assert_eq!(primary.model, "m");
        assert_eq!(primary.api_key.as_deref(), Some("k"));
    }

    #[test]
    #[serial]
    fn same_kind_fallback_with_its_own_settings_survives_legacy_mode() {
        let settings = load_with(&[
            ("LLM_PROVIDER", Some("openrouter")),
            ("LLM_FALLBACK_PROVIDER", Some("openrouter")),
            ("LLM_FALLBACK_MODEL", Some("fallback-model")),
            ("LLM_FALLBACK_API_KEY", Some("fallback-key")),
            ("OPENROUTER_API_KEY", Some("k")),
            ("OPENROUTER_MODEL", Some("m")),
        ])
        .unwrap();

        let fallback = settings.fallback_slot().unwrap().unwrap();
        assert_eq!(fallback.model, "fallback-model");
        assert_eq!(fallback.api_key.as_deref(), Some("fallback-key"));
    }

    #[test]
    #[serial]
    fn errors_on_legacy_values_name_the_variable_the_operator_set() {
        let error = load_with(&[
            ("LLM_PROVIDER", Some("ollama")),
            ("LLM_FALLBACK_PROVIDER", Some("openrouter")),
            ("OPENROUTER_API_KEY", Some("k")),
            ("OPENROUTER_MODEL", Some("m")),
            ("OPENROUTER_BASE_URL", Some("http://10.0.0.1/v1")),
        ])
        .unwrap_err()
        .to_string();
        assert!(error.contains("OPENROUTER_BASE_URL"), "{error}");
        assert!(!error.contains("LLM_FALLBACK_BASE_URL"), "{error}");

        // A slot value keeps its own name even in legacy mode.
        let error = load_with(&[
            ("LLM_PROVIDER", Some("ollama")),
            ("LLM_FALLBACK_PROVIDER", Some("openrouter")),
            ("LLM_FALLBACK_BASE_URL", Some("http://10.0.0.1/v1")),
            ("OPENROUTER_API_KEY", Some("k")),
            ("OPENROUTER_MODEL", Some("m")),
        ])
        .unwrap_err()
        .to_string();
        assert!(error.contains("LLM_FALLBACK_BASE_URL"), "{error}");

        // A missing value names both places it could come from.
        let error = load_with(&[
            ("LLM_PROVIDER", Some("openrouter")),
            ("OPENROUTER_API_KEY", Some("k")),
        ])
        .unwrap_err()
        .to_string();
        assert!(error.contains("LLM_MODEL"), "{error}");
        assert!(error.contains("OPENROUTER_MODEL"), "{error}");
    }

    #[test]
    fn debug_redacts_credentials_in_base_urls() {
        let settings = Settings {
            llm_base_url: Some("https://user:primary-url-secret@gw.example/v1".into()),
            llm_fallback_base_url: Some("https://gw.example/v1?key=fallback-url-secret".into()),
            ollama_host: Some("http://user:ollama-url-secret@gpu:11434".into()),
            ..cloud_primary(LlmProviderKind::OpenAiCompatible)
        };
        let debug = format!("{settings:?}");
        for secret in [
            "primary-url-secret",
            "fallback-url-secret",
            "ollama-url-secret",
        ] {
            assert!(!debug.contains(secret), "{secret} leaked: {debug}");
        }
        assert!(debug.contains("gw.example"), "{debug}");

        let slot = format!("{:?}", settings.primary_slot().unwrap());
        assert!(!slot.contains("primary-url-secret"), "{slot}");
    }

    #[test]
    #[serial]
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
        assert_eq!(d.llm_model, None);
        assert_eq!(d.primary_slot().unwrap().model, DEFAULT_OLLAMA_MODEL);
        assert_eq!(DEFAULT_OLLAMA_MODEL, "lklimek/gemma4-text:26b-a4b-it-qat");
        assert_eq!(d.embed_model, "MultilingualE5Large");
        assert_eq!(d.embed_dims, 1024);
        assert!(d.distill_memories);
        assert!(d.compact_on_startup);
        assert_eq!(d.compact_fragment_threshold, 64);
        assert_eq!(d.mcp_body_read_timeout_secs, 30);
    }

    #[test]
    #[serial]
    fn body_read_timeout_reads_env_and_falls_back_on_garbage() {
        let default = Settings::default().mcp_body_read_timeout_secs;

        let _set = EnvGuard::set(&[("MEMCAN_BODY_READ_TIMEOUT", Some("5"))]);
        assert_eq!(env_u64_or_warn("MEMCAN_BODY_READ_TIMEOUT", default), 5);
        drop(_set);

        // `0` is the documented "disabled" value, not a parse failure.
        let _zero = EnvGuard::set(&[("MEMCAN_BODY_READ_TIMEOUT", Some("0"))]);
        assert_eq!(env_u64_or_warn("MEMCAN_BODY_READ_TIMEOUT", default), 0);
        drop(_zero);

        for garbage in ["", "  ", "thirty", "-1", "1.5"] {
            let _bad = EnvGuard::set(&[("MEMCAN_BODY_READ_TIMEOUT", Some(garbage))]);
            assert_eq!(
                env_u64_or_warn("MEMCAN_BODY_READ_TIMEOUT", default),
                default,
                "{garbage:?} should fall back to the default"
            );
        }

        let _unset = EnvGuard::set(&[("MEMCAN_BODY_READ_TIMEOUT", None)]);
        assert_eq!(
            env_u64_or_warn("MEMCAN_BODY_READ_TIMEOUT", default),
            default
        );
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
