use std::{cmp::Ordering, collections::BTreeSet, sync::Arc};

use axum::{
    Extension,
    extract::{Query, rejection::QueryRejection},
    response::{IntoResponse, Response},
};
use maud::{Markup, html};
use memcan_core::{
    todo::{TodoItem, list_all_todos, list_todos, priority_rank, valid_statuses},
    traits::VectorStore,
};
use tracing::error;

use super::{
    detail, encode_task_id, fmt_date_short, layout, priority_badge, status_badge, url_with_query,
};

const LIMIT: usize = 500;
/// Scoped per-project scan window: kept well above `LIMIT` so the priority
/// sort in `tasks()` runs over the full matching set before truncating to
/// page size, instead of sorting an arbitrarily-ordered store-side slice.
const PROJECT_SCAN_LIMIT: usize = 10_000;

#[derive(Debug)]
struct FilterState {
    project: Option<String>,
    statuses: Vec<&'static str>,
    status_filter_explicit: bool,
    sort: Sort,
}

impl FilterState {
    fn from_pairs(pairs: &[(String, String)]) -> Self {
        let pairs = if ["project", "sort"].iter().any(|name| {
            pairs
                .iter()
                .filter(|(candidate, _)| candidate == name)
                .count()
                > 1
        }) {
            &[]
        } else {
            pairs
        };
        let mut statuses: Vec<_> = valid_statuses()
            .iter()
            .copied()
            .filter(|valid| {
                pairs
                    .iter()
                    .any(|(key, value)| key == "status" && value == valid)
            })
            .collect();
        let marker_present = pairs
            .iter()
            .any(|(key, value)| key == "status_filter_present" && value == "1");
        let status_filter_explicit = marker_present || !statuses.is_empty();
        if !status_filter_explicit {
            statuses = valid_statuses()
                .iter()
                .copied()
                .filter(|status| !matches!(*status, "done" | "cancelled"))
                .collect();
        }

        Self {
            project: single_value(pairs, "project")
                .filter(|value| !value.is_empty())
                .map(String::from),
            statuses,
            status_filter_explicit,
            sort: Sort::from_query(single_value(pairs, "sort")),
        }
    }

    fn query_pairs(&self) -> Vec<(String, String)> {
        let mut pairs = Vec::new();
        if self.status_filter_explicit {
            pairs.extend(
                self.statuses
                    .iter()
                    .map(|status| ("status".into(), (*status).into())),
            );
            pairs.push(("status_filter_present".into(), "1".into()));
        }
        if let Some(project) = &self.project {
            pairs.push(("project".into(), project.clone()));
        }
        pairs.push(("sort".into(), self.sort.value().into()));
        pairs
    }

    fn list_url(&self) -> String {
        url_with_query("/ui/tasks", &self.query_pairs())
    }
}

fn single_value<'a>(pairs: &'a [(String, String)], key: &str) -> Option<&'a str> {
    let mut values = pairs
        .iter()
        .filter(|(candidate, _)| candidate == key)
        .map(|(_, value)| value.as_str());
    let value = values.next()?;
    values.next().is_none().then_some(value)
}

#[derive(Clone, Copy, Debug)]
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
    params: Result<Query<Vec<(String, String)>>, QueryRejection>,
) -> Response {
    let pairs = params.map_or_else(|_| Vec::new(), |Query(params)| params);
    let filters = FilterState::from_pairs(&pairs);
    let all = match list_all_todos(store.as_ref(), None, usize::MAX).await {
        Ok(items) => items,
        Err(source) => {
            error!(error = %source, "web UI task list could not load");
            return detail::service_unavailable(&filters.list_url());
        }
    };

    let selected_project = filters.project.as_deref();
    let mut projects: BTreeSet<_> = all.iter().map(|item| item.project.as_str()).collect();
    if let Some(selected_project) = selected_project {
        projects.insert(selected_project);
    }

    let mut displayed = if let Some(project) = selected_project {
        match list_todos(store.as_ref(), project, None, None, PROJECT_SCAN_LIMIT).await {
            Ok(items) => items,
            Err(source) => {
                error!(error = %source, "web UI task list could not load");
                return detail::service_unavailable(&filters.list_url());
            }
        }
    } else {
        all.clone()
    };
    displayed.retain(|item| {
        if filters.status_filter_explicit {
            filters.statuses.contains(&item.status.as_str())
        } else {
            !matches!(item.status.as_str(), "done" | "cancelled")
        }
    });
    sort_items(&mut displayed, filters.sort);
    displayed.truncate(LIMIT);

    layout(
        "MemCan · Tasks",
        list_markup(&all, &displayed, &projects, &filters),
    )
    .into_response()
}

