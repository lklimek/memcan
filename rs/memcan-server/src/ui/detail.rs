use std::sync::Arc;

use axum::{
    Extension,
    extract::{Path, Query, rejection::QueryRejection},
    http::{HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use maud::{Markup, html};
use memcan_core::{
    todo::{TodoItem, get_todo},
    traits::VectorStore,
};
use tracing::error;

use super::{encode_task_id, fmt_date_long, layout, priority_badge, status_badge, url_with_query};

pub(super) async fn redirect() -> Response {
    (
        StatusCode::FOUND,
        [(header::LOCATION, HeaderValue::from_static("/ui/tasks"))],
    )
        .into_response()
}

pub(super) async fn task(
    Path(id): Path<String>,
    params: Result<Query<Vec<(String, String)>>, QueryRejection>,
    Extension(store): Extension<Arc<dyn VectorStore>>,
) -> Response {
    let params = params.map_or_else(|_| Vec::new(), |Query(params)| params);
    let back_url = url_with_query("/ui/tasks", &params);
    match get_todo(store.as_ref(), &id).await {
        Ok(Some(item)) => layout(
            &format!("{} · MemCan Tasks", item.title),
            task_markup(&item, &back_url, &params),
        )
        .into_response(),
        Ok(None) => not_found(&back_url),
        Err(source) => {
            error!(error = %source, task_id = ?id, "web UI task detail could not load");
            service_unavailable(&back_url)
        }
    }
}

fn task_markup(item: &TodoItem, back_url: &str, params: &[(String, String)]) -> Markup {
    html! {
        nav aria-label="Breadcrumb" { a href=(back_url) { "Back to tasks" } }
        article class="panel task-detail" {
            h1 { (&item.title) }
            dl class="details" {
                dt { "Priority" }
                dd { (priority_badge(&item.priority)) }
                dt { "Status" }
                dd { (status_badge(&item.status)) }
                dt { "Project" }
                dd { (&item.project) }
                dt { "Owner" }
                dd { (item.owner.as_deref().unwrap_or("—")) }
                dt { "Created" }
                dd { (fmt_date_long(&item.created_at)) }
                dt { "Completed" }
                dd {
                    @if let Some(completed_at) = &item.completed_at {
                        (fmt_date_long(completed_at))
                    } @else {
                        "—"
                    }
                }
                dt { "ID" }
                dd { code title=(&item.id) { (&item.id) } }
            }
            section aria-labelledby="description-heading" {
                h2 id="description-heading" { "Description" }
                div class="description" {
                    (item.description.as_deref().filter(|description| !description.is_empty()).unwrap_or("—"))
                }
            }
            @if !item.blocked_by.is_empty() {
                section aria-labelledby="blocked-heading" {
                    h2 id="blocked-heading" { "Blocked by" }
                    ul {
                        @for blocker in &item.blocked_by {
                            li {
                                a href=(url_with_query(
                                    &format!("/ui/tasks/{}", encode_task_id(blocker)),
                                    params,
                                )) { (blocker) }
                            }
                        }
                    }
                }
            }
        }
    }
}

fn not_found(back_url: &str) -> Response {
    (
        StatusCode::NOT_FOUND,
        layout(
            "Task not found",
            html! {
                nav aria-label="Breadcrumb" { a href=(back_url) { "Back to tasks" } }
                section class="panel empty-state" {
                    h1 { "Task not found." }
                    p { "The requested task does not exist." }
                }
            },
        ),
    )
        .into_response()
}

pub(super) fn service_unavailable(back_url: &str) -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        layout(
            "Tasks unavailable",
            html! {
                nav aria-label="Breadcrumb" { a href=(back_url) { "Back to tasks" } }
                h1 { "Tasks unavailable" }
                p { "Unable to load tasks right now." }
            },
        ),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use axum::{
        body::to_bytes,
        http::{StatusCode, header},
    };
    use serial_test::serial;

    use super::super::test_support::{
        app_with_error_store, app_with_items, base_items, get, item, response,
    };

    #[tokio::test]
    #[serial]
    async fn detail_redirect_is_literal_302_to_tasks() {
        let app = app_with_items(Vec::new()).await;
        let response = response(&app.router, "/ui").await;

        assert_eq!(response.status(), StatusCode::FOUND);
        assert_eq!(
            response.headers().get(header::LOCATION).unwrap(),
            "/ui/tasks"
        );
    }

    #[tokio::test]
    #[serial]
    async fn detail_view_renders_every_field_blockers_and_preserved_description() {
        let mut task = base_items().remove(0);
        task.description = Some("Line one\nLine two".into());
        task.blocked_by = vec!["T-BLK-A".into(), "T-BLK-B".into()];
        let app = app_with_items(vec![task]).await;
        let (status, body) = get(&app.router, "/ui/tasks/T-HIGH-1").await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body.matches("<h1>").count(), 1);
        assert!(body.contains("<h1>Blocked deployment</h1>"));
        assert!(body.contains("backend"));
        assert!(body.contains("ana"));
        assert!(body.contains("▲</span> high"));
        assert!(body.contains("●</span> blocked"));
        assert!(body.contains("href=\"/ui/tasks/T-BLK-A\">T-BLK-A</a>"));
        assert!(body.contains("href=\"/ui/tasks/T-BLK-B\">T-BLK-B</a>"));
        assert!(body.contains("class=\"description\">Line one\nLine two</div>"));
        assert!(!body.contains("<pre"));
        assert!(body.contains("2026-07-14 10:00 UTC"));
        assert!(body.contains("<dt>Completed</dt><dd>—</dd>"));
        assert!(body.contains("title=\"T-HIGH-1\">T-HIGH-1"));
        assert!(body.contains("href=\"/ui/tasks\">Back to tasks</a>"));
    }

    #[tokio::test]
    #[serial]
    async fn detail_view_preserves_and_safely_encodes_incoming_filter_query() {
        let app = app_with_items(base_items()).await;
        let (status, body) = get(
            &app.router,
            "/ui/tasks/T-HIGH-1?status=pending%26blocked&project=alpha%26beta%3Dgamma%23frag&sort=created",
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert!(body.contains(
            "href=\"/ui/tasks?status=pending%26blocked&amp;project=alpha%26beta%3Dgamma%23frag&amp;sort=created\">Back to tasks</a>"
        ));
        assert!(!body.contains("project=alpha&amp;beta=gamma#frag"));
    }

    #[tokio::test]
    #[serial]
    async fn detail_view_omits_empty_blocked_by_and_uses_optional_placeholders() {
        let mut task = item(
            "LEGACY",
            "Legacy task",
            "memcan",
            "medium",
            "pending",
            None,
            "not-a-date",
        );
        task.description = Some(String::new());
        let app = app_with_items(vec![task]).await;
        let (status, body) = get(&app.router, "/ui/tasks/LEGACY").await;

        assert_eq!(status, StatusCode::OK);
        assert!(!body.contains("Blocked by"));
        assert!(body.contains("<dt>Owner</dt><dd>—</dd>"));
        assert!(body.contains("<dt>Completed</dt><dd>—</dd>"));
        assert!(body.contains("class=\"description\">—</div>"));
        assert!(body.contains("not-a-date"));
    }

    #[tokio::test]
    #[serial]
    async fn detail_view_dangling_blocker_remains_a_safe_link() {
        let mut task = item(
            "BLOCKED",
            "Blocked task",
            "memcan",
            "medium",
            "blocked",
            None,
            "2026-01-01T00:00:00Z",
        );
        task.blocked_by = vec!["dep/one?x#frag%é".into()];
        let app = app_with_items(vec![task]).await;
        let (status, body) = get(&app.router, "/ui/tasks/BLOCKED").await;

        assert_eq!(status, StatusCode::OK);
        assert!(
            body.contains("href=\"/ui/tasks/dep%2Fone%3Fx%23frag%25%C3%A9\">dep/one?x#frag%é</a>")
        );
    }

    #[tokio::test]
    #[serial]
    async fn detail_view_unknown_id_returns_friendly_404() {
        let app = app_with_items(base_items()).await;
        let (status, body) = get(
            &app.router,
            "/ui/tasks/does-not-exist?status=done&status_filter_present=1",
        )
        .await;

        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(body.contains("Task not found."));
        assert!(body.contains(
            "href=\"/ui/tasks?status=done&amp;status_filter_present=1\">Back to tasks</a>"
        ));
        for internal in ["panic", "unwrap", ".rs:", "lancedb"] {
            assert!(!body.contains(internal));
        }
    }

    #[tokio::test]
    #[serial]
    async fn detail_view_store_failure_returns_friendly_503_without_internal_text() {
        let app = app_with_error_store();
        let (status, body) = get(
            &app.router,
            "/ui/tasks/T-HIGH-1?project=backend&sort=status",
        )
        .await;

        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert!(body.contains("Unable to load tasks right now."));
        assert!(
            body.contains("href=\"/ui/tasks?project=backend&amp;sort=status\">Back to tasks</a>")
        );
        for internal in ["/data/", "lancedb", "SELECT", "panic", ".rs:"] {
            assert!(!body.contains(internal));
        }
    }

    #[tokio::test]
    #[serial]
    async fn detail_view_escapes_all_fields_and_blocker_attribute_context() {
        let mut task = item(
            "XSS",
            "<script>t()</script>",
            "<b>p</b>",
            "urgent-plus",
            "archived_forever",
            Some("\"><svg onload=o()>"),
            "2026-01-01T00:00:00Z",
        );
        task.description = Some("<img src=x onerror=d()>".into());
        task.blocked_by = vec!["bad\"><script>alert(1)</script>".into()];
        let app = app_with_items(vec![task]).await;
        let response = response(&app.router, "/ui/tasks/XSS").await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = String::from_utf8(
            to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap()
                .to_vec(),
        )
        .unwrap();

        for raw in ["<script>", "<img", "<svg", "<b>"] {
            assert!(!body.contains(raw), "unescaped {raw}");
        }
        for escaped in [
            "&lt;script&gt;t()&lt;/script&gt;",
            "&lt;img src=x onerror=d()&gt;",
            "&lt;svg onload=o()&gt;",
            "&lt;b&gt;p&lt;/b&gt;",
            "bad&quot;&gt;&lt;script&gt;alert(1)&lt;/script&gt;",
        ] {
            assert!(body.contains(escaped), "missing {escaped}");
        }
    }
}
