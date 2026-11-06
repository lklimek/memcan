//! Static single-page web UI with fail-closed Basic Auth.
//!
//! Serves only embedded, allowlisted assets. The browser loads all task and server data
//! itself from `/health` and `/mcp`; nothing here reads the vector store.

use axum::{
    Router,
    extract::{Path, Request},
    http::{HeaderValue, StatusCode, Uri, header},
    middleware,
    middleware::Next,
    response::{IntoResponse, Response},
    routing::get,
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use memcan_core::config::Settings;
use subtle::ConstantTimeEq;
use tracing::warn;

const AUTH_CHALLENGE: HeaderValue = HeaderValue::from_static("Basic realm=\"MemCan Tasks\"");
const CONTENT_SECURITY_POLICY: HeaderValue = HeaderValue::from_static(
    "default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; img-src 'self'; font-src 'none'; object-src 'none'; base-uri 'none'; frame-ancestors 'none'; form-action 'none'",
);
const MAX_AUTH_HEADER_LEN: usize = 512;

const HTML: &str = "text/html; charset=utf-8";
const JAVASCRIPT: &str = "text/javascript; charset=utf-8";
const CSS: &str = "text/css; charset=utf-8";
const SVG: &str = "image/svg+xml";

const SHELL: &str = include_str!("static/index.html");

/// Embedded assets served under `/ui/static/<path>`; anything else there is a 404.
const ASSETS: &[(&str, &str, &str)] = &[
    ("app.js", JAVASCRIPT, include_str!("static/app.js")),
    (
        "components.js",
        JAVASCRIPT,
        include_str!("static/components.js"),
    ),
    ("data.js", JAVASCRIPT, include_str!("static/data.js")),
    ("mcp.js", JAVASCRIPT, include_str!("static/mcp.js")),
    ("prefs.js", JAVASCRIPT, include_str!("static/prefs.js")),
    ("router.js", JAVASCRIPT, include_str!("static/router.js")),
    ("store.js", JAVASCRIPT, include_str!("static/store.js")),
    (
        "views/connect.js",
        JAVASCRIPT,
        include_str!("static/views/connect.js"),
    ),
    (
        "views/dashboard.js",
        JAVASCRIPT,
        include_str!("static/views/dashboard.js"),
    ),
    (
        "views/task.js",
        JAVASCRIPT,
        include_str!("static/views/task.js"),
    ),
    (
        "views/tasks.js",
        JAVASCRIPT,
        include_str!("static/views/tasks.js"),
    ),
    ("styles.css", CSS, include_str!("static/styles.css")),
    ("icon.svg", SVG, include_str!("static/icon.svg")),
];

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
        .route("/ui", get(shell))
        .route("/ui/", get(shell))
        .route("/ui/static/{*path}", get(asset))
        .route("/ui/{*rest}", get(shell))
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
    // Pages are authenticated and may show task metadata; keep them out of
    // browser/proxy caches rather than relying on cache defaults.
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
        .headers_mut()
        .insert(header::PRAGMA, HeaderValue::from_static("no-cache"));
    response.headers_mut().insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    response
}

/// Routes the single-page app renders; any other `/ui/*` path gets the shell with a 404
/// status so the app can show its own Not found page.
fn is_app_route(path: &str) -> bool {
    matches!(
        path,
        "/ui" | "/ui/" | "/ui/tasks" | "/ui/tasks/" | "/ui/dashboard" | "/ui/connect"
    ) || path
        .strip_prefix("/ui/tasks/")
        .is_some_and(|id| !id.is_empty() && !id.contains('/'))
}

async fn shell(uri: Uri) -> Response {
    let status = if is_app_route(uri.path()) {
        StatusCode::OK
    } else {
        StatusCode::NOT_FOUND
    };
    (status, [(header::CONTENT_TYPE, HTML)], SHELL).into_response()
}

