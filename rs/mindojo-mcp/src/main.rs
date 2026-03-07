//! MindOJO MCP Server — persistent memory for Claude Code via LanceDB.
//!
//! Embeddings: fastembed (in-process ONNX). LLM: genai (multi-provider).
//! Transport: stdio (launched by Claude Code).

use std::sync::Arc;

use rmcp::{
    ServerHandler, ServiceExt, handler::server::router::tool::ToolRouter,
    handler::server::wrapper::Parameters, model::*, schemars, tool, tool_handler, tool_router,
    transport::stdio,
};
use serde::Deserialize;
use tracing::info;

use mindojo_core::{
    config::Settings,
    embed::FastEmbedProvider,
    error::MindojoError,
    lancedb_store::LanceDbStore,
    llm::GenaiLlmProvider,
    pipeline::{self, CODE_TABLE, MEMORIES_TABLE, STANDARDS_TABLE},
    traits::{EmbeddingProvider, LlmProvider, VectorStore},
};

// ---------------------------------------------------------------------------
// Shared state
// ---------------------------------------------------------------------------

struct SharedState {
    store: Box<dyn VectorStore>,
    embedder: Box<dyn EmbeddingProvider>,
    llm: Box<dyn LlmProvider>,
    config: Settings,
    llm_model: String,
}

