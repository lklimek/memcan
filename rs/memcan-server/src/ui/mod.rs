//! Read-only task pages with fail-closed authentication and escaped HTML views.
//!
//! Handlers render task fields only; settings and internal errors never enter page bodies.

mod detail;
mod list;
#[cfg(test)]
mod test_support;

use axum::{
    Router,
    extract::Request,
    http::{HeaderValue, StatusCode, header},
    middleware,
    middleware::Next,
    response::{IntoResponse, Response},
    routing::get,
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use chrono::Utc;
use maud::{DOCTYPE, Markup, PreEscaped, html};
use memcan_core::config::Settings;
use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};
use subtle::ConstantTimeEq;
use tracing::warn;

const AUTH_CHALLENGE: HeaderValue = HeaderValue::from_static("Basic realm=\"MemCan Tasks\"");
const CONTENT_SECURITY_POLICY: HeaderValue = HeaderValue::from_static(
    "default-src 'self'; script-src 'none'; style-src 'self' 'unsafe-inline'; frame-ancestors 'none'; base-uri 'none'; object-src 'none'",
);
const MAX_AUTH_HEADER_LEN: usize = 512;
const PATH_SEGMENT_ENCODE_SET: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~');
const STYLES: &str = r#"
:root { color-scheme: light dark; font-family: system-ui, sans-serif; line-height: 1.5; }
body { margin: 0; color: #1f2933; background: #f5f7fa; }
header, main { width: min(72rem, calc(100% - 2rem)); margin-inline: auto; }
header { padding-block: 1rem; }
main { padding-bottom: 3rem; }
a { color: #075985; }
a:focus-visible, button:focus-visible, select:focus-visible { outline: 3px solid #f59e0b; outline-offset: 2px; }
.panel { background: white; border: 1px solid #cbd5e1; border-radius: .5rem; padding: 1rem; }
.filters { margin-block: 1rem; display: flex; flex-wrap: wrap; gap: .65rem; align-items: end; }
label { font-weight: 650; }
select, button { font: inherit; padding: .35rem .5rem; }
.table-wrap { overflow-x: auto; padding: 0; }
table { width: 100%; border-collapse: collapse; }
th, td { padding: .7rem; border-bottom: 1px solid #cbd5e1; text-align: left; vertical-align: top; }
th { white-space: nowrap; }
.task-count { color: #475569; }
.task-detail { margin-top: 1rem; }
.details { display: grid; grid-template-columns: max-content minmax(0, 1fr); gap: .5rem 1rem; }
.details dt { font-weight: 700; }
.details dd { margin: 0; overflow-wrap: anywhere; }
.description { white-space: pre-wrap; overflow-wrap: anywhere; }
.badge { display: inline-flex; gap: .35rem; align-items: center; border-radius: 999px; padding: .1rem .55rem; font-size: .875rem; font-weight: 650; }
.status-pending, .priority-low { background: #e2e8f0; color: #334155; }
.status-in-progress, .priority-medium { background: #dbeafe; color: #1e3a8a; }
.status-blocked, .priority-high { background: #fee2e2; color: #7f1d1d; }
.status-done { background: #dcfce7; color: #14532d; }
.status-postponed, .status-cancelled, .neutral { background: #e5e7eb; color: #374151; }
@media (prefers-color-scheme: dark) {
  body { color: #e5e7eb; background: #111827; }
  .panel { background: #1f2937; border-color: #475569; }
  a { color: #7dd3fc; }
}
"#;

/// Build the authenticated tasks UI when both shared credentials are configured.
pub fn router(settings: &Settings) -> Option<Router> {
    let (Some(username), Some(password)) = (
        settings
            .webui_username
            .as_deref()
            .filter(|value| !value.is_empty()),
        settings
            .webui_password
            .as_deref()
            .filter(|value| !value.is_empty()),
    ) else {
        warn!(
            "web UI disabled: set MEMCAN_WEBUI_USERNAME and MEMCAN_WEBUI_PASSWORD to enable /ui*"
        );
        return None;
    };
    if username.contains(':') {
        warn!(
            "web UI disabled: MEMCAN_WEBUI_USERNAME must not contain ':' (Basic Auth uses it as the credential separator)"
        );
        return None;
    }

    let expected = format!("{username}:{password}").into_bytes();
    // A Basic Auth header for these credentials must fit MAX_AUTH_HEADER_LEN, or every
    // request would fail the length filter below regardless of correctness.
    if STANDARD.encode(&expected).len() + "Basic ".len() > MAX_AUTH_HEADER_LEN {
        warn!(
            limit = MAX_AUTH_HEADER_LEN,
            "web UI disabled: MEMCAN_WEBUI_USERNAME and MEMCAN_WEBUI_PASSWORD are too long — their Basic Auth header would exceed the accepted limit, so every request would be rejected even with correct credentials. Shorten them."
        );
        return None;
    }
    // Login throttling belongs at the TLS proxy layer, where trusted client IPs are available.
    let routes = Router::new()
        .route("/ui", get(detail::redirect))
        .route("/ui/tasks", get(list::tasks))
        .route("/ui/tasks/{id}", get(detail::task))
        .layer(middleware::from_fn(move |request: Request, next: Next| {
            let expected = expected.clone();
            async move {
                let authorized = request
                    .headers()
                    .get(header::AUTHORIZATION)
                    .filter(|value| value.as_bytes().len() <= MAX_AUTH_HEADER_LEN)
                    .and_then(|value| value.to_str().ok())
                    .and_then(|value| value.split_once(' '))
                    .filter(|(scheme, _)| scheme.eq_ignore_ascii_case("Basic"))
                    .and_then(|(_, encoded)| STANDARD.decode(encoded).ok())
                    .is_some_and(|decoded| {
                        bool::from(decoded.as_slice().ct_eq(expected.as_slice()))
                    });

                if authorized {
                    next.run(request).await
                } else {
                    unauthorized()
                }
            }
        }))
        .layer(middleware::from_fn(security_headers));
    Some(routes)
}

fn unauthorized() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        [(header::WWW_AUTHENTICATE, AUTH_CHALLENGE)],
    )
        .into_response()
}

async fn security_headers(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    response.headers_mut().insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    response
        .headers_mut()
        .insert(header::CONTENT_SECURITY_POLICY, CONTENT_SECURITY_POLICY);
    response
}

pub(super) fn layout(title: &str, body: Markup) -> Markup {
    html! {
        (DOCTYPE)
        html lang="en" {
            head {
                meta charset="utf-8";
                meta name="viewport" content="width=device-width, initial-scale=1";
                title { (title) }
                style { (PreEscaped(STYLES)) }
            }
            body {
                header {
                    nav aria-label="Primary" { a href="/ui/tasks" { "MemCan · Tasks" } }
                }
                main { (body) }
            }
        }
    }
}

pub(super) fn status_badge(status: &str) -> Markup {
    let (class, glyph) = match status {
        "pending" => ("status-pending", "○"),
        "in_progress" => ("status-in-progress", "◐"),
        "blocked" => ("status-blocked", "●"),
        "done" => ("status-done", "✓"),
        "postponed" => ("status-postponed", "⏸"),
        "cancelled" => ("status-cancelled", "✕"),
        _ => ("neutral", "◆"),
    };
    html! { span class={ "badge " (class) } { span aria-hidden="true" { (glyph) } " " (status) } }
}

pub(super) fn priority_badge(priority: &str) -> Markup {
    let (class, glyph) = match priority {
        "high" => ("priority-high", "▲"),
        "medium" => ("priority-medium", "■"),
        "low" => ("priority-low", "▽"),
        _ => ("neutral", "◆"),
    };
    html! { span class={ "badge " (class) } { span aria-hidden="true" { (glyph) } " " (priority) } }
}

pub(super) fn encode_task_id(id: &str) -> impl std::fmt::Display + '_ {
    utf8_percent_encode(id, PATH_SEGMENT_ENCODE_SET)
}

pub(super) fn fmt_date_long(value: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(value)
        .map(|date| {
            date.with_timezone(&Utc)
                .format("%Y-%m-%d %H:%M UTC")
                .to_string()
        })
        .unwrap_or_else(|_| value.to_string())
}

pub(super) fn fmt_date_short(value: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(value)
        .map(|date| date.with_timezone(&Utc).format("%m-%d").to_string())
        .unwrap_or_else(|_| value.to_string())
}

#[cfg(test)]
mod tests {
    use axum::{
        Router,
        body::{Body, to_bytes},
        http::{Request, StatusCode, header},
    };
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use memcan_core::config::Settings;
    use tower::ServiceExt;

    use super::*;

    fn credentials() -> Settings {
        Settings {
            webui_username: Some("testuser".into()),
            webui_password: Some("testpass".into()),
            ..Settings::default()
        }
    }

    fn mounted(settings: &Settings) -> Router {
        match router(settings) {
            Some(ui) => Router::new().merge(ui),
            None => Router::new(),
        }
    }

    fn request(uri: &str, authorization: Option<&str>) -> Request<Body> {
        let mut builder = Request::builder().uri(uri);
        if let Some(value) = authorization {
            builder = builder.header(header::AUTHORIZATION, value);
        }
        builder.body(Body::empty()).unwrap()
    }

    fn basic(value: &str) -> String {
        format!("Basic {}", STANDARD.encode(value))
    }

    #[tokio::test]
    async fn webui_router_is_fail_closed_without_both_credentials() {
        let cases = [
            Settings::default(),
            Settings {
                webui_username: Some("testuser".into()),
                ..Settings::default()
            },
            Settings {
                webui_password: Some("testpass".into()),
                ..Settings::default()
            },
            Settings {
                webui_username: Some(String::new()),
                webui_password: Some("testpass".into()),
                ..Settings::default()
            },
        ];

        for settings in cases {
            let response = mounted(&settings)
                .oneshot(request("/ui/tasks", None))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::NOT_FOUND);
        }
    }

    #[tokio::test]
    async fn webui_auth_rejects_missing_and_wrong_credentials_with_exact_challenge() {
        for authorization in [
            None,
            Some(basic("").as_str()),
            Some(basic("testuser:wrong").as_str()),
            Some(basic("wrong:testpass").as_str()),
            Some(basic("TestUser:testpass").as_str()),
        ] {
            let response = mounted(&credentials())
                .oneshot(request("/ui/tasks", authorization))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
            assert_eq!(
                response.headers().get(header::WWW_AUTHENTICATE).unwrap(),
                "Basic realm=\"MemCan Tasks\""
            );
            assert!(
                to_bytes(response.into_body(), usize::MAX)
                    .await
                    .unwrap()
                    .is_empty()
            );
        }
    }

    #[tokio::test]
    async fn webui_auth_rejects_wrong_scheme_bad_base64_and_missing_colon() {
        let malformed = [
            "Bearer sometoken".to_string(),
            "Basic not-valid-base64!!!".to_string(),
            basic("testuser"),
        ];

        for authorization in malformed {
            let response = mounted(&credentials())
                .oneshot(request("/ui/tasks", Some(&authorization)))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
            assert_eq!(
                response.headers().get(header::WWW_AUTHENTICATE).unwrap(),
                "Basic realm=\"MemCan Tasks\""
            );
        }
    }

    #[tokio::test]
    async fn webui_auth_accepts_full_user_password_pair() {
        for authorization in [
            basic("testuser:testpass"),
            basic("testuser:testpass").replacen("Basic", "basic", 1),
        ] {
            // `/ui` needs no VectorStore extension, so a real 302 here proves the
            // authorized path actually succeeds (not just "wasn't rejected").
            let response = mounted(&credentials())
                .oneshot(request("/ui", Some(&authorization)))
                .await
                .unwrap();

            assert_eq!(response.status(), StatusCode::FOUND);
        }
    }

    #[tokio::test]
    async fn webui_auth_rejects_oversized_header_without_regressing_normal_credentials() {
        let oversized_authorization = basic(&format!("testuser:{}", "x".repeat(1024)));
        let oversized = mounted(&credentials())
            .oneshot(request("/ui/tasks", Some(&oversized_authorization)))
            .await
            .unwrap();
        assert_eq!(oversized.status(), StatusCode::UNAUTHORIZED);

        let correct = mounted(&credentials())
            .oneshot(request("/ui/tasks", Some(&basic("testuser:testpass"))))
            .await
            .unwrap();
        assert_ne!(correct.status(), StatusCode::UNAUTHORIZED);

        let incorrect = mounted(&credentials())
            .oneshot(request("/ui/tasks", Some(&basic("testuser:wrong"))))
            .await
            .unwrap();
        assert_eq!(incorrect.status(), StatusCode::UNAUTHORIZED);
    }

    #[test]
    fn webui_router_refuses_credentials_too_long_for_the_auth_header_limit() {
        let settings = Settings {
            webui_username: Some("testuser".into()),
            webui_password: Some("x".repeat(1024)),
            ..Settings::default()
        };

        assert!(
            router(&settings).is_none(),
            "a config whose Basic header can never fit MAX_AUTH_HEADER_LEN must fail closed \
             at startup, not lock out every request silently"
        );
    }

    #[test]
    fn webui_router_refuses_username_containing_basic_auth_separator() {
        for username in [":user", "user:name", "user:"] {
            let settings = Settings {
                webui_username: Some(username.into()),
                webui_password: Some("testpass".into()),
                ..Settings::default()
            };

            assert!(router(&settings).is_none());
        }
    }

    #[tokio::test]
    async fn webui_auth_allows_colons_in_password() {
        let settings = Settings {
            webui_username: Some("testuser".into()),
            webui_password: Some("test:pass".into()),
            ..Settings::default()
        };
        let response = mounted(&settings)
            .oneshot(request("/ui/tasks", Some(&basic("testuser:test:pass"))))
            .await
            .unwrap();

        assert_ne!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn webui_auth_runs_before_ui_redirect() {
        let unauthorized = mounted(&credentials())
            .oneshot(request("/ui", None))
            .await
            .unwrap();
        assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn webui_security_headers_cover_success_and_auth_failure() {
        let authorized = basic("testuser:testpass");
        // `/ui` needs no VectorStore extension, so the expected-status assertion below
        // proves the authorized case is a real success, not a masked handler failure.
        for (authorization, expected_status) in [
            (None, StatusCode::UNAUTHORIZED),
            (Some(authorized.as_str()), StatusCode::FOUND),
        ] {
            let response = mounted(&credentials())
                .oneshot(request("/ui", authorization))
                .await
                .unwrap();
            assert_eq!(response.status(), expected_status);
            assert_eq!(
                response.headers().get("x-content-type-options").unwrap(),
                "nosniff"
            );
            assert_eq!(
                response.headers().get("content-security-policy").unwrap(),
                "default-src 'self'; script-src 'none'; style-src 'self' 'unsafe-inline'; frame-ancestors 'none'; base-uri 'none'; object-src 'none'"
            );
        }
    }

    #[tokio::test]
    async fn webui_has_no_static_asset_route() {
        let response = mounted(&credentials())
            .oneshot(request(
                "/ui/static/style.css",
                Some(&basic("testuser:testpass")),
            ))
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[test]
    fn webui_layout_and_badges_are_semantic_script_free_and_escape_unknown_values() {
        let rendered = layout(
            "Tasks",
            maud::html! {
                h1 { "Tasks" }
                (status_badge("<script>future</script>"))
                (priority_badge("urgent-plus"))
            },
        )
        .into_string();

        assert!(rendered.starts_with("<!DOCTYPE html>"));
        assert!(rendered.contains("<html lang=\"en\">"));
        assert_eq!(rendered.matches("<main").count(), 1);
        assert_eq!(rendered.matches("<style").count(), 1);
        assert!(!rendered.contains("<script"));
        assert!(rendered.contains("&lt;script&gt;future&lt;/script&gt;"));
        assert!(rendered.contains("urgent-plus"));
        assert!(!rendered.contains("outline:none"));
        assert!(!rendered.contains("outline: none"));
    }

    #[test]
    fn webui_date_helpers_format_rfc3339_and_preserve_invalid_values() {
        assert_eq!(fmt_date_short("2026-07-15T13:42:00Z"), "07-15");
        assert_eq!(
            fmt_date_long("2026-07-15T13:42:00+00:00"),
            "2026-07-15 13:42 UTC"
        );
        assert_eq!(fmt_date_short("not-a-date"), "not-a-date");
        assert_eq!(fmt_date_long("not-a-date"), "not-a-date");
    }
}
