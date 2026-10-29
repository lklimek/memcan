use std::collections::HashSet;
use std::ffi::OsString;
use std::path::Path;
use std::str::FromStr;

use tracing::{debug, info, warn};

use crate::embed::{model_dims, resolve_model};
use crate::error::{MemcanError, Result};

/// Runtime-selectable LLM backend.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum LlmProviderKind {
    /// Ollama server, configured by the slot's `*_MODEL` / `*_BASE_URL`
    /// (default `OLLAMA_HOST`) / `*_API_KEY` (default `OLLAMA_API_KEY`).
    #[default]
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
    type Err = LlmConfigError;

    fn from_str(value: &str) -> std::result::Result<Self, LlmConfigError> {
        match value.trim().to_ascii_lowercase().as_str() {
            "ollama" => Ok(Self::Ollama),
            "anthropic" | "claude" => Ok(Self::Anthropic),
            "openai" => Ok(Self::OpenAi),
            "openrouter" => Ok(Self::OpenRouter),
            "openai-compatible" | "openai-compat" => Ok(Self::OpenAiCompatible),
            _ => Err(LlmConfigError::UnknownProvider {
                value: value.to_string(),
            }),
        }
    }
}

/// URL parse error, named through `reqwest::Url` to avoid a direct `url` dependency.
pub type UrlParseError = <reqwest::Url as FromStr>::Err;

/// An invalid LLM slot configuration.
///
/// `Display` names the variable to fix and never echoes a credential or URL.
#[derive(Debug, thiserror::Error)]
pub enum LlmConfigError {
    #[error("unknown LLM provider '{value}'; expected one of {ACCEPTED_PROVIDERS}")]
    UnknownProvider { value: String },

    /// A cloud slot lacks a required setting.
    #[error("{} is required when {provider_var}={kind}", with_legacy_alternative(.var, *.legacy))]
    MissingSlotVar {
        var: &'static str,
        /// Deprecated `OPENROUTER_*` variable that could supply it instead.
        legacy: Option<&'static str>,
        provider_var: &'static str,
        kind: LlmProviderKind,
    },

    #[error("{var} is not a valid URL: {source}")]
    InvalidUrl {
        var: &'static str,
        #[source]
        source: UrlParseError,
    },

    #[error("{var} must use HTTPS when configured with a non-loopback host")]
    InsecureUrl { var: &'static str },

    /// A cloud slot names an `ollama::`-namespaced model.
    #[error(
        "{var} names an Ollama model ('ollama::' prefix) but {provider_var}={kind}; set {var} \
         to a model id of that backend"
    )]
    OllamaModelOnCloudSlot {
        var: &'static str,
        provider_var: &'static str,
        kind: LlmProviderKind,
    },

    /// A deprecated `OPENROUTER_*` credential paired with a per-slot
    /// endpoint, or the reverse.
    #[error(
        "{key_var} is not sent to the base URL set by {url_var}: deprecated OPENROUTER_* \
         credentials and endpoints are only used together; set the key and the base URL with \
         the same family of variables"
    )]
    MixedLegacyCredential {
        key_var: &'static str,
        url_var: &'static str,
    },

    /// `./.env` in the working directory sets the endpoint for a key that
    /// came from the environment or the MemCan config file.
    #[error(
        "{url_var} comes from ./.env in the current directory but {key_var} does not; refusing \
         to send a key from the environment or the MemCan config file to an endpoint chosen by a \
         project-local file. Set {url_var} where {key_var} is set, or set both in ./.env"
    )]
    UntrustedEndpoint {
        url_var: &'static str,
        key_var: &'static str,
    },

    #[error("the {role} '{kind}' LLM slot has no base URL")]
    SlotWithoutBaseUrl {
        role: LlmRole,
        kind: LlmProviderKind,
    },

    #[error("the {role} '{kind}' LLM slot has no API key")]
    SlotWithoutApiKey {
        role: LlmRole,
        kind: LlmProviderKind,
    },

    #[error("Ollama is not a cloud LLM backend")]
    NotCloudBackend,

    #[error("could not build the HTTP client for cloud LLM slots: {source}")]
    HttpClient {
        #[source]
        source: reqwest::Error,
    },

    #[error("the '{kind}' LLM backend requires the {feature} feature")]
    FeatureDisabled {
        kind: LlmProviderKind,
        feature: &'static str,
    },
}

fn with_legacy_alternative(var: &str, legacy: Option<&str>) -> String {
    match legacy {
        Some(legacy) => format!("{var} (or the deprecated {legacy})"),
        None => var.to_string(),
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
///
/// Known only to [`Settings::load`]; hand-built settings have no legacy origins.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct LegacyVarOrigins {
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

    /// Deprecated variable that could also supply the missing `var` of `role`.
    fn legacy_alternative(&self, role: LlmRole, var: SlotVar) -> Option<&'static str> {
        var.legacy_name().filter(|_| self.role == Some(role))
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
    pub kind: LlmProviderKind,
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
            .field("kind", &self.kind)
            .field("model", &self.model)
            .field("api_key", &self.api_key.as_ref().map(|_| "***"))
            .field("base_url", &self.base_url.as_deref().map(redact_url))
            .finish()
    }
}

/// One LLM slot as configured, before backend defaults are applied.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct SlotSettings {
    /// Backend (`LLM_PROVIDER` / `LLM_FALLBACK_PROVIDER`).
    pub kind: LlmProviderKind,
    /// Model (`*_MODEL`). `None` selects [`DEFAULT_OLLAMA_MODEL`] for Ollama
    /// and is a configuration error for a cloud backend. The legacy
    /// `"ollama::"` prefix is accepted for Ollama.
    pub model: Option<String>,
    /// API key (`*_API_KEY`); Ollama falls back to `OLLAMA_API_KEY`.
    pub api_key: Option<String>,
    /// Base URL (`*_BASE_URL`); `None` selects the backend default
    /// (`OLLAMA_HOST` for Ollama).
    pub base_url: Option<String>,
}

impl SlotSettings {
    /// Read `role`'s `*_MODEL` / `*_API_KEY` / `*_BASE_URL` variables.
    fn read(role: LlmRole, kind: LlmProviderKind) -> Self {
        Self {
            kind,
            model: env_non_blank(role.var(SlotVar::Model)),
            api_key: env_non_blank(role.var(SlotVar::ApiKey)),
            base_url: env_non_blank(role.var(SlotVar::BaseUrl)),
        }
    }
}

