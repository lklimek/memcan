//! Per-project TODO list CRUD operations.
//!
//! TODOs persist across sessions, are project-scoped, and searchable via
//! unified search. Stored in LanceDB with embeddings for semantic search.

use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

use crate::error::Result;
use crate::query::sanitize_eq;
use crate::traits::{EmbeddingProvider, SearchResult, TableSchema, VectorPoint, VectorStore};

pub const TODOS_TABLE: &str = "memcan_todos";

const VALID_PRIORITIES: &[&str] = &["low", "medium", "high"];
const VALID_STATUSES: &[&str] = &[
    "pending",
    "done",
    "in_progress",
    "blocked",
    "postponed",
    "cancelled",
];

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
    /// Timestamp when the task reached a terminal state (`done` or `cancelled`).
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
    if !VALID_STATUSES.contains(&s) {
        return Err(crate::error::MemcanError::Other(format!(
            "invalid status '{}', must be one of: {}",
            s,
            VALID_STATUSES.join(", ")
        )));
    }
    Ok(())
}

fn is_terminal(status: &str) -> bool {
    matches!(status, "done" | "cancelled")
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

fn priority_rank(p: &str) -> u8 {
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

    let item = TodoItem {
        id: Uuid::new_v4().to_string(),
        title: params.title,
        description: params.description,
        project: params.project,
        priority: priority.to_string(),
        status: "pending".to_string(),
        owner: params.owner,
        blocked_by: params.blocked_by.unwrap_or_default(),
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

    let safe_project = sanitize_eq(project);
    let mut filter = format!("project = '{safe_project}'");
    if let Some(status) = status_filter {
        let safe_status = sanitize_eq(status);
        filter.push_str(&format!(" AND status = '{safe_status}'"));
    }
    if let Some(owner) = owner_filter {
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

pub async fn update_todo(
    store: &dyn VectorStore,
    embedder: &dyn EmbeddingProvider,
    table_schema: &dyn TableSchema,
    todo_id: &str,
    updates: UpdateTodoFields,
) -> Result<TodoItem> {
    if let Some(ref p) = updates.priority {
        validate_priority(p)?;
    }
    if let Some(ref s) = updates.status {
        validate_status(s)?;
    }

    let existing = store.get(TODOS_TABLE, &[todo_id.to_string()]).await?;
    if existing.is_empty() {
        return Err(crate::error::MemcanError::Other(format!(
            "todo not found: {todo_id}"
        )));
    }

    let mut item = parse_todo(&existing[0]);

    let mut text_changed = false;
    if let Some(title) = updates.title {
        item.title = title;
        text_changed = true;
    }
    if let Some(desc) = updates.description {
        item.description = if desc.is_empty() { None } else { Some(desc) };
        text_changed = true;
    }
    if let Some(priority) = updates.priority {
        item.priority = priority;
    }
    if let Some(owner) = updates.owner {
        item.owner = if owner.is_empty() { None } else { Some(owner) };
    }
    if let Some(blocked_by) = updates.blocked_by {
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
        let old_data = existing[0]
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

pub async fn complete_todo(
    store: &dyn VectorStore,
    embedder: &dyn EmbeddingProvider,
    table_schema: &dyn TableSchema,
    todo_id: &str,
) -> Result<TodoItem> {
    update_todo(
        store,
        embedder,
        table_schema,
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
    let results = store.get(TODOS_TABLE, &[todo_id.to_string()]).await?;
    Ok(results.first().map(parse_todo))
}

pub async fn delete_todo(store: &dyn VectorStore, todo_id: &str) -> Result<()> {
    store.delete(TODOS_TABLE, &[todo_id.to_string()]).await
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
            Ok(self
                .records
                .lock()
                .unwrap()
                .values()
                .take(limit)
                .cloned()
                .collect())
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

    fn todo_result(status: &str, completed_at: Option<&str>) -> SearchResult {
        SearchResult {
            id: "todo-id".into(),
            score: 0.0,
            payload: json!({
                "id": "todo-id",
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
        let store = MockStore::default();
        let added = add_todo(
            &store,
            &MockEmbedder,
            &MinimalTableSchema,
            AddTodoParams {
                title: "Owned work".into(),
                description: None,
                project: "memcan".into(),
                priority: None,
                owner: Some("bilby".into()),
                blocked_by: Some(vec!["id-a".into(), "id-b".into()]),
            },
        )
        .await
        .unwrap();

        let listed = list_todos(&store, "memcan", None, None, 50).await.unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, added.id);
        assert_eq!(listed[0].owner.as_deref(), Some("bilby"));
        assert_eq!(listed[0].blocked_by, vec!["id-a", "id-b"]);
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
    async fn test_update_todo_owner_set_clear_and_unchanged() {
        let store = MockStore::with_result(todo_result("pending", None));

        let unchanged = update_todo(
            &store,
            &MockEmbedder,
            &MinimalTableSchema,
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
            "todo-id",
            UpdateTodoFields {
                owner: Some("bilby".into()),
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
            "todo-id",
            UpdateTodoFields {
                owner: Some(String::new()),
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
    async fn test_update_todo_blocked_by_replace_clear_and_unchanged() {
        let store = MockStore::with_result(todo_result("pending", None));

        let unchanged = update_todo(
            &store,
            &MockEmbedder,
            &MinimalTableSchema,
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

        let updated = update_todo(
            &store,
            &MockEmbedder,
            &MinimalTableSchema,
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

        let updated = update_todo(
            &store,
            &MockEmbedder,
            &MinimalTableSchema,
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

        for status in ["in_progress", "blocked", "postponed"] {
            let updated = update_todo(
                &store,
                &MockEmbedder,
                &MinimalTableSchema,
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

        let done = update_todo(
            &store,
            &MockEmbedder,
            &MinimalTableSchema,
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
