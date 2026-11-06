//! Web UI sign-in: one shared account, in-memory cookie sessions with CSRF tokens, login
//! throttling, and the Host / Origin / Fetch Metadata guards for `/ui/auth/*` and `/ui/api/*`.
//!
//! Two ways in, one principal: the login form (`POST /ui/auth/login`), or an
//! `Authorization: Basic` header for the same account (a reverse proxy passing its basicauth
//! header through, or credentials the browser cached), which `GET /ui/auth/session` exchanges
//! for a session. The API only accepts a session cookie plus its CSRF token, so the ambient Basic
//! header never authorizes a state-changing request by itself. No route sends a
//! `WWW-Authenticate` challenge.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex, PoisonError},
    time::Duration,
};

use axum::{
    Json,
    body::{Body, to_bytes},
    extract::State,
    http::{
        HeaderMap, HeaderValue, StatusCode,
        header::{self, InvalidHeaderValue},
    },
    response::{IntoResponse, Response},
};
use base64::{
    Engine as _,
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
};
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use tokio::time::Instant;
use tracing::{error, info, warn};

use super::UiState;

/// Longest `Authorization` header inspected for Basic credentials.
pub(super) const MAX_AUTH_HEADER_LEN: usize = 512;
const MAX_LOGIN_BODY: usize = 4 * 1024;
const MAX_COOKIE_HEADER_LEN: usize = 4 * 1024;
/// Sessions kept at once; the least recently active one is evicted beyond this.
const MAX_SESSIONS: usize = 256;
/// Failed sign-ins per username before it is locked for the rest of the window.
const MAX_FAILURES: u32 = 5;
const FAILURE_WINDOW: Duration = Duration::from_secs(15 * 60);
const MAX_TRACKED_USERNAMES: usize = 1024;
/// Sign-in attempts (form and Basic) accepted across all usernames per window.
const MAX_ATTEMPTS: u32 = 60;
const ATTEMPT_WINDOW: Duration = Duration::from_secs(60);
const CSRF_HEADER: &str = "x-csrf-token";
const SECURE_COOKIE: &str = "__Host-memcan_ui";
const INSECURE_COOKIE: &str = "memcan_ui";

