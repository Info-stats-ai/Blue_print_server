use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Query, State};
use axum::response::Response;
use serde::Deserialize;
use serde_json::json;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{broadcast, RwLock};

use crate::agent;
use crate::auth::decode_jwt;
use crate::errors::AppError;
use crate::models::{ChatMessage, Contract, Intent};
use crate::AppState;

// Bounded so a slow consumer lags (and drops old messages) instead of letting
// a single stalled peer back up memory or block broadcasts to everyone else.
const BROADCAST_CAPACITY: usize = 256;

// Chat history is kept per-session, capped to the most recent N messages, so
// it can be used as context for the AI agent without growing unbounded.
const CHAT_HISTORY_CAP: usize = 50;

/// The sender id used for messages the AI agent posts, and reserved so a
/// human can never trigger the agent by impersonating it.
pub const AGENT_USER_ID: &str = "agent";

struct SessionChannel {
    tx: broadcast::Sender<Message>,
    peers: HashSet<String>,
    history: Vec<ChatMessage>,
    blueprint: String,
    // Keyed by path, not a single slot, so a session can later carry
    // multi-file context without a storage shape change.
    files: HashMap<String, String>,
}

#[derive(Clone, Default)]
pub struct Hub {
    inner: Arc<RwLock<HashMap<String, SessionChannel>>>,
}

impl Hub {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn add_peer(&self, session_id: &str, user_id: &str) -> broadcast::Receiver<Message> {
        let mut map = self.inner.write().await;
        let channel = map.entry(session_id.to_string()).or_insert_with(|| SessionChannel {
            tx: broadcast::channel(BROADCAST_CAPACITY).0,
            peers: HashSet::new(),
            history: Vec::new(),
            blueprint: String::new(),
            files: HashMap::new(),
        });
        channel.peers.insert(user_id.to_string());
        channel.tx.subscribe()
    }

    pub async fn remove_peer(&self, session_id: &str, user_id: &str) {
        let mut map = self.inner.write().await;
        if let Some(channel) = map.get_mut(session_id) {
            channel.peers.remove(user_id);
            if channel.peers.is_empty() {
                map.remove(session_id);
            }
        }
    }

    pub async fn peer_ids(&self, session_id: &str) -> Vec<String> {
        let map = self.inner.read().await;
        map.get(session_id)
            .map(|channel| channel.peers.iter().cloned().collect())
            .unwrap_or_default()
    }

    pub async fn broadcast(&self, session_id: &str, value: &serde_json::Value) {
        let text = value.to_string();
        let map = self.inner.read().await;
        if let Some(channel) = map.get(session_id) {
            let _ = channel.tx.send(Message::Text(text.into()));
        }
    }

    /// Appends to the session's chat history, dropping the oldest entries
    /// once it exceeds `CHAT_HISTORY_CAP`. A no-op if the session has no
    /// active channel (e.g. it was just torn down).
    pub async fn push_chat_message(&self, session_id: &str, message: ChatMessage) {
        let mut map = self.inner.write().await;
        if let Some(channel) = map.get_mut(session_id) {
            channel.history.push(message);
            let len = channel.history.len();
            if len > CHAT_HISTORY_CAP {
                channel.history.drain(0..len - CHAT_HISTORY_CAP);
            }
        }
    }

    /// Returns up to the last `limit` chat messages for the session, oldest first.
    pub async fn recent_chat_history(&self, session_id: &str, limit: usize) -> Vec<ChatMessage> {
        let map = self.inner.read().await;
        map.get(session_id)
            .map(|channel| {
                let start = channel.history.len().saturating_sub(limit);
                channel.history[start..].to_vec()
            })
            .unwrap_or_default()
    }

    /// Returns an empty string if the session has no active channel or no
    /// blueprint has been generated yet.
    pub async fn get_blueprint(&self, session_id: &str) -> String {
        let map = self.inner.read().await;
        map.get(session_id)
            .map(|channel| channel.blueprint.clone())
            .unwrap_or_default()
    }

