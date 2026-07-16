use std::sync::Arc;

use async_trait::async_trait;
use axum::{
    Extension, Router,
    body::{Body, to_bytes},
    http::{Request, header},
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use memcan_core::{
    config::Settings,
    error::{MemcanError, Result},
    lancedb_store::LanceDbStore,
    schema::MemcanTableSchema,
    todo::{TODOS_TABLE, TodoItem},
    traits::{SearchResult, TableSchema, VectorPoint, VectorStore},
};
use tempfile::TempDir;
use tower::ServiceExt;

use super::router;

pub(super) struct TestApp {
    pub(super) router: Router,
    _environment: TestEnvironment,
}

struct TestEnvironment {
    _directory: TempDir,
    originals: Vec<(&'static str, Option<String>)>,
}

impl TestEnvironment {
    fn new() -> Self {
        let directory = tempfile::tempdir().expect("test tempdir");
        let path = directory.path().to_str().expect("UTF-8 temp path");
        let values = [
            ("HOME", path),
            ("XDG_CONFIG_HOME", path),
            ("XDG_DATA_HOME", path),
        ];
        let originals = values
            .iter()
            .map(|(key, _)| (*key, std::env::var(key).ok()))
            .collect();
        // SAFETY: every UI fixture test is serialized and restores these values on drop.
        unsafe {
            for (key, value) in values {
                std::env::set_var(key, value);
            }
        }
        Self {
            _directory: directory,
            originals,
        }
    }

    fn database_path(&self) -> String {
        self._directory
            .path()
            .join("lancedb")
            .to_string_lossy()
            .into_owned()
    }
}

impl Drop for TestEnvironment {
    fn drop(&mut self) {
        // SAFETY: every UI fixture test is serialized and this restores the prior environment.
        unsafe {
            for (key, value) in self.originals.drain(..) {
                match value {
                    Some(value) => std::env::set_var(key, value),
                    None => std::env::remove_var(key),
                }
            }
        }
    }
}

pub(super) async fn app_with_items(items: Vec<TodoItem>) -> TestApp {
    let environment = TestEnvironment::new();
    let store = LanceDbStore::open(&environment.database_path())
        .await
        .expect("open fixture store");
    store
        .ensure_table(TODOS_TABLE, 3, &MemcanTableSchema)
        .await
        .expect("create todos table");
    let points: Vec<_> = items.iter().map(todo_point).collect();
    store
        .upsert(TODOS_TABLE, &points, &MemcanTableSchema)
        .await
        .expect("seed todos table");
    TestApp {
        router: authenticated_router(Arc::new(store)),
        _environment: environment,
    }
}

pub(super) fn app_with_error_store() -> TestApp {
    let environment = TestEnvironment::new();
    TestApp {
        router: authenticated_router(Arc::new(ErrorStore)),
        _environment: environment,
    }
}

fn authenticated_router(store: Arc<dyn VectorStore>) -> Router {
    let settings = Settings {
        webui_username: Some("testuser".into()),
        webui_password: Some("testpass".into()),
        ..Settings::default()
    };
    router(&settings)
        .expect("authenticated UI router")
        .layer(Extension(store))
}

fn todo_point(item: &TodoItem) -> VectorPoint {
    VectorPoint {
        id: item.id.clone(),
        vector: vec![0.0; 3],
        payload: serde_json::json!({
            "id": item.id,
            "data": item.title,
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
        }),
    }
}

pub(super) fn base_items() -> Vec<TodoItem> {
    vec![
        item(
            "T-HIGH-1",
            "Blocked deployment",
            "backend",
            "high",
            "blocked",
            Some("ana"),
            "2026-07-14T10:00:00Z",
        ),
        item(
            "T-MED-1",
            "Write task UI",
            "memcan",
            "medium",
            "pending",
            None,
            "2026-07-13T10:00:00Z",
        ),
        item(
            "T-LOW-1",
            "Polish docs",
            "memcan",
            "low",
            "done",
            Some("sam"),
            "2026-07-10T10:00:00Z",
        ),
        item(
            "T-BLK-A",
            "Provision service",
            "backend",
            "medium",
            "pending",
            Some("ops"),
            "2026-07-12T08:00:00Z",
        ),
        item(
            "T-BLK-B",
            "Approve release",
            "backend",
            "low",
            "pending",
            Some("lead"),
            "2026-07-12T09:00:00Z",
        ),
    ]
}

pub(super) fn item(
    id: &str,
    title: &str,
    project: &str,
    priority: &str,
    status: &str,
    owner: Option<&str>,
    created_at: &str,
) -> TodoItem {
    TodoItem {
        id: id.into(),
        title: title.into(),
        description: None,
        project: project.into(),
        priority: priority.into(),
        status: status.into(),
        owner: owner.map(String::from),
        blocked_by: Vec::new(),
        created_at: created_at.into(),
        completed_at: None,
    }
}

pub(super) async fn response(app: &Router, uri: &str) -> axum::response::Response {
    let credentials = STANDARD.encode("testuser:testpass");
    let request = Request::builder()
        .uri(uri)
        .header(header::AUTHORIZATION, format!("Basic {credentials}"))
        .body(Body::empty())
        .unwrap();
    app.clone().oneshot(request).await.unwrap()
}

pub(super) async fn get(app: &Router, uri: &str) -> (axum::http::StatusCode, String) {
    let response = response(app, uri).await;
    let status = response.status();
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    (status, String::from_utf8(body.to_vec()).unwrap())
}

struct ErrorStore;

#[async_trait]
impl VectorStore for ErrorStore {
    async fn ensure_table(&self, _: &str, _: usize, _: &dyn TableSchema) -> Result<()> {
        Err(unavailable())
    }

    async fn upsert(&self, _: &str, _: &[VectorPoint], _: &dyn TableSchema) -> Result<()> {
        Err(unavailable())
    }

    async fn search(
        &self,
        _: &str,
        _: &[f32],
        _: Option<&str>,
        _: usize,
        _: usize,
    ) -> Result<Vec<SearchResult>> {
        Err(unavailable())
    }

    async fn scroll(
        &self,
        _: &str,
        _: Option<&str>,
        _: usize,
        _: usize,
    ) -> Result<Vec<SearchResult>> {
        Err(unavailable())
    }

    async fn count(&self, _: &str, _: Option<&str>) -> Result<usize> {
        Err(unavailable())
    }

    async fn delete(&self, _: &str, _: &[String]) -> Result<()> {
        Err(unavailable())
    }

    async fn delete_by_filter(&self, _: &str, _: &str) -> Result<usize> {
        Err(unavailable())
    }

    async fn get(&self, _: &str, _: &[String]) -> Result<Vec<SearchResult>> {
        Err(unavailable())
    }
}

fn unavailable() -> MemcanError {
    MemcanError::Other("/data/private/lancedb SELECT panic at src/store.rs:1".into())
}