#[derive(Debug, thiserror::Error)]
pub(super) enum UiError {
    #[error("Sign in to continue.")]
    Unauthenticated,
    #[error(
        "This host name is not allowed. Open the UI via localhost or add the name to MEMCAN_ALLOWED_HOSTS."
    )]
    HostNotAllowed,
    #[error("Cross-origin requests are not allowed.")]
    CrossOrigin,
    #[error("The security token is missing or invalid. Reload the page.")]
    Csrf,
    #[error("Invalid username or password.")]
    InvalidCredentials,
    #[error("Too many sign-in attempts. Try again later.")]
    Throttled(Duration),
    #[error("Requests must be sent as application/json.")]
    UnsupportedMediaType,
    #[error("The request is too large.")]
    PayloadTooLarge,
    #[error("The request could not be read.")]
    BadRequest,
    #[error("Not found.")]
    NotFound,
    #[error("The server is busy. Retry shortly.")]
    Busy,
    #[error("The MCP service returned an unreadable reply.")]
    Upstream,
    #[error("The server could not create a session.")]
    Random(#[source] getrandom::Error),
    #[error("The server could not build a response header.")]
    Header(#[from] InvalidHeaderValue),
    #[error("The server could not forward the request.")]
    Forward(#[from] axum::http::Error),
}

impl UiError {
    fn status_and_code(&self) -> (StatusCode, &'static str) {
        match self {
            Self::Unauthenticated => (StatusCode::UNAUTHORIZED, "unauthenticated"),
            Self::HostNotAllowed => (StatusCode::FORBIDDEN, "host_not_allowed"),
            Self::CrossOrigin => (StatusCode::FORBIDDEN, "cross_origin"),
            Self::Csrf => (StatusCode::FORBIDDEN, "csrf"),
            Self::InvalidCredentials => (StatusCode::UNAUTHORIZED, "invalid_credentials"),
            Self::Throttled(_) => (StatusCode::TOO_MANY_REQUESTS, "throttled"),
            Self::UnsupportedMediaType => {
                (StatusCode::UNSUPPORTED_MEDIA_TYPE, "unsupported_media_type")
            }
            Self::PayloadTooLarge => (StatusCode::PAYLOAD_TOO_LARGE, "payload_too_large"),
            Self::BadRequest => (StatusCode::BAD_REQUEST, "bad_request"),
            Self::NotFound => (StatusCode::NOT_FOUND, "not_found"),
            Self::Busy => (StatusCode::TOO_MANY_REQUESTS, "busy"),
            Self::Upstream => (StatusCode::BAD_GATEWAY, "upstream"),
            Self::Random(_) | Self::Header(_) | Self::Forward(_) => {
                (StatusCode::INTERNAL_SERVER_ERROR, "internal")
            }
        }
    }
}

impl IntoResponse for UiError {
    fn into_response(self) -> Response {
        let (status, code) = self.status_and_code();
        if status.is_server_error() {
            error!(error = ?self, "web UI request failed inside the server");
        }
        let retry_after = match &self {
            Self::Throttled(wait) => Some(wait.as_secs().max(1)),
            Self::Busy => Some(1),
            _ => None,
        };
        let body = json!({ "error": code, "message": self.to_string(), "retryAfter": retry_after });
        let mut response = (status, Json(body)).into_response();
        if let Some(seconds) = retry_after {
            response
                .headers_mut()
                .insert(header::RETRY_AFTER, HeaderValue::from(seconds));
        }
        response
    }
}

/// Fixed role of the shared account. Every role gets the viewer tool allowlist until write
/// features ship (a role never enables tools whose UI phase is not enabled).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Role {
    Viewer,
    Editor,
    Admin,
}

impl Role {
    pub(super) fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "viewer" => Some(Self::Viewer),
            "editor" => Some(Self::Editor),
            "admin" => Some(Self::Admin),
            _ => None,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Viewer => "viewer",
            Self::Editor => "editor",
            Self::Admin => "admin",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Method {
    Form,
    Basic,
}

/// The shared account. Credentials are compared as SHA-256 digests in constant time, with the
/// username and password always both compared, so a wrong username costs the same as a wrong
/// password. A slow password hash would add nothing: the plaintext already sits in the
/// environment, and online guessing is bounded by [`Throttle`].
pub(super) struct Account {
    username: Arc<str>,
    role: Role,
    username_digest: [u8; 32],
    password_digest: [u8; 32],
}

impl Account {
    pub(super) fn new(username: &str, password: &str, role: Role) -> Self {
        Self {
            username: username.into(),
            role,
            username_digest: digest(username.as_bytes()),
            password_digest: digest(password.as_bytes()),
        }
    }

    fn verify(&self, username: &str, password: &str) -> bool {
        let user = digest(username.as_bytes()).ct_eq(&self.username_digest);
        let pass = digest(password.as_bytes()).ct_eq(&self.password_digest);
        bool::from(user & pass)
    }
}

fn digest(value: &[u8]) -> [u8; 32] {
    let mut out = [0; 32];
    out.copy_from_slice(&Sha256::digest(value));
    out
}

fn random_token() -> Result<String, UiError> {
    let mut bytes = [0; 32];
    getrandom::fill(&mut bytes).map_err(UiError::Random)?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

/// Per-username failure counting plus a global attempt cap, both over fixed windows and bounded
/// in memory. A locked username unlocks when its window ends; there is no permanent lockout.
#[derive(Default)]
struct Throttle {
    state: Mutex<ThrottleState>,
}

#[derive(Default)]
struct ThrottleState {
    failures: HashMap<String, (u32, Instant)>,
    attempts: Option<(u32, Instant)>,
}

impl Throttle {
    /// Count an attempt, or return how long to wait before the next one is accepted.
    fn admit(&self, key: &str, now: Instant) -> Result<(), Duration> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let (count, start) = match state.attempts {
            Some((count, start)) if now.duration_since(start) < ATTEMPT_WINDOW => (count, start),
            _ => (0, now),
        };
        if count >= MAX_ATTEMPTS {
            return Err(ATTEMPT_WINDOW - now.duration_since(start));
        }
        if let Some(&(failures, since)) = state.failures.get(key) {
            let elapsed = now.duration_since(since);
            if elapsed >= FAILURE_WINDOW {
                state.failures.remove(key);
            } else if failures >= MAX_FAILURES {
                return Err(FAILURE_WINDOW - elapsed);
            }
        }
        state.attempts = Some((count + 1, start));
        Ok(())
    }

    /// Record a failure; returns true when this failure locked the username.
    fn failed(&self, key: &str, now: Instant) -> bool {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if !state.failures.contains_key(key) && state.failures.len() >= MAX_TRACKED_USERNAMES {
            state
                .failures
                .retain(|_, (_, since)| now.duration_since(*since) < FAILURE_WINDOW);
        }
        // When still full, unknown usernames share one bucket instead of growing the map.
        let key =
            if !state.failures.contains_key(key) && state.failures.len() >= MAX_TRACKED_USERNAMES {
                ""
            } else {
                key
            };
        let entry = state.failures.entry(key.to_string()).or_insert((0, now));
        entry.0 += 1;
        entry.0 == MAX_FAILURES
    }

    fn succeeded(&self, key: &str) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.failures.remove(key);
    }
}

struct Session {
    username: Arc<str>,
    role: Role,
    method: Method,
    csrf: String,
    created: Instant,
    last_activity: Instant,
}

/// What a request learns about its session.
pub(super) struct SessionView {
    username: Arc<str>,
    pub(super) role: Role,
    method: Method,
    csrf: String,
    idle_expires_in: Duration,
    absolute_expires_in: Duration,
}

/// In-memory sessions keyed by the SHA-256 of their opaque cookie value; a restart signs
/// everyone out.
struct SessionStore {
    sessions: Mutex<HashMap<[u8; 32], Session>>,
    idle: Duration,
    absolute: Duration,
}

impl SessionStore {
    fn view(&self, session: &Session, now: Instant) -> Option<SessionView> {
        let idle_left = self
            .idle
            .checked_sub(now.duration_since(session.last_activity))?;
        let absolute_left = self
            .absolute
            .checked_sub(now.duration_since(session.created))?;
        if idle_left.is_zero() || absolute_left.is_zero() {
            return None;
        }
        Some(SessionView {
            username: Arc::clone(&session.username),
            role: session.role,
            method: session.method,
            csrf: session.csrf.clone(),
            idle_expires_in: idle_left.min(absolute_left),
            absolute_expires_in: absolute_left,
        })
    }

    fn create(
        &self,
        account: &Account,
        method: Method,
        now: Instant,
    ) -> Result<(String, SessionView), UiError> {
        let id = random_token()?;
        let session = Session {
            username: Arc::clone(&account.username),
            role: account.role,
            method,
            csrf: random_token()?,
            created: now,
            last_activity: now,
        };
        let view = self.view(&session, now).ok_or(UiError::Unauthenticated)?;
        let mut sessions = self.sessions.lock().unwrap_or_else(PoisonError::into_inner);
        if sessions.len() >= MAX_SESSIONS {
            sessions.retain(|_, session| self.view(session, now).is_some());
        }
        if sessions.len() >= MAX_SESSIONS
            && let Some(oldest) = sessions
                .iter()
                .min_by_key(|(_, session)| session.last_activity)
                .map(|(key, _)| *key)
        {
            sessions.remove(&oldest);
        }
        sessions.insert(digest(id.as_bytes()), session);
        Ok((id, view))
    }

    /// Look a session up, dropping it when expired; `touch` records foreground activity.
    fn get(&self, id: &str, now: Instant, touch: bool) -> Option<SessionView> {
        let key = digest(id.as_bytes());
        let mut sessions = self.sessions.lock().unwrap_or_else(PoisonError::into_inner);
        let session = sessions.get_mut(&key)?;
        if self.view(session, now).is_none() {
            sessions.remove(&key);
            return None;
        }
        if touch {
            session.last_activity = now;
        }
        self.view(session, now)
    }

    fn revoke(&self, id: &str) {
        let mut sessions = self.sessions.lock().unwrap_or_else(PoisonError::into_inner);
        sessions.remove(&digest(id.as_bytes()));
    }
}

pub(super) struct Auth {
    account: Account,
    insecure_http: bool,
    allowed_hosts: Vec<String>,
    sessions: SessionStore,
    throttle: Throttle,
}

impl Auth {
    pub(super) fn new(
        account: Account,
        insecure_http: bool,
        allowed_hosts: &[String],
        idle: Duration,
        absolute: Duration,
    ) -> Self {
        Self {
            account,
            insecure_http,
            allowed_hosts: allowed_hosts.to_vec(),
            sessions: SessionStore {
                sessions: Mutex::default(),
                idle,
                absolute,
            },
            throttle: Throttle::default(),
        }
    }

    fn cookie_name(&self) -> &'static str {
        if self.insecure_http {
            INSECURE_COOKIE
        } else {
            SECURE_COOKIE
        }
    }

    fn cookie(&self, value: &str, clear: bool) -> Result<HeaderValue, UiError> {
        // `__Host-` requires `Secure` and `Path=/`; the plain-HTTP cookie can stay under /ui.
        let attributes = if self.insecure_http {
            "Path=/ui; HttpOnly; SameSite=Strict"
        } else {
            "Path=/; HttpOnly; Secure; SameSite=Strict"
        };
        let expiry = if clear { "; Max-Age=0" } else { "" };
        Ok(HeaderValue::try_from(format!(
            "{}={value}; {attributes}{expiry}",
            self.cookie_name()
        ))?)
    }

    fn session_id<'a>(&self, headers: &'a HeaderMap) -> Option<&'a str> {
        let name = self.cookie_name();
        headers
            .get_all(header::COOKIE)
            .iter()
            .filter(|value| value.len() <= MAX_COOKIE_HEADER_LEN)
            .filter_map(|value| value.to_str().ok())
            .flat_map(|value| value.split(';'))
            .filter_map(|pair| pair.trim().split_once('='))
            .find(|(key, value)| *key == name && !value.is_empty())
            .map(|(_, value)| value)
    }

    /// The request's live session and its cookie value.
    pub(super) fn current<'a>(
        &self,
        headers: &'a HeaderMap,
        now: Instant,
    ) -> Option<(&'a str, SessionView)> {
        let id = self.session_id(headers)?;
        Some((id, self.sessions.get(id, now, false)?))
    }

    /// Reject DNS-rebinding style requests: the Host must be loopback or listed in
    /// `MEMCAN_ALLOWED_HOSTS` (an entry without a port matches any port).
    pub(super) fn check_host(&self, headers: &HeaderMap) -> Result<(), UiError> {
        let host = headers
            .get(header::HOST)
            .and_then(|value| value.to_str().ok())
            .ok_or(UiError::HostNotAllowed)?;
        let (name, port) = split_host(host).ok_or(UiError::HostNotAllowed)?;
        let loopback = ["localhost", "127.0.0.1", "::1"]
            .iter()
            .any(|allowed| name.eq_ignore_ascii_case(allowed));
        let listed = self.allowed_hosts.iter().any(|entry| {
            split_host(entry).is_some_and(|(allowed, allowed_port)| {
                allowed.eq_ignore_ascii_case(name)
                    && (allowed_port.is_none() || allowed_port == port)
            })
        });
        if loopback || listed {
            Ok(())
        } else {
            Err(UiError::HostNotAllowed)
        }
    }

    /// Guards for every UI POST: allowed Host, same-origin Fetch Metadata, and an `Origin` equal
    /// to `<scheme>://<Host>`. The scheme must be `https` (TLS terminated by the proxy) unless
    /// plain HTTP was opted into; forwarding headers are never trusted.
    pub(super) fn check_post(&self, headers: &HeaderMap) -> Result<(), UiError> {
        self.check_host(headers)?;
        check_fetch_site(headers)?;
        let host = headers
            .get(header::HOST)
            .and_then(|value| value.to_str().ok())
            .ok_or(UiError::CrossOrigin)?;
        let origin = headers
            .get(header::ORIGIN)
            .and_then(|value| value.to_str().ok())
            .ok_or(UiError::CrossOrigin)?;
        let same_origin = |scheme: &str| {
            origin
                .strip_prefix(scheme)
                .and_then(|rest| rest.strip_prefix("://"))
                .is_some_and(|authority| authority.eq_ignore_ascii_case(host))
        };
        if same_origin("https") || (self.insecure_http && same_origin("http")) {
            Ok(())
        } else {
            Err(UiError::CrossOrigin)
        }
    }

    /// Check credentials under the throttle, whichever way they arrived.
    fn verify(&self, username: &str, password: &str, now: Instant) -> Result<(), UiError> {
        let key: String = username.trim().to_lowercase().chars().take(256).collect();
        self.throttle.admit(&key, now).map_err(UiError::Throttled)?;
        if self.account.verify(username, password) {
            self.throttle.succeeded(&key);
            return Ok(());
        }
        if self.throttle.failed(&key, now) {
            warn!(
                window_secs = FAILURE_WINDOW.as_secs(),
                "web UI sign-in locked for a username after repeated failures; it unlocks when the window ends"
            );
        }
        Err(UiError::InvalidCredentials)
    }

    fn signed_in(&self, view: &SessionView, cookie: Option<HeaderValue>) -> Response {
        let method = match view.method {
            Method::Form => "session",
            Method::Basic => "basic",
        };
        let mut response = Json(json!({
            "authenticated": true,
            "username": &*view.username,
            "role": view.role.as_str(),
            "method": method,
            "csrfToken": view.csrf,
            "idleExpiresIn": view.idle_expires_in.as_secs(),
            "absoluteExpiresIn": view.absolute_expires_in.as_secs(),
            "insecureHttpAllowed": self.insecure_http,
        }))
        .into_response();
        if let Some(cookie) = cookie {
            response.headers_mut().insert(header::SET_COOKIE, cookie);
        }
        response
    }
}