    /// A no-op if the session has no active channel (e.g. it was just torn down).
    pub async fn set_blueprint(&self, session_id: &str, content: String) {
        let mut map = self.inner.write().await;
        if let Some(channel) = map.get_mut(session_id) {
            channel.blueprint = content;
        }
    }

    /// Stores the latest synced content for a path, overwriting any previous
    /// version. A no-op if the session has no active channel.
    pub async fn save_file(&self, session_id: &str, path: String, content: String) {
        let mut map = self.inner.write().await;
        if let Some(channel) = map.get_mut(session_id) {
            channel.files.insert(path, content);
        }
    }

    /// Returns a snapshot of every synced file for the session (path -> content).
    pub async fn get_files(&self, session_id: &str) -> HashMap<String, String> {
        let map = self.inner.read().await;
        map.get(session_id)
            .map(|channel| channel.files.clone())
            .unwrap_or_default()
    }
}

#[derive(Debug, Deserialize)]
pub struct WsQuery {
    pub session_id: String,
    pub token: String,
}

pub async fn ws_handler(
    ws: WebSocketUpgrade,
    Query(params): Query<WsQuery>,
    State(state): State<AppState>,
) -> Result<Response, AppError> {
    let claims = decode_jwt(&params.token, &state.jwt_secret)?;
    let user_id = claims.sub;

    let membership: Option<(String,)> = sqlx::query_as(
        "SELECT user_id FROM session_members WHERE session_id = ? AND user_id = ?",
    )
    .bind(&params.session_id)
    .bind(&user_id)
    .fetch_optional(&state.pool)
    .await?;

    if membership.is_none() {
        return Err(AppError::Unauthorized(
            "not a member of this session".into(),
        ));
    }

    let session_id = params.session_id;
    Ok(ws.on_upgrade(move |socket| handle_socket(socket, state, session_id, user_id)))
}

