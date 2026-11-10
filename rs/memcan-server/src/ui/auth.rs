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
    collections::{HashMap, VecDeque},
    sync::{
        Arc, Mutex, PoisonError,
        atomic::{AtomicBool, Ordering},
    },
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
use bytes::Bytes;
use http_body_util::BodyExt;
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use tokio::time::Instant;
use tower_http::{
    BoxError,
    timeout::{TimeoutBody, TimeoutError},
};
use tracing::{debug, error, info, warn};

use super::UiState;

/// Longest `Authorization` header inspected for Basic credentials.
pub(super) const MAX_AUTH_HEADER_LEN: usize = 512;
const MAX_LOGIN_BODY: usize = 4 * 1024;
const MAX_COOKIE_HEADER_LEN: usize = 4 * 1024;
/// Sessions kept at once; the least recently active one is evicted beyond this.
const MAX_SESSIONS: usize = 256;
/// Consecutive failed sign-ins answered without delay; later ones back off exponentially.
const FREE_FAILURES: u32 = 3;
/// Longest single wait the backoff imposes. Sustained guessing settles at one try per this
/// interval (288 a day); a longer cap would slow guessing further but stretch each wait for the owner.
const MAX_BACKOFF: Duration = Duration::from_secs(5 * 60);
/// Quiet time after which earlier failures are forgotten.
const FAILURE_MEMORY: Duration = Duration::from_secs(15 * 60);
/// Rejected Basic headers remembered so a stale cached credential is not re-counted per page load.
const MAX_REJECTED_BASIC: usize = 32;
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
    #[error("The request body was not received in time.")]
    BodyTimeout,
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
            Self::BodyTimeout => (StatusCode::REQUEST_TIMEOUT, "request_timeout"),
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

impl Method {
    fn as_str(self) -> &'static str {
        match self {
            Self::Form => "form",
            Self::Basic => "basic",
        }
    }
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

/// Sign-in backoff for the single shared account. All failures (form and Basic, any username)
/// share one counter, so the response never reveals whether a username exists. After
/// [`FREE_FAILURES`] consecutive failures each further attempt must wait 1, 2, 4 … s, capped at
/// [`MAX_BACKOFF`], which bounds online guessing to one try per cap interval. Without a trusted
/// client identity the counter cannot tell an attacker from the owner: a client that keeps failing
/// can keep sign-in refused for as long as it continues. That residual is accepted because the UI
/// sits behind the proxy IP allowlist and per-client `/ui/auth/*` limit; existing sessions are
/// unaffected. Success or [`FAILURE_MEMORY`] of quiet resets it. Checking, verifying and recording
/// happen under one lock, so parallel requests cannot slip past the backoff.
#[derive(Default)]
struct Throttle {
    state: Mutex<ThrottleState>,
}

#[derive(Default)]
struct ThrottleState {
    failures: u32,
    last_failure: Option<Instant>,
}

/// Result of one throttled credential check.
enum Attempt {
    Accepted,
    Rejected { failures: u32 },
    Throttled(Duration),
}

fn backoff(failures: u32) -> Duration {
    match failures.checked_sub(FREE_FAILURES) {
        None => Duration::ZERO,
        Some(step) => Duration::from_secs(1 << step.min(9)).min(MAX_BACKOFF),
    }
}

impl Throttle {
    fn attempt(&self, now: Instant, verify: impl FnOnce() -> bool) -> Attempt {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(last) = state.last_failure {
            let elapsed = now.duration_since(last);
            if elapsed >= FAILURE_MEMORY {
                *state = ThrottleState::default();
            } else if let Some(wait) = backoff(state.failures).checked_sub(elapsed)
                && !wait.is_zero()
            {
                return Attempt::Throttled(wait);
            }
        }
        if verify() {
            *state = ThrottleState::default();
            return Attempt::Accepted;
        }
        state.failures = state.failures.saturating_add(1);
        state.last_failure = Some(now);
        Attempt::Rejected {
            failures: state.failures,
        }
    }
}

/// Kinds of refused request whose warnings are rate-limited, since unauthenticated clients can
/// trigger them at will.
#[derive(Debug, Clone, Copy)]
enum Rejection {
    Host,
    FetchSite,
    Origin,
    Csrf,
    Backoff,
}

