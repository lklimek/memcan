//! Web UI: the static single-page app plus its session-authenticated backend.
//!
//! The HTML shell and embedded assets are public: they carry no data, and the app's own login
//! view needs them before any session exists. Every data path is gated instead: the browser reads
//! tasks only through `POST /ui/api/mcp` (see [`api`]) with a session from `/ui/auth/*` (see
//! [`auth`]), and never holds `MEMCAN_API_KEY`. `/health` stays separate and unauthenticated.

mod api;
mod auth;

use std::{sync::Arc, time::Duration};

use axum::{
    Router,
    extract::{Path, Request},
    http::{HeaderValue, StatusCode, Uri, header},
    middleware,
    middleware::Next,
    response::{IntoResponse, Response},
    routing::{any, get, post},
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use memcan_core::config::Settings;
use tokio::sync::Semaphore;
use tracing::{Instrument, info, info_span, warn};

pub use api::McpDispatch;
use auth::{Account, Auth, MAX_AUTH_HEADER_LEN, Role};

const CONTENT_SECURITY_POLICY: HeaderValue = HeaderValue::from_static(
    "default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; img-src 'self'; font-src 'none'; object-src 'none'; base-uri 'none'; frame-ancestors 'none'; form-action 'none'",
);

const HTML: &str = "text/html; charset=utf-8";
const JAVASCRIPT: &str = "text/javascript; charset=utf-8";
const CSS: &str = "text/css; charset=utf-8";
const SVG: &str = "image/svg+xml";

const SHELL: &str = include_str!("static/index.html");

/// Embedded assets served under `/ui/static/<path>`; anything else there is a 404.
const ASSETS: &[(&str, &str, &str)] = &[
    ("app.js", JAVASCRIPT, include_str!("static/app.js")),
    ("auth.js", JAVASCRIPT, include_str!("static/auth.js")),
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
        "views/dashboard.js",
        JAVASCRIPT,
        include_str!("static/views/dashboard.js"),
    ),
    (
        "views/login.js",
        JAVASCRIPT,
        include_str!("static/views/login.js"),
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

struct UiState {
    auth: Auth,
    dispatch: McpDispatch,
    mcp_permits: Arc<Semaphore>,
    /// Idle bound for UI POST bodies (`MEMCAN_BODY_READ_TIMEOUT`).
    body_read_timeout: Duration,
}

/// Build the web UI when its shared account is fully and validly configured; `dispatch` is the
/// in-process MCP endpoint behind `/ui/api/mcp`.
pub fn router(settings: &Settings, dispatch: McpDispatch) -> Option<Router> {
    let auth = auth_from(settings)?;
    let state = Arc::new(UiState {
        auth,
        dispatch,
        mcp_permits: Arc::new(Semaphore::new(api::MAX_CONCURRENT_CALLS)),
        body_read_timeout: Duration::from_secs(settings.mcp_body_read_timeout_secs),
    });
    let backend = Router::new()
        .route("/ui/auth/session", get(auth::session))
        .route("/ui/auth/login", post(auth::login))
        .route("/ui/auth/logout", post(auth::logout))
        .route("/ui/auth/activity", post(auth::activity))
        .route("/ui/api/mcp", post(api::mcp))
        .route("/ui/auth", any(auth::not_found))
        .route("/ui/auth/", any(auth::not_found))
        .route("/ui/auth/{*rest}", any(auth::not_found))
        .route("/ui/api", any(auth::not_found))
        .route("/ui/api/", any(auth::not_found))
        .route("/ui/api/{*rest}", any(auth::not_found))
        .with_state(state)
        .layer(middleware::from_fn(request_span));
    let routes = Router::new()
        .route("/ui", get(shell))
        .route("/ui/", get(shell))
        .route("/ui/static/{*path}", get(asset))
        .route("/ui/{*rest}", get(shell))
        .merge(backend)
        .layer(middleware::from_fn(security_headers));
    Some(routes)
}

/// Validate the UI settings; any problem disables the UI with a warning naming the variable.
fn auth_from(settings: &Settings) -> Option<Auth> {
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
    // Basic credentials for the account must fit MAX_AUTH_HEADER_LEN, or proxy Basic sign-in
    // would always fail regardless of correctness.
    if STANDARD.encode(format!("{username}:{password}")).len() + "Basic ".len()
        > MAX_AUTH_HEADER_LEN
    {
        warn!(
            limit = MAX_AUTH_HEADER_LEN,
            "web UI disabled: MEMCAN_WEBUI_USERNAME and MEMCAN_WEBUI_PASSWORD are too long — their Basic Auth header would exceed the accepted limit. Shorten them."
        );
        return None;
    }
    let role = match settings.webui_role.as_deref() {
        None => Role::Viewer,
        Some(value) => match Role::parse(value) {
            Some(role) => role,
            None => {
                warn!(
                    role = value,
                    "web UI disabled: MEMCAN_WEBUI_ROLE must be viewer, editor or admin"
                );
                return None;
            }
        },
    };
    let idle = settings.webui_session_idle_secs;
    let absolute = settings.webui_session_absolute_secs;
    if idle == 0 || absolute == 0 || idle > absolute {
        warn!(
            idle,
            absolute,
            "web UI disabled: MEMCAN_WEBUI_SESSION_IDLE_SECS and MEMCAN_WEBUI_SESSION_ABSOLUTE_SECS must be positive, with idle not above absolute"
        );
        return None;
    }
    if settings.webui_allow_insecure_http {
        warn!(
            "web UI accepts sign-in over plain HTTP (MEMCAN_WEBUI_ALLOW_INSECURE_HTTP=true): passwords and session cookies can be intercepted; terminate TLS at the proxy and unset it"
        );
    }
    info!(
        idle_secs = idle,
        absolute_secs = absolute,
        "web UI mounted at /ui with session sign-in"
    );
    Some(Auth::new(
        Account::new(username, password, role),
        settings.webui_allow_insecure_http,
        &settings.mcp_allowed_hosts,
        Duration::from_secs(idle),
        Duration::from_secs(absolute),
    ))
}

/// Tag every security log line from the UI backend with the request it belongs to.
async fn request_span(request: Request, next: Next) -> Response {
    let span = info_span!(
        "webui",
        method = %request.method(),
        path = %request.uri().path()
    );
    next.run(request).instrument(span).await
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
    // Responses may carry task metadata or session tokens; keep them out of
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
        "/ui" | "/ui/" | "/ui/tasks" | "/ui/tasks/" | "/ui/dashboard" | "/ui/login"
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
    use std::sync::Mutex;

    use axum::{
        Router,
        body::{Body, to_bytes},
        http::{HeaderMap, Request, StatusCode, header},
    };
    use base64::engine::general_purpose::STANDARD;
    use memcan_core::config::Settings;
    use serde_json::{Value, json};
    use tower::ServiceExt;

    use super::*;

    const HOST: &str = "localhost:8190";
    const ORIGIN: &str = "https://localhost:8190";

    fn credentials() -> Settings {
        Settings {
            webui_username: Some("testuser".into()),
            webui_password: Some("testpass".into()),
            ..Settings::default()
        }
    }

    /// Requests the fake MCP dispatcher received.
    type Seen = Arc<Mutex<Vec<(HeaderMap, Value)>>>;

    fn fake_dispatch() -> (McpDispatch, Seen) {
        let seen: Seen = Arc::default();
        let recorder = Arc::clone(&seen);
        let dispatch: McpDispatch = Arc::new(move |request: Request<Body>| {
            let recorder = Arc::clone(&recorder);
            Box::pin(async move {
                let (parts, body) = request.into_parts();
                let message: Value =
                    serde_json::from_slice(&to_bytes(body, usize::MAX).await.unwrap()).unwrap();
                recorder
                    .lock()
                    .unwrap()
                    .push((parts.headers.clone(), message.clone()));
                let id = message["id"].clone();
                let result = match message["method"].as_str().unwrap() {
                    "notifications/initialized" => {
                        return StatusCode::ACCEPTED.into_response();
                    }
                    "tools/list" => json!({"tools": [
                        {"name": "get_todo"}, {"name": "delete_todo"}, {"name": "list_todos"},
                        {"name": "add_memory"}, {"name": "export_collection"},
                        {"name": "_import_records"}
                    ]}),
                    _ => json!({"echo": message["params"].clone()}),
                };
                (
                    [
                        (header::CONTENT_TYPE, "application/json"),
                        (header::HeaderName::from_static("mcp-session-id"), "inner"),
                    ],
                    json!({"jsonrpc": "2.0", "id": id, "result": result}).to_string(),
                )
                    .into_response()
            })
        });
        (dispatch, seen)
    }

    fn mounted(settings: &Settings) -> Router {
        let (dispatch, _) = fake_dispatch();
        match router(settings, dispatch) {
            Some(ui) => Router::new().merge(ui),
            None => Router::new(),
        }
    }

    fn app_with(settings: &Settings) -> (Router, Seen) {
        let (dispatch, seen) = fake_dispatch();
        (router(settings, dispatch).unwrap(), seen)
    }

    fn app() -> (Router, Seen) {
        app_with(&credentials())
    }

    fn get(uri: &str) -> Request<Body> {
        Request::builder()
            .uri(uri)
            .header(header::HOST, HOST)
            .body(Body::empty())
            .unwrap()
    }

    fn get_with(uri: &str, headers: &[(&str, &str)]) -> Request<Body> {
        let mut builder = Request::builder().uri(uri).header(header::HOST, HOST);
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
        builder.body(Body::empty()).unwrap()
    }

    /// A same-origin JSON POST; `extra` headers replace the defaults of the same name.
    fn post(uri: &str, body: &str, extra: &[(&str, &str)]) -> Request<Body> {
        let mut headers: Vec<(String, String)> = vec![
            ("host".into(), HOST.into()),
            ("origin".into(), ORIGIN.into()),
            ("sec-fetch-site".into(), "same-origin".into()),
            ("content-type".into(), "application/json".into()),
        ];
        for (name, value) in extra {
            headers.retain(|(existing, _)| existing != name);
            if !value.is_empty() {
                headers.push(((*name).into(), (*value).into()));
            }
        }
        let mut builder = Request::builder().method("POST").uri(uri);
        for (name, value) in headers {
            builder = builder.header(name, value);
        }
        builder.body(Body::from(body.to_string())).unwrap()
    }

    fn basic(value: &str) -> String {
        format!("Basic {}", STANDARD.encode(value))
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

    async fn body_json(response: Response) -> Value {
        serde_json::from_str(&body_text(response).await).unwrap()
    }

    fn set_cookie(response: &Response) -> String {
        response
            .headers()
            .get(header::SET_COOKIE)
            .unwrap()
            .to_str()
            .unwrap()
            .to_string()
    }

    /// `name=value` from a `Set-Cookie` header, as a browser would send it back.
    fn cookie_pair(set_cookie: &str) -> String {
        set_cookie.split(';').next().unwrap().to_string()
    }

    struct Signed {
        cookie: String,
        csrf: String,
    }

    async fn sign_in(app: &Router) -> Signed {
        let response = app
            .clone()
            .oneshot(post(
                "/ui/auth/login",
                r#"{"username":"testuser","password":"testpass"}"#,
                &[],
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let cookie = cookie_pair(&set_cookie(&response));
        let csrf = body_json(response).await["csrfToken"]
            .as_str()
            .unwrap()
            .to_string();
        Signed { cookie, csrf }
    }

    async fn call(app: &Router, signed: &Signed, message: Value) -> Response {
        app.clone()
            .oneshot(post(
                "/ui/api/mcp",
                &message.to_string(),
                &[
                    ("cookie", signed.cookie.as_str()),
                    ("x-csrf-token", signed.csrf.as_str()),
                ],
            ))
            .await
            .unwrap()
    }

    fn tool_call(id: u64, name: &str, arguments: Value) -> Value {
        json!({"jsonrpc": "2.0", "id": id, "method": "tools/call",
            "params": {"name": name, "arguments": arguments}})
    }

    async fn session_state(app: &Router, headers: &[(&str, &str)]) -> (Response, Value) {
        let response = app
            .clone()
            .oneshot(get_with("/ui/auth/session", headers))
            .await
            .unwrap();
        let has_cookie = response.headers().get(header::SET_COOKIE).cloned();
        assert!(response.headers().get(header::WWW_AUTHENTICATE).is_none());
        let status = response.status();
        let mut rebuilt = Response::new(Body::empty());
        *rebuilt.status_mut() = status;
        if let Some(cookie) = has_cookie {
            rebuilt.headers_mut().insert(header::SET_COOKIE, cookie);
        }
        (rebuilt, body_json(response).await)
    }

    // ── configuration ─────────────────────────────────────────────────────

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
            let response = mounted(&settings).oneshot(get("/ui/tasks")).await.unwrap();
            assert_eq!(response.status(), StatusCode::NOT_FOUND);
        }
    }

    #[test]
    fn webui_router_refuses_invalid_account_settings() {
        let (dispatch, _) = fake_dispatch();
        let invalid = [
            Settings {
                webui_password: Some("x".repeat(1024)),
                ..credentials()
            },
            Settings {
                webui_username: Some("user:name".into()),
                ..credentials()
            },
            Settings {
                webui_role: Some("superuser".into()),
                ..credentials()
            },
            Settings {
                webui_session_idle_secs: 0,
                ..credentials()
            },
            Settings {
                webui_session_idle_secs: 600,
                webui_session_absolute_secs: 60,
                ..credentials()
            },
        ];
        for settings in invalid {
            assert!(
                router(&settings, Arc::clone(&dispatch)).is_none(),
                "{settings:?}"
            );
        }
        for role in ["viewer", "Editor", "admin"] {
            let settings = Settings {
                webui_role: Some(role.into()),
                ..credentials()
            };
            assert!(router(&settings, Arc::clone(&dispatch)).is_some(), "{role}");
        }
    }

    // ── public shell and assets ───────────────────────────────────────────

    #[tokio::test]
    async fn webui_app_routes_serve_the_public_shell() {
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
            "/ui/login?returnTo=%2Fui%2Ftasks",
        ] {
            let response = mounted(&credentials()).oneshot(get(uri)).await.unwrap();
            assert_eq!(response.status(), StatusCode::OK, "{uri}");
            assert!(response.headers().get(header::WWW_AUTHENTICATE).is_none());
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
            "/ui/connect",
            "/ui/tasks/a/edit",
            "/ui/static",
            "/ui/unknown/deep/path",
        ] {
            let response = mounted(&credentials()).oneshot(get(uri)).await.unwrap();
            assert_eq!(response.status(), StatusCode::NOT_FOUND, "{uri}");
            assert_eq!(body_text(response).await, SHELL, "{uri}");
        }
    }

    #[tokio::test]
    async fn webui_unknown_backend_paths_are_json_404s_not_the_shell() {
        for uri in [
            "/ui/api/other",
            "/ui/api/",
            "/ui/api",
            "/ui/auth",
            "/ui/auth/whoami",
        ] {
            let response = mounted(&credentials()).oneshot(get(uri)).await.unwrap();
            assert_eq!(response.status(), StatusCode::NOT_FOUND, "{uri}");
            assert_eq!(body_json(response).await["error"], "not_found", "{uri}");
        }
        let response = mounted(&credentials())
            .oneshot(get("/ui/api/mcp"))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
    }

    #[tokio::test]
    async fn webui_assets_are_served_with_their_mime_type() {
        for (path, content_type, body) in ASSETS {
            let response = mounted(&credentials())
                .oneshot(get(&format!("/ui/static/{path}")))
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
            "/ui/static/views/connect.js",
        ] {
            let response = mounted(&credentials()).oneshot(get(uri)).await.unwrap();
            assert_eq!(response.status(), StatusCode::NOT_FOUND, "{uri}");
            assert!(
                response.headers().get(header::CONTENT_TYPE).is_none(),
                "{uri} must not fall back to HTML"
            );
        }
    }

    #[tokio::test]
    async fn webui_routes_and_assets_are_absent_without_credentials() {
        for uri in [
            "/ui/static/app.js",
            "/ui/tasks",
            "/ui/dashboard",
            "/ui/auth/session",
        ] {
            let response = mounted(&Settings::default())
                .oneshot(get(uri))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::NOT_FOUND, "{uri}");
        }
    }

    #[tokio::test]
    async fn webui_does_not_capture_api_paths() {
        for uri in ["/mcp", "/health"] {
            let response = mounted(&credentials()).oneshot(get(uri)).await.unwrap();
            assert_eq!(response.status(), StatusCode::NOT_FOUND, "{uri}");
            assert!(response.headers().get(header::CONTENT_TYPE).is_none());
        }
    }

    #[tokio::test]
    async fn webui_security_headers_cover_shell_assets_and_backend() {
        let (app, _) = app();
        for request in [
            get("/ui/tasks"),
            get("/ui/static/app.js"),
            get("/ui/static/nope.js"),
            get("/ui/auth/session"),
            post("/ui/api/mcp", "{}", &[]),
            post("/ui/auth/login", r#"{"username":"x","password":"y"}"#, &[]),
        ] {
            let uri = request.uri().to_string();
            let response = app.clone().oneshot(request).await.unwrap();
            let headers = response.headers();
            assert!(headers.get(header::WWW_AUTHENTICATE).is_none(), "{uri}");
            assert_eq!(headers.get("x-content-type-options").unwrap(), "nosniff");
            assert_eq!(
                headers.get("content-security-policy").unwrap(),
                "default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; img-src 'self'; font-src 'none'; object-src 'none'; base-uri 'none'; frame-ancestors 'none'; form-action 'none'"
            );
            assert_eq!(headers.get("cache-control").unwrap(), "no-store", "{uri}");
            assert_eq!(headers.get("pragma").unwrap(), "no-cache");
            assert_eq!(headers.get("referrer-policy").unwrap(), "no-referrer");
        }
    }

    // ── sign-in ───────────────────────────────────────────────────────────

    #[tokio::test]
    async fn webui_session_bootstrap_without_credentials_is_unauthenticated_without_challenge() {
        let (app, _) = app();
        for headers in [
            vec![],
            vec![("authorization", "Bearer master-key")],
            vec![("cookie", "__Host-memcan_ui=forged")],
        ] {
            let (response, body) = session_state(&app, &headers).await;
            assert_eq!(response.status(), StatusCode::OK);
            assert!(response.headers().get(header::SET_COOKIE).is_none());
            assert_eq!(
                body,
                json!({"authenticated": false, "insecureHttpAllowed": false})
            );
        }
    }

    #[tokio::test(start_paused = true)]
    async fn webui_login_rejects_wrong_credentials_generically() {
        let (app, _) = app();
        let mut bodies = Vec::new();
        for body in [
            r#"{"username":"testuser","password":"wrong"}"#,
            r#"{"username":"nobody","password":"testpass"}"#,
            r#"{"username":"TestUser","password":"testpass"}"#,
            r#"{"username":"","password":""}"#,
        ] {
            let response = app
                .clone()
                .oneshot(post("/ui/auth/login", body, &[]))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{body}");
            assert!(response.headers().get(header::SET_COOKIE).is_none());
            assert!(response.headers().get(header::WWW_AUTHENTICATE).is_none());
            bodies.push(body_json(response).await);
            // Stay clear of the sign-in backoff between cases.
            tokio::time::advance(Duration::from_secs(30)).await;
        }
        assert!(bodies.windows(2).all(|pair| pair[0] == pair[1]));
        assert_eq!(bodies[0]["error"], "invalid_credentials");
        assert_eq!(bodies[0]["message"], "Invalid username or password.");
    }

    #[tokio::test]
    async fn webui_login_issues_a_secure_session_cookie_and_csrf_token() {
        let (app, _) = app();
        let response = app
            .clone()
            .oneshot(post(
                "/ui/auth/login",
                r#"{"username":"testuser","password":"testpass"}"#,
                &[],
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let cookie = set_cookie(&response);
        let (pair, attributes) = cookie.split_once("; ").unwrap();
        let value = pair.strip_prefix("__Host-memcan_ui=").unwrap();
        assert!(value.len() >= 43, "256-bit opaque ID");
        assert_eq!(attributes, "Path=/; HttpOnly; Secure; SameSite=Strict");
        let body = body_json(response).await;
        assert_eq!(body["authenticated"], true);
        assert_eq!(body["username"], "testuser");
        assert_eq!(body["role"], "viewer");
        assert_eq!(body["method"], "session");
        assert_eq!(body["idleExpiresIn"], 1800);
        assert_eq!(body["absoluteExpiresIn"], 28800);
        let csrf = body["csrfToken"].as_str().unwrap();
        assert!(csrf.len() >= 43 && csrf != value);
        assert!(
            !body.to_string().contains(value),
            "cookie not readable by JS"
        );

        let (_, state) = session_state(&app, &[("cookie", pair)]).await;
        assert_eq!(state["authenticated"], true);
        assert_eq!(state["csrfToken"], csrf);
    }

    #[tokio::test]
    async fn webui_login_rotates_the_session() {
        let (app, _) = app();
        let first = sign_in(&app).await;
        let response = app
            .clone()
            .oneshot(post(
                "/ui/auth/login",
                r#"{"username":"testuser","password":"testpass"}"#,
                &[("cookie", first.cookie.as_str())],
            ))
            .await
            .unwrap();
        let second = cookie_pair(&set_cookie(&response));
        assert_ne!(second, first.cookie);
        let (_, state) = session_state(&app, &[("cookie", &first.cookie)]).await;
        assert_eq!(state["authenticated"], false, "old session revoked");
        let (_, state) = session_state(&app, &[("cookie", &second)]).await;
        assert_eq!(state["authenticated"], true);
    }

    #[tokio::test]
    async fn webui_valid_basic_credentials_are_exchanged_for_a_session() {
        let (app, _) = app();
        let authorization = basic("testuser:testpass");
        let (response, state) = session_state(&app, &[("authorization", &authorization)]).await;
        assert_eq!(state["authenticated"], true);
        assert_eq!(state["method"], "basic");
        assert_eq!(state["role"], "viewer");
        let cookie = cookie_pair(&set_cookie(&response));
        let signed = Signed {
            cookie,
            csrf: state["csrfToken"].as_str().unwrap().to_string(),
        };
        let response = call(
            &app,
            &signed,
            tool_call(1, "get_todo", json!({"todo_id": "a"})),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn webui_invalid_basic_credentials_are_unauthenticated_without_challenge() {
        let (app, _) = app();
        for authorization in [
            basic("testuser:wrong"),
            basic("wrong:testpass"),
            basic("testuser"),
            "Basic not-valid-base64!!!".to_string(),
            basic(&format!("testuser:{}", "x".repeat(1024))),
        ] {
            let (response, state) = session_state(&app, &[("authorization", &authorization)]).await;
            assert_eq!(response.status(), StatusCode::OK);
            assert!(response.headers().get(header::SET_COOKIE).is_none());
            assert_eq!(state["authenticated"], false, "{authorization}");
        }
    }

    #[tokio::test]
    async fn webui_basic_header_alone_never_authorizes_the_api() {
        let (app, seen) = app();
        let authorization = basic("testuser:testpass");
        let response = app
            .clone()
            .oneshot(post(
                "/ui/api/mcp",
                &tool_call(1, "get_todo", json!({})).to_string(),
                &[("authorization", authorization.as_str())],
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert!(response.headers().get(header::WWW_AUTHENTICATE).is_none());
        assert_eq!(body_json(response).await["error"], "unauthenticated");
        assert!(seen.lock().unwrap().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn webui_sign_in_backs_off_briefly_across_form_and_basic_attempts() {
        let (app, _) = app();
        for body in [
            r#"{"username":"testuser","password":"wrong"}"#,
            r#"{"username":"someone","password":"else"}"#,
        ] {
            let response = app
                .clone()
                .oneshot(post("/ui/auth/login", body, &[]))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        }
        session_state(&app, &[("authorization", &basic("testuser:wrong"))]).await;

        let correct = r#"{"username":"testuser","password":"testpass"}"#;
        let response = app
            .clone()
            .oneshot(post("/ui/auth/login", correct, &[]))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(response.headers().get(header::RETRY_AFTER).unwrap(), "1");
        assert_eq!(body_json(response).await["error"], "throttled");
        let (_, state) =
            session_state(&app, &[("authorization", &basic("testuser:testpass"))]).await;
        assert_eq!(state["authenticated"], false, "Basic is throttled too");

        tokio::time::advance(Duration::from_secs(1)).await;
        let response = app
            .clone()
            .oneshot(post("/ui/auth/login", correct, &[]))
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "the wait is short, not a lockout"
        );
    }

    #[tokio::test]
    async fn webui_repeated_stale_basic_header_counts_once() {
        let (app, _) = app();
        let stale = basic("testuser:old-password");
        for _ in 0..10 {
            let (_, state) = session_state(&app, &[("authorization", &stale)]).await;
            assert_eq!(state["authenticated"], false);
        }
        let response = app
            .clone()
            .oneshot(post(
                "/ui/auth/login",
                r#"{"username":"testuser","password":"testpass"}"#,
                &[],
            ))
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "page loads resending one cached credential do not build up a backoff"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn webui_posts_time_out_when_the_body_stalls() {
        let (app, seen) = app();
        let signed = sign_in(&app).await;
        let stalled = || {
            Body::from_stream(futures::stream::pending::<
                Result<bytes::Bytes, std::io::Error>,
            >())
        };
        let mut login = post("/ui/auth/login", "", &[]);
        *login.body_mut() = stalled();
        let response = app.clone().oneshot(login).await.unwrap();
        assert_eq!(response.status(), StatusCode::REQUEST_TIMEOUT);
        assert_eq!(body_json(response).await["error"], "request_timeout");

        let mut api = post(
            "/ui/api/mcp",
            "",
            &[
                ("cookie", signed.cookie.as_str()),
                ("x-csrf-token", signed.csrf.as_str()),
            ],
        );
        *api.body_mut() = stalled();
        let response = app.clone().oneshot(api).await.unwrap();
        assert_eq!(response.status(), StatusCode::REQUEST_TIMEOUT);
        assert!(seen.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn webui_login_requires_json_within_the_size_limit() {
        let (app, _) = app();
        let response = app
            .clone()
            .oneshot(post(
                "/ui/auth/login",
                "username=testuser&password=testpass",
                &[("content-type", "application/x-www-form-urlencoded")],
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
        let huge = format!(
            r#"{{"username":"testuser","password":"{}"}}"#,
            "x".repeat(8192)
        );
        let response = app
            .clone()
            .oneshot(post("/ui/auth/login", &huge, &[]))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
        let response = app
            .clone()
            .oneshot(post("/ui/auth/login", "not json", &[]))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    // ── request guards ────────────────────────────────────────────────────

    #[tokio::test]
    async fn webui_posts_reject_cross_origin_and_foreign_hosts() {
        let (app, seen) = app();
        let signed = sign_in(&app).await;
        let message = tool_call(1, "list_collections", json!({})).to_string();
        for (extra, code) in [
            (vec![("origin", "https://evil.example")], "cross_origin"),
            (vec![("origin", "")], "cross_origin"),
            (vec![("origin", "null")], "cross_origin"),
            (vec![("origin", "http://localhost:8190")], "cross_origin"),
            (
                vec![("origin", "https://localhost:8190.evil")],
                "cross_origin",
            ),
            (vec![("sec-fetch-site", "cross-site")], "cross_origin"),
            (vec![("sec-fetch-site", "same-site")], "cross_origin"),
            (
                vec![("host", "evil.example"), ("origin", "https://evil.example")],
                "host_not_allowed",
            ),
        ] {
            let mut headers = vec![
                ("cookie", signed.cookie.as_str()),
                ("x-csrf-token", signed.csrf.as_str()),
            ];
            headers.extend(extra.iter().copied());
            for uri in ["/ui/api/mcp", "/ui/auth/logout", "/ui/auth/activity"] {
                let response = app
                    .clone()
                    .oneshot(post(uri, &message, &headers))
                    .await
                    .unwrap();
                assert_eq!(response.status(), StatusCode::FORBIDDEN, "{uri} {extra:?}");
                assert_eq!(body_json(response).await["error"], code, "{uri} {extra:?}");
            }
        }
        let login = app
            .clone()
            .oneshot(post(
                "/ui/auth/login",
                r#"{"username":"testuser","password":"testpass"}"#,
                &[("origin", "https://evil.example")],
            ))
            .await
            .unwrap();
        assert_eq!(login.status(), StatusCode::FORBIDDEN);
        let (_, state) = session_state(&app, &[("cookie", &signed.cookie)]).await;
        assert_eq!(
            state["authenticated"], true,
            "rejected logout kept the session"
        );
        assert!(seen.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn webui_session_bootstrap_rejects_cross_site_and_foreign_hosts() {
        let (app, _) = app();
        let response = app
            .clone()
            .oneshot(get_with(
                "/ui/auth/session",
                &[("sec-fetch-site", "cross-site")],
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/ui/auth/session")
                    .header(header::HOST, "rebound.example")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn webui_allowed_hosts_extend_loopback_for_ui_requests() {
        let settings = Settings {
            mcp_allowed_hosts: vec!["agentic.lan".into(), "memcan.example:8190".into()],
            ..credentials()
        };
        let (app, _) = app_with(&settings);
        let login = r#"{"username":"testuser","password":"testpass"}"#;
        for host in [
            "agentic.lan:8190",
            "AGENTIC.lan",
            "memcan.example:8190",
            "127.0.0.1:8190",
            "[::1]:8190",
        ] {
            let origin = format!("https://{host}");
            let response = app
                .clone()
                .oneshot(post(
                    "/ui/auth/login",
                    login,
                    &[("host", host), ("origin", &origin)],
                ))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK, "{host}");
        }
        for host in ["memcan.example:9999", "agentic.lan.evil.example"] {
            let origin = format!("https://{host}");
            let response = app
                .clone()
                .oneshot(post(
                    "/ui/auth/login",
                    login,
                    &[("host", host), ("origin", &origin)],
                ))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::FORBIDDEN, "{host}");
        }
    }

    #[tokio::test]
    async fn webui_insecure_http_opt_in_uses_a_plain_cookie_and_accepts_http_origins() {
        let settings = Settings {
            webui_allow_insecure_http: true,
            ..credentials()
        };
        let (app, _) = app_with(&settings);
        let response = app
            .clone()
            .oneshot(post(
                "/ui/auth/login",
                r#"{"username":"testuser","password":"testpass"}"#,
                &[("origin", "http://localhost:8190")],
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let cookie = set_cookie(&response);
        let (pair, attributes) = cookie.split_once("; ").unwrap();
        assert!(pair.starts_with("memcan_ui="));
        assert_eq!(attributes, "Path=/ui; HttpOnly; SameSite=Strict");
        assert_eq!(body_json(response).await["insecureHttpAllowed"], true);
        let (_, state) = session_state(&app, &[]).await;
        assert_eq!(state["insecureHttpAllowed"], true);
        let (_, state) = session_state(&app, &[("cookie", pair)]).await;
        assert_eq!(state["authenticated"], true);
        let (_, state) = session_state(
            &app,
            &[("cookie", &pair.replace("memcan_ui", "__Host-memcan_ui"))],
        )
        .await;
        assert_eq!(
            state["authenticated"], false,
            "only the configured cookie name"
        );
    }

    #[tokio::test]
    async fn webui_api_rejects_missing_or_forged_csrf_tokens() {
        let (app, seen) = app();
        let signed = sign_in(&app).await;
        let other = sign_in(&app).await;
        let message = tool_call(1, "list_collections", json!({})).to_string();
        for csrf in ["", "forged", other.csrf.as_str()] {
            let response = app
                .clone()
                .oneshot(post(
                    "/ui/api/mcp",
                    &message,
                    &[("cookie", signed.cookie.as_str()), ("x-csrf-token", csrf)],
                ))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::FORBIDDEN, "{csrf}");
            assert_eq!(body_json(response).await["error"], "csrf");
        }
        assert!(seen.lock().unwrap().is_empty());
    }

    // ── BFF policy and dispatch ───────────────────────────────────────────

    #[tokio::test]
    async fn webui_api_dispatches_allowlisted_reads_without_client_credentials() {
        let (app, seen) = app();
        let signed = sign_in(&app).await;
        let initialize = json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {"protocolVersion": "2025-11-25", "capabilities": {},
                "clientInfo": {"name": "t", "version": "1"}}});
        for message in [
            initialize,
            tool_call(2, "get_todo", json!({"todo_id": "81cbe927"})),
            tool_call(3, "list_todos", json!({"project": "memcan"})),
            tool_call(4, "list_collections", json!({})),
            tool_call(5, "get_queue_status", json!({"limit": 10})),
            tool_call(
                6,
                "export_collection",
                json!({"collection": "todos", "limit": 500}),
            ),
            tool_call(
                7,
                "export_collection",
                json!({"collection": "todos", "filter": "project = 'o''brien'"}),
            ),
        ] {
            let response = call(&app, &signed, message.clone()).await;
            assert_eq!(response.status(), StatusCode::OK, "{message}");
            assert!(response.headers().get("mcp-session-id").is_none());
            assert_eq!(body_json(response).await["id"], message["id"]);
        }
        let response = call(
            &app,
            &signed,
            json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
        )
        .await;
        assert_eq!(response.status(), StatusCode::ACCEPTED);

        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 8);
        for (headers, _) in seen.iter() {
            assert_eq!(headers.get(header::HOST).unwrap(), "localhost");
            for name in [
                "cookie",
                "authorization",
                "x-csrf-token",
                "origin",
                "mcp-session-id",
            ] {
                assert!(headers.get(name).is_none(), "{name} forwarded");
            }
        }
    }

    #[tokio::test]
    async fn webui_api_filters_tools_list_to_the_ui_allowlist() {
        let (app, _) = app();
        let signed = sign_in(&app).await;
        let response = call(
            &app,
            &signed,
            json!({"jsonrpc": "2.0", "id": 9, "method": "tools/list"}),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let names: Vec<String> = body_json(response).await["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["name"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(names, ["get_todo", "list_todos", "export_collection"]);
    }

    #[tokio::test]
    async fn webui_api_denies_everything_outside_the_allowlist() {
        let (app, seen) = app();
        let signed = sign_in(&app).await;
        for (message, code) in [
            (tool_call(1, "delete_todo", json!({"todo_id": "a"})), -32602),
            (tool_call(2, "add_memory", json!({"memory": "x"})), -32602),
            (tool_call(3, "_import_records", json!({})), -32602),
            (
                tool_call(4, "export_collection", json!({"collection": "memories"})),
                -32602,
            ),
            (tool_call(5, "export_collection", json!({})), -32602),
            (
                tool_call(
                    6,
                    "export_collection",
                    json!({"collection": "todos", "filter": "project = 'a' OR 1=1"}),
                ),
                -32602,
            ),
            (
                tool_call(
                    7,
                    "export_collection",
                    json!({"collection": "todos", "filter": 1}),
                ),
                -32602,
            ),
            (tool_call(8, "get_todo", json!(["a"])), -32602),
            (
                json!({"jsonrpc": "2.0", "id": 9, "method": "tools/call", "params": {}}),
                -32602,
            ),
            (
                json!({"jsonrpc": "2.0", "id": 10, "method": "resources/list"}),
                -32601,
            ),
            (
                json!({"jsonrpc": "2.0", "id": 11, "method": "ping"}),
                -32601,
            ),
            (
                json!({"jsonrpc": "1.0", "id": 12, "method": "tools/list"}),
                -32600,
            ),
        ] {
            let response = call(&app, &signed, message.clone()).await;
            assert_eq!(response.status(), StatusCode::OK, "{message}");
            let body = body_json(response).await;
            assert_eq!(body["id"], message["id"], "{message}");
            assert_eq!(body["error"]["code"], code, "{message}");
        }
        for (raw, code) in [
            (
                json!([tool_call(1, "get_todo", json!({}))]).to_string(),
                -32600,
            ),
            (
                r#"{"jsonrpc":"2.0","method":"notifications/cancelled"}"#.into(),
                -32601,
            ),
            ("{not json".into(), -32700),
        ] {
            let response = app
                .clone()
                .oneshot(post(
                    "/ui/api/mcp",
                    &raw,
                    &[
                        ("cookie", signed.cookie.as_str()),
                        ("x-csrf-token", signed.csrf.as_str()),
                    ],
                ))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{raw}");
            assert_eq!(body_json(response).await["error"]["code"], code, "{raw}");
        }
        assert!(
            seen.lock().unwrap().is_empty(),
            "nothing denied was dispatched"
        );
    }

    #[tokio::test]
    async fn webui_api_forwards_exactly_the_validated_message() {
        let (app, seen) = app();
        let signed = sign_in(&app).await;
        // Duplicate keys: the policy sees the last name, and only that is forwarded.
        let raw = r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"delete_todo","name":"get_todo","arguments":{"todo_id":"a"}}}"#;
        let response = app
            .clone()
            .oneshot(post(
                "/ui/api/mcp",
                raw,
                &[
                    ("cookie", signed.cookie.as_str()),
                    ("x-csrf-token", signed.csrf.as_str()),
                ],
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let seen = seen.lock().unwrap();
        assert_eq!(seen[0].1["params"]["name"], "get_todo");
        assert!(!seen[0].1.to_string().contains("delete_todo"));
    }

    #[tokio::test]
    async fn webui_api_rejects_oversize_and_non_json_bodies() {
        let (app, seen) = app();
        let signed = sign_in(&app).await;
        let auth = [
            ("cookie", signed.cookie.as_str()),
            ("x-csrf-token", signed.csrf.as_str()),
        ];
        let huge = tool_call(1, "get_todo", json!({"todo_id": "x".repeat(70 * 1024)})).to_string();
        let response = app
            .clone()
            .oneshot(post("/ui/api/mcp", &huge, &auth))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
        let mut text = auth.to_vec();
        text.push(("content-type", "text/plain"));
        let response = app
            .clone()
            .oneshot(post("/ui/api/mcp", "{}", &text))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
        assert!(seen.lock().unwrap().is_empty());
    }

    // ── logout, expiry, restart ───────────────────────────────────────────

    #[tokio::test]
    async fn webui_logout_revokes_the_session_and_clears_the_cookie() {
        let (app, _) = app();
        let signed = sign_in(&app).await;
        let response = app
            .clone()
            .oneshot(post(
                "/ui/auth/logout",
                "",
                &[
                    ("cookie", signed.cookie.as_str()),
                    ("x-csrf-token", "forged"),
                ],
            ))
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::FORBIDDEN,
            "logout needs CSRF"
        );

        let response = app
            .clone()
            .oneshot(post(
                "/ui/auth/logout",
                "",
                &[
                    ("cookie", signed.cookie.as_str()),
                    ("x-csrf-token", signed.csrf.as_str()),
                ],
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert_eq!(
            set_cookie(&response),
            "__Host-memcan_ui=; Path=/; HttpOnly; Secure; SameSite=Strict; Max-Age=0"
        );
        let response = call(&app, &signed, tool_call(1, "list_collections", json!({}))).await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test(start_paused = true)]
    async fn webui_sessions_expire_when_idle_and_activity_extends_only_idle() {
        let settings = Settings {
            webui_session_idle_secs: 60,
            webui_session_absolute_secs: 150,
            ..credentials()
        };
        let (app, _) = app_with(&settings);
        let list = || tool_call(1, "list_collections", json!({}));

        let idle = sign_in(&app).await;
        tokio::time::advance(Duration::from_secs(59)).await;
        assert_eq!(call(&app, &idle, list()).await.status(), StatusCode::OK);
        tokio::time::advance(Duration::from_secs(2)).await;
        assert_eq!(
            call(&app, &idle, list()).await.status(),
            StatusCode::UNAUTHORIZED,
            "API calls (background polls) do not extend the idle timeout"
        );

        let active = sign_in(&app).await;
        let touch = || {
            post(
                "/ui/auth/activity",
                "",
                &[
                    ("cookie", active.cookie.as_str()),
                    ("x-csrf-token", active.csrf.as_str()),
                ],
            )
        };
        for _ in 0..2 {
            tokio::time::advance(Duration::from_secs(50)).await;
            let response = app.clone().oneshot(touch()).await.unwrap();
            assert_eq!(response.status(), StatusCode::OK);
        }
        tokio::time::advance(Duration::from_secs(40)).await;
        assert_eq!(call(&app, &active, list()).await.status(), StatusCode::OK);
        let response = app.clone().oneshot(touch()).await.unwrap();
        assert_eq!(body_json(response).await["absoluteExpiresIn"], 10);
        tokio::time::advance(Duration::from_secs(10)).await;
        assert_eq!(
            call(&app, &active, list()).await.status(),
            StatusCode::UNAUTHORIZED,
            "absolute lifetime is fixed"
        );
        let response = app.clone().oneshot(touch()).await.unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn webui_restart_invalidates_sessions() {
        let (before, _) = app();
        let signed = sign_in(&before).await;
        let (restarted, _) = app();
        let response = call(
            &restarted,
            &signed,
            tool_call(1, "list_collections", json!({})),
        )
        .await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    // ── static content rules ──────────────────────────────────────────────

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

    #[test]
    fn webui_assets_never_mention_the_master_api_key_flow() {
        for (name, _, body) in ASSETS {
            for needle in ["Bearer", "MEMCAN_API_KEY", "API key", "'/mcp'"] {
                assert!(!body.contains(needle), "{name} mentions {needle}");
            }
        }
    }
}