async fn handle_socket(mut socket: WebSocket, state: AppState, session_id: String, user_id: String) {
    let mut rx = state.hub.add_peer(&session_id, &user_id).await;

    let contracts: Vec<Contract> = sqlx::query_as("SELECT * FROM contracts WHERE session_id = ?")
        .bind(&session_id)
        .fetch_all(&state.pool)
        .await
        .unwrap_or_default();
    let intents: Vec<Intent> = sqlx::query_as("SELECT * FROM intents WHERE session_id = ?")
        .bind(&session_id)
        .fetch_all(&state.pool)
        .await
        .unwrap_or_default();
    let peers = state.hub.peer_ids(&session_id).await;
    let blueprint = state.hub.get_blueprint(&session_id).await;

    let snapshot = json!({
        "type": "snapshot",
        "contracts": contracts,
        "intents": intents,
        "peers": peers,
        "blueprint": blueprint,
    });

    if socket
        .send(Message::Text(snapshot.to_string().into()))
        .await
        .is_err()
    {
        state.hub.remove_peer(&session_id, &user_id).await;
        return;
    }

    state
        .hub
        .broadcast(&session_id, &json!({"type": "peer_joined", "user_id": user_id}))
        .await;

    let mut ping_interval = tokio::time::interval(Duration::from_secs(30));
    ping_interval.tick().await; // first tick fires immediately; consume it so we don't ping right on connect

    let mut awaiting_pong = false;
    let mut pong_deadline = Box::pin(tokio::time::sleep(Duration::from_secs(3600)));

    loop {
        tokio::select! {
            _ = ping_interval.tick() => {
                if awaiting_pong {
                    break;
                }
                if socket.send(Message::Ping(Vec::new().into())).await.is_err() {
                    break;
                }
                awaiting_pong = true;
                pong_deadline = Box::pin(tokio::time::sleep(Duration::from_secs(10)));
            }
            _ = &mut pong_deadline, if awaiting_pong => {
                break;
            }
            incoming = socket.recv() => {
                match incoming {
                    Some(Ok(Message::Pong(_))) => { awaiting_pong = false; }
                    Some(Ok(Message::Text(text))) => {
                        handle_client_message(&state, &session_id, &user_id, text.as_str()).await;
                    }
                    Some(Ok(Message::Close(_))) => {
                        // tungstenite already queued the close-reply frame; keep draining
                        // briefly so it actually flushes before we drop the socket, instead
                        // of the peer seeing an abrupt TCP close.
                        let _ = tokio::time::timeout(Duration::from_secs(2), async {
                            while socket.recv().await.is_some() {}
                        })
                        .await;
                        break;
                    }
                    None => break,
                    Some(Err(_)) => break,
                    _ => {}
                }
            }
            outgoing = rx.recv() => {
                match outgoing {
                    Ok(msg) => {
                        if socket.send(msg).await.is_err() {
                            break;
                        }
                    }
                    // we fell behind the broadcast buffer; skip the missed messages and keep going
                    Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        }
    }

    state.hub.remove_peer(&session_id, &user_id).await;
    state
        .hub
        .broadcast(&session_id, &json!({"type": "peer_left", "user_id": user_id}))
        .await;
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ClientMessage {
    Chat { text: String },
    FileOpen { sender: String, path: String },
    FileSave { path: String, content: String },
}

/// Parses an inbound text frame and dispatches it. The sender's identity for
/// chat always comes from the authenticated `user_id`, not the payload, so a
/// peer can't post chat messages impersonating someone else. Unknown types
/// and malformed JSON are ignored silently.
async fn handle_client_message(state: &AppState, session_id: &str, user_id: &str, text: &str) {
    let Ok(message) = serde_json::from_str::<ClientMessage>(text) else {
        return;
    };

    match message {
        ClientMessage::Chat { text } => handle_chat_message(state, session_id, user_id, text).await,
        ClientMessage::FileOpen { sender, path } => {
            state
                .hub
                .broadcast(session_id, &json!({"type": "file_open", "sender": sender, "path": path}))
                .await;
        }
        ClientMessage::FileSave { path, content } => {
            handle_file_save_message(state, session_id, user_id, path, content).await;
        }
    }
}

async fn handle_chat_message(state: &AppState, session_id: &str, user_id: &str, text: String) {
    let message = ChatMessage { sender: user_id.to_string(), text };

    state.hub.push_chat_message(session_id, message.clone()).await;
    state
        .hub
        .broadcast(
            session_id,
            &json!({"type": "chat", "sender": message.sender, "text": message.text}),
        )
        .await;

    // Never let the agent's own messages re-trigger it. "@agent blueprint"
    // generates/replaces the shared blueprint; any other "@agent" mention is
    // a normal reply. Plain human-to-human chat does nothing either way.
    if message.sender == AGENT_USER_ID {
        return;
    }
    let lowered = message.text.to_lowercase();
    if lowered.contains("@agent blueprint") {
        let state = state.clone();
        let session_id = session_id.to_string();
        tokio::spawn(async move {
            agent::handle_blueprint_request(state, session_id).await;
        });
    } else if lowered.contains("@agent") {
        let state = state.clone();
        let session_id = session_id.to_string();
        tokio::spawn(async move {
            agent::handle_mention(state, session_id).await;
        });
    }
}

/// Retains the saved content for later blueprint generation, then runs the
/// cost-tiered watcher: a free string check (no LLM call) decides whether the
/// change touches anything the blueprint cares about, and only then is a
/// single LLM call spawned. The file content is never broadcast to peers.
async fn handle_file_save_message(
    state: &AppState,
    session_id: &str,
    user_id: &str,
    path: String,
    content: String,
) {
    // The agent never saves files itself, but guard against self-triggering
    // the watcher the same way chat does, for consistency.
    if user_id == AGENT_USER_ID {
        return;
    }

    state.hub.save_file(session_id, path.clone(), content.clone()).await;

    let blueprint = state.hub.get_blueprint(session_id).await;
    let matched_symbols = agent::matching_blueprint_symbols(&blueprint, &content);
    if matched_symbols.is_empty() {
        return;
    }

    let state = state.clone();
    let session_id = session_id.to_string();
    tokio::spawn(async move {
        agent::check_file_save(state, session_id, path, content, blueprint, matched_symbols).await;
    });
}