async fn asset(Path(path): Path<String>) -> Response {
    match ASSETS.iter().find(|(name, _, _)| *name == path) {
        Some((_, content_type, body)) => {
            ([(header::CONTENT_TYPE, *content_type)], *body).into_response()
        }
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

#[cfg(test)]
mod tests {
    use axum::{
        Router,
        body::{Body, to_bytes},
        http::{Request, StatusCode, header},
    };
    use base64::engine::general_purpose::STANDARD;
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

    async fn body_text(response: Response) -> String {
        String::from_utf8(
            to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap()
                .to_vec(),
        )
        .unwrap()
    }

    fn authorized(uri: &str) -> Request<Body> {
        request(uri, Some(&basic("testuser:testpass")))
    }

    #[tokio::test]
    async fn webui_auth_accepts_full_user_password_pair() {
        for authorization in [
            basic("testuser:testpass"),
            basic("testuser:testpass").replacen("Basic", "basic", 1),
        ] {
            let response = mounted(&credentials())
                .oneshot(request("/ui/tasks", Some(&authorization)))
                .await
                .unwrap();

            assert_eq!(response.status(), StatusCode::OK);
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
            .oneshot(authorized("/ui/tasks"))
            .await
            .unwrap();
        assert_eq!(correct.status(), StatusCode::OK);

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

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn webui_app_routes_serve_the_shell_including_legacy_task_links() {
        for uri in [
            "/ui",
            "/ui/",
            "/ui/tasks",
            "/ui/tasks/",
            "/ui/tasks?project=a%27b&status=pending&sort=created",
            "/ui/tasks/81cbe927-0000-4000-8000-000000000000",
            "/ui/tasks/a%2Fb%3Fc?project=memcan",
            "/ui/dashboard",
            "/ui/dashboard?project=x",
            "/ui/connect?returnTo=%2Fui%2Ftasks",
        ] {
            let response = mounted(&credentials())
                .oneshot(authorized(uri))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK, "{uri}");
            assert_eq!(
                response.headers().get(header::CONTENT_TYPE).unwrap(),
                "text/html; charset=utf-8",
                "{uri}"
            );
            assert_eq!(body_text(response).await, SHELL, "{uri}");
        }
    }

    #[tokio::test]
    async fn webui_unknown_app_routes_serve_the_shell_as_not_found() {
        for uri in [
            "/ui/memories",
            "/ui/tasks/a/edit",
            "/ui/static",
            "/ui/unknown/deep/path",
        ] {
            let response = mounted(&credentials())
                .oneshot(authorized(uri))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::NOT_FOUND, "{uri}");
            assert_eq!(body_text(response).await, SHELL, "{uri}");
        }
    }

    #[tokio::test]
    async fn webui_assets_are_served_with_their_mime_type() {
        for (path, content_type, body) in ASSETS {
            let response = mounted(&credentials())
                .oneshot(authorized(&format!("/ui/static/{path}")))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK, "{path}");
            assert_eq!(
                response.headers().get(header::CONTENT_TYPE).unwrap(),
                content_type,
                "{path}"
            );
            assert_eq!(body_text(response).await, *body, "{path}");
        }
        let js = mounted(&credentials())
            .oneshot(authorized("/ui/static/app.js"))
            .await
            .unwrap();
        assert_eq!(
            js.headers().get(header::CONTENT_TYPE).unwrap(),
            "text/javascript; charset=utf-8"
        );
    }

    #[tokio::test]
    async fn webui_unknown_or_traversing_asset_paths_are_plain_404s() {
        for uri in [
            "/ui/static/missing.js",
            "/ui/static/../mod.rs",
            "/ui/static/%2e%2e/mod.rs",
            "/ui/static/views/../app.js",
            "/ui/static/index.html",
            "/ui/static/views/",
            "/ui/static/APP.JS",
        ] {
            let response = mounted(&credentials())
                .oneshot(authorized(uri))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::NOT_FOUND, "{uri}");
            assert!(
                response.headers().get(header::CONTENT_TYPE).is_none(),
                "{uri} must not fall back to HTML"
            );
        }
    }

    #[tokio::test]
    async fn webui_assets_and_shell_require_basic_auth() {
        for uri in [
            "/ui/static/app.js",
            "/ui/static/styles.css",
            "/ui/tasks/x",
            "/ui/nope",
        ] {
            let response = mounted(&credentials())
                .oneshot(request(uri, None))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{uri}");
            assert!(body_text(response).await.is_empty(), "{uri}");
        }
    }

    #[tokio::test]
    async fn webui_routes_and_assets_are_absent_without_credentials() {
        for uri in ["/ui/static/app.js", "/ui/tasks", "/ui/dashboard"] {
            let response = mounted(&Settings::default())
                .oneshot(authorized(uri))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::NOT_FOUND, "{uri}");
        }
    }

    #[tokio::test]
    async fn webui_does_not_capture_api_paths() {
        for uri in ["/mcp", "/health"] {
            let response = mounted(&credentials())
                .oneshot(authorized(uri))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::NOT_FOUND, "{uri}");
            assert!(response.headers().get(header::CONTENT_TYPE).is_none());
        }
    }

    #[tokio::test]
    async fn webui_security_headers_cover_shell_assets_and_auth_failure() {
        for (uri, authorization, expected_status) in [
            ("/ui/tasks", None, StatusCode::UNAUTHORIZED),
            (
                "/ui/tasks",
                Some(basic("testuser:testpass")),
                StatusCode::OK,
            ),
            (
                "/ui/static/app.js",
                Some(basic("testuser:testpass")),
                StatusCode::OK,
            ),
            (
                "/ui/static/nope.js",
                Some(basic("testuser:testpass")),
                StatusCode::NOT_FOUND,
            ),
        ] {
            let response = mounted(&credentials())
                .oneshot(request(uri, authorization.as_deref()))
                .await
                .unwrap();
            assert_eq!(response.status(), expected_status, "{uri}");
            let headers = response.headers();
            assert_eq!(headers.get("x-content-type-options").unwrap(), "nosniff");
            assert_eq!(
                headers.get("content-security-policy").unwrap(),
                "default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; img-src 'self'; font-src 'none'; object-src 'none'; base-uri 'none'; frame-ancestors 'none'; form-action 'none'"
            );
            assert_eq!(headers.get("cache-control").unwrap(), "no-store");
            assert_eq!(headers.get("pragma").unwrap(), "no-cache");
            assert_eq!(headers.get("referrer-policy").unwrap(), "no-referrer");
        }
    }

    /// Every asset the shell or a module references must be in the allowlist.
    #[test]
    fn webui_shell_and_module_references_resolve_to_embedded_assets() {
        let names: Vec<&str> = ASSETS.iter().map(|(name, _, _)| *name).collect();
        let mut referenced = Vec::new();
        for part in SHELL.split("/ui/static/").skip(1) {
            referenced.push(part.split('"').next().unwrap().to_string());
        }
        for (name, _, body) in ASSETS.iter().filter(|(name, _, _)| name.ends_with(".js")) {
            let dir = name.rsplit_once('/').map_or("", |(dir, _)| dir);
            for line in body.lines().filter(|line| line.contains(" from '")) {
                let target = line
                    .split(" from '")
                    .nth(1)
                    .unwrap()
                    .split('\'')
                    .next()
                    .unwrap();
                let resolved = if let Some(rest) = target.strip_prefix("../") {
                    rest.to_string()
                } else if let Some(rest) = target.strip_prefix("./") {
                    if dir.is_empty() {
                        rest.to_string()
                    } else {
                        format!("{dir}/{rest}")
                    }
                } else {
                    panic!("{name} imports a non-relative module {target}");
                };
                referenced.push(resolved);
            }
        }
        assert!(referenced.len() > ASSETS.len());
        for reference in referenced {
            assert!(
                names.contains(&reference.as_str()),
                "missing asset {reference}"
            );
        }
        assert!(
            ASSETS.len() + 1 < 20,
            "initial asset fan-out must stay under the proxy burst"
        );
    }

    #[test]
    fn webui_shell_has_no_inline_script_or_style() {
        assert!(SHELL.contains("<script type=\"module\" src=\"/ui/static/app.js\"></script>"));
        assert_eq!(SHELL.matches("<script").count(), 1);
        assert!(!SHELL.contains("<style"));
        assert!(!SHELL.contains("style="));
        for (index, _) in SHELL.match_indices(" on") {
            let attribute: String = SHELL[index + 1..]
                .chars()
                .take_while(|c| c.is_ascii_alphabetic())
                .collect();
            assert!(
                !SHELL[index + 1 + attribute.len()..].starts_with('='),
                "inline event handler {attribute}"
            );
        }
        assert!(SHELL.contains("<noscript>"));
        for (name, _, body) in ASSETS {
            assert!(!body.contains("innerHTML"), "{name} must not use innerHTML");
            assert!(!body.contains("insertAdjacentHTML"), "{name}");
            assert!(!body.contains("eval("), "{name}");
        }
    }
}