// ---------------------------------------------------------------------------
// Tool parameter structs
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct AddMemoryParams {
    /// The memory content to store.
    pub memory: String,
    /// Git remote origin repo name (not dir name) for project-scoped memory. Omit for global.
    pub project: Option<String>,
    /// Explicit user ID override.
    pub user_id: Option<String>,
    /// Optional metadata dict (e.g., {"source": "penny", "type": "lesson"}).
    pub metadata: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SearchMemoriesParams {
    /// Natural language search query.
    pub query: String,
    /// Git remote origin repo name (not dir name) to scope search. Omit for global.
    pub project: Option<String>,
    /// Explicit user ID override.
    pub user_id: Option<String>,
    /// Max results to return (default 10).
    pub limit: Option<u32>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct GetMemoriesParams {
    /// Git remote origin repo name (not dir name) for project-scoped listing. Omit for global.
    pub project: Option<String>,
    /// Explicit user ID override.
    pub user_id: Option<String>,
    /// Max memories to return (default 100).
    pub limit: Option<u32>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct CountMemoriesParams {
    /// Git remote origin repo name (not dir name) for project-scoped count. Omit for global.
    pub project: Option<String>,
    /// Explicit user ID override.
    pub user_id: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct DeleteMemoryParams {
    /// The ID of the memory to delete.
    pub memory_id: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct UpdateMemoryParams {
    /// The ID of the memory to update.
    pub memory_id: String,
    /// New content for the memory.
    pub memory: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SearchStandardsParams {
    /// Natural language search query.
    pub query: String,
    /// Filter by category ("security", "coding", "cve", "guideline").
    pub standard_type: Option<String>,
    /// Filter by standard ID. Use list_collections() to discover available values.
    pub standard_id: Option<String>,
    /// Filter by a cross-reference ID (e.g. "CWE-89", "V5.3.4").
    pub ref_id: Option<String>,
    /// Filter by technology stack (e.g. "python", "rust").
    pub tech_stack: Option<String>,
    /// Filter by language code (e.g. "en").
    pub lang: Option<String>,
    /// Max results (1-100, default 10).
    pub limit: Option<u32>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SearchCodeParams {
    /// Natural language search query.
    pub query: String,
    /// Filter by project name.
    pub project: Option<String>,
    /// Filter by technology stack (e.g. "python", "rust").
    pub tech_stack: Option<String>,
    /// Filter by source file path (substring match).
    pub file_path: Option<String>,
    /// Max results (1-100, default 10).
    pub limit: Option<u32>,
}

// ---------------------------------------------------------------------------
// Helper functions
// ---------------------------------------------------------------------------

/// Determine the user_id for scoping.
/// Priority: explicit user_id > project:<name> > settings.default_user_id.
fn resolve_user_id(project: &Option<String>, user_id: &Option<String>, default: &str) -> String {
    if let Some(uid) = user_id {
        return uid.clone();
    }
    if let Some(proj) = project {
        return format!("project:{proj}");
    }
    default.to_string()
}

/// Escape a value for safe use in LanceDB SQL filters.
/// Escapes single quotes (SQL injection) and LIKE wildcards.
fn sanitize_sql_value(s: &str) -> String {
    s.replace('\'', "''")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

/// Build a LanceDB SQL filter for user_id scoping using the dedicated column.
fn user_filter(user_id: &str) -> String {
    let safe = sanitize_sql_value(user_id);
    format!("user_id = '{safe}'")
}

/// Format memory search results into a JSON array matching the Python output.
fn format_memory_results(results: &[mindojo_core::traits::SearchResult]) -> serde_json::Value {
    let entries: Vec<serde_json::Value> = results
        .iter()
        .map(|r| {
            let payload = &r.payload;
            let mut entry = serde_json::json!({
                "id": r.id,
                "memory": payload.get("data").and_then(|v| v.as_str()).unwrap_or(""),
                "hash": payload.get("hash").and_then(|v| v.as_str()).unwrap_or(""),
                "score": r.score,
                "created_at": payload.get("created_at"),
                "updated_at": payload.get("updated_at"),
                "user_id": payload.get("user_id").and_then(|v| v.as_str()).unwrap_or(""),
            });
            // Include extra metadata fields
            if let Some(obj) = payload.as_object()
                && let Some(entry_obj) = entry.as_object_mut()
            {
                for (k, v) in obj {
                    if !matches!(
                        k.as_str(),
                        "data" | "hash" | "created_at" | "updated_at" | "user_id"
                    ) {
                        entry_obj.insert(k.clone(), v.clone());
                    }
                }
            }
            entry
        })
        .collect();
    serde_json::Value::Array(entries)
}

/// Format standards search results into a JSON array.
fn format_standards_results(results: &[mindojo_core::traits::SearchResult]) -> serde_json::Value {
    let entries: Vec<serde_json::Value> = results
        .iter()
        .map(|r| {
            let p = &r.payload;
            serde_json::json!({
                "score": r.score,
                "data": p.get("data").and_then(|v| v.as_str()).unwrap_or(""),
                "standard_id": p.get("standard_id").and_then(|v| v.as_str()).unwrap_or(""),
                "standard_type": p.get("standard_type").and_then(|v| v.as_str()).unwrap_or(""),
                "section_id": p.get("section_id").and_then(|v| v.as_str()).unwrap_or(""),
                "section_title": p.get("section_title").and_then(|v| v.as_str()).unwrap_or(""),
                "chapter": p.get("chapter").and_then(|v| v.as_str()).unwrap_or(""),
                "ref_ids": p.get("ref_ids").cloned().unwrap_or(serde_json::Value::Array(vec![])),
                "version": p.get("version").and_then(|v| v.as_str()).unwrap_or(""),
                "tech_stack": p.get("tech_stack").and_then(|v| v.as_str()).unwrap_or(""),
                "lang": p.get("lang").and_then(|v| v.as_str()).unwrap_or(""),
                "url": p.get("url").and_then(|v| v.as_str()).unwrap_or(""),
            })
        })
        .collect();
    serde_json::Value::Array(entries)
}

/// Format code search results into a JSON array.
fn format_code_results(results: &[mindojo_core::traits::SearchResult]) -> serde_json::Value {
    let entries: Vec<serde_json::Value> = results
        .iter()
        .map(|r| {
            let p = &r.payload;
            serde_json::json!({
                "score": r.score,
                "data": p.get("data").and_then(|v| v.as_str()).unwrap_or(""),
                "project": p.get("project").and_then(|v| v.as_str()).unwrap_or(""),
                "tech_stack": p.get("tech_stack").and_then(|v| v.as_str()).unwrap_or(""),
                "file_path": p.get("file_path").and_then(|v| v.as_str()).unwrap_or(""),
                "line_start": p.get("line_start"),
                "line_end": p.get("line_end"),
            })
        })
        .collect();
    serde_json::Value::Array(entries)
}

/// Build a hint object for empty search results.
fn empty_hint(filters: &[(&str, Option<&str>)]) -> serde_json::Value {
    let active: Vec<String> = filters
        .iter()
        .filter_map(|(k, v)| v.map(|val| format!("{k}='{val}'")))
        .collect();
    let hint = if active.is_empty() {
        "No semantic matches found. Try broadening your query.".to_string()
    } else {
        format!(
            "No matches found. Applied filters: {}. Use list_collections() to discover valid filter values.",
            active.join(", ")
        )
    };
    serde_json::json!({ "results": [], "hint": hint })
}

// ---------------------------------------------------------------------------
// MCP Service
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct MindojoService {
    tool_router: ToolRouter<Self>,
    state: Arc<SharedState>,
}

impl std::fmt::Debug for SharedState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SharedState")
            .field("config", &self.config)
            .finish()
    }
}

#[tool_router]
impl MindojoService {
    fn new(state: Arc<SharedState>) -> Self {
        Self {
            tool_router: Self::tool_router(),
            state,
        }
    }

    #[tool(description = "Store a memory — lesson learned, decision, preference, or pattern.")]
    async fn add_memory(
        &self,
        Parameters(params): Parameters<AddMemoryParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let uid = resolve_user_id(
            &params.project,
            &params.user_id,
            &self.state.config.default_user_id,
        );
        let metadata = params
            .metadata
            .unwrap_or(serde_json::Value::Object(Default::default()));
        let memory = params.memory;
        let state = Arc::clone(&self.state);
        let llm_model = self.state.llm_model.clone();

        info!(user_id = %uid, len = memory.len(), "add_memory: queued");

        let uid_clone = uid.clone();
        tokio::spawn(async move {
            let result = pipeline::do_add_memory(
                &memory,
                &uid_clone,
                &metadata,
                state.config.distill_memories,
                MEMORIES_TABLE,
                state.store.as_ref(),
                state.embedder.as_ref(),
                state.llm.as_ref(),
                &llm_model,
                None,
            )
            .await;
            match result {
                Ok(_) => info!(user_id = %uid_clone, "add_memory: persisted"),
                Err(e) => {
                    let preview: String = memory.chars().take(120).collect();
                    tracing::error!(
                        user_id = %uid_clone,
                        error = %e,
                        memory_preview = %preview,
                        "add_memory: pipeline failed to store memory"
                    );
                }
            }
        });

        let response = serde_json::json!({ "status": "queued", "user_id": uid });
        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string(&response).unwrap_or_default(),
        )]))
    }

    #[tool(description = "Semantic search across stored memories.")]
    async fn search_memories(
        &self,
        Parameters(params): Parameters<SearchMemoriesParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let limit = params.limit.unwrap_or(10).clamp(1, 1000) as usize;
        let uid = resolve_user_id(
            &params.project,
            &params.user_id,
            &self.state.config.default_user_id,
        );
        info!(query = %params.query, user_id = %uid, limit, "search_memories");

        let vectors = self
            .state
            .embedder
            .embed(&[params.query])
            .await
            .map_err(|e| ErrorData::internal_error(format!("embedding failed: {e}"), None))?;

        let filter = user_filter(&uid);
        let results = self
            .state
            .store
            .search(MEMORIES_TABLE, &vectors[0], Some(&filter), limit)
            .await
            .map_err(|e| ErrorData::internal_error(format!("search failed: {e}"), None))?;

        let output = format_memory_results(&results);
        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string(&output).unwrap_or_default(),
        )]))
    }

    #[tool(description = "List memories for a given scope (up to limit).")]
    async fn get_memories(
        &self,
        Parameters(params): Parameters<GetMemoriesParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let limit = params.limit.unwrap_or(100).clamp(1, 1000) as usize;
        let uid = resolve_user_id(
            &params.project,
            &params.user_id,
            &self.state.config.default_user_id,
        );
        info!(user_id = %uid, limit, "get_memories");

        let filter = user_filter(&uid);
        let results = self
            .state
            .store
            .scroll(MEMORIES_TABLE, Some(&filter), limit)
            .await
            .map_err(|e| ErrorData::internal_error(format!("scroll failed: {e}"), None))?;

        let output = format_memory_results(&results);
        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string(&output).unwrap_or_default(),
        )]))
    }

    #[tool(description = "Count total memories for a given scope.")]
    async fn count_memories(
        &self,
        Parameters(params): Parameters<CountMemoriesParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let uid = resolve_user_id(
            &params.project,
            &params.user_id,
            &self.state.config.default_user_id,
        );
        info!(user_id = %uid, "count_memories");

        let filter = user_filter(&uid);
        let count = self
            .state
            .store
            .count(MEMORIES_TABLE, Some(&filter))
            .await
            .map_err(|e| ErrorData::internal_error(format!("count failed: {e}"), None))?;

        info!(user_id = %uid, count, "count_memories: result");
        let response = serde_json::json!({ "count": count, "user_id": uid });
        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string(&response).unwrap_or_default(),
        )]))
    }

    // INTENTIONAL(SEC-006): No ownership verification — single-user deployment, no multi-user scenarios
    #[tool(description = "Delete a specific memory by ID.")]
    async fn delete_memory(
        &self,
        Parameters(params): Parameters<DeleteMemoryParams>,
    ) -> Result<CallToolResult, ErrorData> {
        info!(memory_id = %params.memory_id, "delete_memory");

        self.state
            .store
            .delete(MEMORIES_TABLE, std::slice::from_ref(&params.memory_id))
            .await
            .map_err(|e| ErrorData::internal_error(format!("delete failed: {e}"), None))?;

        let response = serde_json::json!({ "status": "deleted", "memory_id": params.memory_id });
        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string(&response).unwrap_or_default(),
        )]))
    }

    // INTENTIONAL(SEC-006): No ownership verification — single-user deployment, no multi-user scenarios
    #[tool(description = "Update an existing memory's content.")]
    async fn update_memory(
        &self,
        Parameters(params): Parameters<UpdateMemoryParams>,
    ) -> Result<CallToolResult, ErrorData> {
        info!(memory_id = %params.memory_id, "update_memory");

        // Retrieve existing memory
        let existing = self
            .state
            .store
            .get(MEMORIES_TABLE, std::slice::from_ref(&params.memory_id))
            .await
            .map_err(|e| ErrorData::internal_error(format!("get failed: {e}"), None))?;

        if existing.is_empty() {
            let response =
                serde_json::json!({ "error": "memory not found", "memory_id": params.memory_id });
            return Ok(CallToolResult::success(vec![Content::text(
                serde_json::to_string(&response).unwrap_or_default(),
            )]));
        }

        let old_payload = &existing[0].payload;
        let old_user_id = old_payload
            .get("user_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let old_created_at = old_payload
            .get("created_at")
            .cloned()
            .unwrap_or_else(|| serde_json::Value::String(chrono::Utc::now().to_rfc3339()));

        // Re-embed the new content
        let vectors = self
            .state
            .embedder
            .embed(std::slice::from_ref(&params.memory))
            .await
            .map_err(|e| ErrorData::internal_error(format!("embedding failed: {e}"), None))?;

        let hash = pipeline::md5_hex(&params.memory);
        let now = chrono::Utc::now().to_rfc3339();

        let mut payload = serde_json::json!({
            "data": params.memory,
            "hash": hash,
            "user_id": old_user_id,
            "created_at": old_created_at,
            "updated_at": now,
        });

        // Preserve extra metadata from old payload
        if let (Some(old_obj), Some(new_obj)) = (old_payload.as_object(), payload.as_object_mut()) {
            for (k, v) in old_obj {
                if !matches!(
                    k.as_str(),
                    "data" | "hash" | "user_id" | "created_at" | "updated_at"
                ) {
                    new_obj.insert(k.clone(), v.clone());
                }
            }
        }

        let point = mindojo_core::traits::VectorPoint {
            id: params.memory_id.clone(),
            vector: vectors[0].clone(),
            payload,
        };
        self.state
            .store
            .upsert(MEMORIES_TABLE, &[point])
            .await
            .map_err(|e| ErrorData::internal_error(format!("upsert failed: {e}"), None))?;

        let response = serde_json::json!({ "status": "updated", "memory_id": params.memory_id });
        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string(&response).unwrap_or_default(),
        )]))
    }

    #[tool(
        description = "List available data collections with point counts. Call this to discover what data is indexed and what filter values are valid for search_standards, search_code, and search_memories."
    )]
    async fn list_collections(&self) -> Result<CallToolResult, ErrorData> {
        let known_tables = [MEMORIES_TABLE, STANDARDS_TABLE, CODE_TABLE];
        let mut collections: Vec<serde_json::Value> = Vec::new();

        for name in &known_tables {
            match self.state.store.count(name, None).await {
                Ok(count) => {
                    collections.push(serde_json::json!({
                        "name": name,
                        "count": count,
                    }));
                }
                Err(_) => {
                    // Table doesn't exist yet, skip it
                }
            }
        }

        let response = serde_json::json!({ "collections": collections });
        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string(&response).unwrap_or_default(),
        )]))
    }

    #[tool(description = "Search indexed standards (CWE, OWASP, etc.) by semantic similarity.")]
    async fn search_standards(
        &self,
        Parameters(params): Parameters<SearchStandardsParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let limit = params.limit.unwrap_or(10).clamp(1, 100) as usize;

        let standard_type = params.standard_type.map(|s| s.to_lowercase());
        let standard_id = params.standard_id.map(|s| s.to_lowercase());
        let ref_id = params.ref_id;
        let tech_stack = params.tech_stack;
        let lang = params.lang;

        info!(query = %params.query, limit, "search_standards");

        let vectors = self
            .state
            .embedder
            .embed(std::slice::from_ref(&params.query))
            .await
            .map_err(|e| ErrorData::internal_error(format!("embedding failed: {e}"), None))?;

        // Build filter using dedicated columns
        let mut filter_parts: Vec<String> = Vec::new();
        if let Some(ref v) = standard_type {
            let safe = sanitize_sql_value(v);
            filter_parts.push(format!("standard_type = '{safe}'"));
        }
        if let Some(ref v) = standard_id {
            let safe = sanitize_sql_value(v);
            filter_parts.push(format!("standard_id = '{safe}'"));
        }
        if let Some(ref v) = tech_stack {
            let safe = sanitize_sql_value(v);
            filter_parts.push(format!("tech_stack = '{safe}'"));
        }
        // lang has no dedicated column; filter via LIKE on payload
        if let Some(ref v) = lang {
            let safe = sanitize_sql_value(v);
            filter_parts.push(format!(r#"payload LIKE '%"lang":"{safe}"%'"#));
        }
        // ref_id has no dedicated column; filter via LIKE on payload
        if let Some(ref rid) = ref_id {
            let safe = sanitize_sql_value(rid);
            filter_parts.push(format!(r#"payload LIKE '%"{safe}"%'"#));
        }

        let filter = if filter_parts.is_empty() {
            None
        } else {
            Some(filter_parts.join(" AND "))
        };

        let results = self
            .state
            .store
            .search(STANDARDS_TABLE, &vectors[0], filter.as_deref(), limit)
            .await
            .map_err(|e| ErrorData::internal_error(format!("search failed: {e}"), None))?;

        let output = if results.is_empty() {
            let hint_filters: Vec<(&str, Option<&str>)> = vec![
                ("standard_type", standard_type.as_deref()),
                ("standard_id", standard_id.as_deref()),
                ("ref_id", ref_id.as_deref()),
                ("tech_stack", tech_stack.as_deref()),
                ("lang", lang.as_deref()),
            ];
            empty_hint(&hint_filters)
        } else {
            format_standards_results(&results)
        };

        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string(&output).unwrap_or_default(),
        )]))
    }

    #[tool(description = "Search indexed code snippets by semantic similarity.")]
    async fn search_code(
        &self,
        Parameters(params): Parameters<SearchCodeParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let limit = params.limit.unwrap_or(10).clamp(1, 100) as usize;

        let project = params.project.map(|s| s.to_lowercase());
        let tech_stack = params.tech_stack.map(|s| s.to_lowercase());
        let file_path = params.file_path;

        info!(query = %params.query, limit, "search_code");

        let vectors = self
            .state
            .embedder
            .embed(std::slice::from_ref(&params.query))
            .await
            .map_err(|e| ErrorData::internal_error(format!("embedding failed: {e}"), None))?;

        // Build filter using dedicated columns
        let mut filter_parts: Vec<String> = Vec::new();
        if let Some(ref p) = project {
            let safe = sanitize_sql_value(p);
            filter_parts.push(format!("project = '{safe}'"));
        }
        if let Some(ref ts) = tech_stack {
            let safe = sanitize_sql_value(ts);
            filter_parts.push(format!("tech_stack = '{safe}'"));
        }
        if let Some(ref fp) = file_path {
            let safe = sanitize_sql_value(fp);
            filter_parts.push(format!("file_path LIKE '%{safe}%'"));
        }

        let filter = if filter_parts.is_empty() {
            None
        } else {
            Some(filter_parts.join(" AND "))
        };

        let results = self
            .state
            .store
            .search(CODE_TABLE, &vectors[0], filter.as_deref(), limit)
            .await
            .map_err(|e| ErrorData::internal_error(format!("search failed: {e}"), None))?;

        let output = if results.is_empty() {
            let hint_filters: Vec<(&str, Option<&str>)> = vec![
                ("project", project.as_deref()),
                ("tech_stack", tech_stack.as_deref()),
                ("file_path", file_path.as_deref()),
            ];
            empty_hint(&hint_filters)
        } else {
            format_code_results(&results)
        };

        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string(&output).unwrap_or_default(),
        )]))
    }
}

