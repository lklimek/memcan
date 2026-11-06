//! `POST /ui/api/mcp`: the signed-in browser's only route to MCP.
//!
//! Authenticates the session cookie and CSRF token, admits only allowlisted JSON-RPC methods,
//! tools and arguments, then dispatches in-process to the MCP service. The forwarded request is
//! rebuilt from the validated message: client cookies, `Authorization` and MCP session headers are
//! never passed on, and no `MEMCAN_API_KEY` is involved.

use std::{future::Future, pin::Pin, sync::Arc};

use axum::{
    Json,
    body::{Body, to_bytes},
    extract::{Request, State},
    http::{HeaderMap, HeaderValue, Method, StatusCode, header},
    response::{IntoResponse, Response},
};
use serde_json::{Value, json};
use tokio::time::Instant;

use super::{
    UiState,
    auth::{UiError, check_csrf, require_json},
};

/// In-process MCP endpoint: receives a fresh `POST /mcp` request and returns its response.
pub type McpDispatch =
    Arc<dyn Fn(Request) -> Pin<Box<dyn Future<Output = Response> + Send>> + Send + Sync>;

/// Read-only tools the web UI may call. Write tools join only with their UI phase.
pub(super) const UI_TOOLS: &[&str] = &[
    "get_todo",
    "list_todos",
    "export_collection",
    "list_collections",
    "get_queue_status",
];
const METHODS: &[&str] = &[
    "initialize",
    "notifications/initialized",
    "tools/list",
    "tools/call",
];
const MAX_REQUEST_BODY: usize = 64 * 1024;
const MAX_TOOLS_LIST_BODY: usize = 4 * 1024 * 1024;
pub(super) const MAX_CONCURRENT_CALLS: usize = 8;

/// A JSON-RPC message the policy refuses.
#[derive(Debug, thiserror::Error)]
pub(super) enum Denied {
    #[error("Parse error")]
    Parse,
    #[error("Batch requests are not supported")]
    Batch,
    #[error("Invalid request")]
    Invalid,
    #[error("Method not available in the web UI")]
    Method,
    #[error("Tool not available in the web UI")]
    Tool,
    #[error("Arguments not allowed in the web UI")]
    Arguments,
}

impl Denied {
    fn code(&self) -> i64 {
        match self {
            Self::Parse => -32700,
            Self::Batch | Self::Invalid => -32600,
            Self::Method => -32601,
            Self::Tool | Self::Arguments => -32602,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub(super) enum ApiError {
    #[error(transparent)]
    Ui(#[from] UiError),
    #[error("{error}")]
    Denied { id: Value, error: Denied },
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        match self {
            Self::Ui(error) => error.into_response(),
            Self::Denied { id, error } => {
                // A request with an ID gets a normal JSON-RPC error reply; anything else is malformed.
                let status = if id.is_null() {
                    StatusCode::BAD_REQUEST
                } else {
                    StatusCode::OK
                };
                let body = json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": { "code": error.code(), "message": error.to_string() },
                });
                (status, Json(body)).into_response()
            }
        }
    }
}

fn deny(id: &Value, error: Denied) -> ApiError {
    ApiError::Denied {
        id: id.clone(),
        error,
    }
}

/// Raw SQL never comes from the browser: only `project = '<escaped literal>'` is accepted.
fn is_project_filter(filter: &str) -> bool {
    filter
        .strip_prefix("project = '")
        .and_then(|rest| rest.strip_suffix('\''))
        .is_some_and(|literal| !literal.replace("''", "").contains('\''))
}

/// Check one message against the allowlist; returns whether it is `tools/list`.
fn authorize(message: &Value) -> Result<bool, ApiError> {
    if message.is_array() {
        return Err(deny(&Value::Null, Denied::Batch));
    }
    let id = message.get("id").cloned().unwrap_or(Value::Null);
    if !(id.is_null() || id.is_number() || id.is_string()) {
        return Err(deny(&Value::Null, Denied::Invalid));
    }
    if message.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return Err(deny(&id, Denied::Invalid));
    }
    let method = message
        .get("method")
        .and_then(Value::as_str)
        .ok_or_else(|| deny(&id, Denied::Invalid))?;
    if !METHODS.contains(&method) {
        return Err(deny(&id, Denied::Method));
    }
    if method != "tools/call" {
        return Ok(method == "tools/list");
    }
    let params = message.get("params");
    let tool = params
        .and_then(|params| params.get("name"))
        .and_then(Value::as_str)
        .ok_or_else(|| deny(&id, Denied::Tool))?;
    if !UI_TOOLS.contains(&tool) {
        return Err(deny(&id, Denied::Tool));
    }
    let arguments = params.and_then(|params| params.get("arguments"));
    if arguments.is_some_and(|arguments| !arguments.is_object()) {
        return Err(deny(&id, Denied::Arguments));
    }
    if tool == "export_collection" {
        let collection = arguments.and_then(|arguments| arguments.get("collection"));
        let filter = arguments.and_then(|arguments| arguments.get("filter"));
        let filter_ok = match filter {
            None => true,
            Some(filter) => filter.as_str().is_some_and(is_project_filter),
        };
        if collection.and_then(Value::as_str) != Some("todos") || !filter_ok {
            return Err(deny(&id, Denied::Arguments));
        }
    }
    Ok(false)
}

