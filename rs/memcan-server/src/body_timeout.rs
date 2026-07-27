//! Bounded ingestion of incoming HTTP request bodies on the MCP route.
//!
//! The MCP transport collects a POST body in full before it looks up the
//! session or logs anything, and that collect has no timer of its own. A client
//! or proxy that opens a request and then stalls mid-body therefore parks the
//! call indefinitely with no trace in the server log — the request simply
//! vanishes. [`guard`] bounds that receive phase and reports it loudly.

use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use axum::extract::Request;
use axum::http::{StatusCode, header::CONTENT_LENGTH};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use http_body_util::BodyExt;
use http_body_util::combinators::UnsyncBoxBody;
use rmcp::transport::common::http_header::HEADER_SESSION_ID;
use tower_http::BoxError;
use tower_http::timeout::{TimeoutBody, TimeoutError};
use tracing::{debug, error, warn};

/// Request body handed to the guarded handler.
pub(crate) type GuardedBody = UnsyncBoxBody<Bytes, BoxError>;

/// Placeholder for a header the client did not send.
const ABSENT: &str = "-";

/// Run `inner` with a receive deadline on the request body.
///
/// `timeout` is an *idle* bound: it limits the wait for the next chunk of the
/// body and resets on every chunk received, so a large body that keeps arriving
/// is never cut off, while a stalled connection fails after one quiet interval.
/// It does not constrain the handler once the body is complete, nor a response
/// body such as the MCP SSE stream. `Duration::ZERO` disables the bound.
///
/// A body that stalls is logged at `error` and answered with
/// [`StatusCode::REQUEST_TIMEOUT`], replacing whatever the handler made of the
/// truncated read — the request never completed, so no other status is honest.
pub(crate) async fn guard<F, Fut>(req: Request, timeout: Duration, inner: F) -> Response
where
    F: FnOnce(Request<GuardedBody>) -> Fut,
    Fut: Future<Output = Response>,
{
    let (parts, body) = req.into_parts();

    debug!(
        method = %parts.method,
        session_id = header_str(&parts, HEADER_SESSION_ID),
        content_length = header_str(&parts, CONTENT_LENGTH.as_str()),
        "MCP request received"
    );

    if timeout.is_zero() {
        let body = body.map_err(BoxError::from).boxed_unsync();
        return inner(Request::from_parts(parts, body)).await;
    }

    let stalled = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&stalled);
    let timeout_secs = timeout.as_secs();
    let session_id = header_str(&parts, HEADER_SESSION_ID).to_owned();
    let body = TimeoutBody::new(timeout, body)
        .map_err(move |e: BoxError| {
            if e.is::<TimeoutError>() {
                flag.store(true, Ordering::Release);
                error!(
                    session_id = session_id.as_str(),
                    timeout_secs,
                    "MCP request body stalled before it was fully received; raise MEMCAN_BODY_READ_TIMEOUT if the payload is legitimate"
                );
            } else {
                warn!(session_id = session_id.as_str(), "MCP request body read failed before completion: {e}");
            }
            e
        })
        .boxed_unsync();

    let response = inner(Request::from_parts(parts, body)).await;
    if stalled.load(Ordering::Acquire) {
        return (
            StatusCode::REQUEST_TIMEOUT,
            "Request body was not fully received before the read timeout",
        )
            .into_response();
    }
    response
}

