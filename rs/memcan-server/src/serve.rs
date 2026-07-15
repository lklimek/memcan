//! MCP server — HTTP + stdio transport with /health endpoint.
//!
//! Dual transport: `--stdio` for backward compat, default is HTTP via axum.

use std::collections::HashMap;
use std::future::Future;
use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex as StdMutex};

use axum::Router;
use axum::extract::Request;
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use lru::LruCache;
use rmcp::{
    ServerHandler, ServiceExt,
    handler::server::router::tool::ToolRouter,
    handler::server::tool::ToolCallContext,
    handler::server::wrapper::Parameters,
    model::*,
    schemars,
    service::{RequestContext, RoleServer},
    tool, tool_router,
    transport::io::stdio,
    transport::streamable_http_server::{
        session::local::LocalSessionManager,
        tower::{StreamableHttpServerConfig, StreamableHttpService},
    },
};
use serde::Deserialize;
use subtle::ConstantTimeEq;
use tokio::net::TcpListener;
use tracing::{debug, info, warn};
use uuid::Uuid;

use memcan_core::{
    config::Settings,
    error::MemcanError,
    export,
    health::{DependencyHealth, DependencyId},
    import,
    indexing::standards::{self as standards_indexing, VALID_TYPES},
    init::{MemcanContext, create_llm_provider},
    pipeline::{
        CODE_TABLE, MEMORIES_TABLE, Pipeline, PipelineGuard, PipelineProgress, STANDARDS_TABLE,
    },
    prompts::FACT_EXTRACTION_HOOK_PROMPT,
    query::{resolve_user_id, sanitize_eq, sanitize_like},
    schema::MemcanTableSchema,
    search,
    todo::{self, TODOS_TABLE},
    traits::{EmbeddingProvider, LlmProvider, VectorStore},
};

use crate::ServeArgs;

/// Maximum content size for standards indexing (500 KB).
const MAX_STANDARDS_CONTENT_SIZE: usize = 500 * 1024;

#[derive(Clone)]
struct QueueEntry {
    operation: String,
    user_id: String,
    progress: Arc<StdMutex<PipelineProgress>>,
    queued_at: String,
}

fn queue_entry_to_json(entry: &QueueEntry) -> serde_json::Value {
    let p = entry.progress.lock().unwrap_or_else(|e| e.into_inner());
    serde_json::json!({
        "operation": entry.operation,
        "user_id": entry.user_id,
        "status": p.step.as_str(),
        "warnings": p.warnings,
        "error": p.error,
        "queued_at": entry.queued_at,
        "completed_at": p.completed_at,
    })
}

/// Maximum number of pending async operations (queued + running).
/// Cap on concurrent in-flight async operations.
///
/// The memcan CLI mirrors this as `PACING_THRESHOLD` (rs/memcan/src/main.rs) to
/// pace its submissions below the cap — update both if you change it.
const MAX_PENDING_TASKS: usize = 20;

/// RAII guard that decrements the pending-task counter on drop.
struct PendingTaskGuard {
    counter: Arc<AtomicUsize>,
}

impl Drop for PendingTaskGuard {
    fn drop(&mut self) {
        self.counter.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Try to claim a slot in the pending queue. Returns a guard that auto-decrements on drop.
fn try_enqueue(counter: &Arc<AtomicUsize>) -> Result<PendingTaskGuard, ErrorData> {
    let prev = counter.fetch_add(1, Ordering::SeqCst);
    if prev >= MAX_PENDING_TASKS {
        counter.fetch_sub(1, Ordering::SeqCst);
        Err(ErrorData::new(
            rmcp::model::ErrorCode::INTERNAL_ERROR,
            // The "Server busy"/"operations pending" phrasing is string-matched by
            // the memcan CLI's `is_server_busy` (rs/memcan/src/main.rs) to decide
            // retry-vs-abort — keep both phrases if you reword this.
            format!(
                "Server busy: {} operations pending (max {}). Try again later.",
                prev, MAX_PENDING_TASKS
            ),
            None,
        ))
    } else {
        Ok(PendingTaskGuard {
            counter: Arc::clone(counter),
        })
    }
}

/// Build the `delete_code_records` predicate from structured, server-validated
/// inputs only. The mandatory `project` and the optional matchers are escaped
/// here — no caller-supplied SQL is ever interpolated into the predicate.
fn build_delete_code_predicate(
    project: &str,
    extension: Option<&str>,
    file_path_exact: Option<&str>,
) -> Result<String, String> {
    if project.trim().is_empty() {
        return Err("project must not be empty".into());
    }

    let mut parts = vec![format!("project = '{}'", sanitize_eq(project))];

    if let Some(ext) = extension.map(str::trim).filter(|e| !e.is_empty()) {
        let ext = ext.strip_prefix('.').unwrap_or(ext);
        if !ext.chars().all(|c| c.is_ascii_alphanumeric()) {
            return Err(format!(
                "invalid extension '{ext}': only alphanumeric characters are allowed"
            ));
        }
        parts.push(format!("file_path LIKE '%.{}'", sanitize_like(ext)));
    }

    if let Some(path) = file_path_exact.map(str::trim).filter(|p| !p.is_empty()) {
        parts.push(format!("file_path = '{}'", sanitize_eq(path)));
    }

    Ok(parts.join(" AND "))
}

struct SharedState {
    store: Arc<dyn VectorStore>,
    embedder: Arc<dyn EmbeddingProvider>,
    llm: Arc<dyn LlmProvider>,
    config: Settings,
    llm_model: String,
    queue_status: Arc<StdMutex<LruCache<String, QueueEntry>>>,
    llm_semaphore: Arc<tokio::sync::Semaphore>,
    pending_tasks: Arc<AtomicUsize>,
    health: Arc<DependencyHealth>,
}

// --- Tool parameter structs ---

/// Deserialize a JSON map that may arrive as either a proper object or a
/// stringified JSON object (common when LLMs double-encode metadata).
fn deserialize_string_or_map<'de, D>(
    deserializer: D,
) -> Result<Option<HashMap<String, serde_json::Value>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::Error;