/// Split `host[:port]` or `[v6][:port]` (bare IPv6 has no port).
fn split_host(value: &str) -> Option<(&str, Option<&str>)> {
    if let Some(rest) = value.strip_prefix('[') {
        let (name, after) = rest.split_once(']')?;
        return match after {
            "" => Some((name, None)),
            _ => Some((name, Some(after.strip_prefix(':')?))),
        };
    }
    if value.matches(':').count() > 1 {
        return Some((value, None));
    }
    Some(match value.split_once(':') {
        Some((name, port)) => (name, Some(port)),
        None => (value, None),
    })
}

/// Browsers label cross-site requests; requests without Fetch Metadata are left to the other guards.
fn check_fetch_site(headers: &HeaderMap) -> Result<(), UiError> {
    match headers.get("sec-fetch-site") {
        None => Ok(()),
        Some(value) if value == "same-origin" => Ok(()),
        Some(_) => Err(UiError::CrossOrigin),
    }
}

pub(super) fn check_csrf(view: &SessionView, headers: &HeaderMap) -> Result<(), UiError> {
    let sent = headers.get(CSRF_HEADER).ok_or(UiError::Csrf)?;
    if bool::from(sent.as_bytes().ct_eq(view.csrf.as_bytes())) {
        Ok(())
    } else {
        Err(UiError::Csrf)
    }
}

