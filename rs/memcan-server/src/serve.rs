//! MCP server — HTTP + stdio transport with /health endpoint.
//!
//! Dual transport: `--stdio` for backward compat, default is HTTP via axum.

use std::collections::HashMap;
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
    handler::server::wrapper::Parameters,
    model::*,
    schemars, tool, tool_handler, tool_router,
    transport::io::stdio,
    transport::streamable_http_server::{
        session::local::LocalSessionManager,
        tower::{StreamableHttpServerConfig, StreamableHttpService},
    },
};
use serde::Deserialize;
use subtle::ConstantTimeEq;
use tokio::net::TcpListener;
use tracing::info;
use uuid::Uuid;

use memcan_core::{
    config::Settings,
    error::MemcanError,
    indexing::standards::{self as standards_indexing, VALID_TYPES},
    init::{MemcanContext, create_llm_provider},
    pipeline::{
        CODE_TABLE, MEMORIES_TABLE, Pipeline, PipelineGuard, PipelineProgress, STANDARDS_TABLE,
    },
    prompts::FACT_EXTRACTION_HOOK_PROMPT,
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

struct SharedState {
    store: Arc<dyn VectorStore>,
    embedder: Arc<dyn EmbeddingProvider>,
    llm: Arc<dyn LlmProvider>,
    config: Settings,
    llm_model: String,
    queue_status: Arc<StdMutex<LruCache<String, QueueEntry>>>,
    llm_semaphore: Arc<tokio::sync::Semaphore>,
    pending_tasks: Arc<AtomicUsize>,
}

// --- Tool parameter structs ---

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct AddMemoryParams {
    pub memory: String,
    pub project: Option<String>,
    pub user_id: Option<String>,
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

// --- Helpers ---

fn resolve_user_id(project: &Option<String>, user_id: &Option<String>, default: &str) -> String {
    if let Some(uid) = user_id {
        return uid.clone();
    }
    if let Some(proj) = project {
        return format!("project:{proj}");
    }
    default.to_string()
}

fn sanitize_eq(s: &str) -> String {
    s.replace('\'', "''")
}

fn sanitize_like(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('\'', "''")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

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

        let op_id = Uuid::new_v4().to_string();
        info!(user_id = %uid, len = memory.len(), operation_id = %op_id, "add_memory: queued");

        let is_hook = metadata.get("source").and_then(|v| v.as_str()) == Some("auto-hook");

        let pipeline = Pipeline::new(
            Arc::clone(&self.state.store),
            Arc::clone(&self.state.embedder),
            Arc::clone(&self.state.llm),
            self.state.llm_model.clone(),
            MEMORIES_TABLE,
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

        let task_guard = try_enqueue(&self.state.pending_tasks)?;
        let sem = Arc::clone(&self.state.llm_semaphore);
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
            .search(MEMORIES_TABLE, &vectors[0], Some(&filter), limit, 0)
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

    #[tool(description = "Update an existing memory's content.")]
    async fn update_memory(
        &self,
        Parameters(params): Parameters<UpdateMemoryParams>,
    ) -> Result<CallToolResult, ErrorData> {
        info!(memory_id = %params.memory_id, "update_memory");

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

        let op_id = Uuid::new_v4().to_string();

        let pipeline = Pipeline::new(
            Arc::clone(&self.state.store),
            Arc::clone(&self.state.embedder),
            Arc::clone(&self.state.llm),
            self.state.llm_model.clone(),
            MEMORIES_TABLE,
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

        let task_guard = try_enqueue(&self.state.pending_tasks)?;
        let sem = Arc::clone(&self.state.llm_semaphore);
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
                    guard.complete();
                }
                Err(e) => {
                    tracing::error!(memory_id = %memory_id, error = %e, "update_memory: failed");
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
        let known_tables = [MEMORIES_TABLE, STANDARDS_TABLE, CODE_TABLE];
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
        description = "Index a markdown standards document into the vector store. Returns an operation_id for progress tracking via get_queue_status."
    )]
    async fn index_standards(
        &self,
        Parameters(params): Parameters<IndexStandardsToolParams>,
    ) -> Result<CallToolResult, ErrorData> {
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

        let task_guard = try_enqueue(&self.state.pending_tasks)?;
        let store = Arc::clone(&self.state.store);
        let embedder = Arc::clone(&self.state.embedder);
        let llm = Arc::clone(&self.state.llm);
        let llm_model = self.state.llm_model.clone();
        let embed_dims = self.state.config.embed_dims;
        let sem = Arc::clone(&self.state.llm_semaphore);

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
        if params.standard_id.is_empty() {
            return Err(ErrorData::internal_error(
                "standard_id must not be empty",
                None,
            ));
        }

        info!(standard_id = %params.standard_id, "drop_indexed_standards");

        let deleted = standards_indexing::drop_standards(
            &params.standard_id,
            self.state.store.as_ref(),
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
        description = "Check status of queued async operations (add_memory, update_memory). Returns recent operations or a specific one by ID."
    )]
    async fn get_queue_status(
        &self,
        Parameters(params): Parameters<GetQueueStatusParams>,
    ) -> Result<CallToolResult, ErrorData> {
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
}

// --- ServerHandler ---

#[tool_handler]
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
}

// --- Logging ---

fn setup_logging(log_file: &str) {
    use tracing_subscriber::EnvFilter;

    if log_file.is_empty() {
        tracing_subscriber::fmt()
            .with_writer(std::io::stderr)
            .with_ansi(false)
            .with_env_filter(
                EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
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
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("debug")),
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

async fn health_handler() -> impl IntoResponse {
    axum::response::Json(serde_json::json!({"status": "ok"}))
}

// --- Entry point ---

pub async fn run(args: &ServeArgs) -> Result<(), MemcanError> {
    let ctx = MemcanContext::init().await?;
    ctx.init_llm().await?;
    setup_logging(&ctx.settings.log_file);

    info!("Loading config: lancedb_path={}", ctx.settings.lancedb_path);

    let (llm, llm_model) = create_llm_provider(&ctx.settings);

    let dims = ctx.settings.embed_dims;
    ctx.store.ensure_table(MEMORIES_TABLE, dims).await?;
    ctx.store.ensure_table(STANDARDS_TABLE, dims).await?;
    ctx.store.ensure_table(CODE_TABLE, dims).await?;

    info!("Tables ensured: {MEMORIES_TABLE}, {STANDARDS_TABLE}, {CODE_TABLE}");

    let listen_addr = args
        .listen
        .clone()
        .unwrap_or_else(|| ctx.settings.listen.clone());

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
            .route("/health", get(health_handler))
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