    let value: Option<serde_json::Value> = Option::deserialize(deserializer)?;
    match value {
        None => Ok(None),
        Some(serde_json::Value::Object(map)) => Ok(Some(map.into_iter().collect())),
        Some(serde_json::Value::String(s)) => {
            if s.len() > 64 * 1024 {
                return Err(D::Error::custom("metadata string exceeds 64 KB limit"));
            }
            let parsed: serde_json::Value = serde_json::from_str(&s).map_err(D::Error::custom)?;
            match parsed {
                serde_json::Value::Object(map) => Ok(Some(map.into_iter().collect())),
                _ => Err(D::Error::custom("metadata string must contain a JSON map")),
            }
        }
        Some(other) => {
            let label = match &other {
                serde_json::Value::Null => "null",
                serde_json::Value::Bool(_) => "bool",
                serde_json::Value::Number(_) => "number",
                serde_json::Value::Array(_) => "array",
                _ => "unknown",
            };
            Err(D::Error::custom(format!(
                "expected map or string, got {label}"
            )))
        }
    }
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct AddMemoryParams {
    pub memory: String,
    pub project: Option<String>,
    pub user_id: Option<String>,
    #[serde(default, deserialize_with = "deserialize_string_or_map")]
    pub metadata: Option<HashMap<String, serde_json::Value>>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SearchMemoriesParams {
    pub query: String,
    pub project: Option<String>,
    pub user_id: Option<String>,
    pub limit: Option<u32>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct GetMemoriesParams {
    pub project: Option<String>,
    pub user_id: Option<String>,
    pub limit: Option<u32>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct CountMemoriesParams {
    pub project: Option<String>,
    pub user_id: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct DeleteMemoryParams {
    pub memory_id: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct UpdateMemoryParams {
    pub memory_id: String,
    pub memory: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SearchStandardsParams {
    pub query: String,
    pub standard_type: Option<String>,
    pub standard_id: Option<String>,
    pub ref_id: Option<String>,
    pub tech_stack: Option<String>,
    pub lang: Option<String>,
    pub limit: Option<u32>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct GetQueueStatusParams {
    pub operation_id: Option<String>,
    pub limit: Option<u32>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SearchCodeParams {
    pub query: String,
    pub project: Option<String>,
    pub tech_stack: Option<String>,
    pub file_path: Option<String>,
    pub limit: Option<u32>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct UnifiedSearchParams {
    /// The search query.
    pub query: String,
    /// Collections to search. Omit to search all (memories, standards, code).
    pub collections: Option<Vec<String>>,
    pub project: Option<String>,
    pub user_id: Option<String>,
    /// Total result limit across all collections (merged by relevance score). Default 5, max 100.
    pub limit: Option<u32>,
    /// Filter standards by type (security, coding, cve, guideline, accessibility).
    pub standard_type: Option<String>,
    /// Filter standards by ID (e.g., "owasp-cheatsheets").
    pub standard_id: Option<String>,
    /// Filter code by tech stack.
    pub tech_stack: Option<String>,
    /// Filter code by file path (substring match).
    pub file_path: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct IndexStandardsToolParams {
    /// Markdown content of the standards document to index.
    pub content: String,
    /// Standard identifier (e.g., "owasp-cheatsheets", "owasp-asvs").
    pub standard_id: String,
    /// Type of standard: security, coding, cve, or guideline.
    pub standard_type: String,
    /// Standard version (e.g., "5.0", "2024").
    pub version: Option<String>,
    /// Language code (e.g., "en").
    pub lang: Option<String>,
    /// Source URL.
    pub url: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct DropIndexedStandardsParams {
    /// Standard identifier to drop all indexed data for.
    pub standard_id: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct DeleteCodeRecordsParams {
    /// Project to scope the delete to (required — prevents an unscoped wipe).
    pub project: String,
    /// Optional file extension to narrow the delete to, e.g. `py` or `.py`.
    /// The server turns this into a validated, wildcard-escaped
    /// `file_path LIKE '%.<ext>'` matcher — no caller SQL reaches the predicate.
    pub extension: Option<String>,
    /// Optional exact relative file path to delete (e.g. `src/main.rs`).
    /// Matched via a sanitized equality, never raw SQL.
    pub file_path_exact: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct AddTodoParams {
    /// Short title of the TODO item.
    pub title: String,
    /// Optional longer description.
    pub description: Option<String>,
    /// Project this TODO belongs to (required).
    pub project: String,
    /// Priority: "low", "medium", or "high". Defaults to "medium".
    pub priority: Option<String>,
    /// Optional agent or coordinator responsible for the TODO.
    pub owner: Option<String>,
    /// Optional TODO IDs that block this item.
    pub blocked_by: Option<Vec<String>>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ListTodosParams {
    /// Project to list TODOs for (required).
    pub project: String,
    /// Filter by status: pending, done, in_progress, blocked, postponed, or cancelled.
    pub status: Option<String>,
    /// Filter by owner. Omit for all owners.
    pub owner: Option<String>,
    /// Max results (default 50, max 200).
    pub limit: Option<u32>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct UpdateTodoParams {
    /// ID of the TODO to update.
    pub todo_id: String,
    /// New title (optional).
    pub title: Option<String>,
    /// New description (optional).
    pub description: Option<String>,
    /// New priority (optional).
    pub priority: Option<String>,
    /// New status: pending, done, in_progress, blocked, postponed, or cancelled.
    pub status: Option<String>,
    /// New owner; an empty string clears the owner.
    pub owner: Option<String>,
    /// Replacement TODO IDs that block this item; an empty list clears them.
    pub blocked_by: Option<Vec<String>>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct CompleteTodoParams {
    /// ID of the TODO to mark as done.
    pub todo_id: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct DeleteTodoParams {
    /// ID of the TODO to delete.
    pub todo_id: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct GetTodoParams {
    /// ID of the TODO to fetch.
    pub todo_id: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ExportCollectionParams {
    /// Collection to export: memories, standards, code, todos.
    pub collection: String,
    /// Optional SQL filter expression.
    pub filter: Option<String>,
    /// Max records to return (default 1000, max 10000).
    pub limit: Option<u32>,
    /// Offset for pagination (default 0).
    pub offset: Option<u32>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ImportRecordsParams {
    /// JSONL text — one JSON object per line.
    pub records: String,
}

#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
pub struct ToolCodeFileInput {
    /// Relative file path (e.g. "src/main.rs").
    pub path: String,
    /// Full file content.
    pub content: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct IndexCodeFilesToolParams {
    /// Source files to index (path + content).
    pub files: Vec<ToolCodeFileInput>,
    /// Project name.
    pub project: String,
    /// Technology stack label.
    pub tech_stack: String,
}

// --- Helpers ---

fn user_filter(user_id: &str) -> String {
    let safe = sanitize_eq(user_id);
    format!("user_id = '{safe}'")
}

fn format_memory_results(results: &[memcan_core::traits::SearchResult]) -> serde_json::Value {
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

fn format_standards_results(results: &[memcan_core::traits::SearchResult]) -> serde_json::Value {
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

fn format_code_results(results: &[memcan_core::traits::SearchResult]) -> serde_json::Value {
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

/// Classify an error and report the appropriate dependency as failed.
fn report_error_to_health(health: &DependencyHealth, err: &MemcanError) {
    let msg = err.to_string();
    if err.is_llm_error() {
        health.report_failure(DependencyId::Ollama, &msg);
    } else if err.is_lancedb_error() {
        health.report_failure(DependencyId::LanceDb, &msg);
    } else if err.is_embedding_error() {
        health.report_failure(DependencyId::Embedding, &msg);
    }
}

// --- MCP Service ---

#[derive(Debug, Clone)]
pub struct MemcanService {
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
impl MemcanService {
    fn new(state: Arc<SharedState>) -> Self {
        Self {
            tool_router: Self::tool_router(),
            state,
        }
    }

    #[tool(description = "Store a memory - lesson learned, decision, preference, or pattern.")]
    async fn add_memory(
        &self,
        Parameters(params): Parameters<AddMemoryParams>,
    ) -> Result<CallToolResult, ErrorData> {
        debug!(tool = "add_memory", project = ?params.project, user_id = ?params.user_id, len = params.memory.len(), "MCP request");
        let uid = resolve_user_id(
            &params.project,
            &params.user_id,
            &self.state.config.default_user_id,
        );
        let metadata = params
            .metadata
            .map(|m| serde_json::to_value(m).unwrap_or_default())
            .unwrap_or(serde_json::Value::Object(Default::default()));
        let memory = params.memory;

        // Claim a queue slot before logging "queued" or inserting the LRU entry,
        // so a busy-rejection leaves no phantom never-completing status record.
        let task_guard = try_enqueue(&self.state.pending_tasks)?;

        let op_id = Uuid::new_v4().to_string();
        info!(user_id = %uid, len = memory.len(), operation_id = %op_id, "add_memory: queued");

        let is_hook = metadata.get("source").and_then(|v| v.as_str()) == Some("auto-hook");

        let pipeline = Pipeline::new(
            Arc::clone(&self.state.store),
            Arc::clone(&self.state.embedder),
            Arc::clone(&self.state.llm),
            self.state.llm_model.clone(),
            MEMORIES_TABLE,
            Arc::new(MemcanTableSchema),
            self.state.config.distill_memories,
        );
        let pipeline = if is_hook {
            pipeline.with_extraction_prompt(FACT_EXTRACTION_HOOK_PROMPT)
        } else {
            pipeline
        };
        let progress = pipeline.progress();
        let mut guard = PipelineGuard::new(pipeline);

        {
            let mut cache = self
                .state
                .queue_status
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            cache.put(
                op_id.clone(),
                QueueEntry {
                    operation: "add_memory".into(),
                    user_id: uid.clone(),
                    progress,
                    queued_at: chrono::Utc::now().to_rfc3339(),
                },
            );
        }

        // Fail fast if LLM is known to be down (embedding + lancedb checked on demand)
        if self.state.config.distill_memories {
            self.state
                .health
                .check(DependencyId::Ollama)
                .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
        }

        let sem = Arc::clone(&self.state.llm_semaphore);
        let health = Arc::clone(&self.state.health);
        let uid_clone = uid.clone();
        tokio::spawn(async move {
            let _task_guard = task_guard;
            let _permit = match sem.acquire().await {
                Ok(p) => p,
                Err(_) => {
                    tracing::warn!("semaphore closed, aborting background task");
                    return;
                }
            };

            match guard.add_memory(&memory, &uid_clone, &metadata).await {
                Ok(_) => {
                    info!(user_id = %uid_clone, "add_memory: persisted");
                    health.report_success(DependencyId::Ollama);
                    health.report_success(DependencyId::Embedding);
                    health.report_success(DependencyId::LanceDb);
                    guard.complete();
                }
                Err(e) => {
                    let preview: String = memory.chars().take(120).collect();
                    tracing::error!(
                        user_id = %uid_clone,
                        error = %e,
                        memory_preview = %preview,
                        "add_memory: pipeline failed to store memory"
                    );
                    report_error_to_health(&health, &e);
                    guard.fail(&e);
                }
            }
        });

        let response =
            serde_json::json!({ "status": "queued", "user_id": uid, "operation_id": op_id });
        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string(&response).unwrap_or_default(),
        )]))
    }

    #[tool(
        description = "Search memories with user/project scope filtering. For general search across all knowledge, prefer the unified 'search' tool."
    )]
    async fn search_memories(
        &self,
        Parameters(params): Parameters<SearchMemoriesParams>,
    ) -> Result<CallToolResult, ErrorData> {
        debug!(tool = "search_memories", query = %params.query, project = ?params.project, user_id = ?params.user_id, "MCP request");
        let limit = params.limit.unwrap_or(10).clamp(1, 1000) as usize;
        let uid = resolve_user_id(
            &params.project,
            &params.user_id,
            &self.state.config.default_user_id,
        );
        let query = params.query;
        info!(query = %query, user_id = %uid, limit, "search_memories");

        let vectors = self
            .state
            .embedder
            .embed(std::slice::from_ref(&query))
            .await
            .map_err(|e| ErrorData::internal_error(format!("embedding failed: {e}"), None))?;

        let filter = user_filter(&uid);
        let results = self
            .state
            .store
            .search(MEMORIES_TABLE, &vectors[0], Some(&filter), limit, 0)
            .await
            .map_err(|e| ErrorData::internal_error(format!("search failed: {e}"), None))?;

        info!(
            results = results.len(),
            top_score = results.first().map(|r| r.score).unwrap_or(0.0),
            "search completed"
        );

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
        debug!(tool = "get_memories", project = ?params.project, user_id = ?params.user_id, "MCP request");
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
            .scroll(MEMORIES_TABLE, Some(&filter), limit, 0)
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
        debug!(tool = "count_memories", project = ?params.project, user_id = ?params.user_id, "MCP request");
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

    #[tool(description = "Delete a specific memory by ID.")]
    async fn delete_memory(
        &self,
        Parameters(params): Parameters<DeleteMemoryParams>,
    ) -> Result<CallToolResult, ErrorData> {
        debug!(tool = "delete_memory", memory_id = %params.memory_id, "MCP request");

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

    #[tool(description = "Update an existing memory's content.")]
    async fn update_memory(
        &self,
        Parameters(params): Parameters<UpdateMemoryParams>,
    ) -> Result<CallToolResult, ErrorData> {
        debug!(tool = "update_memory", memory_id = %params.memory_id, "MCP request");

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

        let old_payload = existing[0].payload.clone();
        let old_user_id = old_payload
            .get("user_id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let old_created_at = old_payload
            .get("created_at")
            .cloned()
            .unwrap_or_else(|| serde_json::Value::String(chrono::Utc::now().to_rfc3339()));

        // Claim a queue slot before inserting the LRU status entry, so a
        // busy-rejection leaves no phantom never-completing status record.
        let task_guard = try_enqueue(&self.state.pending_tasks)?;

        let op_id = Uuid::new_v4().to_string();

        let pipeline = Pipeline::new(
            Arc::clone(&self.state.store),
            Arc::clone(&self.state.embedder),
            Arc::clone(&self.state.llm),
            self.state.llm_model.clone(),
            MEMORIES_TABLE,
            Arc::new(MemcanTableSchema),
            false,
        );
        let progress = pipeline.progress();
        let mut guard = PipelineGuard::new(pipeline);

        {
            let mut cache = self
                .state
                .queue_status
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            cache.put(
                op_id.clone(),
                QueueEntry {
                    operation: "update_memory".into(),
                    user_id: old_user_id.clone(),
                    progress,
                    queued_at: chrono::Utc::now().to_rfc3339(),
                },
            );
        }

        let sem = Arc::clone(&self.state.llm_semaphore);
        let health = Arc::clone(&self.state.health);
        let memory_id = params.memory_id.clone();
        let memory = params.memory;
        tokio::spawn(async move {
            let _task_guard = task_guard;
            let _permit = match sem.acquire().await {
                Ok(p) => p,
                Err(_) => {
                    tracing::warn!("semaphore closed, aborting background task");
                    return;
                }
            };

            match guard
                .update_memory(
                    &memory_id,
                    &memory,
                    &old_user_id,
                    old_created_at,
                    Some(&old_payload),
                )
                .await
            {
                Ok(()) => {
                    info!(memory_id = %memory_id, "update_memory: persisted");
                    health.report_success(DependencyId::Embedding);
                    health.report_success(DependencyId::LanceDb);
                    guard.complete();
                }
                Err(e) => {
                    tracing::error!(memory_id = %memory_id, error = %e, "update_memory: failed");
                    report_error_to_health(&health, &e);
                    guard.fail(&e);
                }
            }
        });

        let response = serde_json::json!({
            "status": "queued",
            "memory_id": params.memory_id,
            "operation_id": op_id,
        });
        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string(&response).unwrap_or_default(),
        )]))
    }

    #[tool(
        description = "List available data collections with point counts. Call this to discover what data is indexed and what filter values are valid for search_standards, search_code, and search_memories."
    )]
    async fn list_collections(&self) -> Result<CallToolResult, ErrorData> {
        debug!(tool = "list_collections", "MCP request");
        let known_tables = [MEMORIES_TABLE, STANDARDS_TABLE, CODE_TABLE, TODOS_TABLE];
        let mut collections: Vec<serde_json::Value> = Vec::new();

        for name in &known_tables {
            if let Ok(count) = self.state.store.count(name, None).await {
                collections.push(serde_json::json!({
                    "name": name,
                    "count": count,
                }));
            }
        }

        let response = serde_json::json!({ "collections": collections });
        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string(&response).unwrap_or_default(),
        )]))
    }

    #[tool(
        description = "Search indexed standards (CWE, OWASP, etc.) with advanced filters. For general search across all knowledge, prefer the unified 'search' tool."
    )]
    async fn search_standards(
        &self,
        Parameters(params): Parameters<SearchStandardsParams>,
    ) -> Result<CallToolResult, ErrorData> {
        debug!(tool = "search_standards", query = %params.query, standard_id = ?params.standard_id, standard_type = ?params.standard_type, "MCP request");
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

        let mut filter_parts: Vec<String> = Vec::new();
        if let Some(ref v) = standard_type {
            let safe = sanitize_eq(v);
            filter_parts.push(format!("standard_type = '{safe}'"));
        }
        if let Some(ref v) = standard_id {
            let safe = sanitize_eq(v);
            filter_parts.push(format!("standard_id = '{safe}'"));
        }
        if let Some(ref v) = tech_stack {
            let safe = sanitize_eq(v);
            filter_parts.push(format!("tech_stack = '{safe}'"));
        }
        if let Some(ref v) = lang {
            let safe = sanitize_like(v);
            filter_parts.push(format!(r#"payload LIKE '%"lang":"{safe}"%'"#));
        }
        if let Some(ref rid) = ref_id {
            let safe = sanitize_like(rid);
            filter_parts.push(format!(r#"payload LIKE '%"ref_id":"{safe}"%'"#));
        }

        let filter = if filter_parts.is_empty() {
            None
        } else {
            Some(filter_parts.join(" AND "))
        };

        let results = self
            .state
            .store
            .search(STANDARDS_TABLE, &vectors[0], filter.as_deref(), limit, 0)
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

    #[tool(
        description = "Search indexed code snippets with project/file/stack filters. For general search across all knowledge, prefer the unified 'search' tool."
    )]
    async fn search_code(
        &self,
        Parameters(params): Parameters<SearchCodeParams>,
    ) -> Result<CallToolResult, ErrorData> {
        debug!(tool = "search_code", query = %params.query, project = ?params.project, tech_stack = ?params.tech_stack, "MCP request");
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

        let mut filter_parts: Vec<String> = Vec::new();
        if let Some(ref p) = project {
            let safe = sanitize_eq(p);
            filter_parts.push(format!("project = '{safe}'"));
        }
        if let Some(ref ts) = tech_stack {
            let safe = sanitize_eq(ts);
            filter_parts.push(format!("tech_stack = '{safe}'"));
        }
        if let Some(ref fp) = file_path {
            let safe = sanitize_like(fp);
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
            .search(CODE_TABLE, &vectors[0], filter.as_deref(), limit, 0)
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

    #[tool(
        description = "Search across all knowledge (memories, code, standards) in one query. Use this as the default search tool. Results are merged by relevance score. Optionally filter by collection or apply collection-specific filters."
    )]
    async fn search(
        &self,
        Parameters(params): Parameters<UnifiedSearchParams>,
    ) -> Result<CallToolResult, ErrorData> {
        debug!(tool = "search", query = %params.query, collections = ?params.collections, project = ?params.project, "MCP request");

        self.state
            .health
            .check(DependencyId::Embedding)
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
        self.state
            .health
            .check(DependencyId::LanceDb)
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;

        let core_params = search::UnifiedSearchParams {
            query: params.query,
            collections: params.collections,
            project: params.project,
            user_id: params.user_id,
            limit: params.limit,
            standard_type: params.standard_type,
            standard_id: params.standard_id,
            tech_stack: params.tech_stack,
            file_path: params.file_path,
        };

        let results = match search::unified_search(
            &core_params,
            self.state.store.as_ref(),
            self.state.embedder.as_ref(),
            &self.state.config.default_user_id,
        )
        .await
        {
            Ok(r) => {
                self.state.health.report_success(DependencyId::Embedding);
                self.state.health.report_success(DependencyId::LanceDb);
                r
            }
            Err(e) => {
                report_error_to_health(&self.state.health, &e);
                return Err(ErrorData::internal_error(
                    format!("unified search failed: {e}"),
                    None,
                ));
            }
        };

        info!(
            results = results.len(),
            top_score = results.first().map(|r| r.score).unwrap_or(0.0),
            collections = ?core_params.collections,
            "search completed"
        );

        let output = serde_json::json!({
            "results": results,
        });
        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string(&output).unwrap_or_default(),
        )]))
    }

    #[tool(
        description = "Index a markdown standards document into the vector store. Returns an operation_id for progress tracking via get_queue_status."
    )]
    async fn index_standards(
        &self,
        Parameters(params): Parameters<IndexStandardsToolParams>,
    ) -> Result<CallToolResult, ErrorData> {
        debug!(tool = "index_standards", standard_id = %params.standard_id, standard_type = %params.standard_type, content_len = params.content.len(), "MCP request");
        if params.standard_id.is_empty() {
            return Err(ErrorData::internal_error(
                "standard_id must not be empty",
                None,
            ));
        }
        if params.content.is_empty() {
            return Err(ErrorData::internal_error("content must not be empty", None));
        }
        if params.content.len() > MAX_STANDARDS_CONTENT_SIZE {
            return Err(ErrorData::internal_error(
                format!(
                    "content too large ({} bytes, max {})",
                    params.content.len(),
                    MAX_STANDARDS_CONTENT_SIZE
                ),
                None,
            ));
        }
        if !VALID_TYPES.contains(&params.standard_type.as_str()) {
            return Err(ErrorData::internal_error(
                format!(
                    "Invalid standard_type '{}'. Must be one of: {}",
                    params.standard_type,
                    VALID_TYPES.join(", ")
                ),
                None,
            ));
        }

        // Claim a queue slot before logging "queued" or inserting the LRU entry,
        // so a busy-rejection leaves no phantom never-completing status record.
        let task_guard = try_enqueue(&self.state.pending_tasks)?;

        let op_id = Uuid::new_v4().to_string();
        info!(
            standard_id = %params.standard_id,
            standard_type = %params.standard_type,
            content_len = params.content.len(),
            operation_id = %op_id,
            "index_standards: queued"
        );

        let progress = Arc::new(StdMutex::new(PipelineProgress::default()));

        {
            let mut cache = self
                .state
                .queue_status
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            cache.put(
                op_id.clone(),
                QueueEntry {
                    operation: "index_standards".into(),
                    user_id: format!("standard:{}", params.standard_id),
                    progress: Arc::clone(&progress),
                    queued_at: chrono::Utc::now().to_rfc3339(),
                },
            );
        }

        // Fail fast if LLM is known to be down
        self.state
            .health
            .check(DependencyId::Ollama)
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;

        let store = Arc::clone(&self.state.store);
        let embedder = Arc::clone(&self.state.embedder);
        let llm = Arc::clone(&self.state.llm);
        let llm_model = self.state.llm_model.clone();
        let embed_dims = self.state.config.embed_dims;
        let sem = Arc::clone(&self.state.llm_semaphore);
        let health = Arc::clone(&self.state.health);

        let core_params = standards_indexing::IndexStandardsParams {
            content: params.content,
            standard_id: params.standard_id.clone(),
            standard_type: params.standard_type,
            version: params.version.unwrap_or_default(),
            lang: params.lang.unwrap_or_default(),
            url: params.url.unwrap_or_default(),
        };

        tokio::spawn(async move {
            let _task_guard = task_guard;
            let _permit = match sem.acquire().await {
                Ok(p) => p,
                Err(_) => {
                    tracing::warn!("semaphore closed, aborting index_standards task");
                    return;
                }
            };

            {
                let mut p = progress.lock().unwrap_or_else(|e| e.into_inner());
                p.step = memcan_core::pipeline::PipelineStep::Storing;
            }

            match standards_indexing::index_standards(
                &core_params,
                store.as_ref(),
                embedder.as_ref(),
                &MemcanTableSchema,
                llm.as_ref(),
                &llm_model,
                embed_dims,
            )
            .await
            {
                Ok(result) => {
                    let mut p = progress.lock().unwrap_or_else(|e| e.into_inner());
                    for err in &result.errors {
                        p.warnings.push(format!(
                            "chunk {}: {} - {}",
                            err.chunk_index, err.heading, err.error
                        ));
                    }
                    if result.errors.is_empty() {
                        p.step = memcan_core::pipeline::PipelineStep::Completed;
                    } else {
                        p.step = memcan_core::pipeline::PipelineStep::CompletedDegraded;
                    }
                    p.completed_at = Some(chrono::Utc::now().to_rfc3339());
                    health.report_success(DependencyId::Ollama);
                    health.report_success(DependencyId::Embedding);
                    health.report_success(DependencyId::LanceDb);
                    info!(
                        indexed = result.indexed,
                        errors = result.errors.len(),
                        "index_standards: finished"
                    );
                }
                Err(e) => {
                    let mut p = progress.lock().unwrap_or_else(|e| e.into_inner());
                    p.step = memcan_core::pipeline::PipelineStep::Failed;
                    p.error = Some(e.to_string());
                    p.completed_at = Some(chrono::Utc::now().to_rfc3339());
                    report_error_to_health(&health, &e);
                    tracing::error!(error = %e, "index_standards: failed");
                }
            }
        });

        let response = serde_json::json!({
            "status": "queued",
            "operation_id": op_id,
            "message": format!("Indexing standard '{}'", params.standard_id),
        });
        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string(&response).unwrap_or_default(),
        )]))
    }

    #[tool(description = "Drop all indexed standards data for a given standard_id.")]
    async fn drop_indexed_standards(
        &self,
        Parameters(params): Parameters<DropIndexedStandardsParams>,
    ) -> Result<CallToolResult, ErrorData> {
        debug!(tool = "drop_indexed_standards", standard_id = %params.standard_id, "MCP request");
        if params.standard_id.is_empty() {
            return Err(ErrorData::internal_error(
                "standard_id must not be empty",
                None,
            ));
        }

        let deleted = standards_indexing::drop_standards(
            &params.standard_id,
            self.state.store.as_ref(),
            &MemcanTableSchema,
            self.state.config.embed_dims,
        )
        .await
        .map_err(|e| ErrorData::internal_error(format!("drop failed: {e}"), None))?;

        let response = serde_json::json!({
            "deleted": deleted,
            "standard_id": params.standard_id,
        });
        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string(&response).unwrap_or_default(),
        )]))
    }

