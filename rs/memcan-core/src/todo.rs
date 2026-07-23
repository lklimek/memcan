//! Per-project TODO list CRUD operations.
//!
//! TODOs persist across sessions, are project-scoped, and searchable via
//! unified search. Stored in LanceDB with embeddings for semantic search.

use std::collections::{HashSet, hash_map::DefaultHasher};
use std::hash::{Hash, Hasher};

use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

use crate::error::Result;
use crate::query::{sanitize_eq, sanitize_like};
use crate::traits::{EmbeddingProvider, SearchResult, TableSchema, VectorPoint, VectorStore};

pub const TODOS_TABLE: &str = "memcan_todos";

const VALID_PRIORITIES: &[&str] = &["low", "medium", "high"];
const VALID_STATUSES: &[&str] = &[
    "pending",
    "in_progress",
    "blocked",
    "done",
    "postponed",
    "cancelled",
];
const MAX_LIST_ALL_TODOS_SCAN: usize = 10_000;
const MIN_ID_PREFIX_LEN: usize = 8;
const MAX_ID_PREFIX_MATCHES: usize = 5;

const TODO_WRITE_LOCK_SHARDS: usize = 64;

/// Process-local sharded locks for TODO read-modify-write operations.
#[derive(Debug)]
pub struct TodoWriteLocks {
    locks: [tokio::sync::Mutex<()>; TODO_WRITE_LOCK_SHARDS],
}

impl Default for TodoWriteLocks {
    fn default() -> Self {
        Self {
            locks: std::array::from_fn(|_| tokio::sync::Mutex::new(())),
        }
    }
}

