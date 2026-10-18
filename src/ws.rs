use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Query, State};
use axum::response::Response;
use serde::Deserialize;
use serde_json::json;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{broadcast, RwLock};

use crate::auth::decode_jwt;
use crate::errors::AppError;
use crate::models::{Contract, Intent};
use crate::AppState;

// Bounded so a slow consumer lags (and drops old messages) instead of letting
// a single stalled peer back up memory or block broadcasts to everyone else.
const BROADCAST_CAPACITY: usize = 256;

struct SessionChannel {
    tx: broadcast::Sender<Message>,
    peers: HashSet<String>,
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

    let snapshot = json!({
        "type": "snapshot",
        "contracts": contracts,
        "intents": intents,
        "peers": peers,
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
