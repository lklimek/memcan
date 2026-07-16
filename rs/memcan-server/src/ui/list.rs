use std::{cmp::Ordering, collections::BTreeSet, sync::Arc};

use axum::{
    Extension,
    extract::{Query, rejection::QueryRejection},
    response::{IntoResponse, Response},
};
use maud::{Markup, html};
use memcan_core::{
    todo::{TodoItem, list_all_todos, priority_rank, valid_statuses, validate_status},
    traits::VectorStore,
};
use serde::Deserialize;
use tracing::error;

use super::{detail, fmt_date_short, layout, priority_badge, status_badge};

const LIMIT: usize = 500;

#[derive(Debug, Default, Deserialize)]
pub(super) struct FilterParams {
    project: Option<String>,
    status: Option<String>,
    sort: Option<String>,
}

#[derive(Clone, Copy)]
enum Sort {
    Priority,
    Created,
    Project,
    Status,
}

impl Sort {
    fn from_query(value: Option<&str>) -> Self {
        match value {
            Some("created") => Self::Created,
            Some("project") => Self::Project,
            Some("status") => Self::Status,
            _ => Self::Priority,
        }
    }

    fn value(self) -> &'static str {
        match self {
            Self::Priority => "priority",
            Self::Created => "created",
            Self::Project => "project",
            Self::Status => "status",
        }
    }
}

pub(super) async fn tasks(
    Extension(store): Extension<Arc<dyn VectorStore>>,
    params: Result<Query<FilterParams>, QueryRejection>,
) -> Response {
    let params = params.map_or_else(|_| FilterParams::default(), |Query(params)| params);
    let all = match list_all_todos(store.as_ref(), None, usize::MAX).await {
        Ok(items) => items,
        Err(source) => {
            error!(error = %source, "web UI task list could not load");
            return detail::service_unavailable();
        }
    };

    let selected_status = params
        .status
        .as_deref()
        .filter(|status| validate_status(status).is_ok());
    let selected_project = params.project.as_deref().filter(|value| !value.is_empty());
    let selected_sort = Sort::from_query(params.sort.as_deref());
    let projects: BTreeSet<_> = all.iter().map(|item| item.project.as_str()).collect();

    let mut displayed: Vec<_> = all
        .iter()
        .filter(|item| {
            selected_project.is_none_or(|project| item.project == project)
                && selected_status.is_none_or(|status| item.status == status)
        })
        .cloned()
        .collect();
    sort_items(&mut displayed, selected_sort);
    displayed.truncate(LIMIT);

    layout(
        "MemCan · Tasks",
        list_markup(
            &all,
            &displayed,
            &projects,
            selected_project,
            selected_status,
            selected_sort,
        ),
    )
    .into_response()
}

fn list_markup(
    all: &[TodoItem],
    displayed: &[TodoItem],
    projects: &BTreeSet<&str>,
    selected_project: Option<&str>,
    selected_status: Option<&str>,
    selected_sort: Sort,
) -> Markup {
    html! {
        h1 { "MemCan · Tasks" }
        form role="search" aria-label="Task filters" class="panel filters" method="get" action="/ui/tasks" {
            label for="project" { "Project" }
            select id="project" name="project" {
                option value="" selected[selected_project.is_none()] { "all" }
                @for project in projects {
                    option value=(project) selected[selected_project == Some(*project)] { (project) }
                }
            }
            label for="status" { "Status" }
            select id="status" name="status" {
                option value="" selected[selected_status.is_none()] { "all" }
                @for status in valid_statuses() {
                    option value=(status) selected[selected_status == Some(*status)] { (status) }
                }
            }
            label for="sort" { "Sort" }
            select id="sort" name="sort" {
                @for (value, label) in [
                    ("priority", "priority"),
                    ("created", "created"),
                    ("project", "project"),
                    ("status", "status"),
                ] {
                    option value=(value) selected[selected_sort.value() == value] { (label) }
                }
            }
            button type="submit" { "Apply" }
            a href="/ui/tasks" { "Clear" }
        }

        @if all.is_empty() {
            section class="panel empty-state" { p { "No tasks yet." } }
        } @else if displayed.is_empty() {
            section class="panel empty-state" {
                p { "No tasks match this filter." }
                a href="/ui/tasks" { "Clear filters" }
            }
        } @else {
            div class="panel table-wrap" {
                table {
                    thead {
                        tr {
                            th scope="col" { "Priority" }
                            th scope="col" { "Title" }
                            th scope="col" { "Project" }
                            th scope="col" { "Status" }
                            th scope="col" { "Owner" }
                            th scope="col" { "Created" }
                        }
                    }
                    tbody {
                        @for item in displayed {
                            tr {
                                td { (priority_badge(&item.priority)) }
                                td { a href=(format!("/ui/tasks/{}", item.id)) { (&item.title) } }
                                td { (&item.project) }
                                td { (status_badge(&item.status)) }
                                td { (item.owner.as_deref().unwrap_or("—")) }
                                td { (fmt_date_short(&item.created_at)) }
                            }
                        }
                    }
                }
            }
        }
        p class="task-count" { (displayed.len()) " tasks · limit " (LIMIT) }
    }
}