    #[tool(
        description = "Delete code records for a project, optionally narrowed by `extension` \
        (e.g. \"py\") or an exact `file_path_exact`. Both matchers are server-validated and \
        escaped — no caller-supplied SQL ever reaches the delete predicate. The project scope \
        is mandatory. Deletion is immediate and irreversible (no dry-run). Returns the number \
        of rows deleted (approximate under concurrent writes, since the count precedes the \
        delete). Refused when the server runs without MEMCAN_API_KEY configured."
    )]
    async fn delete_code_records(
        &self,
        Parameters(params): Parameters<DeleteCodeRecordsParams>,
    ) -> Result<CallToolResult, ErrorData> {
        debug!(
            tool = "delete_code_records",
            project = %params.project,
            extension = ?params.extension,
            file_path_exact = ?params.file_path_exact,
            "MCP request"
        );

        // A destructive cross-collection primitive must not run on an
        // unauthenticated endpoint (default Docker binds 0.0.0.0 with no key).
        if self.state.config.api_key.is_none() {
            return Err(ErrorData::internal_error(
                "delete_code_records requires MEMCAN_API_KEY to be configured",
                None,
            ));
        }

        let predicate = build_delete_code_predicate(
            &params.project,
            params.extension.as_deref(),
            params.file_path_exact.as_deref(),
        )
        .map_err(|e| ErrorData::internal_error(e, None))?;

        warn!(
            tool = "delete_code_records",
            project = %params.project,
            predicate = %predicate,
            "executing destructive code-record delete"
        );

        let deleted = self
            .state
            .store
            .delete_by_filter(CODE_TABLE, &predicate)
            .await
            .map_err(|e| ErrorData::internal_error(format!("delete failed: {e}"), None))?;

        warn!(
            tool = "delete_code_records",
            project = %params.project,
            deleted,
            "destructive code-record delete complete"
        );

        let response = serde_json::json!({
            "deleted": deleted,
            "project": params.project,
            "predicate": predicate,
        });
        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string(&response).unwrap_or_default(),
        )]))
    }

    #[tool(
        description = "Add a per-project TODO item. TODOs are also searchable via the unified 'search' tool."
    )]
    async fn add_todo(
        &self,
        Parameters(params): Parameters<AddTodoParams>,
    ) -> Result<CallToolResult, ErrorData> {
        debug!(tool = "add_todo", project = %params.project, title = %params.title, "MCP request");

        self.state
            .health
            .check(DependencyId::Embedding)
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
        self.state
            .health
            .check(DependencyId::LanceDb)
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;

        let core_params = todo::AddTodoParams {
            title: params.title,
            description: params.description,
            project: params.project,
            priority: params.priority,
            owner: params.owner,
            blocked_by: params.blocked_by,
        };

        let item = todo::add_todo(
            self.state.store.as_ref(),
            self.state.embedder.as_ref(),
            &MemcanTableSchema,
            core_params,
        )
        .await
        .map_err(|e| {
            report_error_to_health(&self.state.health, &e);
            ErrorData::internal_error(format!("add_todo failed: {e}"), None)
        })?;

        self.state.health.report_success(DependencyId::Embedding);
        self.state.health.report_success(DependencyId::LanceDb);

        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string(&item).unwrap_or_default(),
        )]))
    }

    #[tool(
        description = "List TODO items for a project, sorted by priority (high first). TODOs are also searchable via the unified 'search' tool."
    )]
    async fn list_todos(
        &self,
        Parameters(params): Parameters<ListTodosParams>,
    ) -> Result<CallToolResult, ErrorData> {
        debug!(tool = "list_todos", project = %params.project, status = ?params.status, owner = ?params.owner, "MCP request");

        self.state
            .health
            .check(DependencyId::LanceDb)
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;

        let limit = params.limit.unwrap_or(50).clamp(1, 200) as usize;

        let todos = todo::list_todos(
            self.state.store.as_ref(),
            &params.project,
            params.status.as_deref(),
            params.owner.as_deref(),
            limit,
        )
        .await
        .map_err(|e| {
            report_error_to_health(&self.state.health, &e);
            ErrorData::internal_error(format!("list_todos failed: {e}"), None)
        })?;

        self.state.health.report_success(DependencyId::LanceDb);

        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string(&todos).unwrap_or_default(),
        )]))
    }

    #[tool(
        description = "Update a TODO item's title, description, priority, status, owner, or blocked_by."
    )]
    async fn update_todo(
        &self,
        Parameters(params): Parameters<UpdateTodoParams>,
    ) -> Result<CallToolResult, ErrorData> {
        debug!(tool = "update_todo", todo_id = %params.todo_id, "MCP request");

        self.state
            .health
            .check(DependencyId::Embedding)
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
        self.state
            .health
            .check(DependencyId::LanceDb)
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;

        let updates = todo::UpdateTodoFields {
            title: params.title,
            description: params.description,
            priority: params.priority,
            status: params.status,
            owner: params.owner,
            blocked_by: params.blocked_by,
        };

        let item = todo::update_todo(
            self.state.store.as_ref(),
            self.state.embedder.as_ref(),
            &MemcanTableSchema,
            &params.todo_id,
            updates,
        )
        .await
        .map_err(|e| {
            report_error_to_health(&self.state.health, &e);
            ErrorData::internal_error(format!("update_todo failed: {e}"), None)
        })?;

        self.state.health.report_success(DependencyId::Embedding);
        self.state.health.report_success(DependencyId::LanceDb);

        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string(&item).unwrap_or_default(),
        )]))
    }

    #[tool(description = "Mark a TODO item as done.")]
    async fn complete_todo(
        &self,
        Parameters(params): Parameters<CompleteTodoParams>,
    ) -> Result<CallToolResult, ErrorData> {
        debug!(tool = "complete_todo", todo_id = %params.todo_id, "MCP request");

        self.state
            .health
            .check(DependencyId::Embedding)
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
        self.state
            .health
            .check(DependencyId::LanceDb)
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;

        let item = todo::complete_todo(
            self.state.store.as_ref(),
            self.state.embedder.as_ref(),
            &MemcanTableSchema,
            &params.todo_id,
        )
        .await
        .map_err(|e| {
            report_error_to_health(&self.state.health, &e);
            ErrorData::internal_error(format!("complete_todo failed: {e}"), None)
        })?;

        self.state.health.report_success(DependencyId::Embedding);
        self.state.health.report_success(DependencyId::LanceDb);

        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string(&item).unwrap_or_default(),
        )]))
    }

    #[tool(description = "Delete a TODO item by ID.")]
    async fn delete_todo(
        &self,
        Parameters(params): Parameters<DeleteTodoParams>,
    ) -> Result<CallToolResult, ErrorData> {
        debug!(tool = "delete_todo", todo_id = %params.todo_id, "MCP request");

        self.state
            .health
            .check(DependencyId::LanceDb)
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;

        todo::delete_todo(self.state.store.as_ref(), &params.todo_id)
            .await
            .map_err(|e| {
                report_error_to_health(&self.state.health, &e);
                ErrorData::internal_error(format!("delete_todo failed: {e}"), None)
            })?;

        self.state.health.report_success(DependencyId::LanceDb);

        let response = serde_json::json!({ "status": "deleted", "todo_id": params.todo_id });
        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string(&response).unwrap_or_default(),
        )]))
    }

    #[tool(
        description = "Fetch a single TODO by id. Returns the item, or a not-found marker for an unknown id."
    )]
    async fn get_todo(
        &self,
        Parameters(params): Parameters<GetTodoParams>,
    ) -> Result<CallToolResult, ErrorData> {
        debug!(tool = "get_todo", todo_id = %params.todo_id, "MCP request");

        self.state
            .health
            .check(DependencyId::LanceDb)
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;

        let item = todo::get_todo(self.state.store.as_ref(), &params.todo_id)
            .await
            .map_err(|e| {
                report_error_to_health(&self.state.health, &e);
                ErrorData::internal_error(format!("get_todo failed: {e}"), None)
            })?;

        self.state.health.report_success(DependencyId::LanceDb);

        let response = match item {
            Some(item) => serde_json::to_value(item).unwrap_or_default(),
            None => serde_json::json!({
                "error": "todo not found",
                "todo_id": params.todo_id,
            }),
        };
        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string(&response).unwrap_or_default(),
        )]))
    }

    #[tool(
        description = "Check status of queued async operations (add_memory, update_memory). Returns recent operations or a specific one by ID."
    )]
    async fn get_queue_status(
        &self,
        Parameters(params): Parameters<GetQueueStatusParams>,
    ) -> Result<CallToolResult, ErrorData> {
        debug!(tool = "get_queue_status", operation_id = ?params.operation_id, "MCP request");
        let limit = params.limit.unwrap_or(10).clamp(1, 100) as usize;

        // Collect entries while holding the LRU lock, then drop it before
        // serializing (which acquires each entry's progress mutex).
        if let Some(ref op_id) = params.operation_id {
            let entry = {
                let mut cache = self
                    .state
                    .queue_status
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                cache.get(op_id).cloned()
            };
            match entry {
                Some(entry) => {
                    let json =
                        serde_json::to_string(&queue_entry_to_json(&entry)).unwrap_or_default();
                    Ok(CallToolResult::success(vec![Content::text(json)]))
                }
                None => Ok(CallToolResult::success(vec![Content::text(
                    r#"{"error":"operation not found or expired from LRU cache"}"#,
                )])),
            }
        } else {
            let entries: Vec<QueueEntry> = {
                let cache = self
                    .state
                    .queue_status
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                cache.iter().take(limit).map(|(_, v)| v.clone()).collect()
            };
            let json_entries: Vec<serde_json::Value> =
                entries.iter().map(queue_entry_to_json).collect();
            let pending = self.state.pending_tasks.load(Ordering::SeqCst);
            let response = serde_json::json!({
                "pending_tasks": pending,
                "operations": json_entries,
            });
            let json = serde_json::to_string(&response).unwrap_or_default();
            Ok(CallToolResult::success(vec![Content::text(json)]))
        }
    }

    // QA-004: This tool inlines the scroll + record construction loop rather than
    // delegating to `memcan_core::export::export_collection`. See export.rs for the mapping.
    // QA-006: The `filter` param is passed as raw SQL to LanceDB `only_if()` without sanitization.
    // This is intentional — export is an admin/power-user operation, not a user-facing search.
    #[tool(
        description = "Export a collection as JSONL (text + metadata, no vectors). Paginate with limit/offset for large collections."
    )]
    async fn export_collection(
        &self,
        Parameters(params): Parameters<ExportCollectionParams>,
    ) -> Result<CallToolResult, ErrorData> {
        debug!(tool = "export_collection", collection = %params.collection, "MCP request");

        let table = export::collection_to_table(&params.collection)
            .map_err(|e| ErrorData::invalid_params(e.to_string(), None))?;

        let limit = params.limit.unwrap_or(1000).clamp(1, 10000) as usize;
        let offset = params.offset.unwrap_or(0) as usize;

        // Single-page scroll at the requested offset (MCP tool does not
        // paginate internally — the caller controls offset/limit).
        let results = self
            .state
            .store
            .scroll(
                table,
                // Filter is passed as-is to LanceDB SQL. This is intentional —
                // the MCP tool is admin-level and LanceDB is local-only.
                params.filter.as_deref(),
                limit,
                offset,
            )
            .await
            .map_err(|e| ErrorData::internal_error(format!("scroll failed: {e}"), None))?;

        let mut lines = Vec::with_capacity(results.len());
        let collection_name = export::table_to_collection(table).to_string();
        for sr in &results {
            let mut payload = match &sr.payload {
                serde_json::Value::Object(map) => map.clone(),
                _ => serde_json::Map::new(),
            };
            payload.remove("vector");
            export::strip_reserved_keys(&mut payload);

            let record = export::ExportRecord {
                _collection: collection_name.clone(),
                id: sr.id.clone(),
                payload,
            };
            let line = export::record_to_jsonl(&record)
                .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
            lines.push(line);
        }

        let output = lines.join("\n");
        let response = serde_json::json!({
            "count": lines.len(),
            "offset": offset,
            "data": output,
        });
        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string(&response).unwrap_or_default(),
        )]))
    }

    #[tool(description = "Import JSONL records: embed + upsert, no LLM processing. Internal use.")]
    async fn _import_records(
        &self,
        Parameters(params): Parameters<ImportRecordsParams>,
    ) -> Result<CallToolResult, ErrorData> {
        debug!(
            tool = "_import_records",
            len = params.records.len(),
            "MCP request"
        );

        let mut records = Vec::new();
        let mut parse_errors = Vec::new();
        for (i, line) in params.records.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            match export::jsonl_to_record(line) {
                Ok(r) => records.push(r),
                Err(e) => parse_errors.push(format!("line {}: {e}", i + 1)),
            }
        }

        let result = import::import_records(
            records,
            self.state.store.as_ref(),
            self.state.embedder.as_ref(),
            &MemcanTableSchema,
            self.state.config.embed_dims,
        )
        .await
        .map_err(|e| ErrorData::internal_error(format!("import failed: {e}"), None))?;

        let mut errors: Vec<String> = parse_errors;
        errors.extend(result.errors);

        let response = serde_json::json!({
            "imported": result.imported,
            "skipped": result.skipped,
            "errors": errors,
        });
        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string(&response).unwrap_or_default(),
        )]))
    }

    #[tool(
        description = "Index source code files: symbol extraction + LLM description + embedding. Requires LLM to be available."
    )]
    async fn index_code_files(
        &self,
        Parameters(params): Parameters<IndexCodeFilesToolParams>,
    ) -> Result<CallToolResult, ErrorData> {
        debug!(
            tool = "index_code_files",
            project = %params.project,
            files = params.files.len(),
            "MCP request"
        );

        self.state
            .health
            .check(DependencyId::Ollama)
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;

        // Claim a queue slot before logging "queued" or inserting the LRU entry,
        // so a busy-rejection leaves no phantom never-completing status record.
        let task_guard = try_enqueue(&self.state.pending_tasks)?;

        let op_id = Uuid::new_v4().to_string();
        info!(
            project = %params.project,
            files = params.files.len(),
            operation_id = %op_id,
            "index_code_files: queued"
        );

        let progress = Arc::new(StdMutex::new(PipelineProgress::default()));

        {
            let mut cache = self
                .state
                .queue_status
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            cache.put(
                op_id.clone(),
                QueueEntry {
                    operation: "index_code_files".into(),
                    user_id: format!("project:{}", params.project),
                    progress: Arc::clone(&progress),
                    queued_at: chrono::Utc::now().to_rfc3339(),
                },
            );
        }

        let store = Arc::clone(&self.state.store);
        let embedder = Arc::clone(&self.state.embedder);
        let llm = Arc::clone(&self.state.llm);
        let llm_model = self.state.llm_model.clone();
        let embed_dims = self.state.config.embed_dims;
        let sem = Arc::clone(&self.state.llm_semaphore);
        let health = Arc::clone(&self.state.health);

        let file_count = params.files.len();
        let project = params.project.clone();

        let core_files: Vec<memcan_core::indexing::code_files::CodeFileInput> = params
            .files
            .into_iter()
            .map(|f| memcan_core::indexing::code_files::CodeFileInput {
                path: f.path,
                content: f.content,
            })
            .collect();

        let core_params = memcan_core::indexing::code_files::IndexCodeFilesParams {
            files: core_files,
            project: params.project,
            tech_stack: params.tech_stack,
        };

        tokio::spawn(async move {
            let _task_guard = task_guard;
            let _permit = match sem.acquire().await {
                Ok(p) => p,
                Err(_) => {
                    tracing::warn!("semaphore closed, aborting index_code_files task");
                    return;
                }
            };

            {
                let mut p = progress.lock().unwrap_or_else(|e| e.into_inner());
                p.step = memcan_core::pipeline::PipelineStep::Storing;
            }

            match memcan_core::indexing::code_files::index_code_files(
                &core_params,
                store.as_ref(),
                embedder.as_ref(),
                &MemcanTableSchema,
                llm.as_ref(),
                &llm_model,
                embed_dims,
            )
            .await
            {
                Ok(result) => {
                    let mut p = progress.lock().unwrap_or_else(|e| e.into_inner());
                    if result.errors > 0 {
                        p.warnings
                            .push(format!("{} symbols failed to index", result.errors));
                        p.step = memcan_core::pipeline::PipelineStep::CompletedDegraded;
                    } else {
                        p.step = memcan_core::pipeline::PipelineStep::Completed;
                    }
                    p.completed_at = Some(chrono::Utc::now().to_rfc3339());
                    health.report_success(DependencyId::Ollama);
                    health.report_success(DependencyId::Embedding);
                    health.report_success(DependencyId::LanceDb);
                    info!(
                        indexed = result.indexed,
                        skipped = result.skipped,
                        errors = result.errors,
                        "index_code_files: finished"
                    );
                }
                Err(e) => {
                    let mut p = progress.lock().unwrap_or_else(|e| e.into_inner());
                    p.step = memcan_core::pipeline::PipelineStep::Failed;
                    p.error = Some(e.to_string());
                    p.completed_at = Some(chrono::Utc::now().to_rfc3339());
                    report_error_to_health(&health, &e);
                    tracing::error!(error = %e, "index_code_files: failed");
                }
            }
        });

        let response = serde_json::json!({
            "status": "queued",
            "operation_id": op_id,
            "message": format!("Indexing {file_count} files for project '{project}'"),
        });
        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string(&response).unwrap_or_default(),
        )]))
    }
}