pub(super) fn require_json(headers: &HeaderMap) -> Result<(), UiError> {
    let json = headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .is_some_and(|value| value.trim().eq_ignore_ascii_case("application/json"));
    if json {
        Ok(())
    } else {
        Err(UiError::UnsupportedMediaType)
    }
}

fn basic_credentials(headers: &HeaderMap) -> Option<(String, String)> {
    let decoded = headers
        .get(header::AUTHORIZATION)
        .filter(|value| value.len() <= MAX_AUTH_HEADER_LEN)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split_once(' '))
        .filter(|(scheme, _)| scheme.eq_ignore_ascii_case("Basic"))
        .and_then(|(_, encoded)| STANDARD.decode(encoded.trim()).ok())?;
    let decoded = String::from_utf8(decoded).ok()?;
    let (username, password) = decoded.split_once(':')?;
    Some((username.to_string(), password.to_string()))
}

/// `GET /ui/auth/session`: the browser's bootstrap. Returns the current session (principal, role,
/// CSRF token, expiry), or exchanges valid Basic credentials for a new session. Without either it
/// answers `authenticated: false` so the UI shows its login form; it never challenges.
pub(super) async fn session(
    State(state): State<Arc<UiState>>,
    headers: HeaderMap,
) -> Result<Response, UiError> {
    let auth = &state.auth;
    auth.check_host(&headers)?;
    check_fetch_site(&headers)?;
    let now = Instant::now();
    if let Some((_, view)) = auth.current(&headers, now) {
        return Ok(auth.signed_in(&view, None));
    }
    if let Some((username, password)) = basic_credentials(&headers)
        && auth.verify(&username, &password, now).is_ok()
    {
        let (id, view) = auth.sessions.create(&auth.account, Method::Basic, now)?;
        info!("web UI session started from proxy Basic credentials");
        return Ok(auth.signed_in(&view, Some(auth.cookie(&id, false)?)));
    }
    Ok(Json(json!({
        "authenticated": false,
        "insecureHttpAllowed": auth.insecure_http,
    }))
    .into_response())
}