impl std::fmt::Debug for SlotSettings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SlotSettings")
            .field("kind", &self.kind)
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
    /// Primary LLM slot (`LLM_*`).
    pub llm_primary: SlotSettings,
    /// Optional fallback slot (`LLM_FALLBACK_*`), used while the primary is
    /// unavailable.
    pub llm_fallback: Option<SlotSettings>,
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
            .field("llm_primary", &self.llm_primary)
            .field("llm_fallback", &self.llm_fallback)
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
            llm_primary: SlotSettings::default(),
            llm_fallback: None,
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
        let config_env = dirs::config_dir().map(|dir| dir.join("memcan").join(".env"));
        Self::load_from(Path::new(".env"), config_env.as_deref())
    }

    /// [`load`](Self::load) with explicit `.env` locations.
    fn load_from(cwd_env: &Path, config_env: Option<&Path>) -> Result<Self> {
        // dotenvy::from_path sets vars only if not already present, so load
        // order determines priority: CWD > config dir; explicit env vars win over both.
        let preexisting: HashSet<OsString> = std::env::vars_os().map(|(key, _)| key).collect();
        if cwd_env.exists() && dotenvy::from_path(cwd_env).is_ok() {
            debug!("Loaded .env from CWD");
        }
        // A project-local file may not redirect credentials configured elsewhere.
        let cwd_vars: HashSet<String> = std::env::vars_os()
            .filter(|(key, _)| !preexisting.contains(key))
            .filter_map(|(key, _)| key.into_string().ok())
            .collect();
        if let Some(config_env) = config_env
            && config_env.exists()
            && dotenvy::from_path(config_env).is_ok()
        {
            debug!("Loaded .env from {}", config_env.display());
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
        let primary_kind = env_non_blank("LLM_PROVIDER")
            .map(|value| value.parse::<LlmProviderKind>())
            .transpose()?
            .unwrap_or_default();
        let fallback_kind = env_non_blank("LLM_FALLBACK_PROVIDER")
            .map(|value| value.parse::<LlmProviderKind>())
            .transpose()?;
        let mut primary = SlotSettings::read(LlmRole::Primary, primary_kind);
        let mut fallback = SlotSettings::read(LlmRole::Fallback, fallback_kind.unwrap_or_default());
        let legacy = LegacyOpenRouterEnv::read();
        let legacy_set = !legacy.is_empty();
        let legacy_report =
            apply_legacy_openrouter_env(&mut primary, fallback_kind, &mut fallback, legacy);
        legacy_report.log(legacy_set);
        let llm_fallback = fallback_kind
            .filter(|_| !legacy_report.drop_fallback)
            .map(|_| fallback);
        let origins = LegacyVarOrigins {
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
            llm_primary: primary,
            llm_fallback,
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
        settings.validate_with(&origins, &cwd_vars)?;
        Ok(settings)
    }

    /// Check invariants on hand-built settings.
    #[cfg(test)]
    fn validate(&self) -> Result<()> {
        self.validate_with(&LegacyVarOrigins::default(), &HashSet::new())
    }

    /// Check invariants, naming deprecated variables per `origins` in errors
    /// and refusing endpoints redirected by `cwd_vars` (those `./.env` set).
    fn validate_with(&self, origins: &LegacyVarOrigins, cwd_vars: &HashSet<String>) -> Result<()> {
        if self.lancedb_path.is_empty() {
            return Err(MemcanError::Config("LANCEDB_PATH must not be empty".into()));
        }

        let primary = self.resolve_slot(LlmRole::Primary, &self.llm_primary, origins)?;
        let fallback = self
            .llm_fallback
            .as_ref()
            .map(|slot| self.resolve_slot(LlmRole::Fallback, slot, origins))
            .transpose()?;
        for resolved in std::iter::once(&primary).chain(fallback.as_ref()) {
            resolved.require_trusted_endpoint(cwd_vars)?;
            let slot = &resolved.slot;
            if resolved.withheld_ollama_key {
                info!(
                    role = %slot.role,
                    "OLLAMA_API_KEY is not sent to the {} Ollama slot because its base URL names \
                     another host than OLLAMA_HOST; set {} if that host needs a key",
                    slot.role,
                    slot.role.var(SlotVar::ApiKey)
                );
            }
            if anthropic_base_url_lacks_version(slot) {
                warn!(
                    role = %slot.role,
                    "The {} anthropic LLM slot's base URL has no /v1 path segment, so requests \
                     go to <base>/messages and will likely fail with 404; set {} to e.g. \
                     https://api.anthropic.com/v1/",
                    slot.role,
                    slot.role.var(SlotVar::BaseUrl)
                );
            }
            if breaks_json_mode(slot) {
                warn!(
                    role = %slot.role,
                    backend = %slot.kind,
                    "The {} LLM slot sends Anthropic's OpenAI-compatible endpoint \
                     `response_format: json_object`, which it has been observed to reject with \
                     a 400 (vendor docs say it is ignored), failing every JSON-mode call; set \
                     {}=anthropic for the native Messages API",
                    slot.role,
                    slot.role.var(SlotVar::Provider)
                );
            }
        }
        let primary = &primary.slot;
        if fallback
            .map(|resolved| resolved.slot)
            .is_some_and(|fallback| {
                fallback.kind == primary.kind
                    && fallback.model == primary.model
                    && fallback.base_url == primary.base_url
                    && fallback.api_key == primary.api_key
            })
        {
            warn!(
                backend = %primary.kind,
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
        let origins = LegacyVarOrigins::default();
        Ok(self
            .resolve_slot(LlmRole::Primary, &self.llm_primary, &origins)?
            .slot)
    }

    /// The resolved fallback LLM slot, if one is configured.
    pub fn fallback_slot(&self) -> Result<Option<LlmSlot>> {
        let origins = LegacyVarOrigins::default();
        Ok(self
            .llm_fallback
            .as_ref()
            .map(|slot| self.resolve_slot(LlmRole::Fallback, slot, &origins))
            .transpose()?
            .map(|resolved| resolved.slot))
    }

    /// Resolve an Ollama slot. `OLLAMA_API_KEY` was issued for `OLLAMA_HOST`
    /// (or the default host), so it is inherited only by a slot aimed at
    /// that origin; a slot on another host needs its own `*_API_KEY`.
    fn resolve_ollama_slot(&self, role: LlmRole, slot: &SlotSettings) -> ResolvedSlot {
        let ollama_host = non_blank(&self.ollama_host);
        let (base_url, url_var) = match non_blank(&slot.base_url) {
            Some(url) => (Some(url), Some(role.var(SlotVar::BaseUrl))),
            None => (ollama_host, ollama_host.map(|_| "OLLAMA_HOST")),
        };
        let key_home = ollama_host.unwrap_or(crate::ollama::DEFAULT_OLLAMA_HOST);
        let key_target = base_url.unwrap_or(crate::ollama::DEFAULT_OLLAMA_HOST);
        let mut withheld_ollama_key = false;
        let (api_key, key_var) = match (non_blank(&slot.api_key), non_blank(&self.ollama_api_key)) {
            (Some(key), _) => (Some(key), Some(role.var(SlotVar::ApiKey))),
            (None, Some(key)) if same_origin(key_target, key_home) => {
                (Some(key), Some("OLLAMA_API_KEY"))
            }
            (None, Some(_)) => {
                withheld_ollama_key = true;
                (None, None)
            }
            (None, None) => (None, None),
        };
        ResolvedSlot {
            slot: LlmSlot {
                role,
                kind: LlmProviderKind::Ollama,
                model: non_blank(&slot.model)
                    .unwrap_or(DEFAULT_OLLAMA_MODEL)
                    .to_string(),
                api_key: api_key.map(str::to_string),
                base_url: base_url.map(str::to_string),
            },
            key_var,
            url_var,
            withheld_ollama_key,
        }
    }

    fn resolve_slot(
        &self,
        role: LlmRole,
        slot: &SlotSettings,
        origins: &LegacyVarOrigins,
    ) -> std::result::Result<ResolvedSlot, LlmConfigError> {
        let kind = slot.kind;
        if kind == LlmProviderKind::Ollama {
            return Ok(self.resolve_ollama_slot(role, slot));
        }
        // A blank value counts as missing: a whitespace key would otherwise
        // reach the HTTP client as an empty credential and 401 on every
        // request instead of failing at startup.
        let missing = |var: SlotVar| LlmConfigError::MissingSlotVar {
            var: role.var(var),
            legacy: origins.legacy_alternative(role, var),
            provider_var: role.var(SlotVar::Provider),
            kind,
        };
        let api_key = non_blank(&slot.api_key).ok_or_else(|| missing(SlotVar::ApiKey))?;
        let model = non_blank(&slot.model).ok_or_else(|| missing(SlotVar::Model))?;
        if crate::ollama::strip_ollama_prefix(model) != model {
            return Err(LlmConfigError::OllamaModelOnCloudSlot {
                var: origins.supplied_by(role, SlotVar::Model),
                provider_var: role.var(SlotVar::Provider),
                kind,
            });
        }
        let (base_url, url_var) = match non_blank(&slot.base_url) {
            Some(url) => {
                let var = origins.supplied_by(role, SlotVar::BaseUrl);
                require_https_or_loopback(var, url)?;
                (url, Some(var))
            }
            None => (
                kind.default_base_url()
                    .ok_or_else(|| missing(SlotVar::BaseUrl))?,
                None,
            ),
        };
        let key_var = origins.supplied_by(role, SlotVar::ApiKey);
        require_paired_legacy_credential(key_var, url_var)?;
        Ok(ResolvedSlot {
            slot: LlmSlot {
                role,
                kind,
                model: model.to_string(),
                api_key: Some(api_key.to_string()),
                base_url: Some(base_url.to_string()),
            },
            key_var: Some(key_var),
            url_var,
            withheld_ollama_key: false,
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
        slot.kind,
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

/// `true` when an Anthropic slot's base URL has no `v1` path segment: the
/// Messages API lives under `/v1`, and the resulting 404 is a content fault
/// that neither fails over nor trips the breaker.
fn anthropic_base_url_lacks_version(slot: &LlmSlot) -> bool {
    slot.kind == LlmProviderKind::Anthropic
        && slot
            .base_url
            .as_deref()
            .and_then(|url| reqwest::Url::parse(url).ok())
            .is_some_and(|url| {
                !url.path_segments()
                    .is_some_and(|mut segments| segments.any(|segment| segment == "v1"))
            })
}

/// `true` when both URLs parse and share scheme, host, and port.
fn same_origin(left: &str, right: &str) -> bool {
    match (reqwest::Url::parse(left), reqwest::Url::parse(right)) {
        (Ok(left), Ok(right)) => left.origin() == right.origin(),
        _ => false,
    }
}

/// A resolved slot plus the variables that supplied its credential and
/// endpoint, for the pairing checks.
struct ResolvedSlot {
    slot: LlmSlot,
    /// Variable that supplied `slot.api_key`.
    key_var: Option<&'static str>,
    /// Variable that supplied `slot.base_url`; `None` for a backend default.
    url_var: Option<&'static str>,
    /// `OLLAMA_API_KEY` was not inherited because the slot targets another host.
    withheld_ollama_key: bool,
}

impl ResolvedSlot {
    /// Refuse an endpoint set by `./.env` for a key that came from elsewhere.
    fn require_trusted_endpoint(
        &self,
        cwd_vars: &HashSet<String>,
    ) -> std::result::Result<(), LlmConfigError> {
        match (self.url_var, self.key_var) {
            (Some(url_var), Some(key_var))
                if cwd_vars.contains(url_var) && !cwd_vars.contains(key_var) =>
            {
                Err(LlmConfigError::UntrustedEndpoint { url_var, key_var })
            }
            _ => Ok(()),
        }
    }
}

/// Refuse to pair a deprecated `OPENROUTER_*` key with a per-slot base URL,
/// or a per-slot key with `OPENROUTER_BASE_URL`: a key only goes to the
/// endpoint configured alongside it (or to the backend default).
fn require_paired_legacy_credential(
    key_var: &'static str,
    url_var: Option<&'static str>,
) -> std::result::Result<(), LlmConfigError> {
    let legacy_key = Some(key_var) == SlotVar::ApiKey.legacy_name();
    match url_var {
        Some(url_var) if legacy_key != (Some(url_var) == SlotVar::BaseUrl.legacy_name()) => {
            Err(LlmConfigError::MixedLegacyCredential { key_var, url_var })
        }
        _ => Ok(()),
    }
}

/// Reject a cloud base URL that is unparsable, or plain HTTP to a remote host.
///
/// The URL itself is not echoed: an unparsable value may still hold a secret.
fn require_https_or_loopback(
    var: &'static str,
    base_url: &str,
) -> std::result::Result<(), LlmConfigError> {
    let parsed = reqwest::Url::parse(base_url)
        .map_err(|source| LlmConfigError::InvalidUrl { var, source })?;
    let loopback = matches!(
        parsed.host_str(),
        Some("localhost" | "127.0.0.1" | "::1" | "[::1]")
    );
    if parsed.scheme() != "https" && !loopback {
        return Err(LlmConfigError::InsecureUrl { var });
    }
    Ok(())
}

/// The deprecated single-cloud `OPENROUTER_*` variables, as read.
#[derive(Default)]
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

impl std::fmt::Debug for LegacyOpenRouterEnv {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LegacyOpenRouterEnv")
            .field("api_key", &self.api_key.as_ref().map(|_| "***"))
            .field("model", &self.model)
            .field("base_url", &self.base_url.as_deref().map(redact_url))
            .finish()
    }
}

/// How a primary-slot clash between `LLM_MODEL` and `OPENROUTER_MODEL` was
/// settled, reported by `load()` naming where each value went.
#[derive(Debug, Clone, PartialEq, Eq)]
enum LegacyModelNote {
    /// Pre-slot `.env`: `LLM_MODEL` kept its old meaning (the Ollama model)
    /// and became the Ollama fallback's model.
    LlmModelMovedToFallback { cloud: String, ollama: String },
    /// Pre-slot `.env` without an Ollama fallback: `LLM_MODEL` kept its old
    /// meaning and has no slot to configure.
    LlmModelIgnored { cloud: String, ignored: String },
    /// As [`Self::LlmModelIgnored`], but `LLM_MODEL` is the stock Ollama model
    /// the old Compose file injected, which carries no operator intent.
    StockLlmModelIgnored { cloud: String, ignored: String },
    /// The file also uses per-slot settings, so `LLM_MODEL` is the primary
    /// model and the stale `OPENROUTER_MODEL` is ignored.
    LegacyModelIgnored { primary: String, ignored: String },
}

/// What [`apply_legacy_openrouter_env`] did, for the startup warnings.
#[derive(Debug, Default, PartialEq, Eq)]
struct LegacyReport {
    /// Slot the legacy values were mapped onto; `None` when none reads them.
    role: Option<LlmRole>,
    /// Settings whose legacy variable was ignored because the slot variable is set.
    shadowed: Vec<SlotVar>,
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
        let shadowed: Vec<_> = self
            .shadowed
            .iter()
            .filter_map(|var| var.legacy_name())
            .collect();
        warn!(
            slot = %role,
            shadowed = %shadowed.join(", "),
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
            Some(LegacyModelNote::StockLlmModelIgnored { cloud, ignored }) => debug!(
                "Legacy mode: OPENROUTER_MODEL='{cloud}' is the primary model; the stock \
                 LLM_MODEL='{ignored}' injected by older Compose files is ignored"
            ),
            Some(LegacyModelNote::LegacyModelIgnored { primary, ignored }) => warn!(
                "LLM_MODEL='{primary}' is the primary model; OPENROUTER_MODEL='{ignored}' is \
                 ignored because per-slot settings are in use"
            ),
            None => {}
        }
    }
}

/// Fill an unset slot setting from its deprecated variable, or record that
/// the set slot variable shadows it.
fn adopt_legacy(
    slot_value: &mut Option<String>,
    legacy_value: Option<String>,
    var: SlotVar,
    report: &mut LegacyReport,
) {
    match (slot_value.is_some(), legacy_value) {
        (false, Some(value)) => {
            report.supplied.push(var);
            *slot_value = Some(value);
        }
        (true, Some(_)) => report.shadowed.push(var),
        (_, None) => {}
    }
}

/// Map deprecated `OPENROUTER_*` settings onto the slot they used to configure.
///
/// This is the authoritative statement of the legacy rule; `.env.example`
/// restates it for operators.
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
    primary: &mut SlotSettings,
    fallback_kind: Option<LlmProviderKind>,
    fallback: &mut SlotSettings,
    legacy: LegacyOpenRouterEnv,
) -> LegacyReport {
    let mut report = LegacyReport::default();
    if legacy.is_empty() {
        return report;
    }
    let primary_kind = primary.kind;
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
                        report.model_note =
                            Some(if fallback_kind == Some(LlmProviderKind::Ollama) {
                                fallback.model = Some(ollama.clone());
                                LegacyModelNote::LlmModelMovedToFallback { cloud, ollama }
                            } else if crate::ollama::strip_ollama_prefix(&ollama)
                                == DEFAULT_OLLAMA_MODEL
                            {
                                LegacyModelNote::StockLlmModelIgnored {
                                    cloud,
                                    ignored: ollama,
                                }
                            } else {
                                LegacyModelNote::LlmModelIgnored {
                                    cloud,
                                    ignored: ollama,
                                }
                            });
                    }
                }
            }
            primary
        }
        LlmRole::Fallback => {
            adopt_legacy(
                &mut fallback.model,
                legacy.model,
                SlotVar::Model,
                &mut report,
            );
            fallback
        }
    };
    adopt_legacy(
        &mut slot.api_key,
        legacy.api_key,
        SlotVar::ApiKey,
        &mut report,
    );
    adopt_legacy(
        &mut slot.base_url,
        legacy.base_url,
        SlotVar::BaseUrl,
        &mut report,
    );
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

    fn slot(kind: LlmProviderKind, model: &str, api_key: &str) -> SlotSettings {
        SlotSettings {
            kind,
            model: Some(model.into()),
            api_key: Some(api_key.into()),
            base_url: None,
        }
    }

    fn cloud_primary(kind: LlmProviderKind) -> Settings {
        Settings {
            llm_primary: slot(kind, "claude-haiku-5-5", "primary-key"),
            ..Settings::default()
        }
    }

    fn with_cloud_fallback(settings: Settings, kind: LlmProviderKind) -> Settings {
        Settings {
            llm_fallback: Some(slot(kind, "gpt-4o-mini", "fallback-key")),
            ..settings
        }
    }

    fn edited(mut settings: Settings, edit: impl FnOnce(&mut Settings)) -> Settings {
        edit(&mut settings);
        settings
    }

    fn fallback_of(settings: &mut Settings) -> &mut SlotSettings {
        settings.llm_fallback.as_mut().expect("a fallback slot")
    }

    /// The typed LLM configuration error behind `error`.
    fn llm_config_error(error: MemcanError) -> LlmConfigError {
        match error {
            MemcanError::LlmConfig(error) => error,
            other => panic!("expected an LLM configuration error, got {other:?}"),
        }
    }

    /// The variable a missing-setting error names.
    fn missing_var(error: MemcanError) -> (&'static str, Option<&'static str>) {
        match llm_config_error(error) {
            LlmConfigError::MissingSlotVar { var, legacy, .. } => (var, legacy),
            other => panic!("expected a missing-setting error, got {other:?}"),
        }
    }

    /// Variables `Settings::load` reads that a test may change.
    const ISOLATED_VARS: [&str; 15] = [
        "LLM_PROVIDER",
        "LLM_MODEL",
        "LLM_API_KEY",
        "LLM_BASE_URL",
        "LLM_FALLBACK_PROVIDER",
        "LLM_FALLBACK_MODEL",
        "LLM_FALLBACK_API_KEY",
        "LLM_FALLBACK_BASE_URL",
        "OPENROUTER_API_KEY",
        "OPENROUTER_MODEL",
        "OPENROUTER_BASE_URL",
        "OLLAMA_HOST",
        "OLLAMA_API_KEY",
        "MEMCAN_WEBUI_USERNAME",
        "MEMCAN_WEBUI_PASSWORD",
    ];

    /// Isolate `Settings::load()` from the host's `.env` files and LLM env.
    fn isolated_llm_env(temp_path: &str, overrides: &[(&'static str, Option<&str>)]) -> EnvGuard {
        let mut values: Vec<(&'static str, Option<&str>)> = vec![
            ("HOME", Some(temp_path)),
            ("XDG_CONFIG_HOME", Some(temp_path)),
            ("XDG_DATA_HOME", Some(temp_path)),
        ];
        values.extend(ISOLATED_VARS.iter().map(|var| (*var, None)));
        for (key, value) in overrides {
            match values.iter_mut().find(|(existing, _)| existing == key) {
                Some(slot) => slot.1 = *value,
                None => values.push((key, *value)),
            }
        }
        EnvGuard::set(&values)
    }

    /// Load with `overrides` in the process env and the given `.env` file
    /// contents (`None` = no such file), never reading the real `./.env`.
    fn load_with_files(
        overrides: &[(&'static str, Option<&str>)],
        cwd_env: Option<&str>,
        config_env: Option<&str>,
    ) -> Result<Settings> {
        let temp = tempfile::tempdir().unwrap();
        let _environment = isolated_llm_env(temp.path().to_str().unwrap(), overrides);
        let cwd_path = temp.path().join("cwd.env");
        let config_path = temp.path().join("config.env");
        for (path, contents) in [(&cwd_path, cwd_env), (&config_path, config_env)] {
            if let Some(contents) = contents {
                std::fs::write(path, contents).unwrap();
            }
        }
        Settings::load_from(&cwd_path, Some(&config_path))
    }

    fn load_with(overrides: &[(&'static str, Option<&str>)]) -> Result<Settings> {
        load_with_files(overrides, None, None)
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
        assert_eq!(primary.kind, LlmProviderKind::Ollama);
        assert_eq!(primary.model, DEFAULT_OLLAMA_MODEL);
        assert_eq!(primary.api_key.as_deref(), Some("ollama-key"));
        assert_eq!(primary.base_url.as_deref(), Some("http://gpu:11434"));
        assert_eq!(settings.fallback_slot().unwrap(), None);

        // Slot values win over the Ollama-wide defaults.
        let overridden = Settings {
            llm_primary: SlotSettings {
                base_url: Some("http://other:11434".into()),
                ..slot(LlmProviderKind::Ollama, "qwen3.5:9b", "slot-key")
            },
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

        let overridden = edited(cloud_primary(LlmProviderKind::Anthropic), |s| {
            s.llm_primary.base_url = Some("https://gateway.example/v1".into());
        });
        assert_eq!(
            overridden.primary_slot().unwrap().base_url.as_deref(),
            Some("https://gateway.example/v1")
        );
    }

    #[test]
    fn missing_cloud_settings_name_the_exact_variable() {
        let primary = cloud_primary(LlmProviderKind::Anthropic);
        let with_fallback =
            |kind| with_cloud_fallback(cloud_primary(LlmProviderKind::Anthropic), kind);
        for (settings, var) in [
            (
                edited(primary.clone(), |s| s.llm_primary.api_key = None),
                "LLM_API_KEY",
            ),
            (
                edited(primary.clone(), |s| {
                    s.llm_primary.api_key = Some("   ".into());
                }),
                "LLM_API_KEY",
            ),
            (
                edited(primary.clone(), |s| s.llm_primary.model = None),
                "LLM_MODEL",
            ),
            (
                edited(with_fallback(LlmProviderKind::OpenAi), |s| {
                    fallback_of(s).api_key = None;
                }),
                "LLM_FALLBACK_API_KEY",
            ),
            (
                edited(with_fallback(LlmProviderKind::OpenAi), |s| {
                    fallback_of(s).model = Some(String::new());
                }),
                "LLM_FALLBACK_MODEL",
            ),
            (
                with_fallback(LlmProviderKind::OpenAiCompatible),
                "LLM_FALLBACK_BASE_URL",
            ),
            (
                cloud_primary(LlmProviderKind::OpenAiCompatible),
                "LLM_BASE_URL",
            ),
        ] {
            assert_eq!(
                missing_var(settings.validate().unwrap_err()),
                (var, None),
                "{var}"
            );
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
            llm_fallback: Some(SlotSettings {
                base_url: Some("http://second-gpu:11434".into()),
                ..SlotSettings::default()
            }),
            ..Settings::default()
        };
        assert!(ollama_twice.validate().is_ok());
    }

    #[test]
    fn same_backend_kind_twice_keeps_separate_credentials() {
        let settings = edited(
            with_cloud_fallback(
                cloud_primary(LlmProviderKind::Anthropic),
                LlmProviderKind::Anthropic,
            ),
            |s| fallback_of(s).model = Some("claude-opus-5-5".into()),
        );

        let primary = settings.primary_slot().unwrap();
        let fallback = settings.fallback_slot().unwrap().unwrap();
        assert_eq!(fallback.role, LlmRole::Fallback);
        assert_eq!(primary.api_key.as_deref(), Some("primary-key"));
        assert_eq!(fallback.api_key.as_deref(), Some("fallback-key"));
        assert_eq!(fallback.model, "claude-opus-5-5");
        assert_eq!(primary.base_url, fallback.base_url);
    }

    #[test]
    fn fallback_values_without_a_fallback_backend_are_not_validated() {
        // `load()` drops LLM_FALLBACK_* values when no fallback backend is set.
        let settings = Settings {
            llm_primary: SlotSettings {
                base_url: Some("http://gpu:11434".into()),
                ..SlotSettings::default()
            },
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
            let loopback = edited(cloud_primary(LlmProviderKind::OpenRouter), |s| {
                s.llm_primary.base_url = Some(base_url.into());
            });
            assert!(
                loopback.validate().is_ok(),
                "loopback URL should be accepted: {base_url}"
            );
        }

        let remote = edited(
            with_cloud_fallback(Settings::default(), LlmProviderKind::OpenAiCompatible),
            |s| fallback_of(s).base_url = Some("http://gateway.example/v1".into()),
        );
        assert!(matches!(
            llm_config_error(remote.validate().unwrap_err()),
            LlmConfigError::InsecureUrl {
                var: "LLM_FALLBACK_BASE_URL"
            }
        ));

        // Ollama keeps plain-HTTP LAN hosts working.
        let ollama = Settings {
            llm_primary: SlotSettings {
                base_url: Some("http://gpu.lan:11434".into()),
                ..SlotSettings::default()
            },
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
        let error = "gemini".parse::<LlmProviderKind>().unwrap_err();
        assert!(matches!(
            &error,
            LlmConfigError::UnknownProvider { value } if value == "gemini"
        ));
        let error = error.to_string();
        for accepted in [
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
            llm_primary: slot(LlmProviderKind::Ollama, "m", "raw-primary-secret"),
            llm_fallback: Some(slot(LlmProviderKind::Ollama, "m", "raw-fallback-secret")),
            ..Settings::default()
        };
        let debug = format!("{settings:?}");
        assert!(!debug.contains("raw-primary-secret"), "{debug}");
        assert!(!debug.contains("raw-fallback-secret"), "{debug}");
        assert_eq!(
            debug.matches("api_key: Some(\"***\")").count(),
            2,
            "{debug}"
        );

        let slot = format!(
            "{:?}",
            cloud_primary(LlmProviderKind::Anthropic).primary_slot()
        );
        assert!(!slot.contains("primary-key"), "{slot}");
    }

    #[test]
    fn debug_of_the_legacy_env_redacts_its_key_and_url_credentials() {
        let legacy = LegacyOpenRouterEnv {
            api_key: Some("raw-legacy-secret".into()),
            model: Some("openai/gpt-4o-mini".into()),
            base_url: Some("https://user:url-secret@gw.example/v1".into()),
        };
        let debug = format!("{legacy:?}");
        assert!(!debug.contains("raw-legacy-secret"), "{debug}");
        assert!(!debug.contains("url-secret"), "{debug}");
        assert!(debug.contains("api_key: Some(\"***\")"), "{debug}");
        assert!(debug.contains("gw.example"), "{debug}");
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
        assert_eq!(primary.kind, LlmProviderKind::Anthropic);
        assert_eq!(primary.model, "claude-haiku-5-5");
        assert_eq!(primary.api_key.as_deref(), Some("primary-key"));
        assert_eq!(
            primary.base_url.as_deref(),
            Some("https://api.anthropic.com/v1/")
        );
        let fallback = settings.fallback_slot().unwrap().unwrap();
        assert_eq!(fallback.kind, LlmProviderKind::OpenAi);
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

        assert_eq!(settings.llm_primary, SlotSettings::default());
        assert_eq!(settings.llm_fallback, None);
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
        .unwrap_err();
        assert_eq!(missing_var(error), ("LLM_MODEL", None));
    }

    #[test]
    #[serial]
    fn unknown_provider_env_value_fails_load() {
        let error = load_with(&[("LLM_FALLBACK_PROVIDER", Some("anthropik"))]).unwrap_err();
        assert!(matches!(
            llm_config_error(error),
            LlmConfigError::UnknownProvider { value } if value == "anthropik"
        ));
    }

    #[test]
    #[serial]
    fn a_project_dot_env_never_leaks_into_the_test_process() {
        // The real `./.env` (the crate dir under cargo test) is never read:
        // load_from only reads the temp files it is handed.
        let settings = load_with_files(&[], None, Some("LLM_MODEL=from-config-file\n")).unwrap();
        assert_eq!(
            settings.llm_primary.model.as_deref(),
            Some("from-config-file")
        );
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
        assert_eq!(primary.kind, LlmProviderKind::OpenRouter);
        assert_eq!(primary.model, "claude-haiku-5-5");
        assert_eq!(primary.api_key.as_deref(), Some("legacy-key"));
        assert_eq!(
            primary.base_url.as_deref(),
            Some("https://gateway.example/v1")
        );
        let fallback = settings.fallback_slot().unwrap().unwrap();
        assert_eq!(fallback.kind, LlmProviderKind::Ollama);
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
        assert_eq!(primary.kind, LlmProviderKind::Ollama);
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
        assert_eq!(settings.llm_fallback, None);
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
        .unwrap_err();
        assert_eq!(missing_var(error), ("LLM_API_KEY", None));
    }

    fn legacy(model: Option<&str>, api_key: Option<&str>) -> LegacyOpenRouterEnv {
        LegacyOpenRouterEnv {
            api_key: api_key.map(str::to_string),
            model: model.map(str::to_string),
            base_url: None,
        }
    }

    fn slot_env(kind: LlmProviderKind, model: Option<&str>) -> SlotSettings {
        SlotSettings {
            kind,
            model: model.map(str::to_string),
            ..SlotSettings::default()
        }
    }

    #[test]
    fn legacy_model_clash_in_a_pre_slot_file_moves_llm_model_to_the_ollama_fallback() {
        let mut primary = slot_env(LlmProviderKind::OpenRouter, Some("qwen3.5:9b"));
        let mut fallback = SlotSettings::default();

        let report = apply_legacy_openrouter_env(
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
        let mut primary = slot_env(LlmProviderKind::OpenRouter, Some("qwen3.5:9b"));
        let mut fallback = SlotSettings::default();

        let report = apply_legacy_openrouter_env(
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

        // The old Compose file injected the default model: reported quietly.
        let stock = format!("ollama::{DEFAULT_OLLAMA_MODEL}");
        let mut primary = slot_env(LlmProviderKind::OpenRouter, Some(&stock));
        let report = apply_legacy_openrouter_env(
            &mut primary,
            None,
            &mut SlotSettings::default(),
            legacy(Some("openai/gpt-4o-mini"), Some("key")),
        );
        assert_eq!(
            report.model_note,
            Some(LegacyModelNote::StockLlmModelIgnored {
                cloud: "openai/gpt-4o-mini".into(),
                ignored: stock,
            })
        );
    }

    #[test]
    fn any_per_slot_setting_makes_llm_model_the_primary_model() {
        // Each of these settings did not exist before slots, so a file using
        // it is per-slot and LLM_MODEL must keep its per-slot meaning.
        let primary = |kind| slot_env(kind, Some("claude-haiku-5-5"));
        let cases: [(SlotSettings, Option<LlmProviderKind>, SlotSettings); 5] = [
            (
                primary(LlmProviderKind::OpenAiCompatible),
                None,
                SlotSettings::default(),
            ),
            (
                SlotSettings {
                    api_key: Some("new-key".into()),
                    ..primary(LlmProviderKind::OpenRouter)
                },
                None,
                SlotSettings::default(),
            ),
            (
                SlotSettings {
                    base_url: Some("https://gateway.example/v1".into()),
                    ..primary(LlmProviderKind::OpenRouter)
                },
                None,
                SlotSettings::default(),
            ),
            (
                primary(LlmProviderKind::OpenRouter),
                Some(LlmProviderKind::Ollama),
                slot_env(LlmProviderKind::Ollama, Some("glm4:9b")),
            ),
            (
                primary(LlmProviderKind::OpenRouter),
                Some(LlmProviderKind::Anthropic),
                SlotSettings::default(),
            ),
        ];
        for (mut primary, fallback_kind, mut fallback) in cases {
            let primary_kind = primary.kind;
            let fallback_model = fallback.model.clone();
            let report = apply_legacy_openrouter_env(
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
        let mut fallback = SlotSettings {
            kind: LlmProviderKind::OpenRouter,
            model: Some("new-model".into()),
            api_key: Some("new-key".into()),
            base_url: Some("https://new.example/v1".into()),
        };
        let report = apply_legacy_openrouter_env(
            &mut SlotSettings::default(),
            Some(LlmProviderKind::OpenRouter),
            &mut fallback,
            LegacyOpenRouterEnv {
                api_key: Some("legacy-key".into()),
                model: Some("legacy-model".into()),
                base_url: Some("https://legacy.example/v1".into()),
            },
        );

        assert_eq!(report.role, Some(LlmRole::Fallback));
        assert_eq!(
            report.shadowed,
            [SlotVar::Model, SlotVar::ApiKey, SlotVar::BaseUrl]
        );
        assert!(report.supplied.is_empty());
        assert_eq!(fallback.model.as_deref(), Some("new-model"));
        assert_eq!(fallback.base_url.as_deref(), Some("https://new.example/v1"));
    }

    /// The typed error of a load that must fail.
    fn load_error(
        overrides: &[(&'static str, Option<&str>)],
        cwd_env: Option<&str>,
        config_env: Option<&str>,
    ) -> LlmConfigError {
        llm_config_error(load_with_files(overrides, cwd_env, config_env).unwrap_err())
    }

    #[test]
    #[serial]
    fn a_legacy_api_key_is_never_sent_to_a_slot_base_url() {
        // A stale OPENROUTER_API_KEY must not reach a gateway configured for
        // another credential.
        let error = load_error(
            &[
                ("LLM_PROVIDER", Some("openai-compatible")),
                ("LLM_MODEL", Some("m")),
                ("LLM_BASE_URL", Some("https://gw.example/v1")),
                ("OPENROUTER_API_KEY", Some("legacy-key")),
            ],
            None,
            None,
        );
        assert!(
            matches!(
                error,
                LlmConfigError::MixedLegacyCredential {
                    key_var: "OPENROUTER_API_KEY",
                    url_var: "LLM_BASE_URL"
                }
            ),
            "{error:?}"
        );
    }

    #[test]
    #[serial]
    fn a_slot_api_key_is_never_sent_to_a_legacy_base_url() {
        let error = load_error(
            &[
                ("LLM_PROVIDER", Some("openrouter")),
                ("LLM_MODEL", Some("m")),
                ("LLM_API_KEY", Some("new-key")),
                (
                    "OPENROUTER_BASE_URL",
                    Some("https://old-gateway.example/v1"),
                ),
            ],
            None,
            None,
        );
        assert!(
            matches!(
                error,
                LlmConfigError::MixedLegacyCredential {
                    key_var: "LLM_API_KEY",
                    url_var: "OPENROUTER_BASE_URL"
                }
            ),
            "{error:?}"
        );
    }

    #[test]
    fn ollama_api_key_only_goes_to_the_ollama_host_origin() {
        let base = Settings {
            ollama_host: Some("http://gpu:11434".into()),
            ollama_api_key: Some("ollama-key".into()),
            ..Settings::default()
        };
        let fallback_at = |url: &str| Settings {
            llm_fallback: Some(SlotSettings {
                base_url: Some(url.into()),
                ..SlotSettings::default()
            }),
            ..base.clone()
        };

        let other_host = fallback_at("http://second-gpu:11434");
        assert_eq!(
            other_host.primary_slot().unwrap().api_key.as_deref(),
            Some("ollama-key")
        );
        let fallback = other_host.fallback_slot().unwrap().unwrap();
        assert_eq!(fallback.api_key, None, "key leaked to another host");
        assert!(other_host.validate().is_ok());

        let same_origin = fallback_at("http://gpu:11434/");
        assert_eq!(
            same_origin
                .fallback_slot()
                .unwrap()
                .unwrap()
                .api_key
                .as_deref(),
            Some("ollama-key")
        );

        // Without OLLAMA_HOST the key belongs to the default endpoint.
        let default_host = Settings {
            ollama_host: None,
            llm_primary: SlotSettings {
                base_url: Some("http://localhost:11434".into()),
                ..SlotSettings::default()
            },
            ..base.clone()
        };
        assert_eq!(
            default_host.primary_slot().unwrap().api_key.as_deref(),
            Some("ollama-key")
        );

        // The slot's own key always goes to the slot's own host.
        let own_key = edited(fallback_at("http://second-gpu:11434"), |s| {
            fallback_of(s).api_key = Some("second-key".into());
        });
        assert_eq!(
            own_key.fallback_slot().unwrap().unwrap().api_key.as_deref(),
            Some("second-key")
        );
    }

    #[test]
    #[serial]
    fn a_cwd_dot_env_cannot_redirect_a_key_set_elsewhere() {
        let operator_config =
            "LLM_PROVIDER=anthropic\nLLM_MODEL=claude-haiku-5-5\nLLM_API_KEY=operator-key\n";
        let error = load_error(
            &[],
            Some("LLM_BASE_URL=https://attacker.example/v1\n"),
            Some(operator_config),
        );
        assert!(
            matches!(
                error,
                LlmConfigError::UntrustedEndpoint {
                    url_var: "LLM_BASE_URL",
                    key_var: "LLM_API_KEY"
                }
            ),
            "{error:?}"
        );

        // A key exported in the process env is just as off-limits.
        let error = load_error(
            &[
                ("LLM_FALLBACK_PROVIDER", Some("openai-compatible")),
                ("LLM_FALLBACK_MODEL", Some("m")),
                ("LLM_FALLBACK_API_KEY", Some("operator-key")),
            ],
            Some("LLM_FALLBACK_BASE_URL=https://attacker.example/v1\n"),
            None,
        );
        assert!(
            matches!(
                error,
                LlmConfigError::UntrustedEndpoint {
                    url_var: "LLM_FALLBACK_BASE_URL",
                    key_var: "LLM_FALLBACK_API_KEY"
                }
            ),
            "{error:?}"
        );

        // So is the Ollama bearer.
        let error = load_error(
            &[],
            Some("OLLAMA_HOST=http://attacker.example:11434\n"),
            Some("OLLAMA_API_KEY=operator-key\n"),
        );
        assert!(
            matches!(
                error,
                LlmConfigError::UntrustedEndpoint {
                    url_var: "OLLAMA_HOST",
                    key_var: "OLLAMA_API_KEY"
                }
            ),
            "{error:?}"
        );
    }

    #[test]
    #[serial]
    fn a_cwd_dot_env_may_configure_its_own_key_and_endpoint() {
        // Both from ./.env: the file only redirects its own credential.
        let settings = load_with_files(
            &[],
            Some("LLM_BASE_URL=https://gw.example/v1\nLLM_API_KEY=project-key\n"),
            Some("LLM_PROVIDER=openai-compatible\nLLM_MODEL=m\n"),
        )
        .unwrap();
        assert_eq!(
            settings.primary_slot().unwrap().api_key.as_deref(),
            Some("project-key")
        );

        // An operator endpoint with a ./.env key sends nothing of the operator's.
        let settings = load_with_files(
            &[],
            Some("LLM_API_KEY=project-key\n"),
            Some(
                "LLM_PROVIDER=openai-compatible\nLLM_MODEL=m\nLLM_BASE_URL=https://gw.example/v1\n",
            ),
        )
        .unwrap();
        assert_eq!(
            settings.primary_slot().unwrap().base_url.as_deref(),
            Some("https://gw.example/v1")
        );
    }

    #[test]
    fn a_cloud_slot_rejects_an_ollama_model() {
        // A stale `ollama::` model left by an installer must fail at startup,
        // not as a model-not-found 4xx on every call.
        let settings = edited(cloud_primary(LlmProviderKind::Anthropic), |s| {
            s.llm_primary.model = Some("ollama::qwen3.5:9b".into());
        });
        assert!(matches!(
            llm_config_error(settings.validate().unwrap_err()),
            LlmConfigError::OllamaModelOnCloudSlot {
                var: "LLM_MODEL",
                kind: LlmProviderKind::Anthropic,
                ..
            }
        ));
    }

    #[test]
    fn anthropic_base_url_without_a_version_segment_is_detected() {
        let anthropic = |base_url: &str| LlmSlot {
            role: LlmRole::Primary,
            kind: LlmProviderKind::Anthropic,
            model: "claude-haiku-5-5".into(),
            api_key: Some("key".into()),
            base_url: Some(base_url.into()),
        };
        assert!(anthropic_base_url_lacks_version(&anthropic(
            "https://api.anthropic.com"
        )));
        assert!(anthropic_base_url_lacks_version(&anthropic(
            "https://gw.example/anthropic/"
        )));
        for ok in [
            "https://api.anthropic.com/v1/",
            "https://api.anthropic.com/v1",
            "https://gw.example/anthropic/v1",
        ] {
            assert!(!anthropic_base_url_lacks_version(&anthropic(ok)), "{ok}");
        }
        // Only the native backend appends /messages to the base URL.
        let openai = LlmSlot {
            kind: LlmProviderKind::OpenAi,
            ..anthropic("https://gw.example")
        };
        assert!(!anthropic_base_url_lacks_version(&openai));
    }

    #[test]
    fn json_mode_breakage_is_detected_for_openai_adapters_aimed_at_anthropic() {
        let slot = |kind, base_url: &str| LlmSlot {
            role: LlmRole::Primary,
            kind,
            model: "claude-haiku-5-5".into(),
            api_key: Some("key".into()),
            base_url: Some(base_url.into()),
        };
        for kind in [
            LlmProviderKind::OpenRouter,
            LlmProviderKind::OpenAiCompatible,
            LlmProviderKind::OpenAi,
        ] {
            assert!(
                breaks_json_mode(&slot(kind, "https://api.anthropic.com/v1")),
                "{kind}"
            );
            assert!(breaks_json_mode(&slot(
                kind,
                "https://API.Anthropic.com/v1/"
            )));
            assert!(!breaks_json_mode(&slot(
                kind,
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
        let settings = edited(cloud_primary(LlmProviderKind::OpenAiCompatible), |s| {
            s.llm_primary.base_url = Some("htp//secret-token".into());
        });
        let error = llm_config_error(settings.validate().unwrap_err());
        assert!(matches!(
            error,
            LlmConfigError::InvalidUrl {
                var: "LLM_BASE_URL",
                ..
            }
        ));
        assert!(!error.to_string().contains("secret-token"), "{error}");
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

        assert_eq!(settings.llm_fallback, None);
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
        .unwrap_err();
        assert!(matches!(
            llm_config_error(error),
            LlmConfigError::InsecureUrl {
                var: "OPENROUTER_BASE_URL"
            }
        ));

        // A slot value keeps its own name even in legacy mode.
        let error = load_with(&[
            ("LLM_PROVIDER", Some("ollama")),
            ("LLM_FALLBACK_PROVIDER", Some("openrouter")),
            ("LLM_FALLBACK_API_KEY", Some("k")),
            ("LLM_FALLBACK_BASE_URL", Some("http://10.0.0.1/v1")),
            ("OPENROUTER_MODEL", Some("m")),
        ])
        .unwrap_err();
        assert!(matches!(
            llm_config_error(error),
            LlmConfigError::InsecureUrl {
                var: "LLM_FALLBACK_BASE_URL"
            }
        ));

        // A missing value names both places it could come from.
        let error = load_with(&[
            ("LLM_PROVIDER", Some("openrouter")),
            ("OPENROUTER_API_KEY", Some("k")),
        ])
        .unwrap_err();
        assert_eq!(missing_var(error), ("LLM_MODEL", Some("OPENROUTER_MODEL")));
    }

    #[test]
    fn debug_redacts_credentials_in_base_urls() {
        let settings = Settings {
            llm_primary: SlotSettings {
                base_url: Some("https://user:primary-url-secret@gw.example/v1".into()),
                ..slot(LlmProviderKind::OpenAiCompatible, "m", "k")
            },
            llm_fallback: Some(SlotSettings {
                base_url: Some("https://gw.example/v1?key=fallback-url-secret".into()),
                ..slot(LlmProviderKind::OpenAiCompatible, "m", "k")
            }),
            ollama_host: Some("http://user:ollama-url-secret@gpu:11434".into()),
            ..Settings::default()
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
        assert_eq!(d.llm_primary, SlotSettings::default());
        assert_eq!(d.llm_primary.kind, LlmProviderKind::Ollama);
        assert_eq!(d.llm_fallback, None);
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
        let settings = load_with(&[
            ("MEMCAN_WEBUI_USERNAME", Some("operator")),
            ("MEMCAN_WEBUI_PASSWORD", Some("correct horse")),
        ])
        .unwrap();
        assert_eq!(settings.webui_username.as_deref(), Some("operator"));
        assert_eq!(settings.webui_password.as_deref(), Some("correct horse"));

        let settings = load_with(&[
            ("MEMCAN_WEBUI_USERNAME", Some("")),
            ("MEMCAN_WEBUI_PASSWORD", Some("still-set")),
        ])
        .unwrap();
        assert!(settings.webui_username.is_none());
        assert_eq!(settings.webui_password.as_deref(), Some("still-set"));

        let settings = load_with(&[("MEMCAN_WEBUI_USERNAME", Some("operator"))]).unwrap();
        assert_eq!(settings.webui_username.as_deref(), Some("operator"));
        assert!(settings.webui_password.is_none());
    }

    #[test]
    #[serial]
    fn test_webui_password_is_masked_in_settings_debug() {
        let settings = load_with(&[
            ("MEMCAN_WEBUI_USERNAME", Some("operator")),
            ("MEMCAN_WEBUI_PASSWORD", Some("never-print-me")),
        ])
        .unwrap();

        let debug = format!("{settings:?}");
        assert!(debug.contains("operator"));
        assert!(debug.contains("***"));
        assert!(!debug.contains("never-print-me"));
    }
}