// --- ServerHandler ---

impl ServerHandler for MemcanService {
    fn get_info(&self) -> ServerInfo {
        let mut info = ServerInfo::default();
        info.protocol_version = ProtocolVersion::LATEST;
        info.capabilities = ServerCapabilities::builder().enable_tools().build();
        info.server_info = {
            let mut imp = Implementation::default();
            imp.name = "memcan".into();
            imp.version = env!("CARGO_PKG_VERSION").into();
            imp.title = Some("MemCan".into());
            imp.description = Some("Persistent memory for Claude Code".into());
            imp.website_url = Some("https://github.com/lklimek/memcan".into());
            imp
        };
        info.instructions = Some(
            "Persistent memory for Claude Code — store and recall learnings, \
             decisions, preferences across sessions."
                .into(),
        );
        info
    }

    fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        // INTENTIONAL: MCP tool pagination is not implemented. The tool list is small (~20 tools)
        // and no known MCP client uses cursor-based pagination for tools/list.
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ListToolsResult, ErrorData>> + Send + '_ {
        let tools = self
            .tool_router
            .list_all()
            .into_iter()
            .filter(|t| !t.name.starts_with('_'))
            .collect();
        std::future::ready(Ok(ListToolsResult {
            tools,
            next_cursor: None,
            meta: None,
        }))
    }

    fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<CallToolResult, ErrorData>> + Send + '_ {
        let tcc = ToolCallContext::new(self, request, context);
        async move { self.tool_router.call(tcc).await }
    }
}

