//! Typed error hierarchy for MindOJO.
//!
//! [`MindojoError`] replaces blanket `anyhow::Error` usage, giving callers
//! the ability to match on specific failure modes.

/// Convenience alias used throughout the crate and its dependents.
pub type Result<T> = std::result::Result<T, MindojoError>;

#[derive(Debug, thiserror::Error)]
pub enum MindojoError {
    // -- I/O -----------------------------------------------------------------
    #[error("{context}: {source}")]
    Io {
        context: String,
        source: std::io::Error,
    },

    // -- JSON ----------------------------------------------------------------
    #[error("{context}: {source}")]
    Json {
        context: String,
        source: serde_json::Error,
    },

    // -- LanceDB -------------------------------------------------------------
    #[error("{context}: {source}")]
    LanceDb {
        context: String,
        source: lancedb::Error,
    },

    // -- Arrow ---------------------------------------------------------------
    #[error("Arrow error: {0}")]
    Arrow(#[from] arrow_schema::ArrowError),

    // -- Embedding -----------------------------------------------------------
    #[error("Embedding error ({context}): {detail}")]
    Embedding { context: String, detail: String },

    // -- LLM Chat ------------------------------------------------------------
    #[error("LLM chat error ({context}): {detail}")]
    LlmChat { context: String, detail: String },

    // -- Validation ----------------------------------------------------------
    #[error("vector dimension mismatch: expected {expected}, got {actual}")]
    DimensionMismatch { expected: usize, actual: usize },

    #[error("could not determine vector dimensions from table schema")]
    SchemaDimensions,

    // -- Generic (replaces bail!/anyhow!) ------------------------------------
    #[error("{0}")]
    Other(String),
}

// -- Manual From impls with no context --------------------------------------

impl From<std::io::Error> for MindojoError {
    fn from(e: std::io::Error) -> Self {
        Self::Io {
            context: "I/O error".into(),
            source: e,
        }
    }
}

impl From<serde_json::Error> for MindojoError {
    fn from(e: serde_json::Error) -> Self {
        Self::Json {
            context: "JSON error".into(),
            source: e,
        }
    }
}

impl From<lancedb::Error> for MindojoError {
    fn from(e: lancedb::Error) -> Self {
        Self::LanceDb {
            context: "LanceDB error".into(),
            source: e,
        }
    }
}

// -- Helper trait: .context() analog for Result<T, E> -----------------------

/// Extension trait that attaches a string context to any `Result` whose error
/// converts into [`MindojoError`].
pub trait ResultExt<T> {
    fn context(self, ctx: &str) -> Result<T>;
    fn with_context<F: FnOnce() -> String>(self, f: F) -> Result<T>;
}

impl<T> ResultExt<T> for std::result::Result<T, std::io::Error> {
    fn context(self, ctx: &str) -> Result<T> {
        self.map_err(|e| MindojoError::Io {
            context: ctx.to_string(),
            source: e,
        })
    }
    fn with_context<F: FnOnce() -> String>(self, f: F) -> Result<T> {
        self.map_err(|e| MindojoError::Io {
            context: f(),
            source: e,
        })
    }
}

impl<T> ResultExt<T> for std::result::Result<T, serde_json::Error> {
    fn context(self, ctx: &str) -> Result<T> {
        self.map_err(|e| MindojoError::Json {
            context: ctx.to_string(),
            source: e,
        })
    }
    fn with_context<F: FnOnce() -> String>(self, f: F) -> Result<T> {
        self.map_err(|e| MindojoError::Json {
            context: f(),
            source: e,
        })
    }
}

impl<T> ResultExt<T> for std::result::Result<T, lancedb::Error> {
    fn context(self, ctx: &str) -> Result<T> {
        self.map_err(|e| MindojoError::LanceDb {
            context: ctx.to_string(),
            source: e,
        })
    }
    fn with_context<F: FnOnce() -> String>(self, f: F) -> Result<T> {
        self.map_err(|e| MindojoError::LanceDb {
            context: f(),
            source: e,
        })
    }
}