impl TodoWriteLocks {
    async fn lock(&self, todo_id: &str) -> tokio::sync::MutexGuard<'_, ()> {
        let mut hasher = DefaultHasher::new();
        todo_id.hash(&mut hasher);
        let index = hasher.finish() as usize % TODO_WRITE_LOCK_SHARDS;
        self.locks[index].lock().await
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct AddTodoParams {
    pub title: String,
    pub description: Option<String>,
    pub project: String,
    pub priority: Option<String>,
    pub owner: Option<String>,
    pub blocked_by: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TodoItem {
    pub id: String,
    pub title: String,
    pub description: Option<String>,
    pub project: String,
    pub priority: String,
    pub status: String,
    pub owner: Option<String>,
    #[serde(default)]
    pub blocked_by: Vec<String>,
    pub created_at: String,
    /// Timestamp when the task first reached a terminal state (`done` or `cancelled`).
    /// A terminal-to-terminal change (e.g. `cancelled` to `done`) keeps the earlier timestamp.
    pub completed_at: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct UpdateTodoFields {
    pub title: Option<String>,
    pub description: Option<String>,
    pub priority: Option<String>,
    pub status: Option<String>,
    pub owner: Option<String>,
    pub blocked_by: Option<Vec<String>>,
}

pub fn validate_priority(p: &str) -> Result<()> {
    if !VALID_PRIORITIES.contains(&p) {
        return Err(crate::error::MemcanError::Other(format!(
            "invalid priority '{}', must be one of: {}",
            p,
            VALID_PRIORITIES.join(", ")
        )));
    }
    Ok(())
}

pub fn validate_status(s: &str) -> Result<()> {
    if !valid_statuses().contains(&s) {
        return Err(crate::error::MemcanError::Other(format!(
            "invalid status '{}', must be one of: {}",
            s,
            valid_statuses().join(", ")
        )));
    }
    Ok(())
}

/// Return the canonical TODO status vocabulary in display order.
pub fn valid_statuses() -> &'static [&'static str] {
    VALID_STATUSES
}

fn is_terminal(status: &str) -> bool {
    matches!(status, "done" | "cancelled")
}

fn normalize_optional_text(value: String) -> Option<String> {
    let value = value.trim().to_string();
    (!value.is_empty()).then_some(value)
}

fn normalize_id_list(values: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    values
        .into_iter()
        .filter_map(normalize_optional_text)
        .filter(|value| seen.insert(value.clone()))
        .collect()
}

async fn resolve_todo_prefix(store: &dyn VectorStore, todo_id: &str) -> Result<Option<String>> {
    if todo_id.chars().count() < MIN_ID_PREFIX_LEN {
        return Ok(None);
    }

    let filter = format!("id LIKE '{}%'", sanitize_like(todo_id));
    let candidates = store
        .scroll(TODOS_TABLE, Some(&filter), MAX_ID_PREFIX_MATCHES, 0)
        .await?;
    let mut candidate_ids: Vec<_> = candidates
        .into_iter()
        .map(|candidate| candidate.id)
        .collect();
    candidate_ids.sort();
    candidate_ids.dedup();

    match candidate_ids.len() {
        0 => Ok(None),
        1 => Ok(candidate_ids.pop()),
        count => Err(crate::error::MemcanError::Other(format!(
            "ambiguous todo id prefix '{todo_id}' matches {count} todos ({}) — provide the full UUID",
            candidate_ids.join(", ")
        ))),
    }
}

async fn resolve_todo_id(store: &dyn VectorStore, todo_id: &str) -> Result<Option<String>> {
    let exact = store.get(TODOS_TABLE, &[todo_id.to_string()]).await?;
    if let Some(item) = exact.first() {
        return Ok(Some(item.id.clone()));
    }

    resolve_todo_prefix(store, todo_id).await
}

async fn resolve_blocked_by(
    store: &dyn VectorStore,
    blocked_by: Vec<String>,
) -> Result<Vec<String>> {
    let mut resolved = Vec::new();
    for reference in blocked_by.into_iter().filter_map(normalize_optional_text) {
        match resolve_todo_id(store, &reference).await {
            Ok(Some(todo_id)) => resolved.push(todo_id),
            Ok(None) => {
                return Err(crate::error::MemcanError::Other(format!(
                    "blocked_by reference '{reference}' not found"
                )));
            }
            Err(crate::error::MemcanError::Other(message))
                if message.starts_with("ambiguous todo id prefix") =>
            {
                return Err(crate::error::MemcanError::Other(format!(
                    "blocked_by reference '{reference}' could not be resolved: {message}"
                )));
            }
            Err(error) => return Err(error),
        }
    }
    Ok(normalize_id_list(resolved))
}

fn build_data(title: &str, description: Option<&str>) -> String {
    match description {
        Some(d) if !d.is_empty() => format!("{title}\n{d}"),
        _ => title.to_string(),
    }
}

fn build_payload(item: &TodoItem) -> serde_json::Value {
    json!({
        "id": item.id,
        "data": build_data(&item.title, item.description.as_deref()),
        "title": item.title,
        "description": item.description,
        "project": item.project,
        "priority": item.priority,
        "status": item.status,
        "owner": item.owner,
        "blocked_by": item.blocked_by,
        "created_at": item.created_at,
        "completed_at": item.completed_at,
        "collection": "todos",
    })
}

fn parse_todo(r: &SearchResult) -> TodoItem {
    let p = &r.payload;
    TodoItem {
        id: r.id.clone(),
        title: p
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        description: p
            .get("description")
            .and_then(|v| v.as_str())
            .map(String::from),
        project: p
            .get("project")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        priority: p
            .get("priority")
            .and_then(|v| v.as_str())
            .unwrap_or("medium")
            .to_string(),
        status: p
            .get("status")
            .and_then(|v| v.as_str())
            .unwrap_or("pending")
            .to_string(),
        owner: p.get("owner").and_then(|v| v.as_str()).map(String::from),
        blocked_by: p
            .get("blocked_by")
            .and_then(|v| v.as_array())
            .map(|values| {
                values
                    .iter()
                    .filter_map(|value| value.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default(),
        created_at: p
            .get("created_at")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        completed_at: p
            .get("completed_at")
            .and_then(|v| v.as_str())
            .map(String::from),
    }
}

/// Return the default display rank for a TODO priority.
pub fn priority_rank(p: &str) -> u8 {
    match p {
        "high" => 0,
        "medium" => 1,
        "low" => 2,
        _ => 3,
    }
}

pub async fn add_todo(
    store: &dyn VectorStore,
    embedder: &dyn EmbeddingProvider,
    table_schema: &dyn TableSchema,
    params: AddTodoParams,
) -> Result<TodoItem> {
    let priority = params.priority.as_deref().unwrap_or("medium");
    validate_priority(priority)?;
    let blocked_by = resolve_blocked_by(store, params.blocked_by.unwrap_or_default()).await?;

    let item = TodoItem {
        id: Uuid::new_v4().to_string(),
        title: params.title,
        description: params.description.and_then(normalize_optional_text),
        project: params.project,
        priority: priority.to_string(),
        status: "pending".to_string(),
        owner: params.owner.and_then(normalize_optional_text),
        blocked_by,
        created_at: Utc::now().to_rfc3339(),
        completed_at: None,
    };

    let data = build_data(&item.title, item.description.as_deref());
    let vectors = embedder.embed(&[data]).await?;
    let payload = build_payload(&item);

    let point = VectorPoint {
        id: item.id.clone(),
        vector: vectors[0].clone(),
        payload,
    };
    store.upsert(TODOS_TABLE, &[point], table_schema).await?;
    Ok(item)
}

pub async fn list_todos(
    store: &dyn VectorStore,
    project: &str,
    status_filter: Option<&str>,
    owner_filter: Option<&str>,
    limit: usize,
) -> Result<Vec<TodoItem>> {
    if let Some(s) = status_filter {
        validate_status(s)?;
    }
    let owner_filter = owner_filter
        .map(str::to_owned)
        .and_then(normalize_optional_text);

    let safe_project = sanitize_eq(project);
    let mut filter = format!("project = '{safe_project}'");
    if let Some(status) = status_filter {
        let safe_status = sanitize_eq(status);
        filter.push_str(&format!(" AND status = '{safe_status}'"));
    }
    if let Some(owner) = owner_filter.as_deref() {
        let safe_owner = sanitize_eq(owner);
        filter.push_str(&format!(" AND owner = '{safe_owner}'"));
    }

    let results = store.scroll(TODOS_TABLE, Some(&filter), limit, 0).await?;

    let mut todos: Vec<TodoItem> = results.iter().map(parse_todo).collect();
    todos.sort_by(|a, b| {
        priority_rank(&a.priority)
            .cmp(&priority_rank(&b.priority))
            .then_with(|| a.created_at.cmp(&b.created_at))
    });

    Ok(todos)
}

/// List TODO items across every project, optionally filtered by status.
///
/// The store scan is hard-capped at 10,000 rows in store-defined order before sorting.
/// Once matching rows reach that cap, results can silently omit TODOs outside the scan.
/// The caller's limit is applied after sorting the bounded scan.
pub async fn list_all_todos(
    store: &dyn VectorStore,
    status_filter: Option<&str>,
    limit: usize,
) -> Result<Vec<TodoItem>> {
    if let Some(status) = status_filter {
        validate_status(status)?;
    }

    let filter = status_filter.map(|status| format!("status = '{}'", sanitize_eq(status)));
    let results = store
        .scroll(TODOS_TABLE, filter.as_deref(), MAX_LIST_ALL_TODOS_SCAN, 0)
        .await?;
    if results.len() == MAX_LIST_ALL_TODOS_SCAN {
        tracing::warn!(
            scan_limit = MAX_LIST_ALL_TODOS_SCAN,
            status_filter = status_filter.unwrap_or("all"),
            "TODO list scan reached the hard cap; results may omit stored tasks"
        );
    }

    let mut todos: Vec<TodoItem> = results.iter().map(parse_todo).collect();
    todos.sort_by(|a, b| {
        priority_rank(&a.priority)
            .cmp(&priority_rank(&b.priority))
            .then_with(|| a.created_at.cmp(&b.created_at))
            .then_with(|| a.id.cmp(&b.id))
    });
    todos.truncate(limit);
    Ok(todos)
}

async fn apply_todo_update(
    store: &dyn VectorStore,
    embedder: &dyn EmbeddingProvider,
    table_schema: &dyn TableSchema,
    existing: SearchResult,
    updates: UpdateTodoFields,
) -> Result<TodoItem> {
    let blocked_by = match updates.blocked_by {
        Some(blocked_by) => Some(resolve_blocked_by(store, blocked_by).await?),
        None => None,
    };
    let mut item = parse_todo(&existing);

    let mut text_changed = false;
    if let Some(title) = updates.title {
        item.title = title;
        text_changed = true;
    }
    if let Some(desc) = updates.description {
        item.description = normalize_optional_text(desc);
        text_changed = true;
    }
    if let Some(priority) = updates.priority {
        item.priority = priority;
    }
    if let Some(owner) = updates.owner {
        item.owner = normalize_optional_text(owner);
    }
    if let Some(blocked_by) = blocked_by {
        item.blocked_by = blocked_by;
    }
    if let Some(status) = updates.status {
        let was_terminal = is_terminal(&item.status);
        let will_be_terminal = is_terminal(&status);
        if !was_terminal && will_be_terminal {
            item.completed_at = Some(Utc::now().to_rfc3339());
        } else if was_terminal && !will_be_terminal {
            item.completed_at = None;
        }
        item.status = status;
    }

    let data = build_data(&item.title, item.description.as_deref());
    let vector = if text_changed {
        let vecs = embedder.embed(std::slice::from_ref(&data)).await?;
        vecs[0].clone()
    } else {
        let old_data = existing
            .payload
            .get("data")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let vecs = embedder.embed(&[old_data.to_string()]).await?;
        vecs[0].clone()
    };

    let payload = build_payload(&item);
    let point = VectorPoint {
        id: item.id.clone(),
        vector,
        payload,
    };
    store.upsert(TODOS_TABLE, &[point], table_schema).await?;
    Ok(item)
}

pub async fn update_todo(
    store: &dyn VectorStore,
    embedder: &dyn EmbeddingProvider,
    table_schema: &dyn TableSchema,
    write_locks: &TodoWriteLocks,
    todo_id: &str,
    updates: UpdateTodoFields,
) -> Result<TodoItem> {
    if let Some(ref priority) = updates.priority {
        validate_priority(priority)?;
    }
    if let Some(ref status) = updates.status {
        validate_status(status)?;
    }

    // Fixed sharding bounds memory but may serialize unrelated IDs in one shard.
    // Separate processes still require storage-level compare-and-swap.
    let exact_guard = write_locks.lock(todo_id).await;
    let exact = store.get(TODOS_TABLE, &[todo_id.to_string()]).await?;
    if let Some(existing) = exact.into_iter().next() {
        return apply_todo_update(store, embedder, table_schema, existing, updates).await;
    }
    drop(exact_guard);

    let Some(resolved_id) = resolve_todo_prefix(store, todo_id).await? else {
        return Err(crate::error::MemcanError::Other(format!(
            "todo not found: {todo_id}"
        )));
    };
    let _write_guard = write_locks.lock(&resolved_id).await;
    let existing = store
        .get(TODOS_TABLE, std::slice::from_ref(&resolved_id))
        .await?
        .into_iter()
        .next()
        .ok_or_else(|| crate::error::MemcanError::Other(format!("todo not found: {todo_id}")))?;

    apply_todo_update(store, embedder, table_schema, existing, updates).await
}

pub async fn complete_todo(
    store: &dyn VectorStore,
    embedder: &dyn EmbeddingProvider,
    table_schema: &dyn TableSchema,
    write_locks: &TodoWriteLocks,
    todo_id: &str,
) -> Result<TodoItem> {
    update_todo(
        store,
        embedder,
        table_schema,
        write_locks,
        todo_id,
        UpdateTodoFields {
            status: Some("done".to_string()),
            ..Default::default()
        },
    )
    .await
}

/// Fetch a single TODO by ID.
/// Returns `Ok(None)` when the ID does not exist.
pub async fn get_todo(store: &dyn VectorStore, todo_id: &str) -> Result<Option<TodoItem>> {
    let exact = store.get(TODOS_TABLE, &[todo_id.to_string()]).await?;
    if let Some(item) = exact.first() {
        return Ok(Some(parse_todo(item)));
    }

    let Some(resolved_id) = resolve_todo_prefix(store, todo_id).await? else {
        return Ok(None);
    };
    let results = store
        .get(TODOS_TABLE, std::slice::from_ref(&resolved_id))
        .await?;
    Ok(results.first().map(parse_todo))
}

pub async fn delete_todo(
    store: &dyn VectorStore,
    write_locks: &TodoWriteLocks,
    todo_id: &str,
) -> Result<String> {
    let exact_guard = write_locks.lock(todo_id).await;
    let exact = store.get(TODOS_TABLE, &[todo_id.to_string()]).await?;
    if !exact.is_empty() {
        store.delete(TODOS_TABLE, &[todo_id.to_string()]).await?;
        return Ok(todo_id.to_string());
    }
    drop(exact_guard);

    let Some(resolved_id) = resolve_todo_prefix(store, todo_id).await? else {
        return Err(crate::error::MemcanError::Other(format!(
            "todo not found: {todo_id}"
        )));
    };
    let _write_guard = write_locks.lock(&resolved_id).await;
    let existing = store
        .get(TODOS_TABLE, std::slice::from_ref(&resolved_id))
        .await?;
    if existing.is_empty() {
        return Err(crate::error::MemcanError::Other(format!(
            "todo not found: {todo_id}"
        )));
    }
    store
        .delete(TODOS_TABLE, std::slice::from_ref(&resolved_id))
        .await?;
    Ok(resolved_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::collections::HashMap;
    use std::sync::Mutex;

    use crate::traits::MinimalTableSchema;

    struct MockEmbedder;

    #[async_trait]
    impl EmbeddingProvider for MockEmbedder {
        async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
            Ok(texts.iter().map(|_| vec![0.1, 0.2, 0.3]).collect())
        }

        fn dimensions(&self) -> usize {
            3
        }
    }

    struct DelayedEmbedder;

    #[async_trait]
    impl EmbeddingProvider for DelayedEmbedder {
        async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
            Ok(texts.iter().map(|_| vec![0.1, 0.2, 0.3]).collect())
        }

        fn dimensions(&self) -> usize {
            3
        }
    }

    #[derive(Default)]
    struct MockStore {
        records: Mutex<HashMap<String, SearchResult>>,
        last_filter: Mutex<Option<String>>,
    }

    impl MockStore {
        fn with_result(result: SearchResult) -> Self {
            Self {
                records: Mutex::new(HashMap::from([(result.id.clone(), result)])),
                last_filter: Mutex::new(None),
            }
        }

        fn with_results(results: impl IntoIterator<Item = SearchResult>) -> Self {
            Self {
                records: Mutex::new(
                    results
                        .into_iter()
                        .map(|result| (result.id.clone(), result))
                        .collect(),
                ),
                last_filter: Mutex::new(None),
            }
        }
    }

    #[async_trait]
    impl VectorStore for MockStore {
        async fn ensure_table(
            &self,
            _name: &str,
            _dims: usize,
            _schema: &dyn TableSchema,
        ) -> Result<()> {
            Ok(())
        }

        async fn upsert(
            &self,
            _table: &str,
            points: &[VectorPoint],
            _schema: &dyn TableSchema,
        ) -> Result<()> {
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
            _table: &str,
            _vector: &[f32],
            _filter: Option<&str>,
            _limit: usize,
            _offset: usize,
        ) -> Result<Vec<SearchResult>> {
            Ok(vec![])
        }

        async fn scroll(
            &self,
            _table: &str,
            filter: Option<&str>,
            limit: usize,
            _offset: usize,
        ) -> Result<Vec<SearchResult>> {
            *self.last_filter.lock().unwrap() = filter.map(String::from);
            let mut records: Vec<_> = self.records.lock().unwrap().values().cloned().collect();
            if let Some(filter) = filter {
                if let Some(prefix) = filter
                    .strip_prefix("id LIKE '")
                    .and_then(|value| value.strip_suffix("%'"))
                {
                    records.retain(|record| record.id.starts_with(prefix));
                } else if let Some(status) = filter
                    .strip_prefix("status = '")
                    .and_then(|value| value.strip_suffix('\''))
                {
                    records.retain(|record| {
                        record
                            .payload
                            .get("status")
                            .and_then(|value| value.as_str())
                            == Some(status)
                    });
                } else if let Some(project_filter) = filter.strip_prefix("project = '") {
                    let project = project_filter
                        .split('\'')
                        .next()
                        .unwrap_or_default()
                        .replace("''", "'");
                    records.retain(|record| {
                        record
                            .payload
                            .get("project")
                            .and_then(|value| value.as_str())
                            == Some(project.as_str())
                    });
                    if let Some(owner) = filter
                        .split(" AND owner = '")
                        .nth(1)
                        .and_then(|value| value.strip_suffix('\''))
                    {
                        let owner = owner.replace("''", "'");
                        records.retain(|record| {
                            record.payload.get("owner").and_then(|value| value.as_str())
                                == Some(owner.as_str())
                        });
                    }
                }
            }
            records.truncate(limit);
            Ok(records)
        }

        async fn count(&self, _table: &str, _filter: Option<&str>) -> Result<usize> {
            Ok(self.records.lock().unwrap().len())
        }

        async fn delete(&self, _table: &str, ids: &[String]) -> Result<()> {
            let mut records = self.records.lock().unwrap();
            for id in ids {
                records.remove(id);
            }
            Ok(())
        }

        async fn delete_by_filter(&self, _table: &str, _filter: &str) -> Result<usize> {
            Ok(0)
        }

        async fn get(&self, _table: &str, ids: &[String]) -> Result<Vec<SearchResult>> {
            let records = self.records.lock().unwrap();
            Ok(ids
                .iter()
                .filter_map(|id| records.get(id).cloned())
                .collect())
        }
    }

    fn todo_result_with_id(id: &str, status: &str, completed_at: Option<&str>) -> SearchResult {
        SearchResult {
            id: id.into(),
            score: 0.0,
            payload: json!({
                "id": id,
                "data": "Test TODO",
                "title": "Test TODO",
                "description": null,
                "project": "memcan",
                "priority": "medium",
                "status": status,
                "owner": "coordinator",
                "blocked_by": ["dependency-id"],
                "created_at": "2026-01-01T00:00:00Z",
                "completed_at": completed_at,
                "collection": "todos",
            }),
        }
    }

    fn todo_result(status: &str, completed_at: Option<&str>) -> SearchResult {
        todo_result_with_id("todo-id", status, completed_at)
    }

    fn listed_todo(
        id: &str,
        project: &str,
        priority: &str,
        status: &str,
        created_at: &str,
    ) -> SearchResult {
        SearchResult {
            id: id.into(),
            score: 0.0,
            payload: json!({
                "title": id,
                "project": project,
                "priority": priority,
                "status": status,
                "created_at": created_at,
            }),
        }
    }

    fn blocker_result(id: &str) -> SearchResult {
        listed_todo(
            id,
            "dependencies",
            "medium",
            "pending",
            "2026-01-01T00:00:00Z",
        )
    }

    const PREFIXED_TODO_ID: &str = "69964ce6-1111-4111-8111-111111111111";
    const AMBIGUOUS_TODO_ID: &str = "69964ce6-2222-4222-8222-222222222222";
    const BLOCKER_TODO_ID: &str = "abcdef12-3333-4333-8333-333333333333";

    #[test]
    fn test_validate_priority_valid() {
        assert!(validate_priority("low").is_ok());
        assert!(validate_priority("medium").is_ok());
        assert!(validate_priority("high").is_ok());
    }

    #[test]
    fn test_validate_priority_invalid() {
        let err = validate_priority("urgent").unwrap_err();
        assert!(err.to_string().contains("invalid priority"));
    }

    #[test]
    fn test_validate_status_valid() {
        for status in [
            "pending",
            "done",
            "in_progress",
            "blocked",
            "postponed",
            "cancelled",
        ] {
            assert!(validate_status(status).is_ok(), "status: {status}");
        }
    }

    #[test]
    fn test_validate_status_invalid() {
        let err = validate_status("archived").unwrap_err();
        assert!(err.to_string().contains("invalid status"));
    }

    #[test]
    fn test_build_data_with_description() {
        assert_eq!(
            build_data("Fix bug", Some("in login flow")),
            "Fix bug\nin login flow"
        );
    }

    #[test]
    fn test_build_data_without_description() {
        assert_eq!(build_data("Fix bug", None), "Fix bug");
        assert_eq!(build_data("Fix bug", Some("")), "Fix bug");
    }

    #[test]
    fn test_build_payload_has_required_fields() {
        let item = TodoItem {
            id: "test-id".into(),
            title: "Do something".into(),
            description: Some("details".into()),
            project: "myproj".into(),
            priority: "high".into(),
            status: "pending".into(),
            owner: Some("bilby".into()),
            blocked_by: vec!["id-a".into(), "id-b".into()],
            created_at: "2026-01-01T00:00:00Z".into(),
            completed_at: None,
        };
        let payload = build_payload(&item);

        assert_eq!(payload["id"], "test-id");
        assert_eq!(payload["data"], "Do something\ndetails");
        assert_eq!(payload["title"], "Do something");
        assert_eq!(payload["description"], "details");
        assert_eq!(payload["project"], "myproj");
        assert_eq!(payload["priority"], "high");
        assert_eq!(payload["status"], "pending");
        assert_eq!(payload["owner"], "bilby");
        assert_eq!(payload["blocked_by"], json!(["id-a", "id-b"]));
        assert_eq!(payload["collection"], "todos");
        assert!(payload["completed_at"].is_null());
    }

    #[test]
    fn test_build_payload_emits_null_owner_and_empty_blocked_by() {
        let item = TodoItem {
            id: "test-id".into(),
            title: "Do something".into(),
            description: None,
            project: "myproj".into(),
            priority: "medium".into(),
            status: "pending".into(),
            owner: None,
            blocked_by: vec![],
            created_at: "2026-01-01T00:00:00Z".into(),
            completed_at: None,
        };

        let payload = build_payload(&item);

        assert!(payload["owner"].is_null());
        assert_eq!(payload["blocked_by"], json!([]));
    }

    #[test]
    fn test_priority_rank_ordering() {
        assert!(priority_rank("high") < priority_rank("medium"));
        assert!(priority_rank("medium") < priority_rank("low"));
        assert!(priority_rank("low") < priority_rank("unknown"));
    }

    #[test]
    fn test_parse_todo_from_search_result() {
        let r = SearchResult {
            id: "abc-123".into(),
            score: 0.9,
            payload: json!({
                "title": "Refactor auth",
                "description": "split into modules",
                "project": "backend",
                "priority": "high",
                "status": "pending",
                "owner": "bilby",
                "blocked_by": ["id-a", "id-b"],
                "created_at": "2026-01-01T00:00:00Z",
                "completed_at": null,
            }),
        };
        let todo = parse_todo(&r);
        assert_eq!(todo.id, "abc-123");
        assert_eq!(todo.title, "Refactor auth");
        assert_eq!(todo.description.as_deref(), Some("split into modules"));
        assert_eq!(todo.project, "backend");
        assert_eq!(todo.priority, "high");
        assert_eq!(todo.status, "pending");
        assert_eq!(todo.owner.as_deref(), Some("bilby"));
        assert_eq!(todo.blocked_by, vec!["id-a", "id-b"]);
        assert!(todo.completed_at.is_none());
    }

    #[test]
    fn test_parse_todo_defaults_for_missing_fields() {
        let r = SearchResult {
            id: "id".into(),
            score: 0.5,
            payload: json!({}),
        };
        let todo = parse_todo(&r);
        assert_eq!(todo.title, "");
        assert_eq!(todo.priority, "medium");
        assert_eq!(todo.status, "pending");
        assert!(todo.owner.is_none());
        assert!(todo.blocked_by.is_empty());
    }

    #[test]
    fn test_parse_todo_legacy_payload_preserves_fields() {
        let r = SearchResult {
            id: "legacy-id".into(),
            score: 0.5,
            payload: json!({
                "title": "Legacy TODO",
                "description": "Stored before metadata fields",
                "project": "memcan",
                "priority": "high",
                "status": "done",
                "created_at": "2026-01-01T00:00:00Z",
                "completed_at": "2026-01-02T00:00:00Z",
            }),
        };

        let todo = parse_todo(&r);

        assert_eq!(todo.id, "legacy-id");
        assert_eq!(todo.title, "Legacy TODO");
        assert_eq!(
            todo.description.as_deref(),
            Some("Stored before metadata fields")
        );
        assert_eq!(todo.project, "memcan");
        assert_eq!(todo.priority, "high");
        assert_eq!(todo.status, "done");
        assert_eq!(todo.created_at, "2026-01-01T00:00:00Z");
        assert_eq!(todo.completed_at.as_deref(), Some("2026-01-02T00:00:00Z"));
        assert!(todo.owner.is_none());
        assert!(todo.blocked_by.is_empty());
    }

    #[test]
    fn test_parse_todo_malformed_blocked_by_defaults_empty() {
        let r = SearchResult {
            id: "id".into(),
            score: 0.5,
            payload: json!({"blocked_by": "not-an-array"}),
        };

        assert!(parse_todo(&r).blocked_by.is_empty());
    }

    #[test]
    fn test_todo_item_serialization() {
        let item = TodoItem {
            id: "id".into(),
            title: "test".into(),
            description: None,
            project: "proj".into(),
            priority: "low".into(),
            status: "done".into(),
            owner: Some("codex-sol".into()),
            blocked_by: vec!["id-a".into()],
            created_at: "2026-01-01T00:00:00Z".into(),
            completed_at: Some("2026-01-02T00:00:00Z".into()),
        };
        let json = serde_json::to_value(&item).unwrap();
        assert_eq!(json["status"], "done");
        assert_eq!(json["completed_at"], "2026-01-02T00:00:00Z");
        assert_eq!(json["owner"], "codex-sol");
        assert_eq!(json["blocked_by"], json!(["id-a"]));

        let round_trip: TodoItem = serde_json::from_value(json).unwrap();
        assert_eq!(round_trip.owner, item.owner);
        assert_eq!(round_trip.blocked_by, item.blocked_by);
    }

    #[tokio::test]
    async fn test_add_todo_round_trips_owner_and_blocked_by() {
        let store = MockStore::with_results([blocker_result("id-a"), blocker_result("id-b")]);
        let added = add_todo(
            &store,
            &MockEmbedder,
            &MinimalTableSchema,
            AddTodoParams {
                title: "Owned work".into(),
                description: None,
                project: "memcan".into(),
                priority: None,
                owner: Some(" bilby ".into()),
                blocked_by: Some(vec!["id-a".into(), "id-b".into()]),
            },
        )
        .await
        .unwrap();

        let listed = list_todos(&store, "memcan", None, Some("bilby"), 50)
            .await
            .unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, added.id);
        assert_eq!(added.owner.as_deref(), Some("bilby"));
        assert_eq!(listed[0].owner.as_deref(), Some("bilby"));
        assert_eq!(listed[0].blocked_by, vec!["id-a", "id-b"]);
        assert_eq!(
            store.last_filter.lock().unwrap().as_deref(),
            Some("project = 'memcan' AND owner = 'bilby'")
        );
    }

    #[tokio::test]
    async fn test_add_todo_empty_optional_text_is_normalized_to_none() {
        let added = add_todo(
            &MockStore::default(),
            &MockEmbedder,
            &MinimalTableSchema,
            AddTodoParams {
                title: "Unassigned work".into(),
                description: Some(String::new()),
                project: "memcan".into(),
                priority: None,
                owner: Some(String::new()),
                blocked_by: None,
            },
        )
        .await
        .unwrap();

        assert!(added.owner.is_none());
        assert!(added.description.is_none());
    }

    #[tokio::test]
    async fn test_add_todo_normalizes_blocked_by() {
        let store = MockStore::with_result(blocker_result("id-a"));
        let added = add_todo(
            &store,
            &MockEmbedder,
            &MinimalTableSchema,
            AddTodoParams {
                title: "Blocked work".into(),
                description: None,
                project: "memcan".into(),
                priority: None,
                owner: None,
                blocked_by: Some(vec![" id-a ".into(), "".into(), "id-a".into()]),
            },
        )
        .await
        .unwrap();

        assert_eq!(added.blocked_by, vec!["id-a"]);
    }

    #[tokio::test]
    async fn test_update_todo_normalizes_blocked_by() {
        let store = MockStore::with_results([todo_result("pending", None), blocker_result("id-a")]);
        let write_locks = TodoWriteLocks::default();
        let updated = update_todo(
            &store,
            &MockEmbedder,
            &MinimalTableSchema,
            &write_locks,
            "todo-id",
            UpdateTodoFields {
                blocked_by: Some(vec![" id-a ".into(), "".into(), "id-a".into()]),
                ..Default::default()
            },
        )
        .await
        .unwrap();

        assert_eq!(updated.blocked_by, vec!["id-a"]);
    }

    #[tokio::test]
    async fn test_add_and_update_todo_normalize_blocked_by_the_same_way_and_preserve_order() {
        let blocked_by = vec![
            " id-b ".into(),
            "".into(),
            "id-a".into(),
            "id-b".into(),
            " id-c ".into(),
        ];
        let blocker_results = [
            blocker_result("id-a"),
            blocker_result("id-b"),
            blocker_result("id-c"),
        ];
        let add_store = MockStore::with_results(blocker_results.clone());
        let created = add_todo(
            &add_store,
            &MockEmbedder,
            &MinimalTableSchema,
            AddTodoParams {
                title: "Blocked work".into(),
                description: None,
                project: "memcan".into(),
                priority: None,
                owner: None,
                blocked_by: Some(blocked_by.clone()),
            },
        )
        .await
        .unwrap();

        let store = MockStore::with_results(
            std::iter::once(todo_result("pending", None)).chain(blocker_results),
        );
        let write_locks = TodoWriteLocks::default();
        let updated = update_todo(
            &store,
            &MockEmbedder,
            &MinimalTableSchema,
            &write_locks,
            "todo-id",
            UpdateTodoFields {
                blocked_by: Some(blocked_by),
                ..Default::default()
            },
        )
        .await
        .unwrap();

        assert_eq!(created.blocked_by, vec!["id-b", "id-a", "id-c"]);
        assert_eq!(created.blocked_by, updated.blocked_by);
    }

    #[tokio::test]
    async fn test_add_todo_resolves_and_deduplicates_blocker_prefixes() {
        let store = MockStore::with_result(blocker_result(BLOCKER_TODO_ID));

        let added = add_todo(
            &store,
            &MockEmbedder,
            &MinimalTableSchema,
            AddTodoParams {
                title: "Blocked work".into(),
                description: None,
                project: "memcan".into(),
                priority: None,
                owner: None,
                blocked_by: Some(vec!["abcdef12".into(), "abcdef12-3333".into()]),
            },
        )
        .await
        .unwrap();
        let stored = get_todo(&store, &added.id).await.unwrap().unwrap();

        assert_eq!(added.blocked_by, vec![BLOCKER_TODO_ID]);
        assert_eq!(stored.blocked_by, vec![BLOCKER_TODO_ID]);
    }

    #[tokio::test]
    async fn test_update_todo_resolves_blocker_prefix_to_full_id() {
        let store = MockStore::with_results([
            todo_result_with_id(PREFIXED_TODO_ID, "pending", None),
            blocker_result(BLOCKER_TODO_ID),
        ]);
        let write_locks = TodoWriteLocks::default();

        let updated = update_todo(
            &store,
            &MockEmbedder,
            &MinimalTableSchema,
            &write_locks,
            "69964ce6",
            UpdateTodoFields {
                blocked_by: Some(vec!["abcdef12".into()]),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let stored = get_todo(&store, PREFIXED_TODO_ID).await.unwrap().unwrap();

        assert_eq!(updated.blocked_by, vec![BLOCKER_TODO_ID]);
        assert_eq!(stored.blocked_by, vec![BLOCKER_TODO_ID]);
    }

    #[tokio::test]
    async fn test_add_todo_rejects_unresolved_blockers_without_writing() {
        let missing_store = MockStore::default();
        let missing_error = add_todo(
            &missing_store,
            &MockEmbedder,
            &MinimalTableSchema,
            AddTodoParams {
                title: "Blocked work".into(),
                description: None,
                project: "memcan".into(),
                priority: None,
                owner: None,
                blocked_by: Some(vec!["deadbeef".into()]),
            },
        )
        .await
        .unwrap_err();

        assert!(missing_error.to_string().contains("'deadbeef'"));
        assert!(missing_error.to_string().contains("not found"));
        assert!(missing_store.records.lock().unwrap().is_empty());

        let ambiguous_store = MockStore::with_results([
            blocker_result(PREFIXED_TODO_ID),
            blocker_result(AMBIGUOUS_TODO_ID),
        ]);
        let ambiguous_error = add_todo(
            &ambiguous_store,
            &MockEmbedder,
            &MinimalTableSchema,
            AddTodoParams {
                title: "Blocked work".into(),
                description: None,
                project: "memcan".into(),
                priority: None,
                owner: None,
                blocked_by: Some(vec!["69964ce6".into()]),
            },
        )
        .await
        .unwrap_err();

        assert!(ambiguous_error.to_string().contains("'69964ce6'"));
        assert!(ambiguous_error.to_string().contains("ambiguous"));
        assert!(
            ambiguous_error
                .to_string()
                .contains("provide the full UUID")
        );
        assert_eq!(ambiguous_store.records.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn test_update_todo_rejects_unresolved_blockers_without_writing() {
        let missing_store =
            MockStore::with_result(todo_result_with_id(PREFIXED_TODO_ID, "pending", None));
        let write_locks = TodoWriteLocks::default();
        let missing_error = update_todo(
            &missing_store,
            &MockEmbedder,
            &MinimalTableSchema,
            &write_locks,
            PREFIXED_TODO_ID,
            UpdateTodoFields {
                title: Some("Changed".into()),
                blocked_by: Some(vec!["deadbeef".into()]),
                ..Default::default()
            },
        )
        .await
        .unwrap_err();

        assert!(missing_error.to_string().contains("'deadbeef'"));
        assert!(missing_error.to_string().contains("not found"));
        assert_eq!(
            get_todo(&missing_store, PREFIXED_TODO_ID)
                .await
                .unwrap()
                .unwrap()
                .title,
            "Test TODO"
        );

        let ambiguous_store = MockStore::with_results([
            todo_result_with_id(PREFIXED_TODO_ID, "pending", None),
            blocker_result("abcdef12-1111-4111-8111-111111111111"),
            blocker_result("abcdef12-2222-4222-8222-222222222222"),
        ]);
        let ambiguous_error = update_todo(
            &ambiguous_store,
            &MockEmbedder,
            &MinimalTableSchema,
            &write_locks,
            PREFIXED_TODO_ID,
            UpdateTodoFields {
                title: Some("Changed".into()),
                blocked_by: Some(vec!["abcdef12".into()]),
                ..Default::default()
            },
        )
        .await
        .unwrap_err();

        assert!(ambiguous_error.to_string().contains("'abcdef12'"));
        assert!(ambiguous_error.to_string().contains("ambiguous"));
        assert!(
            ambiguous_error
                .to_string()
                .contains("provide the full UUID")
        );
        assert_eq!(
            get_todo(&ambiguous_store, PREFIXED_TODO_ID)
                .await
                .unwrap()
                .unwrap()
                .title,
            "Test TODO"
        );
    }

    #[tokio::test]
    async fn test_add_and_update_todo_normalize_whitespace_only_description_the_same_way() {
        let created = add_todo(
            &MockStore::default(),
            &MockEmbedder,
            &MinimalTableSchema,
            AddTodoParams {
                title: "Whitespace description".into(),
                description: Some("   ".into()),
                project: "memcan".into(),
                priority: None,
                owner: None,
                blocked_by: None,
            },
        )
        .await
        .unwrap();
        assert!(created.description.is_none());

        let store = MockStore::with_result(todo_result("pending", None));
        let write_locks = TodoWriteLocks::default();
        let updated = update_todo(
            &store,
            &MockEmbedder,
            &MinimalTableSchema,
            &write_locks,
            "todo-id",
            UpdateTodoFields {
                description: Some("   ".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert!(updated.description.is_none());

        assert_eq!(created.description, updated.description);
    }

    #[tokio::test]
    async fn test_list_todos_owner_filter_is_sanitized() {
        let mut result = todo_result("pending", None);
        result.payload["owner"] = json!("bilby's");
        let store = MockStore::with_result(result);

        let listed = list_todos(&store, "memcan", None, Some("bilby's"), 50)
            .await
            .unwrap();

        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].owner.as_deref(), Some("bilby's"));
        assert_eq!(
            store.last_filter.lock().unwrap().as_deref(),
            Some("project = 'memcan' AND owner = 'bilby''s'")
        );
    }

    #[tokio::test]
    async fn test_list_all_todos_returns_cross_project_rows() {
        let store = MockStore::with_results([
            listed_todo(
                "backend",
                "backend",
                "medium",
                "pending",
                "2026-01-01T00:00:00Z",
            ),
            listed_todo(
                "memcan",
                "memcan",
                "medium",
                "pending",
                "2026-01-01T00:00:00Z",
            ),
        ]);

        let listed = list_all_todos(&store, None, 50).await.unwrap();

        assert_eq!(listed.len(), 2);
        assert!(listed.iter().any(|item| item.project == "backend"));
        assert!(listed.iter().any(|item| item.project == "memcan"));
        assert!(store.last_filter.lock().unwrap().is_none());
    }

    #[tokio::test]
    async fn test_list_all_todos_filters_valid_status() {
        let store = MockStore::with_results([
            listed_todo(
                "pending",
                "memcan",
                "medium",
                "pending",
                "2026-01-01T00:00:00Z",
            ),
            listed_todo("done", "backend", "high", "done", "2026-01-02T00:00:00Z"),
        ]);

        let listed = list_all_todos(&store, Some("pending"), 50).await.unwrap();

        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, "pending");
        assert_eq!(
            store.last_filter.lock().unwrap().as_deref(),
            Some("status = 'pending'")
        );
    }

    #[tokio::test]
    async fn test_list_all_todos_empty_store_is_empty() {
        let listed = list_all_todos(&MockStore::default(), None, 50)
            .await
            .unwrap();

        assert!(listed.is_empty());
    }

    #[tokio::test]
    async fn test_list_all_todos_has_deterministic_priority_created_id_order() {
        let store = MockStore::with_results([
            listed_todo("z-low", "a", "low", "pending", "2026-01-01T00:00:00Z"),
            listed_todo("b-medium", "a", "medium", "pending", "2026-01-01T00:00:00Z"),
            listed_todo("a-medium", "b", "medium", "pending", "2026-01-01T00:00:00Z"),
            listed_todo("high", "a", "high", "pending", "2026-01-03T00:00:00Z"),
        ]);

        let listed = list_all_todos(&store, None, 50).await.unwrap();
        let ids: Vec<_> = listed.iter().map(|item| item.id.as_str()).collect();

        assert_eq!(ids, ["high", "a-medium", "b-medium", "z-low"]);
    }

    #[tokio::test]
    async fn test_list_all_todos_applies_limit_after_sorting() {
        let store = MockStore::with_results([
            listed_todo("z-low", "a", "low", "pending", "2026-01-01T00:00:00Z"),
            listed_todo("b-medium", "a", "medium", "pending", "2026-01-01T00:00:00Z"),
            listed_todo("a-medium", "b", "medium", "pending", "2026-01-01T00:00:00Z"),
            listed_todo("high", "a", "high", "pending", "2026-01-03T00:00:00Z"),
        ]);

        let listed = list_all_todos(&store, None, 2).await.unwrap();
        let ids: Vec<_> = listed.iter().map(|item| item.id.as_str()).collect();

        assert_eq!(ids, ["high", "a-medium"]);
    }

    #[tokio::test]
    async fn test_list_all_todos_rejects_invalid_status_before_store_access() {
        let store = MockStore::default();

        let error = list_all_todos(&store, Some("archived"), 50)
            .await
            .unwrap_err();

        assert!(error.to_string().contains("invalid status"));
        assert!(store.last_filter.lock().unwrap().is_none());
    }

    #[tokio::test]
    async fn test_list_todos_empty_owner_filter_is_omitted() {
        let store = MockStore::with_result(todo_result("pending", None));

        for owner_filter in ["", "   "] {
            let listed = list_todos(&store, "memcan", None, Some(owner_filter), 50)
                .await
                .unwrap();

            assert_eq!(listed.len(), 1);
            assert_eq!(listed[0].id, "todo-id");
            assert_eq!(
                store.last_filter.lock().unwrap().as_deref(),
                Some("project = 'memcan'")
            );
        }
    }

    #[tokio::test]
    async fn test_get_todo_returns_known_item_with_all_fields() {
        let store = MockStore::with_result(todo_result("blocked", None));

        let item = get_todo(&store, "todo-id").await.unwrap().unwrap();

        assert_eq!(item.id, "todo-id");
        assert_eq!(item.title, "Test TODO");
        assert_eq!(item.status, "blocked");
        assert_eq!(item.owner.as_deref(), Some("coordinator"));
        assert_eq!(item.blocked_by, vec!["dependency-id"]);
    }

    #[tokio::test]
    async fn test_get_todo_returns_none_for_unknown_id() {
        let store = MockStore::default();

        assert!(get_todo(&store, "missing-id").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn test_exact_todo_id_wins_without_prefix_scan() {
        let store = MockStore::with_results([
            todo_result_with_id("69964ce6", "pending", None),
            todo_result_with_id(PREFIXED_TODO_ID, "pending", None),
        ]);

        let item = get_todo(&store, "69964ce6").await.unwrap().unwrap();

        assert_eq!(item.id, "69964ce6");
        assert!(store.last_filter.lock().unwrap().is_none());
    }

    #[tokio::test]
    async fn test_get_todo_resolves_unique_prefix_to_full_id() {
        let store = MockStore::with_result(todo_result_with_id(PREFIXED_TODO_ID, "pending", None));

        let item = get_todo(&store, "69964ce6").await.unwrap().unwrap();

        assert_eq!(item.id, PREFIXED_TODO_ID);
        assert_eq!(
            store.last_filter.lock().unwrap().as_deref(),
            Some("id LIKE '69964ce6%'")
        );
    }

    #[tokio::test]
    async fn test_get_todo_short_prefix_returns_not_found_without_scan() {
        let store = MockStore::with_result(todo_result_with_id(PREFIXED_TODO_ID, "pending", None));

        assert!(get_todo(&store, "69964ce").await.unwrap().is_none());
        assert!(store.last_filter.lock().unwrap().is_none());
    }

    #[tokio::test]
    async fn test_get_todo_long_unknown_prefix_returns_not_found() {
        let store = MockStore::default();

        assert!(get_todo(&store, "deadbeef").await.unwrap().is_none());
        assert_eq!(
            store.last_filter.lock().unwrap().as_deref(),
            Some("id LIKE 'deadbeef%'")
        );
    }

    #[tokio::test]
    async fn test_get_todo_ambiguous_prefix_lists_candidates() {
        let store = MockStore::with_results([
            todo_result_with_id(PREFIXED_TODO_ID, "pending", None),
            todo_result_with_id(AMBIGUOUS_TODO_ID, "pending", None),
        ]);

        let error = get_todo(&store, "69964ce6").await.unwrap_err();
        let message = error.to_string();

        assert!(message.contains("ambiguous todo id prefix '69964ce6'"));
        assert!(message.contains("matches 2 todos"));
        assert!(message.contains(PREFIXED_TODO_ID));
        assert!(message.contains(AMBIGUOUS_TODO_ID));
        assert!(message.contains("provide the full UUID"));
    }

    #[tokio::test]
    async fn test_update_todo_resolves_unique_prefix_to_full_id() {
        let store = MockStore::with_result(todo_result_with_id(PREFIXED_TODO_ID, "pending", None));
        let write_locks = TodoWriteLocks::default();

        let item = update_todo(
            &store,
            &MockEmbedder,
            &MinimalTableSchema,
            &write_locks,
            "69964ce6",
            UpdateTodoFields {
                priority: Some("high".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();

        assert_eq!(item.id, PREFIXED_TODO_ID);
        assert_eq!(item.priority, "high");
    }

    #[tokio::test]
    async fn test_complete_todo_resolves_unique_prefix_to_full_id() {
        let store = MockStore::with_result(todo_result_with_id(PREFIXED_TODO_ID, "pending", None));
        let write_locks = TodoWriteLocks::default();

        let item = complete_todo(
            &store,
            &MockEmbedder,
            &MinimalTableSchema,
            &write_locks,
            "69964ce6",
        )
        .await
        .unwrap();

        assert_eq!(item.id, PREFIXED_TODO_ID);
        assert_eq!(item.status, "done");
    }

    #[tokio::test]
    async fn test_delete_todo_resolves_unique_prefix_and_returns_full_id() {
        let store = MockStore::with_result(todo_result_with_id(PREFIXED_TODO_ID, "pending", None));
        let write_locks = TodoWriteLocks::default();

        let deleted_id = delete_todo(&store, &write_locks, "69964ce6").await.unwrap();

        assert_eq!(deleted_id, PREFIXED_TODO_ID);
        assert!(get_todo(&store, PREFIXED_TODO_ID).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn test_two_prefix_lengths_resolve_to_same_full_id() {
        let store = MockStore::with_result(todo_result_with_id(PREFIXED_TODO_ID, "pending", None));

        let short = resolve_todo_id(&store, "69964ce6").await.unwrap().unwrap();
        let longer = resolve_todo_id(&store, "69964ce6-1111")
            .await
            .unwrap()
            .unwrap();

        assert_eq!(short, PREFIXED_TODO_ID);
        assert_eq!(longer, PREFIXED_TODO_ID);
    }

    #[tokio::test]
    async fn test_update_todo_owner_set_clear_and_unchanged() {
        let store = MockStore::with_result(todo_result("pending", None));
        let write_locks = TodoWriteLocks::default();

        let unchanged = update_todo(
            &store,
            &MockEmbedder,
            &MinimalTableSchema,
            &write_locks,
            "todo-id",
            UpdateTodoFields::default(),
        )
        .await
        .unwrap();
        assert_eq!(unchanged.owner.as_deref(), Some("coordinator"));

        let set = update_todo(
            &store,
            &MockEmbedder,
            &MinimalTableSchema,
            &write_locks,
            "todo-id",
            UpdateTodoFields {
                owner: Some(" bilby ".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(set.owner.as_deref(), Some("bilby"));
        assert_eq!(
            get_todo(&store, "todo-id")
                .await
                .unwrap()
                .unwrap()
                .owner
                .as_deref(),
            Some("bilby")
        );

        let cleared = update_todo(
            &store,
            &MockEmbedder,
            &MinimalTableSchema,
            &write_locks,
            "todo-id",
            UpdateTodoFields {
                owner: Some(" \t ".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert!(cleared.owner.is_none());
        assert!(
            get_todo(&store, "todo-id")
                .await
                .unwrap()
                .unwrap()
                .owner
                .is_none()
        );
    }

    #[tokio::test]
    async fn test_concurrent_updates_same_todo_preserve_both_changes() {
        let store = MockStore::with_result(todo_result("pending", None));
        let write_locks = TodoWriteLocks::default();

        let priority_update = update_todo(
            &store,
            &DelayedEmbedder,
            &MinimalTableSchema,
            &write_locks,
            "todo-id",
            UpdateTodoFields {
                priority: Some("high".into()),
                ..Default::default()
            },
        );
        let owner_update = update_todo(
            &store,
            &DelayedEmbedder,
            &MinimalTableSchema,
            &write_locks,
            "todo-id",
            UpdateTodoFields {
                owner: Some("bilby".into()),
                ..Default::default()
            },
        );

        let (priority_result, owner_result) = tokio::join!(priority_update, owner_update);
        priority_result.unwrap();
        owner_result.unwrap();

        let updated = get_todo(&store, "todo-id").await.unwrap().unwrap();
        assert_eq!(updated.priority, "high");
        assert_eq!(updated.owner.as_deref(), Some("bilby"));
    }

    #[tokio::test]
    async fn test_concurrent_update_and_delete_does_not_resurrect_todo() {
        let store = MockStore::with_result(todo_result("pending", None));
        let write_locks = TodoWriteLocks::default();

        let update = update_todo(
            &store,
            &DelayedEmbedder,
            &MinimalTableSchema,
            &write_locks,
            "todo-id",
            UpdateTodoFields {
                priority: Some("high".into()),
                ..Default::default()
            },
        );
        let delete = delete_todo(&store, &write_locks, "todo-id");

        let (update_result, delete_result) = tokio::join!(update, delete);
        update_result.unwrap();
        delete_result.unwrap();

        assert!(get_todo(&store, "todo-id").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn test_update_todo_blocked_by_replace_clear_and_unchanged() {
        let store = MockStore::with_results([
            todo_result("pending", None),
            blocker_result("id-a"),
            blocker_result("id-b"),
        ]);
        let write_locks = TodoWriteLocks::default();

        let unchanged = update_todo(
            &store,
            &MockEmbedder,
            &MinimalTableSchema,
            &write_locks,
            "todo-id",
            UpdateTodoFields::default(),
        )
        .await
        .unwrap();
        assert_eq!(unchanged.blocked_by, vec!["dependency-id"]);

        let replaced = update_todo(
            &store,
            &MockEmbedder,
            &MinimalTableSchema,
            &write_locks,
            "todo-id",
            UpdateTodoFields {
                blocked_by: Some(vec!["id-a".into(), "id-b".into()]),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(replaced.blocked_by, vec!["id-a", "id-b"]);
        assert_eq!(
            get_todo(&store, "todo-id")
                .await
                .unwrap()
                .unwrap()
                .blocked_by,
            vec!["id-a", "id-b"]
        );

        let cleared = update_todo(
            &store,
            &MockEmbedder,
            &MinimalTableSchema,
            &write_locks,
            "todo-id",
            UpdateTodoFields {
                blocked_by: Some(vec![]),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert!(cleared.blocked_by.is_empty());
        assert!(
            get_todo(&store, "todo-id")
                .await
                .unwrap()
                .unwrap()
                .blocked_by
                .is_empty()
        );
    }

    #[tokio::test]
    async fn test_update_todo_active_to_cancelled_sets_completed_at() {
        let store = MockStore::with_result(todo_result("blocked", None));
        let write_locks = TodoWriteLocks::default();

        let updated = update_todo(
            &store,
            &MockEmbedder,
            &MinimalTableSchema,
            &write_locks,
            "todo-id",
            UpdateTodoFields {
                status: Some("cancelled".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();

        let completed_at = updated.completed_at.expect("terminal timestamp");
        assert!(chrono::DateTime::parse_from_rfc3339(&completed_at).is_ok());
    }

    #[tokio::test]
    async fn test_update_todo_cancelled_to_pending_clears_completed_at() {
        let store = MockStore::with_result(todo_result("cancelled", Some("2026-01-02T00:00:00Z")));
        let write_locks = TodoWriteLocks::default();

        let updated = update_todo(
            &store,
            &MockEmbedder,
            &MinimalTableSchema,
            &write_locks,
            "todo-id",
            UpdateTodoFields {
                status: Some("pending".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();

        assert!(updated.completed_at.is_none());
    }

    #[tokio::test]
    async fn test_update_todo_active_statuses_leave_completed_at_empty() {
        let store = MockStore::with_result(todo_result("pending", None));
        let write_locks = TodoWriteLocks::default();

        for status in ["in_progress", "blocked", "postponed"] {
            let updated = update_todo(
                &store,
                &MockEmbedder,
                &MinimalTableSchema,
                &write_locks,
                "todo-id",
                UpdateTodoFields {
                    status: Some(status.into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
            assert!(updated.completed_at.is_none(), "status: {status}");
        }
    }

    #[tokio::test]
    async fn test_update_todo_done_pending_regression() {
        let store = MockStore::with_result(todo_result("pending", None));
        let write_locks = TodoWriteLocks::default();

        let done = update_todo(
            &store,
            &MockEmbedder,
            &MinimalTableSchema,
            &write_locks,
            "todo-id",
            UpdateTodoFields {
                status: Some("done".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let original_completed_at = done.completed_at.clone();
        assert!(original_completed_at.is_some());

        let still_done = update_todo(
            &store,
            &MockEmbedder,
            &MinimalTableSchema,
            &write_locks,
            "todo-id",
            UpdateTodoFields {
                status: Some("done".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(still_done.completed_at, original_completed_at);

        let pending = update_todo(
            &store,
            &MockEmbedder,
            &MinimalTableSchema,
            &write_locks,
            "todo-id",
            UpdateTodoFields {
                status: Some("pending".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert!(pending.completed_at.is_none());
    }
}
