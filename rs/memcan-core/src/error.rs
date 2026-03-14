//! Typed error hierarchy for MemCan.
//!
//! [`MemcanError`] replaces blanket `anyhow::Error` usage, giving callers
//! the ability to match on specific failure modes.

/// Convenience alias used throughout the crate and its dependents.
pub type Result<T> = std::result::Result<T, MemcanError>;

#[derive(Debug, thiserror::Error)]
pub enum MemcanError {
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

    #[error("invalid configuration: {0}")]
    Config(String),

    // -- Dependency health ---------------------------------------------------
    #[error("dependency '{dependency}' unavailable: {message}")]
    DependencyUnavailable { dependency: String, message: String },

    // -- Generic (replaces bail!/anyhow!) ------------------------------------
    #[error("{0}")]
    Other(String),
}

impl MemcanError {
    /// Returns `true` when this error originated from an LLM chat call.
    pub fn is_llm_error(&self) -> bool {
        matches!(self, MemcanError::LlmChat { .. })
    }

    /// Returns `true` when this error originated from a LanceDB operation.
    pub fn is_lancedb_error(&self) -> bool {
        matches!(self, MemcanError::LanceDb { .. })
    }

    /// Returns `true` when this error originated from an embedding operation.
    pub fn is_embedding_error(&self) -> bool {
        matches!(self, MemcanError::Embedding { .. })
    }

    /// Returns `true` when this error indicates a dependency is unavailable.
    pub fn is_dependency_unavailable(&self) -> bool {
        matches!(self, MemcanError::DependencyUnavailable { .. })
    }
}

// -- Manual From impls with no context --------------------------------------

impl From<std::io::Error> for MemcanError {
    fn from(e: std::io::Error) -> Self {
        Self::Io {
            context: "I/O error".into(),
            source: e,
        }
    }
}

impl From<serde_json::Error> for MemcanError {
    fn from(e: serde_json::Error) -> Self {
        Self::Json {
            context: "JSON error".into(),
            source: e,
        }
    }
}

impl From<lancedb::Error> for MemcanError {
    fn from(e: lancedb::Error) -> Self {
        Self::LanceDb {
            context: "LanceDB error".into(),
            source: e,
        }
    }
}

// -- VectorStoreError --------------------------------------------------------

/// Error type for [`crate::typed_table::TypedTable`] and vector-store
/// operations.  All public methods on `TypedTable` and the `typed_table()`
/// factory on `LanceDbStore` return this.
#[derive(Debug, thiserror::Error)]
pub enum VectorStoreError {
    #[error("stale table handle for '{table}': {reason}")]
    StaleHandle { table: String, reason: String },

    #[error("table '{0}' not found")]
    TableNotFound(String),

    #[error("embedding failed: {0}")]
    Embedding(String),

    #[error("schema mismatch on table '{table}': {detail}")]
    SchemaMismatch { table: String, detail: String },

    #[error("lancedb error: {0}")]
    Store(#[from] lancedb::Error),

    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("memcan error: {0}")]
    Memcan(#[from] MemcanError),
}

impl From<VectorStoreError> for MemcanError {
    fn from(e: VectorStoreError) -> Self {
        match e {
            VectorStoreError::StaleHandle { table, reason } => MemcanError::LanceDb {
                context: format!("stale handle for table '{table}'"),
                source: lancedb::Error::Runtime { message: reason },
            },
            VectorStoreError::TableNotFound(name) => {
                MemcanError::Other(format!("table '{name}' not found"))
            }
            VectorStoreError::Embedding(detail) => MemcanError::Embedding {
                context: "vector store".into(),
                detail,
            },
            VectorStoreError::SchemaMismatch { table, detail } => {
                MemcanError::Config(format!("schema mismatch on table '{table}': {detail}"))
            }
            VectorStoreError::Store(e) => MemcanError::LanceDb {
                context: "LanceDB error".into(),
                source: e,
            },
            VectorStoreError::Serialization(e) => MemcanError::Json {
                context: "vector store serialization".into(),
                source: e,
            },
            VectorStoreError::Memcan(e) => e,
        }
    }
}

// -- Helper trait: .context() analog for Result<T, E> -----------------------

/// Extension trait that attaches a string context to any `Result` whose error
/// converts into [`MemcanError`].
pub trait ResultExt<T> {
    fn context(self, ctx: &str) -> Result<T>;
    fn with_context<F: FnOnce() -> String>(self, f: F) -> Result<T>;
}

/// Implements [`ResultExt`] for a source error type that maps to a
/// [`MemcanError`] variant with `context` + `source` fields.
macro_rules! impl_result_ext {
    ($source:ty => $variant:ident) => {
        impl<T> ResultExt<T> for std::result::Result<T, $source> {
            fn context(self, ctx: &str) -> Result<T> {
                self.map_err(|e| MemcanError::$variant {
                    context: ctx.to_string(),
                    source: e,
                })
            }
            fn with_context<F: FnOnce() -> String>(self, f: F) -> Result<T> {
                self.map_err(|e| MemcanError::$variant {
                    context: f(),
                    source: e,
                })
            }
        }
    };
}

impl_result_ext!(std::io::Error => Io);
impl_result_ext!(serde_json::Error => Json);
impl_result_ext!(lancedb::Error => LanceDb);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_llm_error_true() {
        let err = MemcanError::LlmChat {
            context: "test".into(),
            detail: "fail".into(),
        };
        assert!(err.is_llm_error());
    }

    #[test]
    fn test_is_llm_error_false_for_other_variants() {
        let err = MemcanError::Embedding {
            context: "test".into(),
            detail: "fail".into(),
        };
        assert!(!err.is_llm_error());

        let err = MemcanError::Other("something".into());
        assert!(!err.is_llm_error());

        let err = MemcanError::Config("bad".into());
        assert!(!err.is_llm_error());
    }

    #[test]
    fn test_is_lancedb_error() {
        let err = MemcanError::LanceDb {
            context: "test".into(),
            source: lancedb::Error::Runtime {
                message: "fail".into(),
            },
        };
        assert!(err.is_lancedb_error());
        assert!(!err.is_llm_error());
        assert!(!err.is_embedding_error());
    }

    #[test]
    fn test_is_embedding_error() {
        let err = MemcanError::Embedding {
            context: "test".into(),
            detail: "fail".into(),
        };
        assert!(err.is_embedding_error());
        assert!(!err.is_llm_error());
        assert!(!err.is_lancedb_error());
    }

    #[test]
    fn test_is_dependency_unavailable() {
        let err = MemcanError::DependencyUnavailable {
            dependency: "ollama".into(),
            message: "connection refused".into(),
        };
        assert!(err.is_dependency_unavailable());
        assert!(!err.is_llm_error());
        assert!(!err.is_lancedb_error());
        assert!(!err.is_embedding_error());
        assert!(err.to_string().contains("ollama"));
        assert!(err.to_string().contains("connection refused"));
    }
}