/// Minimum spacing of warnings per [`Rejection`] kind; refusals in between are only counted.
const REJECTION_LOG_INTERVAL: Duration = Duration::from_secs(60);

/// Per-kind warning budget: the first refusal of a kind is logged, then at most one per
/// [`REJECTION_LOG_INTERVAL`] carrying the count suppressed since the previous warning.
#[derive(Default)]
struct RejectionLog {
    kinds: Mutex<[(Option<Instant>, u64); 5]>,
}

impl RejectionLog {
    /// `Some(suppressed)` when this refusal should be logged now.
    fn note(&self, kind: Rejection) -> Option<u64> {
        let now = Instant::now();
        let mut kinds = self.kinds.lock().unwrap_or_else(PoisonError::into_inner);
        let (last, suppressed) = &mut kinds[kind as usize];
        if last.is_some_and(|last| now.duration_since(last) < REJECTION_LOG_INTERVAL) {
            *suppressed += 1;
            return None;
        }
        *last = Some(now);
        Some(std::mem::take(suppressed))
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
    rejected_basic: Mutex<VecDeque<[u8; 32]>>,
    rejections: RejectionLog,
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
            rejected_basic: Mutex::default(),
            rejections: RejectionLog::default(),
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
            .unwrap_or_default();
        let Some((name, port)) = split_host(host) else {
            return Err(self.reject_host(host));
        };
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
            Err(self.reject_host(host))
        }
    }

    /// Guards for every UI POST: allowed Host, same-origin Fetch Metadata, and an `Origin` equal
    /// to `<scheme>://<Host>`. The scheme must be `https` (TLS terminated by the proxy) unless
    /// plain HTTP was opted into; forwarding headers are never trusted.
    pub(super) fn check_post(&self, headers: &HeaderMap) -> Result<(), UiError> {
        self.check_host(headers)?;
        self.check_fetch_site(headers)?;
        let host = headers
            .get(header::HOST)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default();
        let origin = headers
            .get(header::ORIGIN)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default();
        let same_origin = |scheme: &str| {
            origin
                .strip_prefix(scheme)
                .and_then(|rest| rest.strip_prefix("://"))
                .is_some_and(|authority| authority.eq_ignore_ascii_case(host))
        };
        if same_origin("https") || (self.insecure_http && same_origin("http")) {
            Ok(())
        } else {
            if let Some(suppressed) = self.rejections.note(Rejection::Origin) {
                warn!(
                    origin = ?clip(origin),
                    host = ?clip(host),
                    insecure_http_allowed = self.insecure_http,
                    suppressed_since_last_warning = suppressed,
                    "web UI request rejected: Origin is missing or not this Host (http:// origins need MEMCAN_WEBUI_ALLOW_INSECURE_HTTP=true)"
                );
            }
            Err(UiError::CrossOrigin)
        }
    }

    /// Check credentials under the throttle, whichever way they arrived.
    fn verify(
        &self,
        username: &str,
        password: &str,
        method: Method,
        now: Instant,
    ) -> Result<(), UiError> {
        match self
            .throttle
            .attempt(now, || self.account.verify(username, password))
        {
            Attempt::Accepted => Ok(()),
            Attempt::Rejected { failures } => {
                warn!(
                    method = method.as_str(),
                    consecutive_failures = failures,
                    next_wait_secs = backoff(failures).as_secs(),
                    "web UI sign-in failed: wrong username or password"
                );
                Err(UiError::InvalidCredentials)
            }
            Attempt::Throttled(wait) => {
                if let Some(suppressed) = self.rejections.note(Rejection::Backoff) {
                    warn!(
                        method = method.as_str(),
                        retry_after_secs = wait.as_secs().max(1),
                        suppressed_since_last_warning = suppressed,
                        "web UI sign-in refused while backing off after repeated failures"
                    );
                }
                Err(UiError::Throttled(wait))
            }
        }
    }

    /// Whether this exact Basic header was already rejected; browsers resend cached credentials on
    /// every page load, and one stale header must not count (or log) as a new failure each time.
    fn known_bad_basic(&self, header: &[u8]) -> bool {
        let key = digest(header);
        let rejected = self
            .rejected_basic
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        rejected.contains(&key)
    }

    fn remember_bad_basic(&self, header: &[u8]) {
        let mut rejected = self
            .rejected_basic
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if rejected.len() >= MAX_REJECTED_BASIC {
            rejected.pop_front();
        }
        rejected.push_back(digest(header));
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

impl Auth {
    /// Browsers label cross-site requests; requests without Fetch Metadata are left to the other guards.
    fn check_fetch_site(&self, headers: &HeaderMap) -> Result<(), UiError> {
        match headers.get("sec-fetch-site") {
            None => Ok(()),
            Some(value) if value == "same-origin" => Ok(()),
            Some(value) => {
                if let Some(suppressed) = self.rejections.note(Rejection::FetchSite) {
                    warn!(
                        sec_fetch_site = ?clip(value.to_str().unwrap_or_default()),
                        suppressed_since_last_warning = suppressed,
                        "web UI request rejected: the browser marked it as not same-origin"
                    );
                }
                Err(UiError::CrossOrigin)
            }
        }
    }

    fn reject_host(&self, host: &str) -> UiError {
        if let Some(suppressed) = self.rejections.note(Rejection::Host) {
            warn!(
                host = ?clip(host),
                suppressed_since_last_warning = suppressed,
                "web UI request rejected: Host is neither loopback nor listed in MEMCAN_ALLOWED_HOSTS"
            );
        }
        UiError::HostNotAllowed
    }

    pub(super) fn check_csrf(
        &self,
        view: &SessionView,
        headers: &HeaderMap,
    ) -> Result<(), UiError> {
        let valid = headers
            .get(CSRF_HEADER)
            .is_some_and(|sent| bool::from(sent.as_bytes().ct_eq(view.csrf.as_bytes())));
        if valid {
            return Ok(());
        }
        if let Some(suppressed) = self.rejections.note(Rejection::Csrf) {
            warn!(
                suppressed_since_last_warning = suppressed,
                "web UI request rejected: missing or stale CSRF token (another tab may have signed in again)"
            );
        }
        Err(UiError::Csrf)
    }
}

/// Bound attacker-controlled text before it reaches a log line.
pub(super) fn clip(value: &str) -> String {
    value.chars().take(80).collect()
}

/// Read a request body within `limit`, failing with `408` when it stalls for `timeout`
/// (`MEMCAN_BODY_READ_TIMEOUT` semantics: an idle bound that resets per chunk; zero disables it).
pub(super) async fn read_body(
    body: Body,
    limit: usize,
    timeout: Duration,
) -> Result<Bytes, UiError> {
    if timeout.is_zero() {
        return to_bytes(body, limit)
            .await
            .map_err(|_| UiError::PayloadTooLarge);
    }
    let stalled = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&stalled);
    let body = Body::new(
        TimeoutBody::new(timeout, body).map_err(move |error: BoxError| {
            if error.is::<TimeoutError>() {
                flag.store(true, Ordering::Release);
            }
            error
        }),
    );
    match to_bytes(body, limit).await {
        Ok(bytes) => Ok(bytes),
        Err(_) if stalled.load(Ordering::Acquire) => {
            warn!(
                timeout_secs = timeout.as_secs(),
                "web UI request body stalled before it was fully received; raise MEMCAN_BODY_READ_TIMEOUT if the client is legitimate"
            );
            Err(UiError::BodyTimeout)
        }
        Err(_) => Err(UiError::PayloadTooLarge),
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
    auth.check_fetch_site(&headers)?;
    let now = Instant::now();
    if let Some((_, view)) = auth.current(&headers, now) {
        return Ok(auth.signed_in(&view, None));
    }
    if let (Some(header), Some((username, password))) = (
        headers.get(header::AUTHORIZATION),
        basic_credentials(&headers),
    ) {
        if auth.known_bad_basic(header.as_bytes()) {
            debug!("web UI ignored proxy Basic credentials it already rejected");
        } else {
            match auth.verify(&username, &password, Method::Basic, now) {
                Ok(()) => {
                    let (id, view) = auth.sessions.create(&auth.account, Method::Basic, now)?;
                    info!("web UI session started from proxy Basic credentials");
                    return Ok(auth.signed_in(&view, Some(auth.cookie(&id, false)?)));
                }
                Err(UiError::InvalidCredentials) => {
                    auth.remember_bad_basic(header.as_bytes());
                    warn!(
                        "web UI ignored proxy Basic credentials that do not match MEMCAN_WEBUI_USERNAME/MEMCAN_WEBUI_PASSWORD; align the proxy basicauth user or clear the browser's saved login"
                    );
                }
                Err(_) => {}
            }
        }
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
    let body = read_body(body, MAX_LOGIN_BODY, state.body_read_timeout).await?;
    let request: LoginRequest = serde_json::from_slice(&body).map_err(|_| UiError::BadRequest)?;
    let now = Instant::now();
    auth.verify(&request.username, &request.password, Method::Form, now)?;
    if let Some(previous) = auth.session_id(&headers) {
        auth.sessions.revoke(previous);
    }
    let (id, view) = auth.sessions.create(&auth.account, Method::Form, now)?;
    info!("web UI sign-in form accepted; session started");
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
        auth.check_csrf(&view, &headers)?;
        auth.sessions.revoke(id);
        info!(method = view.method.as_str(), "web UI session signed out");
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
    auth.check_csrf(&view, &headers)?;
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

    fn fail(throttle: &Throttle) -> Attempt {
        throttle.attempt(Instant::now(), || false)
    }

    #[tokio::test(start_paused = true)]
    async fn throttle_backs_off_exponentially_up_to_a_short_cap() {
        let throttle = Throttle::default();
        for expected in 1..=FREE_FAILURES {
            assert!(
                matches!(fail(&throttle), Attempt::Rejected { failures } if failures == expected)
            );
        }
        // From here on every attempt must wait, doubling up to MAX_BACKOFF.
        for wait in [1, 2, 4, 8, 16, 32, 64, 128, 256, 300, 300] {
            let Attempt::Throttled(left) = throttle.attempt(Instant::now(), || true) else {
                panic!("expected backoff of {wait}s");
            };
            assert_eq!(left, Duration::from_secs(wait));
            tokio::time::advance(left).await;
            assert!(matches!(fail(&throttle), Attempt::Rejected { .. }));
        }
        let Attempt::Throttled(left) = throttle.attempt(Instant::now(), || true) else {
            panic!("still backing off");
        };
        assert!(left <= MAX_BACKOFF, "a single wait never exceeds the cap");
        tokio::time::advance(left).await;
        assert!(matches!(
            throttle.attempt(Instant::now(), || true),
            Attempt::Accepted
        ));
        assert!(
            matches!(fail(&throttle), Attempt::Rejected { failures: 1 }),
            "success resets the backoff"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn throttle_forgets_failures_after_a_quiet_period() {
        let throttle = Throttle::default();
        for _ in 0..FREE_FAILURES + 2 {
            fail(&throttle);
            tokio::time::advance(MAX_BACKOFF).await;
        }
        tokio::time::advance(FAILURE_MEMORY).await;
        assert!(matches!(fail(&throttle), Attempt::Rejected { failures: 1 }));
    }

    #[tokio::test(start_paused = true)]
    async fn rejection_warnings_are_rate_limited_per_kind_and_count_the_rest() {
        let log = RejectionLog::default();
        assert_eq!(log.note(Rejection::Origin), Some(0));
        for _ in 0..50 {
            assert_eq!(log.note(Rejection::Origin), None);
        }
        assert_eq!(log.note(Rejection::Host), Some(0), "kinds are independent");
        tokio::time::advance(REJECTION_LOG_INTERVAL).await;
        assert_eq!(log.note(Rejection::Origin), Some(50));
        assert_eq!(log.note(Rejection::Origin), None);
    }

    #[test]
    fn throttle_does_not_verify_while_backing_off() {
        let throttle = Throttle::default();
        for _ in 0..FREE_FAILURES {
            fail(&throttle);
        }
        let mut verified = false;
        let attempt = throttle.attempt(Instant::now(), || {
            verified = true;
            true
        });
        assert!(matches!(attempt, Attempt::Throttled(_)));
        assert!(!verified, "no credential check while throttled");
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