#[derive(Deserialize)]
struct LoginRequest {
    username: String,
    password: String,
}

/// `POST /ui/auth/login`: verify the form credentials and start a fresh session, revoking any
/// session the request still carried. Login needs no CSRF token: there is nothing to forge with a
/// single shared account, and the Origin, Fetch Metadata and JSON-only checks still apply.
pub(super) async fn login(
    State(state): State<Arc<UiState>>,
    headers: HeaderMap,
    body: Body,
) -> Result<Response, UiError> {
    let auth = &state.auth;
    auth.check_post(&headers)?;
    require_json(&headers)?;
    let body = to_bytes(body, MAX_LOGIN_BODY)
        .await
        .map_err(|_| UiError::PayloadTooLarge)?;
    let request: LoginRequest = serde_json::from_slice(&body).map_err(|_| UiError::BadRequest)?;
    let now = Instant::now();
    auth.verify(&request.username, &request.password, now)?;
    if let Some(previous) = auth.session_id(&headers) {
        auth.sessions.revoke(previous);
    }
    let (id, view) = auth.sessions.create(&auth.account, Method::Form, now)?;
    info!("web UI sign-in succeeded");
    Ok(auth.signed_in(&view, Some(auth.cookie(&id, false)?)))
}

/// `POST /ui/auth/logout`: revoke the session and clear its cookie. Browser-cached Basic
/// credentials cannot be revoked by the server; the UI says so.
pub(super) async fn logout(
    State(state): State<Arc<UiState>>,
    headers: HeaderMap,
) -> Result<Response, UiError> {
    let auth = &state.auth;
    auth.check_post(&headers)?;
    if let Some((id, view)) = auth.current(&headers, Instant::now()) {
        check_csrf(&view, &headers)?;
        auth.sessions.revoke(id);
        info!("web UI session signed out");
    }
    Ok((
        StatusCode::NO_CONTENT,
        [(header::SET_COOKIE, auth.cookie("", true)?)],
    )
        .into_response())
}

