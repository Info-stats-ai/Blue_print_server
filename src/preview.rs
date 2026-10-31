//! Shared live preview: a session member shares an app running on their own
//! `localhost:<port>`, and teammates open it through this server. Each HTTP
//! request is relayed over the sharer's existing websocket as a
//! `preview_request`; their IDE fetches it from localhost and answers with a
//! `preview_response`. Nothing runs in the cloud and no port is opened on the
//! sharer's machine.
//!
//! A share is reachable only by its unguessable token, which is announced to
//! session members alone and dies when the sharer stops or disconnects. The
//! first visit to `/preview/<token>/…` sets a cookie so the app's absolute
//! asset paths (`/src/main.ts`, `/_next/…`) reach the same share through the
//! fallback route. Plain HTTP only: websockets (hot reload) aren't relayed.

use axum::body::{Body, to_bytes};
use axum::extract::{Path, Request, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use serde::Deserialize;
use serde_json::json;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::oneshot;

use crate::AppState;

const COOKIE: &str = "i_preview";
const MAX_REQUEST_BODY: usize = 10 * 1024 * 1024;
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(30);

/// Headers that describe one hop of a connection, or that the relay changes
/// (bodies arrive decoded, so their old length/encoding no longer apply).
const DROPPED_HEADERS: [&str; 9] = [
    "connection",
    "keep-alive",
    "transfer-encoding",
    "upgrade",
    "proxy-connection",
    "te",
    "trailer",
    "content-length",
    "content-encoding",
];

#[derive(Clone, Debug, PartialEq)]
pub struct Share {
    pub session_id: String,
    pub user_id: String,
    pub port: u16,
}

#[derive(Debug, Deserialize)]
pub struct Reply {
    pub status: u16,
    #[serde(default)]
    pub headers: Vec<(String, String)>,
    // Base64.
    #[serde(default)]
    pub body: String,
}

#[derive(Clone, Default)]
pub struct Previews {
    shares: Arc<Mutex<HashMap<String, Share>>>,
    waiters: Arc<Mutex<HashMap<u64, oneshot::Sender<Reply>>>>,
    next_request: Arc<AtomicU64>,
}

impl Previews {
    /// Starts sharing `port` for this user in this session, replacing their
    /// previous share there. Returns the new token.
    pub fn start(&self, session_id: &str, user_id: &str, port: u16) -> String {
        let token = uuid::Uuid::new_v4().simple().to_string();
        if let Ok(mut shares) = self.shares.lock() {
            shares.retain(|_, share| !(share.session_id == session_id && share.user_id == user_id));
            shares.insert(
                token.clone(),
                Share {
                    session_id: session_id.to_string(),
                    user_id: user_id.to_string(),
                    port,
                },
            );
        }
        token
    }

    /// Ends this user's share in this session. Returns whether there was one.
    pub fn stop(&self, session_id: &str, user_id: &str) -> bool {
        let Ok(mut shares) = self.shares.lock() else {
            return false;
        };
        let before = shares.len();
        shares.retain(|_, share| !(share.session_id == session_id && share.user_id == user_id));
        shares.len() != before
    }

    pub fn get(&self, token: &str) -> Option<Share> {
        self.shares.lock().ok()?.get(token).cloned()
    }

    /// The live shares in a session: (user id, port, token).
    pub fn in_session(&self, session_id: &str) -> Vec<(String, u16, String)> {
        let Ok(shares) = self.shares.lock() else {
            return Vec::new();
        };
        let mut found: Vec<_> = shares
            .iter()
            .filter(|(_, share)| share.session_id == session_id)
            .map(|(token, share)| (share.user_id.clone(), share.port, token.clone()))
            .collect();
        found.sort();
        found
    }

    fn wait_for_reply(&self) -> (u64, oneshot::Receiver<Reply>) {
        let id = self.next_request.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        if let Ok(mut waiters) = self.waiters.lock() {
            waiters.insert(id, tx);
        }
        (id, rx)
    }

    fn forget(&self, id: u64) {
        if let Ok(mut waiters) = self.waiters.lock() {
            waiters.remove(&id);
        }
    }

    /// Delivers the sharer's answer to the waiting HTTP request.
    pub fn complete(&self, id: u64, reply: Reply) {
        if let Some(waiter) = self.waiters.lock().ok().and_then(|mut waiters| waiters.remove(&id)) {
            let _ = waiter.send(reply);
        }
    }
}

/// Who each in-flight request was sent to, so only that sharer can answer it.
#[derive(Clone, Default)]
pub struct Routing {
    sent_to: Arc<Mutex<HashMap<u64, (String, String)>>>,
}

impl Routing {
    fn record(&self, id: u64, session_id: &str, user_id: &str) {
        if let Ok(mut sent_to) = self.sent_to.lock() {
            sent_to.insert(id, (session_id.to_string(), user_id.to_string()));
        }
    }

    fn forget(&self, id: u64) {
        if let Ok(mut sent_to) = self.sent_to.lock() {
            sent_to.remove(&id);
        }
    }

    pub fn is_sharer(&self, id: u64, session_id: &str, user_id: &str) -> bool {
        self.sent_to
            .lock()
            .ok()
            .and_then(|sent_to| sent_to.get(&id).cloned())
            .is_some_and(|(session, user)| session == session_id && user == user_id)
    }
}

/// `/preview/<token>` and `/preview/<token>/<path>`: opens a share and
/// remembers it in a cookie for the app's absolute paths.
pub async fn open(State(state): State<AppState>, Path(params): Path<HashMap<String, String>>, request: Request) -> Response {
    let Some(token) = params.get("token").cloned() else {
        return not_shared();
    };
    let rest = params.get("rest").cloned().unwrap_or_default();
    let query = request.uri().query().map(|query| format!("?{query}")).unwrap_or_default();
    let path = format!("/{rest}{query}");
    let mut response = relay(&state, &token, path, request).await;
    if let Ok(cookie) = HeaderValue::from_str(&format!("{COOKIE}={token}; Path=/; HttpOnly; SameSite=Lax")) {
        response.headers_mut().append(header::SET_COOKIE, cookie);
    }
    response
}

/// Any path the server doesn't own: forwarded to the share named by the
/// preview cookie, if there is one.
pub async fn fallback(State(state): State<AppState>, request: Request) -> Response {
    let Some(token) = cookie_token(request.headers()) else {
        return (StatusCode::NOT_FOUND, "Not found").into_response();
    };
    let path = request
        .uri()
        .path_and_query()
        .map(|path| path.as_str().to_string())
        .unwrap_or_else(|| "/".to_string());
    relay(&state, &token, path, request).await
}

fn cookie_token(headers: &HeaderMap) -> Option<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';'))
        .find_map(|pair| pair.trim().strip_prefix(&format!("{COOKIE}=")).map(str::to_string))
}