// ---------------------------------------------------------------------------
// ServerHandler implementation
// ---------------------------------------------------------------------------

#[tool_handler]
impl ServerHandler for MindojoService {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            protocol_version: ProtocolVersion::V_2024_11_05,
            capabilities: ServerCapabilities::builder().enable_tools().build(),
            server_info: Implementation {
                name: "mindojo".into(),
                title: Some("MindOJO".into()),
                version: env!("CARGO_PKG_VERSION").into(),
                description: Some("Persistent memory for Claude Code".into()),
                icons: None,
                website_url: Some("https://github.com/lklimek/mindojo".into()),
            },
            instructions: Some(
                "Persistent memory for Claude Code — store and recall learnings, \
                 decisions, preferences across sessions."
                    .into(),
            ),
        }
    }
}

// ---------------------------------------------------------------------------
// Logging setup
// ---------------------------------------------------------------------------

fn setup_logging(log_file: &str) {
    use tracing_subscriber::EnvFilter;

    if log_file.is_empty() {
        tracing_subscriber::fmt()
            .with_writer(std::io::stderr)
            .with_ansi(false)
            .with_env_filter(EnvFilter::from_default_env())
            .init();
        return;
    }

    // Ensure log directory exists
    if let Some(parent) = std::path::Path::new(log_file).parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    let file_appender = tracing_appender::rolling::never(
        std::path::Path::new(log_file)
            .parent()
            .unwrap_or(std::path::Path::new(".")),
        std::path::Path::new(log_file)
            .file_name()
            .unwrap_or(std::ffi::OsStr::new("mindojo-mcp.log")),
    );

    tracing_subscriber::fmt()
        .with_writer(file_appender)
        .with_ansi(false)
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("debug")),
        )
        .init();

    info!(log_file, "MindOJO MCP server starting");
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() -> Result<(), MindojoError> {
    let config = Settings::load();
    setup_logging(&config.log_file);

    info!("Loading config: lancedb_path={}", config.lancedb_path);

    // Create embedding provider (in-process fastembed ONNX)
    let embedder = FastEmbedProvider::from_settings(&config)?;

    // Create LLM provider (genai — multi-provider: Ollama, OpenAI, Anthropic, etc.)
    let llm = GenaiLlmProvider::from_settings(&config);
    let llm_model = llm.default_model().to_string();

    // Open LanceDB store
    let store = LanceDbStore::open(&config.lancedb_path).await?;

    // Ensure tables exist
    let dims = config.embed_dims;
    store.ensure_table(MEMORIES_TABLE, dims).await?;
    store.ensure_table(STANDARDS_TABLE, dims).await?;
    store.ensure_table(CODE_TABLE, dims).await?;

    info!("Tables ensured: {MEMORIES_TABLE}, {STANDARDS_TABLE}, {CODE_TABLE}");

    let shared = Arc::new(SharedState {
        store: Box::new(store),
        embedder: Box::new(embedder),
        llm: Box::new(llm),
        config,
        llm_model,
    });

    let service = MindojoService::new(shared);
    let transport = stdio();
    let server = service
        .serve(transport)
        .await
        .inspect_err(|e| tracing::error!("serving error: {:?}", e))
        .map_err(|e| MindojoError::Other(format!("MCP serve failed: {e}")))?;

    info!("MindOJO MCP server running on stdio");
    server
        .waiting()
        .await
        .map_err(|e| MindojoError::Other(format!("MCP server error: {e}")))?;

    Ok(())
}
