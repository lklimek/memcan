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
use crate::traits::{EmbeddingProvider, SearchResult, VectorPoint, VectorStore};

pub const TODOS_TABLE: &str = "memcan_todos";

const VALID_PRIORITIES: &[&str] = &["low", "medium", "high"];
const VALID_STATUSES: &[&str] = &["pending", "done"];

#[derive(Debug, Clone, Deserialize)]
pub struct AddTodoParams {
    pub title: String,
    pub description: Option<String>,
    pub project: String,
    pub priority: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TodoItem {
    pub id: String,
    pub title: String,
    pub description: Option<String>,
    pub project: String,
    pub priority: String,
    pub status: String,
    pub created_at: String,
    pub completed_at: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct UpdateTodoFields {
    pub title: Option<String>,
    pub description: Option<String>,
    pub priority: Option<String>,
    pub status: Option<String>,
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
    store.upsert(TODOS_TABLE, &[point]).await?;
    Ok(item)
}

pub async fn list_todos(
    store: &dyn VectorStore,
    project: &str,
    status_filter: Option<&str>,
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
    if let Some(status) = updates.status {
        if status == "done" && item.status != "done" {
            item.completed_at = Some(Utc::now().to_rfc3339());
        } else if status == "pending" {
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
    store.upsert(TODOS_TABLE, &[point]).await?;
    Ok(item)
}

pub async fn complete_todo(
    store: &dyn VectorStore,
    embedder: &dyn EmbeddingProvider,
    todo_id: &str,
) -> Result<TodoItem> {
    update_todo(
        store,
        embedder,
        todo_id,
        UpdateTodoFields {
            status: Some("done".to_string()),
            ..Default::default()
        },
    )
    .await
}

pub async fn delete_todo(store: &dyn VectorStore, todo_id: &str) -> Result<()> {
    store.delete(TODOS_TABLE, &[todo_id.to_string()]).await
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert!(validate_status("pending").is_ok());
        assert!(validate_status("done").is_ok());
    }

    #[test]
    fn test_validate_status_invalid() {
        let err = validate_status("cancelled").unwrap_err();
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
        assert_eq!(payload["collection"], "todos");
        assert!(payload["completed_at"].is_null());
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
            created_at: "2026-01-01T00:00:00Z".into(),
            completed_at: Some("2026-01-02T00:00:00Z".into()),
        };
        let json = serde_json::to_value(&item).unwrap();
        assert_eq!(json["status"], "done");
        assert_eq!(json["completed_at"], "2026-01-02T00:00:00Z");
    }
}