/// Keep only allowlisted tools in a `tools/list` reply.
fn filter_tools(mut reply: Value) -> Value {
    if let Some(tools) = reply
        .get_mut("result")
        .and_then(|result| result.get_mut("tools"))
        .and_then(Value::as_array_mut)
    {
        tools.retain(|tool| {
            tool.get("name")
                .and_then(Value::as_str)
                .is_some_and(|name| UI_TOOLS.contains(&name))
        });
    }
    reply
}

pub(super) async fn mcp(
    State(state): State<Arc<UiState>>,
    headers: HeaderMap,
    body: Body,
) -> Result<Response, ApiError> {
    let auth = &state.auth;
    auth.check_post(&headers)?;
    let (_, session) = auth
        .current(&headers, Instant::now())
        .ok_or(UiError::Unauthenticated)?;
    check_csrf(&session, &headers)?;
    require_json(&headers)?;
    let body = to_bytes(body, MAX_REQUEST_BODY)
        .await
        .map_err(|_| UiError::PayloadTooLarge)?;
    let message: Value =
        serde_json::from_slice(&body).map_err(|_| deny(&Value::Null, Denied::Parse))?;
    let is_tools_list = authorize(&message)?;
    let _permit = Arc::clone(&state.mcp_permits)
        .try_acquire_owned()
        .map_err(|_| UiError::Busy)?;

    // Forward the validated message, re-serialized so the dispatcher parses exactly what was checked.
    let mut forwarded = Request::builder()
        .method(Method::POST)
        .uri("/mcp")
        .header(header::HOST, "localhost")
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::ACCEPT, "application/json, text/event-stream");
    if let Some(version) = headers
        .get("mcp-protocol-version")
        .filter(|value| value.len() <= 32)
    {
        forwarded = forwarded.header("mcp-protocol-version", version);
    }
    let forwarded = forwarded
        .body(Body::from(message.to_string()))
        .map_err(UiError::from)?;
    let (parts, body) = (state.dispatch)(forwarded).await.into_parts();

    let content_type = parts.headers.get(header::CONTENT_TYPE).cloned();
    let body = if is_tools_list && parts.status == StatusCode::OK {
        // Never pass an unfiltered tool list through.
        let bytes = to_bytes(body, MAX_TOOLS_LIST_BODY)
            .await
            .map_err(|_| UiError::Upstream)?;
        let reply: Value = serde_json::from_slice(&bytes).map_err(|_| UiError::Upstream)?;
        Body::from(filter_tools(reply).to_string())
    } else {
        body
    };
    let mut response = Response::new(body);
    *response.status_mut() = parts.status;
    if let Some(content_type) = content_type {
        response
            .headers_mut()
            .insert(header::CONTENT_TYPE, content_type);
    } else if parts.status == StatusCode::OK {
        response.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );
    }
    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_filter_accepts_only_escaped_equality() {
        for ok in [
            "project = 'memcan'",
            "project = ''",
            "project = 'o''brien & co?#/x'",
            "project = 'abc'''",
        ] {
            assert!(is_project_filter(ok), "{ok}");
        }
        for bad in [
            "",
            "project = 'a' OR '1'='1'",
            "project = 'a'' OR 1=1",
            "project = 'a'''' ",
            "project='a'",
            "status = 'pending'",
            "project = 'a' --",
            "project = '",
        ] {
            assert!(!is_project_filter(bad), "{bad}");
        }
    }
}