/// `POST /ui/auth/activity`: foreground user activity extends the idle timeout; background polls
/// do not. The absolute lifetime never moves.
pub(super) async fn activity(
    State(state): State<Arc<UiState>>,
    headers: HeaderMap,
) -> Result<Response, UiError> {
    let auth = &state.auth;
    auth.check_post(&headers)?;
    let now = Instant::now();
    let (id, view) = auth
        .current(&headers, now)
        .ok_or(UiError::Unauthenticated)?;
    check_csrf(&view, &headers)?;
    let view = auth
        .sessions
        .get(id, now, true)
        .ok_or(UiError::Unauthenticated)?;
    Ok(Json(json!({
        "idleExpiresIn": view.idle_expires_in.as_secs(),
        "absoluteExpiresIn": view.absolute_expires_in.as_secs(),
    }))
    .into_response())
}

pub(super) async fn not_found() -> UiError {
    UiError::NotFound
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> SessionStore {
        SessionStore {
            sessions: Mutex::default(),
            idle: Duration::from_secs(60),
            absolute: Duration::from_secs(600),
        }
    }

    #[tokio::test(start_paused = true)]
    async fn session_store_is_bounded_and_evicts_the_least_recently_active() {
        let store = store();
        let account = Account::new("u", "p", Role::Viewer);
        let (first, _) = store
            .create(&account, Method::Form, Instant::now())
            .unwrap();
        tokio::time::advance(Duration::from_secs(1)).await;
        let (second, _) = store
            .create(&account, Method::Form, Instant::now())
            .unwrap();
        tokio::time::advance(Duration::from_secs(1)).await;
        assert!(store.get(&first, Instant::now(), true).is_some());
        for _ in 0..MAX_SESSIONS - 1 {
            store
                .create(&account, Method::Form, Instant::now())
                .unwrap();
        }
        let now = Instant::now();
        assert_eq!(store.sessions.lock().unwrap().len(), MAX_SESSIONS);
        assert!(
            store.get(&first, now, false).is_some(),
            "recently active kept"
        );
        assert!(
            store.get(&second, now, false).is_none(),
            "least active evicted"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn throttle_caps_attempts_across_usernames() {
        let throttle = Throttle::default();
        let now = Instant::now();
        for attempt in 0..MAX_ATTEMPTS {
            assert!(throttle.admit(&format!("user{attempt}"), now).is_ok());
        }
        let wait = throttle.admit("fresh", now).unwrap_err();
        assert_eq!(wait, ATTEMPT_WINDOW);
        tokio::time::advance(ATTEMPT_WINDOW).await;
        assert!(throttle.admit("fresh", Instant::now()).is_ok());
    }

    #[test]
    fn throttle_bounds_tracked_usernames() {
        let throttle = Throttle::default();
        let now = Instant::now();
        for index in 0..MAX_TRACKED_USERNAMES + 50 {
            throttle.failed(&format!("user{index}"), now);
        }
        let state = throttle.state.lock().unwrap();
        assert_eq!(state.failures.len(), MAX_TRACKED_USERNAMES + 1);
        assert_eq!(state.failures[""].0, 50, "overflow shares one bucket");
    }

    #[test]
    fn account_verification_needs_both_exact_values() {
        let account = Account::new("viewer", "s3cret:x", Role::Viewer);
        assert!(account.verify("viewer", "s3cret:x"));
        for (username, password) in [
            ("viewer", "s3cret"),
            ("Viewer", "s3cret:x"),
            ("viewer ", "s3cret:x"),
            ("", ""),
        ] {
            assert!(!account.verify(username, password));
        }
    }
}