fn sort_items(items: &mut [TodoItem], sort: Sort) {
    match sort {
        Sort::Priority => {}
        Sort::Created => items.sort_by(|left, right| {
            left.created_at
                .cmp(&right.created_at)
                .then_with(|| left.id.cmp(&right.id))
        }),
        Sort::Project => items.sort_by(|left, right| {
            left.project
                .to_ascii_lowercase()
                .cmp(&right.project.to_ascii_lowercase())
                .then_with(|| default_order(left, right))
        }),
        Sort::Status => items.sort_by(|left, right| {
            left.status
                .cmp(&right.status)
                .then_with(|| default_order(left, right))
        }),
    }
}

fn default_order(left: &TodoItem, right: &TodoItem) -> Ordering {
    priority_rank(&left.priority)
        .cmp(&priority_rank(&right.priority))
        .then_with(|| left.created_at.cmp(&right.created_at))
        .then_with(|| left.id.cmp(&right.id))
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;
    use serial_test::serial;

    use super::super::test_support::{app_with_error_store, app_with_items, base_items, get, item};

    fn ordered(body: &str, ids: &[&str]) {
        let positions: Vec<_> = ids
            .iter()
            .map(|id| body.find(id).unwrap_or_else(|| panic!("missing {id}")))
            .collect();
        assert!(positions.windows(2).all(|pair| pair[0] < pair[1]));
    }

    #[tokio::test]
    #[serial]
    async fn list_view_renders_semantic_table_default_order_and_controls() {
        let app = app_with_items(base_items()).await;
        let (status, body) = get(&app.router, "/ui/tasks").await;

        assert_eq!(status, StatusCode::OK);
        for heading in ["Priority", "Title", "Project", "Status", "Owner", "Created"] {
            assert!(body.contains(&format!("<th scope=\"col\">{heading}</th>")));
        }
        ordered(
            &body,
            &["T-HIGH-1", "T-BLK-A", "T-MED-1", "T-LOW-1", "T-BLK-B"],
        );
        assert!(body.contains(
            "<form role=\"search\" aria-label=\"Task filters\" class=\"panel filters\" method=\"get\" action=\"/ui/tasks\">"
        ));
        assert!(!body.contains("<nav aria-label=\"Task filters\""));
        assert!(body.contains("<select id=\"project\" name=\"project\">"));
        assert!(body.contains("<select id=\"status\" name=\"status\">"));
        assert!(body.contains("<select id=\"sort\" name=\"sort\">"));
        assert!(body.contains("5 tasks · limit 500"));
        assert!(body.contains("●</span> blocked"));
        assert!(body.contains("<td>—</td>"));
        assert!(body.contains("href=\"/ui/tasks/T-HIGH-1\">Blocked deployment</a>"));
    }

    #[tokio::test]
    #[serial]
    async fn list_view_filters_project_status_and_reflects_active_controls() {
        let app = app_with_items(base_items()).await;
        let (status, body) = get(
            &app.router,
            "/ui/tasks?project=backend&status=pending&sort=created",
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("T-BLK-A"));
        assert!(body.contains("T-BLK-B"));
        assert!(!body.contains("T-HIGH-1"));
        assert!(body.contains("value=\"backend\" selected"));
        assert!(body.contains("value=\"pending\" selected"));
        assert!(body.contains("value=\"created\" selected"));
        assert!(body.contains("2 tasks · limit 500"));
    }

    #[tokio::test]
    #[serial]
    async fn list_view_applies_all_deterministic_sort_modes() {
        let app = app_with_items(base_items()).await;
        let (_, created) = get(&app.router, "/ui/tasks?sort=created").await;
        ordered(
            &created,
            &["T-LOW-1", "T-BLK-A", "T-BLK-B", "T-MED-1", "T-HIGH-1"],
        );

        let (_, project) = get(&app.router, "/ui/tasks?sort=project").await;
        ordered(
            &project,
            &["T-HIGH-1", "T-BLK-A", "T-BLK-B", "T-MED-1", "T-LOW-1"],
        );

        let (_, status) = get(&app.router, "/ui/tasks?sort=status").await;
        ordered(
            &status,
            &["T-HIGH-1", "T-LOW-1", "T-BLK-A", "T-MED-1", "T-BLK-B"],
        );
    }

    #[tokio::test]
    #[serial]
    async fn list_view_ignores_invalid_status_sort_and_unknown_query_params() {
        let app = app_with_items(base_items()).await;
        let (_, body) = get(
            &app.router,
            "/ui/tasks?status=archived_forever&sort=bogus_field&bogus=1",
        )
        .await;

        for id in ["T-HIGH-1", "T-MED-1", "T-LOW-1", "T-BLK-A", "T-BLK-B"] {
            assert!(body.contains(id));
        }
        ordered(
            &body,
            &["T-HIGH-1", "T-BLK-A", "T-MED-1", "T-LOW-1", "T-BLK-B"],
        );
        assert!(!body.contains("archived_forever"));
        assert!(!body.contains("bogus_field"));
        assert!(!body.contains("bogus"));
        assert!(body.contains("value=\"priority\" selected"));
    }

    #[tokio::test]
    #[serial]
    async fn list_view_ignores_duplicate_query_params_inside_page_shell() {
        let app = app_with_items(base_items()).await;

        for uri in [
            "/ui/tasks?project=backend&project=memcan",
            "/ui/tasks?status=pending&status=done",
        ] {
            let (status, body) = get(&app.router, uri).await;

            assert_eq!(status, StatusCode::OK);
            assert!(body.starts_with("<!DOCTYPE html>"));
            assert!(body.contains("<title>MemCan · Tasks</title>"));
            assert!(body.contains("5 tasks · limit 500"));
            assert!(!body.contains("Failed to deserialize query string"));
        }
    }

    #[tokio::test]
    #[serial]
    async fn list_view_distinguishes_empty_and_filtered_empty_states() {
        let empty = app_with_items(Vec::new()).await;
        let (_, empty_body) = get(&empty.router, "/ui/tasks").await;
        assert!(empty_body.contains("No tasks yet."));
        assert!(!empty_body.contains("No tasks match this filter."));

        let filtered = app_with_items(base_items()).await;
        let (_, filtered_body) = get(&filtered.router, "/ui/tasks?project=missing").await;
        assert!(filtered_body.contains("No tasks match this filter."));
        assert!(filtered_body.contains("href=\"/ui/tasks\">Clear"));
    }

    #[tokio::test]
    #[serial]
    async fn list_view_derives_project_options_and_keeps_all_status_options() {
        let app = app_with_items(base_items()).await;
        let (_, body) = get(&app.router, "/ui/tasks").await;

        assert!(body.contains("value=\"backend\""));
        assert!(body.contains("value=\"memcan\""));
        assert!(!body.contains("value=\"docs\""));
        for status in [
            "pending",
            "in_progress",
            "blocked",
            "done",
            "postponed",
            "cancelled",
        ] {
            assert!(body.contains(&format!("value=\"{status}\"")));
        }
    }

    #[tokio::test]
    #[serial]
    async fn list_view_escapes_all_user_fields_and_unknown_badges() {
        let mut poisoned = item(
            "XSS",
            "<script>t()</script>",
            "<b>p</b>",
            "urgent-plus",
            "archived_forever",
            Some("\"><svg onload=o()>"),
            "not-a-date",
        );
        poisoned.description = Some("<img src=x onerror=d()>".into());
        let preescaped = item(
            "ENTITY",
            "&lt;script&gt;",
            "memcan",
            "medium",
            "pending",
            None,
            "2026-01-01T00:00:00Z",
        );
        let app = app_with_items(vec![poisoned, preescaped]).await;
        let (_, body) = get(&app.router, "/ui/tasks").await;

        for raw in ["<script>", "<b>", "<svg"] {
            assert!(!body.contains(raw), "unescaped {raw}");
        }
        for escaped in [
            "&lt;script&gt;t()&lt;/script&gt;",
            "&lt;b&gt;p&lt;/b&gt;",
            "&lt;svg onload=o()&gt;",
        ] {
            assert!(body.contains(escaped), "missing {escaped}");
        }
        assert!(body.contains("urgent-plus"));
        assert!(body.contains("archived_forever"));
        assert!(body.contains("&amp;lt;script&amp;gt;"));
    }

    #[tokio::test]
    #[serial]
    async fn list_view_enforces_fixed_limit_without_pagination() {
        let items = (0..501)
            .map(|index| {
                item(
                    &format!("LIMIT-{index:03}"),
                    &format!("Task {index}"),
                    "memcan",
                    "medium",
                    "pending",
                    None,
                    "2026-01-01T00:00:00Z",
                )
            })
            .collect();
        let app = app_with_items(items).await;
        let (_, body) = get(&app.router, "/ui/tasks").await;

        assert!(body.contains("500 tasks · limit 500"));
        assert!(!body.contains("pagination"));
    }

    #[tokio::test]
    #[serial]
    async fn list_view_filter_finds_match_beyond_display_limit() {
        let mut items: Vec<_> = (0..501)
            .map(|index| {
                item(
                    &format!("NOISE-{index:03}"),
                    &format!("Noise task {index}"),
                    "noise-project",
                    "medium",
                    "pending",
                    None,
                    "2026-01-01T00:00:00Z",
                )
            })
            .collect();
        items.push(item(
            "TARGET-1",
            "Target task",
            "target-project",
            "high",
            "pending",
            None,
            "2026-01-01T00:00:00Z",
        ));
        let app = app_with_items(items).await;
        let (_, body) = get(&app.router, "/ui/tasks?project=target-project").await;

        assert!(body.contains("TARGET-1"));
        assert!(body.contains("1 tasks · limit 500"));
    }

    #[tokio::test]
    #[serial]
    async fn list_view_store_failure_returns_friendly_503_without_internal_text() {
        let app = app_with_error_store();
        let (status, body) = get(&app.router, "/ui/tasks").await;

        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert!(body.contains("Unable to load tasks right now."));
        for secret in ["/data/", "lancedb", "SELECT", "panic", ".rs:"] {
            assert!(!body.contains(secret));
        }
    }
}