fn not_shared() -> Response {
    (StatusCode::NOT_FOUND, "This preview isn't being shared right now.").into_response()
}

async fn relay(state: &AppState, token: &str, path: String, request: Request) -> Response {
    let Some(share) = state.previews.get(token) else {
        return not_shared();
    };
    let method = request.method().to_string();
    let headers: Vec<(String, String)> = request
        .headers()
        .iter()
        .filter(|(name, _)| {
            let name = name.as_str();
            name != "host" && name != "accept-encoding" && !DROPPED_HEADERS.contains(&name)
        })
        .filter_map(|(name, value)| Some((name.to_string(), value.to_str().ok()?.to_string())))
        .map(|(name, value)| {
            if name == "cookie" {
                (name, strip_preview_cookie(&value))
            } else {
                (name, value)
            }
        })
        .filter(|(_, value)| !value.is_empty())
        .collect();
    let body = match to_bytes(request.into_body(), MAX_REQUEST_BODY).await {
        Ok(body) => body,
        Err(_) => return (StatusCode::PAYLOAD_TOO_LARGE, "Request body too large").into_response(),
    };

    let (id, reply) = state.previews.wait_for_reply();
    state.preview_routing.record(id, &share.session_id, &share.user_id);
    let delivered = state
        .hub
        .send_to_user(
            &share.session_id,
            &share.user_id,
            &json!({
                "type": "preview_request",
                "id": id,
                "method": method,
                "path": path,
                "headers": headers,
                "body": BASE64.encode(&body),
            }),
        )
        .await;
    if !delivered {
        state.previews.forget(id);
        state.preview_routing.forget(id);
        return (StatusCode::BAD_GATEWAY, "The person sharing this preview is offline.").into_response();
    }
    let reply = tokio::time::timeout(RESPONSE_TIMEOUT, reply).await;
    state.previews.forget(id);
    state.preview_routing.forget(id);
    match reply {
        Ok(Ok(reply)) => build_response(reply),
        Ok(Err(_)) | Err(_) => {
            (StatusCode::GATEWAY_TIMEOUT, "The shared app didn't answer in time.").into_response()
        }
    }
}

fn strip_preview_cookie(value: &str) -> String {
    value
        .split(';')
        .map(str::trim)
        .filter(|pair| !pair.starts_with(&format!("{COOKIE}=")))
        .collect::<Vec<_>>()
        .join("; ")
}

fn build_response(reply: Reply) -> Response {
    let status = StatusCode::from_u16(reply.status).unwrap_or(StatusCode::BAD_GATEWAY);
    let body = BASE64.decode(reply.body.as_bytes()).unwrap_or_default();
    let mut response = Response::new(Body::from(body));
    *response.status_mut() = status;
    for (name, value) in reply.headers {
        if DROPPED_HEADERS.contains(&name.to_ascii_lowercase().as_str()) {
            continue;
        }
        if let (Ok(name), Ok(value)) = (HeaderName::try_from(name), HeaderValue::from_str(&value)) {
            response.headers_mut().append(name, value);
        }
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_share_replaces_the_old_one_and_stop_ends_it() {
        let previews = Previews::default();
        let first = previews.start("s1", "alice", 3000);
        let second = previews.start("s1", "alice", 5173);
        assert_eq!(previews.get(&first), None);
        assert_eq!(previews.get(&second).map(|share| share.port), Some(5173));
        let bobs = previews.start("s1", "bob", 8000);
        assert_eq!(previews.in_session("s1").len(), 2);
        assert!(previews.stop("s1", "alice"));
        assert!(!previews.stop("s1", "alice"));
        assert_eq!(previews.in_session("s1"), vec![("bob".to_string(), 8000, bobs)]);
    }

    #[test]
    fn the_preview_cookie_is_read_and_not_passed_to_the_app() {
        let mut headers = HeaderMap::new();
        headers.insert(header::COOKIE, HeaderValue::from_static("theme=dark; i_preview=abc123; sid=9"));
        assert_eq!(cookie_token(&headers).as_deref(), Some("abc123"));
        assert_eq!(strip_preview_cookie("theme=dark; i_preview=abc123; sid=9"), "theme=dark; sid=9");
    }

    #[test]
    fn only_the_sharer_can_answer_a_request() {
        let routing = Routing::default();
        routing.record(7, "s1", "alice");
        assert!(routing.is_sharer(7, "s1", "alice"));
        assert!(!routing.is_sharer(7, "s1", "bob"));
        assert!(!routing.is_sharer(8, "s1", "alice"));
    }
}