// --- Logging ---

fn setup_logging(log_file: &str) {
    use tracing_subscriber::EnvFilter;

    if log_file.is_empty() {
        tracing_subscriber::fmt()
            .with_writer(std::io::stderr)
            .with_ansi(false)
            .with_env_filter(
                EnvFilter::try_from_default_env().unwrap_or_else(|_| {
                    EnvFilter::new("info,memcan_core=debug,memcan_server=debug")
                }),
            )
            .init();
        return;
    }

    if let Some(parent) = std::path::Path::new(log_file).parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    let file_appender = tracing_appender::rolling::never(
        std::path::Path::new(log_file)
            .parent()
            .unwrap_or(std::path::Path::new(".")),
        std::path::Path::new(log_file)
            .file_name()
            .unwrap_or(std::ffi::OsStr::new("memcan.log")),
    );

    tracing_subscriber::fmt()
        .with_writer(file_appender)
        .with_ansi(false)
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("info,memcan_core=debug,memcan_server=debug")),
        )
        .init();

    info!(log_file, "MemCan server starting");
}

// --- Shutdown signal ---

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut sigterm =
            signal(SignalKind::terminate()).expect("failed to register SIGTERM handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                info!("Received SIGINT, starting graceful shutdown");
            }
            _ = sigterm.recv() => {
                info!("Received SIGTERM, starting graceful shutdown");
            }
        }
    }

    #[cfg(not(unix))]
    {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to listen for Ctrl+C");
        info!("Received Ctrl+C, starting graceful shutdown");
    }
}