/// Header value as a printable string, or [`ABSENT`] when missing or not text.
fn header_str<'a>(parts: &'a axum::http::request::Parts, name: &str) -> &'a str {
    parts
        .headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .unwrap_or(ABSENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::io;
    use std::sync::Mutex;

    use axum::Router;
    use axum::body::Body;
    use axum::http::header::{ACCEPT, CONTENT_TYPE, HOST};
    use rmcp::ServerHandler;
    use rmcp::transport::streamable_http_server::{
        session::local::LocalSessionManager,
        tower::{StreamableHttpServerConfig, StreamableHttpService},
    };
    use tower::ServiceExt;
    use tracing::subscriber::DefaultGuard;
    use tracing_subscriber::fmt::MakeWriter;

    const TIMEOUT: Duration = Duration::from_secs(30);

    /// Body that never yields a chunk — a connection that opened and went quiet.
    fn stalled_body() -> Body {
        Body::from_stream(futures::stream::pending::<io::Result<Bytes>>())
    }

    /// Body whose read fails for a reason other than a stall.
    fn broken_body() -> Body {
        Body::from_stream(futures::stream::once(async {
            io::Result::<Bytes>::Err(io::Error::new(
                io::ErrorKind::ConnectionReset,
                "peer went away",
            ))
        }))
    }

    fn request(method: &str, body: Body) -> Request {
        Request::builder()
            .method(method)
            .uri("/mcp")
            .header(HEADER_SESSION_ID, "session-under-test")
            .body(body)
            .unwrap()
    }

    /// Minimal MCP server — enough for the transport to run real sessions.
    #[derive(Clone)]
    struct TestHandler;

    impl ServerHandler for TestHandler {}

    /// The production `/mcp` route, wired to a stub MCP server.
    fn mcp_app(timeout: Duration) -> Router {
        crate::serve::mcp_router(
            StreamableHttpService::new(
                || Ok(TestHandler),
                Arc::new(LocalSessionManager::default()),
                StreamableHttpServerConfig::default(),
            ),
            timeout,
        )
    }

    /// Open a real MCP session through the route and return its ID.
    ///
    /// The `Host` header is what hyper always supplies and the transport's
    /// DNS-rebinding check requires; a hand-built request must carry it too.
    async fn initialize(app: &Router) -> String {
        let request = Request::builder()
            .method("POST")
            .uri("/mcp")
            .header(HOST, "localhost")
            .header(ACCEPT, "application/json, text/event-stream")
            .header(CONTENT_TYPE, "application/json")
            .body(Body::from(
                r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"body-timeout-test","version":"0.0.0"}}}"#,
            ))
            .unwrap();

        let response = app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK, "initialize was rejected");
        response
            .headers()
            .get(HEADER_SESSION_ID)
            .expect("transport must assign a session id")
            .to_str()
            .unwrap()
            .to_owned()
    }

    /// Read the body to completion and echo the outcome, like the MCP handler does.
    async fn collect_body(req: Request<GuardedBody>) -> Response {
        match req.into_body().collect().await {
            Ok(collected) => (StatusCode::OK, collected.to_bytes()).into_response(),
            // The status the MCP transport returns for an unreadable body.
            Err(e) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to read request body: {e}"),
            )
                .into_response(),
        }
    }

    async fn body_text(response: Response) -> String {
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        String::from_utf8(bytes.to_vec()).unwrap()
    }

    #[derive(Clone, Default)]
    struct CapturedLogs(Arc<Mutex<Vec<u8>>>);

    impl CapturedLogs {
        fn text(&self) -> String {
            String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
        }
    }

    impl io::Write for CapturedLogs {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl<'a> MakeWriter<'a> for CapturedLogs {
        type Writer = Self;

        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    /// Capture this thread's `tracing` output until the returned guard drops.
    fn capture_logs() -> (CapturedLogs, DefaultGuard) {
        let logs = CapturedLogs::default();
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::DEBUG)
            .with_ansi(false)
            .with_writer(logs.clone())
            .finish();
        (logs.clone(), tracing::subscriber::set_default(subscriber))
    }

    #[tokio::test]
    async fn complete_body_reaches_the_handler_untouched() {
        let (logs, _guard) = capture_logs();

        let response = guard(
            request("POST", Body::from(r#"{"jsonrpc":"2.0"}"#)),
            TIMEOUT,
            collect_body,
        )
        .await;

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(body_text(response).await, r#"{"jsonrpc":"2.0"}"#);
        assert!(logs.text().contains("MCP request received"));
        assert!(!logs.text().contains("stalled"));
    }

    #[tokio::test(start_paused = true)]
    async fn stalled_body_is_logged_and_answered_with_408() {
        let (logs, _guard) = capture_logs();

        let response = guard(request("POST", stalled_body()), TIMEOUT, collect_body).await;

        assert_eq!(response.status(), StatusCode::REQUEST_TIMEOUT);
        assert!(body_text(response).await.contains("not fully received"));

        let logs = logs.text();
        assert!(
            logs.contains("MCP request body stalled before it was fully received"),
            "missing stall log line in: {logs}"
        );
        assert!(
            logs.contains(r#"session_id="session-under-test""#),
            "logs: {logs}"
        );
        assert!(logs.contains("timeout_secs=30"), "logs: {logs}");
    }

    /// Without the guard the same stalled body hangs forever — the bug itself.
    #[tokio::test(start_paused = true)]
    async fn stalled_body_hangs_when_the_timeout_is_disabled() {
        let call = guard(
            request("POST", stalled_body()),
            Duration::ZERO,
            collect_body,
        );

        let outcome = tokio::time::timeout(Duration::from_secs(3600), call).await;

        assert!(outcome.is_err(), "disabled guard must not bound the read");
    }

    /// The receive bound must not become a handler bound: MCP tool calls can run
    /// for tens of seconds against the LLM after their body is complete.
    #[tokio::test(start_paused = true)]
    async fn slow_handler_after_a_complete_body_is_not_interrupted() {
        let response = guard(
            request("POST", Body::from("payload")),
            TIMEOUT,
            |req| async move {
                let body = req.into_body().collect().await.unwrap().to_bytes();
                tokio::time::sleep(TIMEOUT * 10).await;
                (StatusCode::OK, body).into_response()
            },
        )
        .await;

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(body_text(response).await, "payload");
    }

    /// A handler that drops the body unread arms no timer, however long it runs.
    #[tokio::test(start_paused = true)]
    async fn unread_body_never_times_out() {
        let (logs, _guard) = capture_logs();

        let response = guard(request("GET", Body::empty()), TIMEOUT, |req| async move {
            drop(req.into_body());
            tokio::time::sleep(TIMEOUT * 100).await;
            StatusCode::OK.into_response()
        })
        .await;

        assert_eq!(response.status(), StatusCode::OK);
        assert!(!logs.text().contains("stalled"), "logs: {}", logs.text());
    }

    /// The same guarantee against the real transport: an SSE stream opened by a
    /// real GET through the real route must outlive the body timeout, because
    /// the transport drops the GET's request body without ever polling it.
    #[tokio::test(start_paused = true)]
    async fn real_sse_get_outlives_the_body_timeout() {
        let (logs, _guard) = capture_logs();
        let app = mcp_app(TIMEOUT);
        let session_id = initialize(&app).await;

        let request = Request::builder()
            .method("GET")
            .uri("/mcp")
            .header(HOST, "localhost")
            .header(ACCEPT, "text/event-stream")
            .header(HEADER_SESSION_ID, &session_id)
            .body(Body::empty())
            .unwrap();
        let response = app.oneshot(request).await.unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[CONTENT_TYPE], "text/event-stream");

        // Leave the stream untouched for well past the body timeout, then
        // confirm it is still live: neither failed nor closed.
        let mut body = response.into_body();
        tokio::time::advance(TIMEOUT * 3).await;
        let frame = tokio::time::timeout(TIMEOUT, body.frame())
            .await
            .expect("idle SSE stream stopped producing keep-alives")
            .expect("SSE stream closed while idle");
        frame.expect("SSE stream failed while idle");

        let logs = logs.text();
        assert!(!logs.contains("stalled"), "logs: {logs}");
        assert!(logs.contains("method=GET"), "logs: {logs}");
    }

    /// End to end against the real transport: the stall is cut short by the
    /// guard instead of parking inside the transport's unbounded body read.
    #[tokio::test(start_paused = true)]
    async fn real_post_with_stalled_body_returns_408() {
        let (logs, _guard) = capture_logs();
        let app = mcp_app(TIMEOUT);
        let session_id = initialize(&app).await;

        let request = Request::builder()
            .method("POST")
            .uri("/mcp")
            .header(HOST, "localhost")
            .header(ACCEPT, "application/json, text/event-stream")
            .header(CONTENT_TYPE, "application/json")
            .header(HEADER_SESSION_ID, &session_id)
            .body(stalled_body())
            .unwrap();
        let response = app.oneshot(request).await.unwrap();

        assert_eq!(response.status(), StatusCode::REQUEST_TIMEOUT);
        assert!(
            logs.text().contains("MCP request body stalled"),
            "logs: {}",
            logs.text()
        );
    }

    /// The arrival line is the only trace a vanished request leaves, so its
    /// fields must carry real values.
    #[tokio::test]
    async fn arrival_line_records_the_method_and_content_length() {
        let (logs, _guard) = capture_logs();
        let payload = r#"{"jsonrpc":"2.0","method":"ping"}"#;

        let request = Request::builder()
            .method("DELETE")
            .uri("/mcp")
            .header(HEADER_SESSION_ID, "session-under-test")
            .header(CONTENT_LENGTH, payload.len().to_string())
            .body(Body::from(payload))
            .unwrap();
        let response = guard(request, TIMEOUT, collect_body).await;

        assert_eq!(response.status(), StatusCode::OK);

        let logs = logs.text();
        assert!(logs.contains("method=DELETE"), "logs: {logs}");
        assert!(
            logs.contains(&format!(r#"content_length="{}""#, payload.len())),
            "logs: {logs}"
        );
    }

    /// A body that fails for another reason is reported, but keeps the handler's
    /// own status — only a stall is a timeout.
    #[tokio::test(start_paused = true)]
    async fn broken_body_is_warned_and_keeps_the_handler_status() {
        let (logs, _guard) = capture_logs();

        let response = guard(request("POST", broken_body()), TIMEOUT, collect_body).await;

        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);

        let logs = logs.text();
        assert!(
            logs.contains("MCP request body read failed before completion"),
            "missing read-failure log line in: {logs}"
        );
        assert!(logs.contains("peer went away"), "logs: {logs}");
        assert!(!logs.contains("stalled"), "logs: {logs}");
    }

    #[tokio::test]
    async fn missing_session_header_is_logged_as_absent() {
        let (logs, _guard) = capture_logs();

        let req = Request::builder()
            .method("POST")
            .uri("/mcp")
            .body(Body::from("x"))
            .unwrap();
        let response = guard(req, TIMEOUT, collect_body).await;

        assert_eq!(response.status(), StatusCode::OK);
        assert!(
            logs.text().contains(r#"session_id="-""#),
            "logs: {}",
            logs.text()
        );
    }
}