fn list_markup(
    all: &[TodoItem],
    displayed: &[TodoItem],
    projects: &BTreeSet<&str>,
    filters: &FilterState,
) -> Markup {
    let selected_project = filters.project.as_deref();
    let task_query = filters.query_pairs();
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
            fieldset {
                legend { "Status" }
                div class="status-options" {
                    @for status in valid_statuses() {
                        label class="status-option" {
                            input type="checkbox" name="status" value=(status) checked[filters.statuses.contains(status)];
                            (status)
                        }
                    }
                }
            }
            input type="hidden" name="status_filter_present" value="1";
            label for="sort" { "Sort" }
            select id="sort" name="sort" {
                @for (value, label) in [
                    ("priority", "priority"),
                    ("created", "created"),
                    ("project", "project"),
                    ("status", "status"),
                ] {
                    option value=(value) selected[filters.sort.value() == value] { (label) }
                }
            }
            button type="submit" { "Apply" }
            a href="/ui/tasks" { "Clear" }
        }

        @if all.is_empty() && selected_project.is_none() && !filters.status_filter_explicit {
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
                                td {
                                    a href=(url_with_query(
                                        &format!("/ui/tasks/{}", encode_task_id(&item.id)),
                                        &task_query,
                                    )) { (&item.title) }
                                }
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
        Sort::Priority => items.sort_by(default_order),
        Sort::Created => items.sort_by(|left, right| {
            left.created_at
                .cmp(&right.created_at)
                .then_with(|| left.id.cmp(&right.id))
        }),
        Sort::Project => items.sort_by(|left, right| {
            left.project
                .bytes()
                .map(|byte| byte.to_ascii_lowercase())
                .cmp(right.project.bytes().map(|byte| byte.to_ascii_lowercase()))
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
    use std::sync::Mutex;

    use async_trait::async_trait;
    use axum::body::to_bytes;
    use axum::http::StatusCode;
    use memcan_core::{
        error::Result,
        traits::{SearchResult, TableSchema, VectorPoint},
    };
    use serde_json::json;
    use serial_test::serial;

    use super::super::test_support::{app_with_error_store, app_with_items, base_items, get, item};
    use super::*;

    #[derive(Default)]
    struct RecordingStore {
        filters: Mutex<Vec<Option<String>>>,
    }

    #[async_trait]
    impl VectorStore for RecordingStore {
        async fn ensure_table(
            &self,
            _name: &str,
            _dims: usize,
            _schema: &dyn TableSchema,
        ) -> Result<()> {
            panic!("unexpected ensure_table call")
        }

        async fn upsert(
            &self,
            _table: &str,
            _points: &[VectorPoint],
            _schema: &dyn TableSchema,
        ) -> Result<()> {
            panic!("unexpected upsert call")
        }

        async fn search(
            &self,
            _table: &str,
            _vector: &[f32],
            _filter: Option<&str>,
            _limit: usize,
            _offset: usize,
        ) -> Result<Vec<SearchResult>> {
            panic!("unexpected search call")
        }

        async fn scroll(
            &self,
            _table: &str,
            filter: Option<&str>,
            _limit: usize,
            _offset: usize,
        ) -> Result<Vec<SearchResult>> {
            self.filters.lock().unwrap().push(filter.map(String::from));
            if filter == Some("project = 'target-project'") {
                return Ok(vec![SearchResult {
                    id: "TARGET-1".into(),
                    score: 0.0,
                    payload: json!({
                        "title": "Target task",
                        "project": "target-project",
                        "priority": "high",
                        "status": "pending",
                        "created_at": "2026-01-01T00:00:00Z",
                    }),
                }]);
            }
            Ok(Vec::new())
        }

        async fn count(&self, _table: &str, _filter: Option<&str>) -> Result<usize> {
            panic!("unexpected count call")
        }

        async fn delete(&self, _table: &str, _ids: &[String]) -> Result<()> {
            panic!("unexpected delete call")
        }

        async fn delete_by_filter(&self, _table: &str, _filter: &str) -> Result<usize> {
            panic!("unexpected delete_by_filter call")
        }

        async fn get(&self, _table: &str, _ids: &[String]) -> Result<Vec<SearchResult>> {
            panic!("unexpected get call")
        }
    }

    fn ordered(body: &str, ids: &[&str]) {
        let positions: Vec<_> = ids
            .iter()
            .map(|id| body.find(id).unwrap_or_else(|| panic!("missing {id}")))
            .collect();
        assert!(positions.windows(2).all(|pair| pair[0] < pair[1]));
    }

    fn status_checkbox(body: &str, status: &str, checked: bool) {
        let checked = if checked { " checked" } else { "" };
        assert!(
            body.contains(&format!(
                "<input type=\"checkbox\" name=\"status\" value=\"{status}\"{checked}>"
            )),
            "unexpected {status} checkbox state"
        );
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
        ordered(&body, &["T-HIGH-1", "T-BLK-A", "T-MED-1", "T-BLK-B"]);
        assert!(body.contains(
            "<form role=\"search\" aria-label=\"Task filters\" class=\"panel filters\" method=\"get\" action=\"/ui/tasks\">"
        ));
        assert!(!body.contains("<nav aria-label=\"Task filters\""));
        assert!(body.contains("<select id=\"project\" name=\"project\">"));
        assert!(body.contains("<fieldset><legend>Status</legend>"));
        assert!(
            body.contains("<input type=\"hidden\" name=\"status_filter_present\" value=\"1\">")
        );
        assert!(!body.contains("<select id=\"status\""));
        assert!(body.contains("<select id=\"sort\" name=\"sort\">"));
        assert!(body.contains("4 tasks · limit 500"));
        assert!(body.contains("●</span> blocked"));
        assert!(body.contains("<td>—</td>"));
        assert!(!body.contains("T-LOW-1"));
        assert!(body.contains("href=\"/ui/tasks/T-HIGH-1?"));
    }

    #[tokio::test]
    #[serial]
    async fn list_view_default_statuses_hide_done_and_cancelled() {
        let items = valid_statuses()
            .iter()
            .map(|status| {
                item(
                    &format!("STATUS-{status}"),
                    status,
                    "memcan",
                    "medium",
                    status,
                    None,
                    "2026-01-01T00:00:00Z",
                )
            })
            .collect();
        let app = app_with_items(items).await;
        let (_, body) = get(&app.router, "/ui/tasks").await;

        for status in ["pending", "in_progress", "blocked", "postponed"] {
            assert!(body.contains(&format!("STATUS-{status}")));
            status_checkbox(&body, status, true);
        }
        for status in ["done", "cancelled"] {
            assert!(!body.contains(&format!("STATUS-{status}")));
            status_checkbox(&body, status, false);
        }
        assert!(body.contains("4 tasks · limit 500"));
    }

    #[tokio::test]
    #[serial]
    async fn list_view_percent_encodes_task_ids_in_links() {
        let task = item(
            "dep/one?x#frag",
            "Dependency",
            "memcan",
            "medium",
            "pending",
            None,
            "2026-01-01T00:00:00Z",
        );
        let app = app_with_items(vec![task]).await;
        let (status, body) = get(&app.router, "/ui/tasks").await;

        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("href=\"/ui/tasks/dep%2Fone%3Fx%23frag?"));
        assert!(!body.contains("href=\"/ui/tasks/dep/one?x#frag\""));
    }

    #[tokio::test]
    #[serial]
    async fn list_view_detail_links_preserve_normalized_filters_and_reserved_characters() {
        let task = item(
            "TASK-1",
            "Reserved project task",
            "alpha&beta=gamma#frag",
            "medium",
            "pending",
            None,
            "2026-01-01T00:00:00Z",
        );
        let app = app_with_items(vec![task]).await;
        let (_, body) = get(
            &app.router,
            "/ui/tasks?project=alpha%26beta%3Dgamma%23frag&status=pending&sort=created",
        )
        .await;

        assert!(body.contains(
            "href=\"/ui/tasks/TASK-1?status=pending&amp;status_filter_present=1&amp;project=alpha%26beta%3Dgamma%23frag&amp;sort=created\""
        ));
        assert!(!body.contains("project=alpha&amp;beta=gamma#frag"));
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
        status_checkbox(&body, "pending", true);
        status_checkbox(&body, "blocked", false);
        assert!(body.contains("value=\"created\" selected"));
        assert!(body.contains("2 tasks · limit 500"));
    }

    #[tokio::test]
    #[serial]
    async fn list_view_round_trips_explicit_multi_status_selection() {
        let app = app_with_items(base_items()).await;
        let (_, body) = get(
            &app.router,
            "/ui/tasks?status=done&status=pending&status_filter_present=1",
        )
        .await;

        for id in ["T-MED-1", "T-LOW-1", "T-BLK-A", "T-BLK-B"] {
            assert!(body.contains(id));
        }
        assert!(!body.contains("T-HIGH-1"));
        status_checkbox(&body, "pending", true);
        status_checkbox(&body, "done", true);
        status_checkbox(&body, "blocked", false);
        assert!(body.contains("status=pending&amp;status=done&amp;status_filter_present=1"));
        assert!(body.contains("4 tasks · limit 500"));
    }

    #[tokio::test]
    #[serial]
    async fn list_view_explicit_empty_status_selection_shows_nothing() {
        let app = app_with_items(base_items()).await;
        let (_, body) = get(&app.router, "/ui/tasks?status_filter_present=1").await;

        assert!(body.contains("No tasks match this filter."));
        assert!(body.contains("0 tasks · limit 500"));
        for status in valid_statuses() {
            status_checkbox(&body, status, false);
        }
        for id in ["T-HIGH-1", "T-MED-1", "T-LOW-1", "T-BLK-A", "T-BLK-B"] {
            assert!(!body.contains(id));
        }
    }

    #[tokio::test]
    #[serial]
    async fn list_view_drops_invalid_statuses_without_losing_valid_ones() {
        let app = app_with_items(base_items()).await;
        let (_, body) = get(
            &app.router,
            "/ui/tasks?status=archived%26bad&status=blocked&status=done",
        )
        .await;

        assert!(body.contains("T-HIGH-1"));
        assert!(body.contains("T-LOW-1"));
        assert!(!body.contains("T-MED-1"));
        assert!(!body.contains("archived&amp;bad"));
        status_checkbox(&body, "blocked", true);
        status_checkbox(&body, "done", true);
    }

    #[tokio::test]
    #[serial]
    async fn list_view_deduplicates_repeated_statuses_in_navigation() {
        let app = app_with_items(base_items()).await;
        let (_, body) = get(
            &app.router,
            "/ui/tasks?status=pending&status=pending&sort=created",
        )
        .await;

        assert_eq!(body.matches("status=pending").count(), 3);
        assert!(!body.contains("status=pending&amp;status=pending"));
        status_checkbox(&body, "pending", true);
    }

    #[tokio::test]
    #[serial]
    async fn list_view_applies_all_deterministic_sort_modes() {
        let app = app_with_items(base_items()).await;
        let (_, created) = get(&app.router, "/ui/tasks?sort=created").await;
        ordered(&created, &["T-BLK-A", "T-BLK-B", "T-MED-1", "T-HIGH-1"]);

        let (_, project) = get(&app.router, "/ui/tasks?sort=project").await;
        ordered(&project, &["T-HIGH-1", "T-BLK-A", "T-BLK-B", "T-MED-1"]);

        let (_, status) = get(&app.router, "/ui/tasks?sort=status").await;
        ordered(&status, &["T-HIGH-1", "T-BLK-A", "T-MED-1", "T-BLK-B"]);
    }

    #[test]
    fn project_sort_is_ascii_case_insensitive_with_default_tie_breaking() {
        let mut items = vec![
            item(
                "LOWER-LOW",
                "Lowercase low priority",
                "alpha",
                "low",
                "pending",
                None,
                "2026-01-01T00:00:00Z",
            ),
            item(
                "UPPER-HIGH",
                "Uppercase high priority",
                "ALPHA",
                "high",
                "pending",
                None,
                "2026-01-02T00:00:00Z",
            ),
            item(
                "ZEBRA",
                "Zebra project",
                "Zebra",
                "high",
                "pending",
                None,
                "2026-01-01T00:00:00Z",
            ),
        ];

        sort_items(&mut items, Sort::Project);

        assert_eq!(
            items
                .iter()
                .map(|item| item.id.as_str())
                .collect::<Vec<_>>(),
            ["UPPER-HIGH", "LOWER-LOW", "ZEBRA"]
        );
    }

    #[test]
    fn priority_sort_uses_deterministic_default_order() {
        let mut items = vec![
            item(
                "Z-MEDIUM",
                "Later ID",
                "memcan",
                "medium",
                "pending",
                None,
                "2026-01-01T00:00:00Z",
            ),
            item(
                "A-MEDIUM",
                "Earlier ID",
                "memcan",
                "medium",
                "pending",
                None,
                "2026-01-01T00:00:00Z",
            ),
            item(
                "HIGH",
                "High priority",
                "memcan",
                "high",
                "pending",
                None,
                "2026-01-02T00:00:00Z",
            ),
        ];

        sort_items(&mut items, Sort::Priority);

        assert_eq!(
            items
                .iter()
                .map(|item| item.id.as_str())
                .collect::<Vec<_>>(),
            ["HIGH", "A-MEDIUM", "Z-MEDIUM"]
        );
    }

    #[tokio::test]
    async fn list_view_fetches_unfiltered_data_for_web_layer_status_filtering() {
        let store = Arc::new(RecordingStore::default());
        let extension: Arc<dyn VectorStore> = store.clone();
        let response = tasks(
            Extension(extension),
            Ok(Query(vec![("status".into(), "done".into())])),
        )
        .await;

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(store.filters.lock().unwrap().as_slice(), [None]);
    }

    #[tokio::test]
    async fn selected_project_uses_scoped_query_when_unscoped_scan_omits_target() {
        let store = Arc::new(RecordingStore::default());
        let extension: Arc<dyn VectorStore> = store.clone();
        let response = tasks(
            Extension(extension),
            Ok(Query(vec![("project".into(), "target-project".into())])),
        )
        .await;
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body = String::from_utf8(body.to_vec()).unwrap();

        assert!(body.contains("TARGET-1"));
        assert_eq!(
            store.filters.lock().unwrap().as_slice(),
            [None, Some("project = 'target-project'".into())]
        );
    }

    /// Store stub simulating a real backend: `scroll` truncates to the
    /// requested `limit` in raw storage order, before any priority sort runs.
    struct TruncatingScanStore {
        raw_order: Vec<TodoItem>,
    }

    #[async_trait]
    impl VectorStore for TruncatingScanStore {
        async fn ensure_table(&self, _: &str, _: usize, _: &dyn TableSchema) -> Result<()> {
            panic!("unexpected ensure_table call")
        }

        async fn upsert(&self, _: &str, _: &[VectorPoint], _: &dyn TableSchema) -> Result<()> {
            panic!("unexpected upsert call")
        }

        async fn search(
            &self,
            _: &str,
            _: &[f32],
            _: Option<&str>,
            _: usize,
            _: usize,
        ) -> Result<Vec<SearchResult>> {
            panic!("unexpected search call")
        }

        async fn scroll(
            &self,
            _table: &str,
            filter: Option<&str>,
            limit: usize,
            _offset: usize,
        ) -> Result<Vec<SearchResult>> {
            if filter != Some("project = 'target-project'") {
                return Ok(Vec::new());
            }
            Ok(self
                .raw_order
                .iter()
                .take(limit)
                .map(|item| SearchResult {
                    id: item.id.clone(),
                    score: 0.0,
                    payload: json!({
                        "title": item.title,
                        "project": item.project,
                        "priority": item.priority,
                        "status": item.status,
                        "created_at": item.created_at,
                    }),
                })
                .collect())
        }

        async fn count(&self, _: &str, _: Option<&str>) -> Result<usize> {
            panic!("unexpected count call")
        }

        async fn delete(&self, _: &str, _: &[String]) -> Result<()> {
            panic!("unexpected delete call")
        }

        async fn delete_by_filter(&self, _: &str, _: &str) -> Result<usize> {
            panic!("unexpected delete_by_filter call")
        }

        async fn get(&self, _: &str, _: &[String]) -> Result<Vec<SearchResult>> {
            panic!("unexpected get call")
        }
    }

    #[tokio::test]
    async fn selected_project_scan_reaches_high_priority_item_past_the_display_limit() {
        let mut raw_order: Vec<TodoItem> = (0..LIMIT)
            .map(|index| {
                item(
                    &format!("LOW-{index:03}"),
                    &format!("Low priority task {index}"),
                    "target-project",
                    "low",
                    "pending",
                    None,
                    "2026-01-01T00:00:00Z",
                )
            })
            .collect();
        // Placed after LIMIT items in raw storage order — a naive scan
        // truncated to the display limit before sorting would drop it.
        raw_order.push(item(
            "HIGH-1",
            "Urgent task",
            "target-project",
            "high",
            "pending",
            None,
            "2026-01-01T00:00:00Z",
        ));

        let store: Arc<dyn VectorStore> = Arc::new(TruncatingScanStore { raw_order });
        let response = tasks(
            Extension(store),
            Ok(Query(vec![("project".into(), "target-project".into())])),
        )
        .await;
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body = String::from_utf8(body.to_vec()).unwrap();

        assert!(
            body.contains("HIGH-1"),
            "high-priority task beyond the store's raw-order scan window must still surface"
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

        for id in ["T-HIGH-1", "T-MED-1", "T-BLK-A", "T-BLK-B"] {
            assert!(body.contains(id));
        }
        assert!(!body.contains("T-LOW-1"));
        ordered(&body, &["T-HIGH-1", "T-BLK-A", "T-MED-1", "T-BLK-B"]);
        assert!(!body.contains("archived_forever"));
        assert!(!body.contains("bogus_field"));
        assert!(!body.contains("bogus"));
        assert!(body.contains("value=\"priority\" selected"));
    }

    #[tokio::test]
    #[serial]
    async fn list_view_duplicate_scalars_fall_back_without_rejecting_page() {
        let app = app_with_items(base_items()).await;

        for uri in [
            "/ui/tasks?status=done&project=backend&project=memcan",
            "/ui/tasks?status=done&sort=created&sort=status",
        ] {
            let (status, body) = get(&app.router, uri).await;

            assert_eq!(status, StatusCode::OK);
            assert!(body.starts_with("<!DOCTYPE html>"));
            assert!(body.contains("<title>MemCan · Tasks</title>"));
            assert!(body.contains("4 tasks · limit 500"));
            assert!(!body.contains("T-LOW-1"));
            status_checkbox(&body, "pending", true);
            status_checkbox(&body, "done", false);
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

        let (_, status_filtered_body) = get(&filtered.router, "/ui/tasks?status=postponed").await;
        assert!(status_filtered_body.contains("No tasks match this filter."));
        assert!(!status_filtered_body.contains("No tasks yet."));

        let (_, cross_filtered_body) =
            get(&filtered.router, "/ui/tasks?project=backend&status=done").await;
        assert!(cross_filtered_body.contains("value=\"backend\" selected"));
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