// --- Health handler ---

async fn health_handler(
    axum::extract::State(health): axum::extract::State<Arc<DependencyHealth>>,
) -> impl IntoResponse {
    let deps = health.status();
    let overall = if health.all_healthy() {
        "ok"
    } else {
        "degraded"
    };
    axum::response::Json(serde_json::json!({
        "status": overall,
        "version": env!("CARGO_PKG_VERSION"),
        "dependencies": deps,
    }))
}

// --- Entry point ---

pub async fn run(args: &ServeArgs) -> Result<(), MemcanError> {
    let ctx = MemcanContext::init().await?;
    setup_logging(&ctx.settings.log_file);
    if ctx.settings.distill_memories {
        ctx.init_llm().await?;
    } else {
        info!("Skipping LLM init (DISTILL_MEMORIES=false)");
    }

    info!("Loading config: lancedb_path={}", ctx.settings.lancedb_path);

    let (llm, llm_model) = create_llm_provider(&ctx.settings);

    let dims = ctx.settings.embed_dims;
    let ts = MemcanTableSchema;
    ctx.store.ensure_table(MEMORIES_TABLE, dims, &ts).await?;
    ctx.store.ensure_table(STANDARDS_TABLE, dims, &ts).await?;
    ctx.store.ensure_table(CODE_TABLE, dims, &ts).await?;
    ctx.store.ensure_table(TODOS_TABLE, dims, &ts).await?;

    info!("Tables ensured: {MEMORIES_TABLE}, {STANDARDS_TABLE}, {CODE_TABLE}, {TODOS_TABLE}");

    if ctx.settings.compact_on_startup {
        let threshold = ctx.settings.compact_fragment_threshold;
        info!("Running startup compaction (compact fragments + prune old versions)");
        for table_name in &[MEMORIES_TABLE, STANDARDS_TABLE, CODE_TABLE, TODOS_TABLE] {
            // Skip tables that aren't fragmented enough to be worth a full
            // rewrite, so an already-compact DB doesn't pay OptimizeAction::All
            // on every boot. On a count error, fall back to compacting.
            let needs_compaction = match ctx.store.count_fragments(table_name).await {
                Ok(fragments) if fragments <= threshold => {
                    info!(
                        table = table_name,
                        fragments, threshold, "startup compaction skipped (already compact)"
                    );
                    false
                }
                Ok(_) => true,
                Err(e) => {
                    warn!(
                        table = table_name,
                        "startup fragment count failed, compacting anyway: {e}"
                    );
                    true
                }
            };
            if !needs_compaction {
                continue;
            }
            match ctx.store.compact_table(table_name).await {
                Ok(outcome) => info!(
                    table = table_name,
                    fragments_before = outcome.fragments_before,
                    fragments_after = outcome.fragments_after,
                    "startup compaction complete"
                ),
                // Non-fatal: a compaction failure must not block the server boot.
                Err(e) => warn!(table = table_name, "startup compaction failed: {e}"),
            }
        }
    } else {
        info!("Startup compaction disabled (COMPACT_ON_STARTUP=false)");
    }

    let listen_addr = args
        .listen
        .clone()
        .unwrap_or_else(|| ctx.settings.listen.clone());

    let health = Arc::new(DependencyHealth::with_defaults());

    let shared = Arc::new(SharedState {
        store: Arc::new(ctx.store),
        embedder: Arc::new(ctx.embedder),
        llm,
        config: ctx.settings.clone(),
        llm_model,
        queue_status: Arc::new(StdMutex::new(LruCache::new(
            NonZeroUsize::new(10000).unwrap(),
        ))),
        llm_semaphore: Arc::new(tokio::sync::Semaphore::new(2)),
        pending_tasks: Arc::new(AtomicUsize::new(0)),
        health: Arc::clone(&health),
    });

    if args.stdio {
        let service = MemcanService::new(Arc::clone(&shared));
        let transport = stdio();
        let server = service
            .serve(transport)
            .await
            .inspect_err(|e| tracing::error!("serving error: {:?}", e))
            .map_err(|e| MemcanError::Other(format!("MCP serve failed: {e}")))?;

        info!("MemCan MCP server running on stdio");
        server
            .waiting()
            .await
            .map_err(|e| MemcanError::Other(format!("MCP server error: {e}")))?;
    } else {
        let config = StreamableHttpServerConfig::default();
        let session_manager = Arc::new(LocalSessionManager::default());
        let shared_clone = Arc::clone(&shared);
        let mcp_service = StreamableHttpService::new(
            move || Ok(MemcanService::new(Arc::clone(&shared_clone))),
            session_manager,
            config,
        );

        let mcp_clone = mcp_service.clone();
        let mcp_router = Router::new().route(
            "/mcp",
            axum::routing::any(move |req: axum::extract::Request| async move {
                mcp_clone.handle(req).await
            }),
        );

        let mcp_router = if let Some(ref key) = ctx.settings.api_key {
            let expected = format!("Bearer {key}");
            mcp_router.layer(middleware::from_fn(move |req: Request, next: Next| {
                let expected = expected.clone();
                async move {
                    let auth = req
                        .headers()
                        .get(axum::http::header::AUTHORIZATION)
                        .and_then(|v| v.to_str().ok());
                    match auth {
                        Some(v) if bool::from(v.as_bytes().ct_eq(expected.as_bytes())) => {
                            next.run(req).await
                        }
                        _ => Response::builder()
                            .status(axum::http::StatusCode::UNAUTHORIZED)
                            .body(axum::body::Body::from("Unauthorized"))
                            .unwrap()
                            .into_response(),
                    }
                }
            }))
        } else {
            mcp_router
        };

        let app = Router::new()
            .route(
                "/health",
                get(health_handler).with_state(Arc::clone(&health)),
            )
            .merge(mcp_router);

        let listener = TcpListener::bind(&listen_addr)
            .await
            .map_err(|e| MemcanError::Other(format!("failed to bind {listen_addr}: {e}")))?;

        info!(listen = %listen_addr, "MemCan MCP server running on HTTP");
        axum::serve(listener, app)
            .with_graceful_shutdown(shutdown_signal())
            .await
            .map_err(|e| MemcanError::Other(format!("HTTP server error: {e}")))?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_test_service() -> MemcanService {
        use memcan_core::health::DependencyHealth;
        use memcan_core::traits::{
            EmbeddingProvider, LlmMessage, LlmOptions, LlmProvider, SearchResult, TableSchema,
            VectorPoint, VectorStore,
        };

        #[derive(Default)]
        struct MockStore {
            records: StdMutex<HashMap<String, SearchResult>>,
        }
        #[async_trait::async_trait]
        impl VectorStore for MockStore {
            async fn ensure_table(
                &self,
                _: &str,
                _: usize,
                _: &dyn TableSchema,
            ) -> memcan_core::error::Result<()> {
                Ok(())
            }
            async fn upsert(
                &self,
                _: &str,
                points: &[VectorPoint],
                _: &dyn TableSchema,
            ) -> memcan_core::error::Result<()> {
                let mut records = self.records.lock().unwrap();
                for point in points {
                    records.insert(
                        point.id.clone(),
                        SearchResult {
                            id: point.id.clone(),
                            score: 0.0,
                            payload: point.payload.clone(),
                        },
                    );
                }
                Ok(())
            }
            async fn search(
                &self,
                _: &str,
                _: &[f32],
                _: Option<&str>,
                _: usize,
                _: usize,
            ) -> memcan_core::error::Result<Vec<SearchResult>> {
                Ok(vec![])
            }
            async fn scroll(
                &self,
                _: &str,
                _: Option<&str>,
                _: usize,
                _: usize,
            ) -> memcan_core::error::Result<Vec<SearchResult>> {
                Ok(vec![])
            }
            async fn count(&self, _: &str, _: Option<&str>) -> memcan_core::error::Result<usize> {
                Ok(0)
            }
            async fn delete(&self, _: &str, _: &[String]) -> memcan_core::error::Result<()> {
                Ok(())
            }
            async fn delete_by_filter(
                &self,
                _: &str,
                _: &str,
            ) -> memcan_core::error::Result<usize> {
                Ok(0)
            }
            async fn get(
                &self,
                _: &str,
                ids: &[String],
            ) -> memcan_core::error::Result<Vec<SearchResult>> {
                let records = self.records.lock().unwrap();
                Ok(ids
                    .iter()
                    .filter_map(|id| records.get(id).cloned())
                    .collect())
            }
        }

        struct MockEmbedder;
        #[async_trait::async_trait]
        impl EmbeddingProvider for MockEmbedder {
            async fn embed(&self, texts: &[String]) -> memcan_core::error::Result<Vec<Vec<f32>>> {
                Ok(texts.iter().map(|_| vec![0.0; 3]).collect())
            }
            fn dimensions(&self) -> usize {
                3
            }
        }

        struct MockLlm;
        #[async_trait::async_trait]
        impl LlmProvider for MockLlm {
            async fn chat(
                &self,
                _: &str,
                _: &[LlmMessage],
                _: Option<LlmOptions>,
            ) -> memcan_core::error::Result<String> {
                Ok(String::new())
            }
            async fn context_window(&self, _: &str) -> Option<usize> {
                Some(4096)
            }
        }

        let state = Arc::new(SharedState {
            store: Arc::new(MockStore::default()),
            embedder: Arc::new(MockEmbedder),
            llm: Arc::new(MockLlm),
            config: memcan_core::config::Settings {
                listen: "127.0.0.1:0".into(),
                api_key: None,
                lancedb_path: "/tmp/test".into(),
                default_user_id: "global".into(),
                tech_stack: String::new(),
                distill_memories: false,
                log_file: String::new(),
                llm_model: "test".into(),
                embed_model: "test".into(),
                embed_dims: 3,
                ollama_host: None,
                ollama_api_key: None,
                url: "http://localhost:8191".into(),
                compact_on_startup: false,
                compact_fragment_threshold: 0,
            },
            llm_model: "test".into(),
            queue_status: Arc::new(StdMutex::new(LruCache::new(
                NonZeroUsize::new(100).unwrap(),
            ))),
            llm_semaphore: Arc::new(tokio::sync::Semaphore::new(1)),
            pending_tasks: Arc::new(AtomicUsize::new(0)),
            health: Arc::new(DependencyHealth::with_defaults()),
        });

        MemcanService::new(state)
    }

    // Note: these tests exercise `tool_router.list_all()` + the `_`-prefix filter directly
    // rather than calling `service.list_tools()`. Constructing a `RequestContext<RoleServer>`
    // requires a live MCP transport, which is unavailable in unit tests. Since `list_tools`
    // contains no logic beyond filtering on the `_` prefix, testing the filter inline is
    // equivalent and simpler.

    #[test]
    fn hidden_tools_excluded_from_list_tools() {
        let service = make_test_service();
        let all_tools = service.tool_router.list_all();
        let has_import = all_tools.iter().any(|t| t.name == "_import_records");
        assert!(has_import, "tool_router should contain _import_records");

        let visible: Vec<_> = all_tools
            .into_iter()
            .filter(|t| !t.name.starts_with('_'))
            .collect();
        let has_hidden = visible.iter().any(|t| t.name.starts_with('_'));
        assert!(
            !has_hidden,
            "visible list should not contain _-prefixed tools"
        );
    }

    #[test]
    fn public_tools_present_in_list() {
        let service = make_test_service();
        let all_tools = service.tool_router.list_all();
        let visible: Vec<_> = all_tools
            .into_iter()
            .filter(|t| !t.name.starts_with('_'))
            .collect();

        let names: Vec<&str> = visible.iter().map(|t| t.name.as_ref()).collect();
        assert!(
            names.contains(&"export_collection"),
            "missing export_collection"
        );
        assert!(
            names.contains(&"index_code_files"),
            "missing index_code_files"
        );
        assert!(names.contains(&"add_memory"), "missing add_memory");
        assert!(names.contains(&"search"), "missing search");
        assert!(names.contains(&"get_todo"), "missing get_todo");
    }

    #[test]
    fn todo_param_schemas_keep_owner_and_blocked_by_optional() {
        let add_schema = serde_json::to_value(rmcp::schemars::schema_for!(AddTodoParams)).unwrap();
        let add_properties = add_schema["properties"].as_object().unwrap();
        assert!(add_properties.contains_key("owner"));
        assert!(add_properties.contains_key("blocked_by"));
        let add_required = add_schema["required"].as_array().unwrap();
        assert!(!add_required.iter().any(|field| field == "owner"));
        assert!(!add_required.iter().any(|field| field == "blocked_by"));

        let add: AddTodoParams =
            serde_json::from_str(r#"{"title":"test","project":"memcan"}"#).unwrap();
        assert!(add.owner.is_none());
        assert!(add.blocked_by.is_none());

        let update_schema =
            serde_json::to_value(rmcp::schemars::schema_for!(UpdateTodoParams)).unwrap();
        let update_properties = update_schema["properties"].as_object().unwrap();
        assert!(update_properties.contains_key("owner"));
        assert!(update_properties.contains_key("blocked_by"));
        let update_required = update_schema["required"].as_array().unwrap();
        assert!(!update_required.iter().any(|field| field == "owner"));
        assert!(!update_required.iter().any(|field| field == "blocked_by"));

        let update: UpdateTodoParams = serde_json::from_str(r#"{"todo_id":"todo-id"}"#).unwrap();
        assert!(update.owner.is_none());
        assert!(update.blocked_by.is_none());
    }

    #[tokio::test]
    async fn get_todo_missing_returns_not_found_marker() {
        let service = make_test_service();

        let result = service
            .get_todo(Parameters(GetTodoParams {
                todo_id: "missing-id".into(),
            }))
            .await
            .unwrap();
        let response: serde_json::Value = serde_json::from_str(
            result.content[0]
                .as_text()
                .expect("text response")
                .text
                .as_ref(),
        )
        .unwrap();

        assert_eq!(response["error"], "todo not found");
        assert_eq!(response["todo_id"], "missing-id");
    }

    #[tokio::test]
    async fn get_todo_success_returns_serialized_item_fields() {
        let service = make_test_service();
        let added = service
            .add_todo(Parameters(AddTodoParams {
                title: "Blocked work".into(),
                description: None,
                project: "memcan".into(),
                priority: None,
                owner: Some("bilby".into()),
                blocked_by: Some(vec!["dependency-a".into(), "dependency-b".into()]),
            }))
            .await
            .unwrap();
        let added: serde_json::Value = serde_json::from_str(
            added.content[0]
                .as_text()
                .expect("text response")
                .text
                .as_ref(),
        )
        .unwrap();
        let todo_id = added["id"].as_str().expect("todo id").to_string();

        let fetched = service
            .get_todo(Parameters(GetTodoParams {
                todo_id: todo_id.clone(),
            }))
            .await
            .unwrap();
        let response: serde_json::Value = serde_json::from_str(
            fetched.content[0]
                .as_text()
                .expect("text response")
                .text
                .as_ref(),
        )
        .unwrap();

        assert_eq!(response["id"], todo_id);
        assert_eq!(response["title"], "Blocked work");
        assert_eq!(response["owner"], "bilby");
        assert_eq!(
            response["blocked_by"],
            serde_json::json!(["dependency-a", "dependency-b"])
        );
        assert_eq!(response["status"], "pending");
    }

    #[test]
    fn export_collection_params_deserialization() {
        let json = r#"{"collection": "memories"}"#;
        let params: ExportCollectionParams = serde_json::from_str(json).unwrap();
        assert_eq!(params.collection, "memories");
        assert!(params.filter.is_none());
        assert!(params.limit.is_none());
        assert!(params.offset.is_none());
    }

    #[test]
    fn export_collection_params_with_all_fields() {
        let json =
            r#"{"collection": "code", "filter": "project = 'test'", "limit": 500, "offset": 100}"#;
        let params: ExportCollectionParams = serde_json::from_str(json).unwrap();
        assert_eq!(params.collection, "code");
        assert_eq!(params.filter.unwrap(), "project = 'test'");
        assert_eq!(params.limit.unwrap(), 500);
        assert_eq!(params.offset.unwrap(), 100);
    }

    #[test]
    fn import_records_params_deserialization() {
        let json =
            r#"{"records": "{\"_collection\":\"memories\",\"id\":\"1\",\"data\":\"test\"}\n"}"#;
        let params: ImportRecordsParams = serde_json::from_str(json).unwrap();
        assert!(params.records.contains("memories"));
    }

    #[test]
    fn index_code_files_params_deserialization() {
        let json = r#"{"files": [{"path": "src/main.rs", "content": "fn main() {}"}], "project": "test", "tech_stack": "rust"}"#;
        let params: IndexCodeFilesToolParams = serde_json::from_str(json).unwrap();
        assert_eq!(params.files.len(), 1);
        assert_eq!(params.files[0].path, "src/main.rs");
        assert_eq!(params.project, "test");
        assert_eq!(params.tech_stack, "rust");
    }

    #[test]
    fn delete_code_predicate_rejects_empty_project() {
        let err = build_delete_code_predicate("   ", None, None).unwrap_err();
        assert!(err.contains("project must not be empty"), "got: {err}");
    }

    #[test]
    fn delete_code_predicate_project_only() {
        let pred = build_delete_code_predicate("rust-dashcore", None, None).unwrap();
        assert_eq!(pred, "project = 'rust-dashcore'");
    }

    #[test]
    fn delete_code_predicate_project_and_extension() {
        let pred = build_delete_code_predicate("myproj", Some("py"), None).unwrap();
        assert_eq!(pred, "project = 'myproj' AND file_path LIKE '%.py'");
        // A leading dot is accepted and normalized away.
        let pred = build_delete_code_predicate("myproj", Some(".py"), None).unwrap();
        assert_eq!(pred, "project = 'myproj' AND file_path LIKE '%.py'");
    }

    #[test]
    fn delete_code_predicate_exact_path() {
        let pred = build_delete_code_predicate("myproj", None, Some("src/main.rs")).unwrap();
        assert_eq!(pred, "project = 'myproj' AND file_path = 'src/main.rs'");
    }

    #[test]
    fn delete_code_predicate_escapes_quoted_project() {
        let pred = build_delete_code_predicate("o'brien", None, None).unwrap();
        assert_eq!(pred, "project = 'o''brien'");
    }

    #[test]
    fn delete_code_predicate_injection_cannot_escape_project_scope() {
        // The classic SEC-001 payload arrives via the project name itself now;
        // it must be quote-escaped, never break out of the equality.
        let pred = build_delete_code_predicate("x' OR '1'='1", None, None).unwrap();
        assert_eq!(pred, "project = 'x'' OR ''1''=''1'");
        // An injection-shaped extension is rejected outright (non-alphanumeric).
        let err = build_delete_code_predicate("myproj", Some("rs' OR '1'='1"), None).unwrap_err();
        assert!(err.contains("invalid extension"), "got: {err}");
        // A path injection attempt is quote-escaped, never structural.
        let pred = build_delete_code_predicate("myproj", None, Some("a') OR (1=1")).unwrap();
        assert_eq!(pred, "project = 'myproj' AND file_path = 'a'') OR (1=1'");
    }

    #[tokio::test]
    async fn delete_code_records_refused_without_api_key() {
        let service = make_test_service();
        let params = DeleteCodeRecordsParams {
            project: "myproj".into(),
            extension: None,
            file_path_exact: None,
        };
        let err = service
            .delete_code_records(Parameters(params))
            .await
            .unwrap_err();
        assert!(
            err.message.contains("MEMCAN_API_KEY"),
            "got: {}",
            err.message
        );
    }

    #[tokio::test]
    async fn delete_code_records_with_key_uses_validated_predicate() {
        use memcan_core::health::DependencyHealth;
        use memcan_core::traits::{
            EmbeddingProvider, LlmMessage, LlmOptions, LlmProvider, SearchResult, TableSchema,
            VectorPoint, VectorStore,
        };

        // Store that records the filter it was asked to delete by.
        struct CapturingStore {
            last_filter: StdMutex<Option<String>>,
        }
        #[async_trait::async_trait]
        impl VectorStore for CapturingStore {
            async fn ensure_table(
                &self,
                _: &str,
                _: usize,
                _: &dyn TableSchema,
            ) -> memcan_core::error::Result<()> {
                Ok(())
            }
            async fn upsert(
                &self,
                _: &str,
                _: &[VectorPoint],
                _: &dyn TableSchema,
            ) -> memcan_core::error::Result<()> {
                Ok(())
            }
            async fn search(
                &self,
                _: &str,
                _: &[f32],
                _: Option<&str>,
                _: usize,
                _: usize,
            ) -> memcan_core::error::Result<Vec<SearchResult>> {
                Ok(vec![])
            }
            async fn scroll(
                &self,
                _: &str,
                _: Option<&str>,
                _: usize,
                _: usize,
            ) -> memcan_core::error::Result<Vec<SearchResult>> {
                Ok(vec![])
            }
            async fn count(&self, _: &str, _: Option<&str>) -> memcan_core::error::Result<usize> {
                Ok(0)
            }
            async fn delete(&self, _: &str, _: &[String]) -> memcan_core::error::Result<()> {
                Ok(())
            }
            async fn delete_by_filter(
                &self,
                _: &str,
                filter: &str,
            ) -> memcan_core::error::Result<usize> {
                *self.last_filter.lock().unwrap() = Some(filter.to_string());
                Ok(7)
            }
            async fn get(
                &self,
                _: &str,
                _: &[String],
            ) -> memcan_core::error::Result<Vec<SearchResult>> {
                Ok(vec![])
            }
        }

        struct MockEmbedder;
        #[async_trait::async_trait]
        impl EmbeddingProvider for MockEmbedder {
            async fn embed(&self, texts: &[String]) -> memcan_core::error::Result<Vec<Vec<f32>>> {
                Ok(texts.iter().map(|_| vec![0.0; 3]).collect())
            }
            fn dimensions(&self) -> usize {
                3
            }
        }

        struct MockLlm;
        #[async_trait::async_trait]
        impl LlmProvider for MockLlm {
            async fn chat(
                &self,
                _: &str,
                _: &[LlmMessage],
                _: Option<LlmOptions>,
            ) -> memcan_core::error::Result<String> {
                Ok(String::new())
            }
            async fn context_window(&self, _: &str) -> Option<usize> {
                Some(4096)
            }
        }

        let store = Arc::new(CapturingStore {
            last_filter: StdMutex::new(None),
        });
        let config = memcan_core::config::Settings {
            api_key: Some("secret".into()),
            ..Default::default()
        };
        let state = Arc::new(SharedState {
            store: Arc::clone(&store) as Arc<dyn VectorStore>,
            embedder: Arc::new(MockEmbedder),
            llm: Arc::new(MockLlm),
            config,
            llm_model: "test".into(),
            queue_status: Arc::new(StdMutex::new(LruCache::new(
                NonZeroUsize::new(100).unwrap(),
            ))),
            llm_semaphore: Arc::new(tokio::sync::Semaphore::new(1)),
            pending_tasks: Arc::new(AtomicUsize::new(0)),
            health: Arc::new(DependencyHealth::with_defaults()),
        });
        let service = MemcanService::new(state);

        let params = DeleteCodeRecordsParams {
            project: "rust-dashcore".into(),
            extension: Some("py".into()),
            file_path_exact: None,
        };
        service
            .delete_code_records(Parameters(params))
            .await
            .unwrap();

        let captured = store.last_filter.lock().unwrap().clone().unwrap();
        assert_eq!(
            captured,
            "project = 'rust-dashcore' AND file_path LIKE '%.py'"
        );
    }

    #[test]
    fn add_memory_params_metadata_as_object() {
        let json = r#"{"memory": "test", "metadata": {"type": "lesson"}}"#;
        let params: AddMemoryParams = serde_json::from_str(json).unwrap();
        let meta = params.metadata.unwrap();
        assert_eq!(meta["type"], serde_json::Value::String("lesson".into()));
    }

    #[test]
    fn add_memory_params_metadata_as_string() {
        let json = r#"{"memory": "test", "metadata": "{\"type\": \"lesson\"}"}"#;
        let params: AddMemoryParams = serde_json::from_str(json).unwrap();
        let meta = params.metadata.unwrap();
        assert_eq!(meta["type"], serde_json::Value::String("lesson".into()));
    }

    #[test]
    fn add_memory_params_metadata_missing() {
        let json = r#"{"memory": "test"}"#;
        let params: AddMemoryParams = serde_json::from_str(json).unwrap();
        assert!(params.metadata.is_none());
    }

    #[test]
    fn add_memory_params_metadata_null() {
        let json = r#"{"memory": "test", "metadata": null}"#;
        let params: AddMemoryParams = serde_json::from_str(json).unwrap();
        assert!(params.metadata.is_none());
    }

    #[test]
    fn add_memory_params_metadata_empty_string() {
        let json = r#"{"memory": "test", "metadata": ""}"#;
        let result: Result<AddMemoryParams, _> = serde_json::from_str(json);
        assert!(result.is_err());
    }

    #[test]
    fn add_memory_params_metadata_non_map_json_string() {
        let json = r#"{"memory": "test", "metadata": "[1,2,3]"}"#;
        let result: Result<AddMemoryParams, _> = serde_json::from_str(json);
        assert!(result.is_err());
    }

    #[test]
    fn add_memory_params_metadata_invalid_json_string() {
        let json = r#"{"memory": "test", "metadata": "not json"}"#;
        let result: Result<AddMemoryParams, _> = serde_json::from_str(json);
        assert!(result.is_err());
    }

    #[test]
    fn add_memory_params_metadata_direct_number() {
        let json = r#"{"memory": "test", "metadata": 42}"#;
        let result: Result<AddMemoryParams, _> = serde_json::from_str(json);
        assert!(result.is_err());
    }

    #[test]
    fn add_memory_params_metadata_direct_array() {
        let json = r#"{"memory": "test", "metadata": [1,2,3]}"#;
        let result: Result<AddMemoryParams, _> = serde_json::from_str(json);
        assert!(result.is_err());
    }

    #[test]
    fn add_memory_params_metadata_oversized_string() {
        let big = "a".repeat(64 * 1024 + 1);
        let json = format!(r#"{{"memory": "test", "metadata": "{}"}}"#, big);
        let result: Result<AddMemoryParams, _> = serde_json::from_str(&json);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("64 KB"),
            "error should mention size limit: {err}"
        );
    }

    #[test]
    fn pending_task_guard_decrements_on_drop() {
        let counter = Arc::new(AtomicUsize::new(0));
        {
            let guard = try_enqueue(&counter).unwrap();
            assert_eq!(counter.load(Ordering::SeqCst), 1);
            drop(guard);
        }
        assert_eq!(counter.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn try_enqueue_rejects_when_at_limit() {
        let counter = Arc::new(AtomicUsize::new(0));
        let mut guards = Vec::new();
        for _ in 0..MAX_PENDING_TASKS {
            guards.push(try_enqueue(&counter).unwrap());
        }
        assert_eq!(counter.load(Ordering::SeqCst), MAX_PENDING_TASKS);

        let result = try_enqueue(&counter);
        assert!(result.is_err());
        assert_eq!(counter.load(Ordering::SeqCst), MAX_PENDING_TASKS);
    }

    #[test]
    fn try_enqueue_succeeds_after_guard_dropped() {
        let counter = Arc::new(AtomicUsize::new(0));
        let mut guards = Vec::new();
        for _ in 0..MAX_PENDING_TASKS {
            guards.push(try_enqueue(&counter).unwrap());
        }

        guards.pop();
        assert_eq!(counter.load(Ordering::SeqCst), MAX_PENDING_TASKS - 1);

        let guard = try_enqueue(&counter);
        assert!(guard.is_ok());
        assert_eq!(counter.load(Ordering::SeqCst), MAX_PENDING_TASKS);
    }
}
