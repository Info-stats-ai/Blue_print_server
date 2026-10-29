use anyhow::Context as _;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Query, State};
use axum::response::Response;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{RwLock, broadcast};

use crate::AppState;
use crate::agent;
use crate::auth::decode_jwt;
use crate::crdt::WireOperation;
use crate::errors::AppError;
use crate::models::{ChatMessage, Contract, Intent};
use crate::symbols::{self, Symbol};

// Bounded so a slow consumer lags (and drops old messages) instead of letting
// a single stalled peer back up memory or block broadcasts to everyone else.
const BROADCAST_CAPACITY: usize = 256;

// Chat history is kept per-session, capped to the most recent N messages, so
// it can be used as context for the AI agent without growing unbounded.
const CHAT_HISTORY_CAP: usize = 50;

// Trailing-edge debounce window for the save-watcher: a burst of saves to the
// same path within this window collapses into a single check after the burst
// goes quiet, instead of one LLM call per save.
const WATCHER_DEBOUNCE: Duration = Duration::from_secs(3);

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
    // Most-recently-saved path first. Used as the relevance tiebreaker in
    // `relevant_files` — the primary ranking is symbol/term overlap, not
    // recency, but recency still matters when nothing else distinguishes
    // two candidates.
    file_order: Vec<String>,
    // Structural index: top-level definition names per path, extracted via
    // tree-sitter (see `symbols.rs`). Recomputed whenever a path's content
    // changes. Empty for a path whose language isn't supported or that
    // hasn't been synced yet — `relevant_files` falls back to a raw
    // substring scan over `files` in that case.
    symbols: HashMap<String, Vec<Symbol>>,
    // The server's own mirror of each collaboratively-edited file's CRDT
    // buffer, keyed by path. Running with replica id `REMOTE_SERVER` so its
    // merges are attributable like any other peer's. `files`/`file_order`
    // stay the plain-text projection of this for everything that only needs
    // to read text (agent context, blueprint generation, the watcher).
    buffers: HashMap<String, text::Buffer>,
    // Stable per-session replica id per user, so a reconnecting client keeps
    // the same identity in the CRDT history instead of fragmenting across
    // multiple replica ids for the same person.
    next_replica_id: u16,
    // Phase 4 presence: the path each peer most recently opened, so a
    // (re)joining client can render "who's working on what" immediately
    // instead of waiting to observe every peer's next `file_open`.
    presence: HashMap<String, String>,
    // Monotonically increasing count of every chat message ever pushed to
    // this session, never decremented by `push_chat_message`'s cap-eviction
    // drain — unlike `history.len()` (capped, and reshuffled by `drain`),
    // this is a stable counter `last_seen_total_messages` can be compared
    // against regardless of how much drain has happened since.
    total_messages: u64,
    // `total_messages`'s value the last time each user disconnected — the
    // input to the Phase 7 async catch-up digest (`catch_up_digest`): on
    // rejoin, `total_messages - this` is how many messages they missed. A
    // user who's never left has no entry, which `catch_up_digest` treats as
    // "nothing to catch up on" rather than "catch up on the whole history."
    // Deliberately *not* a `history` vec index — `history` gets left-drained
    // once it exceeds `CHAT_HISTORY_CAP`, which would silently invalidate a
    // stored index into it (the index would land on the wrong message after
    // a drain shifted everything down), while this counter never does.
    last_seen_total_messages: HashMap<String, u64>,
    // Bumped on every meaningful activity (join, chat, save, merge) — the
    // input to `Hub::evict_idle_sessions`. Without this, `remove_peer` no
    // longer tearing down an empty channel (needed for Phase 7 resume)
    // would mean every session ever touched sits in memory forever; this
    // is what lets genuinely abandoned ones eventually be freed instead.
    last_activity: Instant,
}

// Tracks the latest file_save generation per (session_id, path) so the watcher
// can debounce: a save that arrives, then is immediately superseded by
// another save to the same path, never triggers its own LLM call.
type WatcherKey = (String, String);

pub struct BufferJoin {
    pub replica_id: clock::ReplicaId,
    pub text: String,
    pub base_text: String,
    pub operations: Vec<WireOperation>,
}

#[derive(Clone, Default)]
pub struct Hub {
    inner: Arc<RwLock<HashMap<String, SessionChannel>>>,
    watcher_generations: Arc<RwLock<HashMap<WatcherKey, u64>>>,
    // Single-flight state for blueprint regeneration: a session_id in
    // `blueprint_inflight` has a job running; if another trigger arrives
    // while it's running, it's recorded in `blueprint_dirty` instead of
    // spawning a second concurrent job, and the running job re-runs exactly
    // once more after it finishes.
    blueprint_inflight: Arc<RwLock<HashSet<String>>>,
    blueprint_dirty: Arc<RwLock<HashSet<String>>>,
    // Same single-flight pattern as the blueprint pair above, for triage
    // runs — independent of it (a blueprint regen and a triage pass can
    // legitimately run concurrently; they write to different places).
    triage_inflight: Arc<RwLock<HashSet<String>>>,
    triage_dirty: Arc<RwLock<HashSet<String>>>,
    // Phase 6's "conflicts avoided" numbers, kept per session and pushed to
    // every peer on each change (see `broadcast_metrics`) so the overlay
    // ticks up live instead of needing a poll.
    metrics: Arc<RwLock<HashMap<String, SessionMetrics>>>,
}

/// Live counters behind Phase 6's "conflicts avoided" metric overlay —
/// turning the collision-prevention thesis into numbers on screen rather
/// than a claim. Everything here is a running total for one session's
/// lifetime (reset only by the session disappearing from the `Hub`), not a
/// rate, so the client renders them as-is.
#[derive(Clone, Default, Serialize)]
pub struct SessionMetrics {
    // Every CRDT merge applied (`apply_buffer_op`) is, by construction, an
    // edit that landed without clobbering a concurrent one — the mechanical
    // half of "conflicts avoided."
    pub merges_applied: u64,
    // Times the watcher's reply was anything other than a bare "OK" — a
    // real divergence from the blueprint caught before it reached a silent
    // overwrite. The logical half of "conflicts avoided."
    pub divergences_caught: u64,
    // Times a save landed outside the saver's assigned lane per the
    // blueprint's "## Work Split" section (see `agent::parse_work_split`).
    pub lane_warnings: u64,
    pub llm_calls: u64,
    // Sum of `agent::estimate_cost_usd` across every call this session has
    // made — approximate by construction (see that function's doc comment),
    // not a substitute for the real Anthropic invoice.
    pub estimated_cost_usd: f64,
}

impl Hub {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn add_peer(&self, session_id: &str, user_id: &str) -> broadcast::Receiver<Message> {
        let mut map = self.inner.write().await;
        let channel = map
            .entry(session_id.to_string())
            .or_insert_with(|| SessionChannel {
                tx: broadcast::channel(BROADCAST_CAPACITY).0,
                peers: HashSet::new(),
                history: Vec::new(),
                blueprint: String::new(),
                files: HashMap::new(),
                file_order: Vec::new(),
                symbols: HashMap::new(),
                buffers: HashMap::new(),
                next_replica_id: clock::ReplicaId::FIRST_COLLAB_ID.as_u16(),
                presence: HashMap::new(),
                total_messages: 0,
                last_seen_total_messages: HashMap::new(),
                last_activity: Instant::now(),
            });
        channel.peers.insert(user_id.to_string());
        channel.last_activity = Instant::now();
        channel.tx.subscribe()
    }

    /// Marks `user_id` as no longer connected to `session_id`. Deliberately
    /// does **not** tear down the `SessionChannel` even once `peers` is
    /// empty — Phase 7 requires chat history/files/blueprint to survive
    /// everyone disconnecting and later rejoining, and chat history has no
    /// durable copy outside this in-memory channel (files/blueprint do, via
    /// `checkpoint.rs`, but only on a 5-minute cadence). The tradeoff is an
    /// abandoned session's state living in memory indefinitely until the
    /// process restarts; a real TTL-based eviction for long-dead sessions is
    /// a follow-up this pass doesn't cover, not something this fixes.
    pub async fn remove_peer(&self, session_id: &str, user_id: &str) {
        let mut map = self.inner.write().await;
        if let Some(channel) = map.get_mut(session_id) {
            channel.peers.remove(user_id);
            channel.presence.remove(user_id);
            channel
                .last_seen_total_messages
                .insert(user_id.to_string(), channel.total_messages);
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
            channel.total_messages += 1;
            channel.last_activity = Instant::now();
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
    /// version, and moves it to the front of the recency order. A no-op if
    /// the session has no active channel.
    pub async fn save_file(&self, session_id: &str, path: String, content: String) {
        // Parsed *before* taking the write lock — tree-sitter is a CPU-bound
        // parse that can take real time on a large file, and holding the
        // single lock that every session on the server shares while it runs
        // would stall every other session's unrelated operations behind
        // this one file's parse. See `apply_buffer_op` for the same fix on
        // the much hotter per-keystroke path.
        let new_symbols = symbols::extract_symbols(&path, &content);
        let mut map = self.inner.write().await;
        if let Some(channel) = map.get_mut(session_id) {
            channel.symbols.insert(path.clone(), new_symbols);
            channel.files.insert(path.clone(), content);
            channel.file_order.retain(|existing| existing != &path);
            channel.file_order.insert(0, path);
            channel.last_activity = Instant::now();
        }
    }

    /// Returns the previously synced content for a single path, if any —
    /// used to diff against a new save instead of resending the whole file.
    pub async fn get_file(&self, session_id: &str, path: &str) -> Option<String> {
        let map = self.inner.read().await;
        map.get(session_id)?.files.get(path).cloned()
    }

    /// Returns every synced file in the session (path -> content). Used for
    /// an explicit, deliberate snapshot (an "accept this state" action), not
    /// per-request LLM context — unlike `relevant_files`, this is
    /// intentionally uncapped, since a snapshot that silently dropped files
    /// would be a correctness bug, not a cost optimization.
    pub async fn get_all_files(&self, session_id: &str) -> HashMap<String, String> {
        let map = self.inner.read().await;
        map.get(session_id)
            .map(|channel| channel.files.clone())
            .unwrap_or_default()
    }

    /// Snapshots every currently-active session with at least one synced
    /// file — the input to the periodic auto-checkpoint sweep (see
    /// `agent::run_checkpoint_sweep`). A session with no files yet has
    /// nothing worth checkpointing, so it's left out rather than writing an
    /// empty snapshot row.
    pub async fn active_session_snapshots(&self) -> Vec<(String, HashMap<String, String>, String)> {
        let map = self.inner.read().await;
        map.iter()
            .filter(|(_, channel)| !channel.files.is_empty())
            .map(|(session_id, channel)| {
                (
                    session_id.clone(),
                    channel.files.clone(),
                    channel.blueprint.clone(),
                )
            })
            .collect()
    }

    /// Records that `user_id` currently has `path` open — Phase 4 presence.
    /// A no-op if the session has no active channel.
    pub async fn set_presence(&self, session_id: &str, user_id: &str, path: String) {
        let mut map = self.inner.write().await;
        if let Some(channel) = map.get_mut(session_id) {
            channel.presence.insert(user_id.to_string(), path);
        }
    }

    /// Returns the current user_id -> open-path map for the session, for
    /// seeding a (re)joining client's presence list immediately instead of
    /// waiting to observe each peer's next `file_open`.
    pub async fn get_presence(&self, session_id: &str) -> HashMap<String, String> {
        let map = self.inner.read().await;
        map.get(session_id)
            .map(|channel| channel.presence.clone())
            .unwrap_or_default()
    }

    /// Builds a short digest of chat activity `user_id` missed while
    /// disconnected — Phase 7's async catch-up. Deliberately a cheap,
    /// deterministic summary (who said what, most recent first) rather than
    /// an LLM call: it's generated on every (re)join, which would otherwise
    /// mean paying for a summarization call just to greet someone, for
    /// information already sitting right there in `history`. Returns `None`
    /// when there's nothing to report: a first-time join (no prior
    /// disconnect recorded) or no messages happened during the gap.
    pub async fn catch_up_digest(&self, session_id: &str, user_id: &str) -> Option<String> {
        const MAX_DIGEST_MESSAGES: usize = 5;

        let map = self.inner.read().await;
        let channel = map.get(session_id)?;
        let last_seen = *channel.last_seen_total_messages.get(user_id)?;
        let count = channel.total_messages.saturating_sub(last_seen);
        if count == 0 {
            return None;
        }
        // `count` is how many messages were actually missed, by the stable
        // counter — but `history` itself is capped and left-drained (see
        // `push_chat_message`), so if more was missed than the cap retained,
        // this can only show the tail end that's still in memory. That's an
        // honest degradation (show what's left of what was missed), not the
        // silent-wrong-messages bug a drained vec index would produce.
        let available = (count as usize).min(channel.history.len());
        let missed = &channel.history[channel.history.len() - available..];
        let recent = missed
            .iter()
            .rev()
            .take(MAX_DIGEST_MESSAGES)
            .rev()
            .map(|message| format!("[{}] {}", message.sender, message.text));
        let noun = if count == 1 { "message" } else { "messages" };
        Some(format!(
            "{count} {noun} while you were away:\n{}",
            recent.collect::<Vec<_>>().join("\n")
        ))
    }

    /// Bulk-seeds a session's files/symbols directly (bypassing the CRDT
    /// path, the same way `restore_file` does) — used only to rehydrate a
    /// freshly-created, still-empty channel from its last DB checkpoint
    /// after a server restart (see `rehydrate_session_from_checkpoint`). A
    /// no-op if the channel already has files, so it can never clobber live
    /// state with a stale checkpoint.
    pub async fn seed_files_if_empty(
        &self,
        session_id: &str,
        files: HashMap<String, String>,
        blueprint: String,
    ) {
        // Parsed before the lock, same reason as `save_file` — and more
        // pressing here, since this can be N files' worth of parsing in one
        // go (a whole checkpoint's snapshot) rather than just one.
        let parsed: Vec<(String, String, Vec<symbols::Symbol>)> = files
            .into_iter()
            .map(|(path, content)| {
                let path_symbols = symbols::extract_symbols(&path, &content);
                (path, content, path_symbols)
            })
            .collect();

        let mut map = self.inner.write().await;
        if let Some(channel) = map.get_mut(session_id)
            && channel.files.is_empty()
        {
            for (path, content, path_symbols) in parsed {
                channel.symbols.insert(path.clone(), path_symbols);
                channel.file_order.push(path.clone());
                channel.files.insert(path, content);
            }
            if channel.blueprint.is_empty() {
                channel.blueprint = blueprint;
            }
        }
    }

    /// Overwrites a file's content directly, without going through the CRDT
    /// buffer — used only by `restore_plan` to apply a snapshot, which is a
    /// deliberate, infrequent, single-actor action where last-write-wins is
    /// the *intended* semantics (the whole point is "make this the new
    /// current state"), not the accidental-data-loss case ordinary
    /// concurrent edits need the CRDT path to avoid. Also drops any CRDT
    /// buffer for the path, so the next edit re-joins fresh from the
    /// restored text instead of merging against now-stale CRDT history.
    pub async fn restore_file(&self, session_id: &str, path: String, content: String) {
        // See `save_file` — parsed before the lock, same reason.
        let new_symbols = symbols::extract_symbols(&path, &content);
        let mut map = self.inner.write().await;
        if let Some(channel) = map.get_mut(session_id) {
            channel.buffers.remove(&path);
            channel.symbols.insert(path.clone(), new_symbols);
            channel.files.insert(path.clone(), content);
            channel.file_order.retain(|existing| existing != &path);
            channel.file_order.insert(0, path);
            channel.last_activity = Instant::now();
        }
    }

    /// Bumps and returns the save generation for (session_id, path). The
    /// watcher debounce task spawned for this save compares its captured
    /// generation against the latest one after waiting out the debounce
    /// window; a mismatch means a newer save superseded it, so it skips the
    /// LLM call instead of running a stale check.
    pub async fn bump_watcher_generation(&self, session_id: &str, path: &str) -> u64 {
        let mut generations = self.watcher_generations.write().await;
        let key = (session_id.to_string(), path.to_string());
        let next = generations.get(&key).copied().unwrap_or(0) + 1;
        generations.insert(key, next);
        next
    }

    /// True if `generation` is still the latest recorded save for
    /// (session_id, path) — i.e. nothing newer arrived during the debounce wait.
    pub async fn is_current_watcher_generation(
        &self,
        session_id: &str,
        path: &str,
        generation: u64,
    ) -> bool {
        let generations = self.watcher_generations.read().await;
        let key = (session_id.to_string(), path.to_string());
        generations.get(&key).copied() == Some(generation)
    }

    /// Single-flight gate for blueprint regeneration. Returns true if the
    /// caller should start a job now; false means a job is already running
    /// for this session, and the trigger has been recorded so the running
    /// job reruns once more after it finishes instead of a second job
    /// starting concurrently.
    pub async fn try_start_blueprint_job(&self, session_id: &str) -> bool {
        let mut inflight = self.blueprint_inflight.write().await;
        if inflight.contains(session_id) {
            drop(inflight);
            self.blueprint_dirty
                .write()
                .await
                .insert(session_id.to_string());
            false
        } else {
            inflight.insert(session_id.to_string());
            true
        }
    }

    /// Call when a blueprint job finishes. Returns true if another trigger
    /// arrived while it was running, meaning the caller should run exactly
    /// one more job rather than piling one up per trigger received.
    pub async fn finish_blueprint_job(&self, session_id: &str) -> bool {
        self.blueprint_inflight.write().await.remove(session_id);
        self.blueprint_dirty.write().await.remove(session_id)
    }

    /// Single-flight gate for a triage run — same semantics as
    /// `try_start_blueprint_job`, independent state.
    pub async fn try_start_triage_job(&self, session_id: &str) -> bool {
        let mut inflight = self.triage_inflight.write().await;
        if inflight.contains(session_id) {
            drop(inflight);
            self.triage_dirty.write().await.insert(session_id.to_string());
            false
        } else {
            inflight.insert(session_id.to_string());
            true
        }
    }

    /// Call when a triage job finishes — same semantics as
    /// `finish_blueprint_job`.
    pub async fn finish_triage_job(&self, session_id: &str) -> bool {
        self.triage_inflight.write().await.remove(session_id);
        self.triage_dirty.write().await.remove(session_id)
    }

    /// Records one CRDT merge for the session's "conflicts avoided" metric
    /// and pushes the updated totals to every peer.
    pub async fn record_merge(&self, session_id: &str) {
        self.update_metrics(session_id, |metrics| metrics.merges_applied += 1)
            .await;
    }

    /// Records one watcher-caught divergence from the blueprint.
    pub async fn record_divergence_caught(&self, session_id: &str) {
        self.update_metrics(session_id, |metrics| metrics.divergences_caught += 1)
            .await;
    }

    /// Records one work-lane warning (a save landed outside its assigned owner's lane).
    pub async fn record_lane_warning(&self, session_id: &str) {
        self.update_metrics(session_id, |metrics| metrics.lane_warnings += 1)
            .await;
    }

    /// Records one LLM call's estimated cost.
    pub async fn record_llm_call(&self, session_id: &str, cost_usd: f64) {
        self.update_metrics(session_id, |metrics| {
            metrics.llm_calls += 1;
            metrics.estimated_cost_usd += cost_usd;
        })
        .await;
    }

    /// Returns the session's current metric totals (zeroed if nothing has
    /// happened yet) — used to seed a (re)joining peer's snapshot so they
    /// don't see the overlay start from zero mid-session.
    pub async fn get_metrics(&self, session_id: &str) -> SessionMetrics {
        self.metrics
            .read()
            .await
            .get(session_id)
            .cloned()
            .unwrap_or_default()
    }

    /// Removes every session with zero currently-connected peers whose
    /// `last_activity` is older than `idle_threshold`, returning the
    /// evicted session ids (for logging). This is what keeps the Hub's
    /// memory bounded now that `remove_peer` no longer tears a channel down
    /// just because it's momentarily empty (needed for Phase 7 resume) —
    /// without it, every session ever touched across the server's whole
    /// uptime would accumulate in memory forever. Safe to call alongside
    /// anything else: a session can only be evicted while it has zero
    /// peers, so it can never race an active `apply_buffer_op`/chat/save
    /// call, which always comes from a currently-connected peer. Also
    /// clears the corresponding metrics/watcher-generation entries so
    /// *those* maps don't become their own slow leak in parallel.
    pub async fn evict_idle_sessions(&self, idle_threshold: Duration) -> Vec<String> {
        let evicted: Vec<String> = {
            let mut map = self.inner.write().await;
            let to_evict: Vec<String> = map
                .iter()
                .filter(|(_, channel)| channel.peers.is_empty() && channel.last_activity.elapsed() >= idle_threshold)
                .map(|(session_id, _)| session_id.clone())
                .collect();
            for session_id in &to_evict {
                map.remove(session_id);
            }
            to_evict
        };

        if !evicted.is_empty() {
            // `HashSet` lookup, not `Vec::contains` — `retain` below calls
            // this once per `watcher_generations` entry, so an O(n) lookup
            // here would make the whole cleanup O(sessions × generations)
            // at real scale (many evicted sessions, many tracked
            // (session, path) pairs across the server).
            let evicted_set: HashSet<&str> = evicted.iter().map(String::as_str).collect();
            let mut watcher_generations = self.watcher_generations.write().await;
            watcher_generations.retain(|(session_id, _), _| !evicted_set.contains(session_id.as_str()));
            let mut metrics = self.metrics.write().await;
            for session_id in &evicted {
                metrics.remove(session_id);
            }
        }
        evicted
    }

    async fn update_metrics(&self, session_id: &str, update: impl FnOnce(&mut SessionMetrics)) {
        let snapshot = {
            let mut metrics = self.metrics.write().await;
            let entry = metrics.entry(session_id.to_string()).or_default();
            update(entry);
            entry.clone()
        };
        self.broadcast(
            session_id,
            &json!({"type": "metrics_update", "metrics": snapshot}),
        )
        .await;
    }

    /// Ensures a CRDT buffer exists for `path` in this session (seeded from
    /// its current plain-text content, if any), assigns `user_id` a stable
    /// replica id for the session if it doesn't already have one, and
    /// returns both plus the buffer's current text — what a (re)joining
    /// client needs to construct a matching local buffer.
    ///
    /// Caveat: a client that joins after a buffer already has edit history
    /// only inherits the current *text*, not the fragment-level provenance
    /// of who originally wrote each pre-existing span. Every operation from
    /// this point forward — by this client or any other — merges with full
    /// CRDT correctness; only the exact boundary of text that existed before
    /// this join falls back to whichever edit landed last, rather than true
    /// provenance-preserving merge. A full fix needs wire-transferring the
    /// buffer's internal fragment snapshot, not just its text, which is a
    /// larger follow-up than this pass covers.
    /// Hands out a replica for `path`. Every join gets a fresh replica id —
    /// a reconnecting client starts its Lamport clock over, so reusing its
    /// old id would mint timestamps the buffer has already seen and its new
    /// edits would be dropped as duplicates. The reply carries the base text
    /// and full operation history (not just the visible text) so the joiner
    /// rebuilds the exact same CRDT state; offsets in later edits only line
    /// up if deleted fragments exist on both sides.
    pub async fn join_buffer(
        &self,
        session_id: &str,
        _user_id: &str,
        path: &str,
    ) -> anyhow::Result<BufferJoin> {
        let mut map = self.inner.write().await;
        let channel = map
            .get_mut(session_id)
            .context("session has no active channel")?;

        let replica_id = clock::ReplicaId::new(channel.next_replica_id);
        channel.next_replica_id = channel
            .next_replica_id
            .checked_add(1)
            .context("session ran out of replica ids")?;

        if !channel.buffers.contains_key(path) {
            let base_text = channel.files.get(path).cloned().unwrap_or_default();
            let buffer_id = text::BufferId::new(1).context("failed to construct buffer id")?;
            channel.buffers.insert(
                path.to_string(),
                text::Buffer::new(clock::ReplicaId::REMOTE_SERVER, buffer_id, base_text),
            );
        }

        let buffer = channel
            .buffers
            .get(path)
            .context("buffer missing right after insertion")?;
        Ok(BufferJoin {
            replica_id,
            text: buffer.text(),
            base_text: buffer.base_text().to_string(),
            operations: buffer
                .operations()
                .iter()
                .map(|(_, operation)| WireOperation::from(operation))
                .collect(),
        })
    }

    /// Applies a remote operation to the session's server-side mirror buffer
    /// for `path` and refreshes the plain-text snapshot (`files`) to match,
    /// so the rest of the system (agent context, blueprint generation, the
    /// watcher) transparently sees the merged result. Returns false if the
    /// session or buffer doesn't exist yet — the caller should always
    /// `join_buffer` before sending operations.
    /// This is the hottest write path in the whole server — called on
    /// every merged keystroke, from every file, in every session. Its
    /// locking is split into two short critical sections specifically so
    /// the CPU-bound tree-sitter parse in the middle never runs while
    /// holding the lock: `self.inner` is one global `RwLock` shared by
    /// every session on the server, so a parse taking even a few
    /// milliseconds while holding it would stall unrelated operations in
    /// every *other* session behind this one file's merge — at thousands of
    /// concurrent editors, that turns a per-file cost into a server-wide one.
    pub async fn apply_buffer_op(
        &self,
        session_id: &str,
        path: &str,
        operation: text::Operation,
    ) -> bool {
        let merged_text = {
            let mut map = self.inner.write().await;
            let Some(channel) = map.get_mut(session_id) else {
                return false;
            };
            let Some(buffer) = channel.buffers.get_mut(path) else {
                return false;
            };
            buffer.apply_ops([operation]);
            buffer.text()
        };

        // No lock held across this — the whole point of splitting the
        // critical section. Benign tradeoff: a concurrent merge to the same
        // path from another peer could land in between, and the symbol
        // index committed below would then briefly reflect an
        // already-superseded text snapshot. That's fine for a relevance
        // signal (see `symbols.rs`'s own doc comment — "ranked on weaker
        // evidence," not correctness-critical); the *next* merge to this
        // path re-parses and corrects it. `files`/`file_order`, which *do*
        // need to end up correct, are written from `merged_text` captured
        // under the second lock below, specific to this call's own result,
        // not from anything read outside it.
        let new_symbols = symbols::extract_symbols(path, &merged_text);

        {
            let mut map = self.inner.write().await;
            let Some(channel) = map.get_mut(session_id) else {
                return false;
            };
            channel.symbols.insert(path.to_string(), new_symbols);
            channel.files.insert(path.to_string(), merged_text);
            channel.file_order.retain(|existing| existing != path);
            channel.file_order.insert(0, path.to_string());
            channel.last_activity = Instant::now();
        }
        self.record_merge(session_id).await;
        true
    }

    /// Ranks every synced file in the session by relevance to `query_terms`
    /// (symbol names, typically — either the watcher's `matched_symbols` or
    /// terms pulled from the planning conversation) and greedily returns
    /// files in that order until `budget_chars` of content is collected.
    /// This replaces a fixed file-count cap: the number of files returned
    /// varies with how many are actually relevant and how big they are, not
    /// a flat N regardless of session size or content.
    ///
    /// Scoring: a query term matching one of a file's indexed symbol names
    /// (from `symbols.rs`) counts for more than the same term merely
    /// appearing somewhere in the raw text, since a symbol-name match means
    /// the file *defines or directly names* the thing in question rather
    /// than just mentioning it in passing (e.g. a comment). Files with no
    /// symbol index (unsupported language, or simply unparsed yet) still
    /// get scored via the raw-text fallback, so they're never silently
    /// excluded — just ranked on weaker evidence. Recency (position in
    /// `file_order`) breaks ties among equally-scored files.
    pub async fn relevant_files(
        &self,
        session_id: &str,
        query_terms: &[String],
        exclude_path: Option<&str>,
        budget_chars: usize,
    ) -> Vec<(String, String)> {
        const SYMBOL_MATCH_SCORE: u32 = 3;
        const TEXT_MATCH_SCORE: u32 = 1;

        let map = self.inner.read().await;
        let Some(channel) = map.get(session_id) else {
            return Vec::new();
        };

        // Every synced file is scored, including a zero score for one that
        // matches nothing — this is deliberate, not an oversight: it means
        // empty/unmatched `query_terms` degrade gracefully to a pure
        // recency ordering (via the tiebreak below) instead of returning
        // nothing, so a caller never has to special-case "no terms yet."
        let mut scored: Vec<(u32, usize, &String)> = channel
            .file_order
            .iter()
            .enumerate()
            .filter(|(_, path)| Some(path.as_str()) != exclude_path)
            .filter_map(|(recency_rank, path)| {
                let content = channel.files.get(path)?;
                let symbol_names = channel.symbols.get(path);
                let mut score = 0u32;
                for term in query_terms {
                    let symbol_hit = symbol_names
                        .map(|names| names.iter().any(|symbol| symbol.name == *term))
                        .unwrap_or(false);
                    if symbol_hit {
                        score += SYMBOL_MATCH_SCORE;
                    } else if content.contains(term.as_str()) {
                        score += TEXT_MATCH_SCORE;
                    }
                }
                Some((score, recency_rank, path))
            })
            .collect();

        // Higher score first; among equal scores, more-recently-saved first
        // (lower recency_rank, since file_order is most-recent-first).
        scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));

        let mut selected = Vec::new();
        let mut used_chars = 0usize;
        for (_, _, path) in scored {
            let Some(content) = channel.files.get(path) else {
                continue;
            };
            if used_chars > 0 && used_chars + content.chars().count() > budget_chars {
                break;
            }
            used_chars += content.chars().count();
            selected.push((path.clone(), content.clone()));
        }
        selected
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

    let membership: Option<(String,)> =
        sqlx::query_as("SELECT user_id FROM session_members WHERE session_id = ? AND user_id = ?")
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

/// A readable name for every member of the session, keyed by user id. Users
/// only have an email on record, so the part before the "@" stands in.
async fn member_display_names(state: &AppState, session_id: &str) -> HashMap<String, String> {
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT users.id, users.email FROM session_members \
         JOIN users ON users.id = session_members.user_id \
         WHERE session_members.session_id = ?",
    )
    .bind(session_id)
    .fetch_all(&state.pool)
    .await
    .unwrap_or_else(|error| {
        eprintln!("ws: loading member names for {session_id} failed: {error}");
        Vec::new()
    });
    rows.into_iter()
        .map(|(id, email)| {
            let name = email.split('@').next().unwrap_or(&email).to_string();
            (id, name)
        })
        .collect()
}

async fn handle_socket(
    mut socket: WebSocket,
    state: AppState,
    session_id: String,
    user_id: String,
) {
    let mut rx = state.hub.add_peer(&session_id, &user_id).await;
    // A channel with no files yet might be brand new, or might be an empty
    // shell the server just recreated after a restart — rehydrate from the
    // last DB checkpoint before anyone reads from it. A no-op (fast: one
    // indexed SELECT) when the channel already has state, which is the
    // common case on every join after the first.
    rehydrate_session_from_checkpoint(&state, &session_id).await;

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
    // Phase 7 resume: a reconnecting client needs its files/chat history
    // back, not just contracts/intents/blueprint — those three were already
    // covered, but files and chat were previously dropped on reconnect.
    let files = state.hub.get_all_files(&session_id).await;
    let chat_history = state
        .hub
        .recent_chat_history(&session_id, CHAT_HISTORY_CAP)
        .await;
    let catch_up = state.hub.catch_up_digest(&session_id, &user_id).await;
    let presence = state.hub.get_presence(&session_id).await;
    let metrics = state.hub.get_metrics(&session_id).await;
    // (has_leader, owner_id, name) — every peer needs this from the
    // session's own record, not just whoever created it locally, so
    // everyone agrees on whether this session has a leader, who it is,
    // and what the session is actually called (a joiner otherwise has no
    // way to learn the name the creator typed — see problem.md #8).
    let leader_info: Option<(bool, String, String, String)> =
        sqlx::query_as("SELECT has_leader, owner_id, name, goal FROM sessions WHERE id = ?")
            .bind(&session_id)
            .fetch_optional(&state.pool)
            .await
            .unwrap_or(None);
    let (has_leader, leader_id, session_name, session_goal) = match leader_info {
        Some((has_leader, owner_id, name, goal)) => {
            (has_leader, has_leader.then_some(owner_id), name, goal)
        }
        None => (true, None, String::new(), String::new()),
    };

    let peer_names = member_display_names(&state, &session_id).await;

    let snapshot = json!({
        "type": "snapshot",
        "contracts": contracts,
        "intents": intents,
        "peers": peers,
        "blueprint": blueprint,
        "files": files,
        "chat_history": chat_history,
        "catch_up": catch_up,
        "presence": presence,
        "metrics": metrics,
        "has_leader": has_leader,
        "leader_id": leader_id,
        "name": session_name,
        "goal": session_goal,
        "peer_names": peer_names,
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
        .broadcast(
            &session_id,
            &json!({
                "type": "peer_joined",
                "user_id": user_id,
                "name": peer_names.get(&user_id),
            }),
        )
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
                        if let Some(reply) = handle_client_message(&state, &session_id, &user_id, text.as_str()).await
                            && socket.send(Message::Text(reply.to_string().into())).await.is_err() {
                                break;
                        }
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
        .broadcast(
            &session_id,
            &json!({"type": "peer_left", "user_id": user_id}),
        )
        .await;
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ClientMessage {
    Chat { text: String },
    // A model reply generated on one client (with that user's own key),
    // relayed so the whole session sees one conversation.
    AiReply { label: String, text: String },
    // Clients still send a `sender` field; it's ignored (unknown fields are
    // skipped) because the server stamps the authenticated user instead.
    FileOpen { path: String },
    FileSave { path: String, content: String },
    // A live cursor/caret position within `path`, broadcast on a best-effort
    // basis as the sender's editor selection moves. Not persisted anywhere
    // (no `Hub` state, not part of `Snapshot`) — purely ephemeral, relayed
    // straight through the same way `FileOpen` is, since a newly-joining
    // peer doesn't need anyone's historical cursor position, only live
    // updates from the moment they connect onward.
    CursorMoved { path: String, row: u32 },
    // Requests (or re-requests, after a reconnect) a CRDT replica id and the
    // current text for `path`, so the client can construct a local buffer
    // that merges correctly with everyone else's edits from this point on.
    BufferJoin { path: String },
    // A locally-generated edit operation, produced by the client's own
    // buffer, to merge into the server's mirror and relay to every other peer.
    BufferOp { path: String, op: WireOperation },
}

/// Parses an inbound text frame and dispatches it. The sender's identity for
/// chat always comes from the authenticated `user_id`, not the payload, so a
/// peer can't post chat messages impersonating someone else. Unknown types
/// and malformed JSON are ignored silently. Returns a reply to send back to
/// just this connection, for message types (like `BufferJoin`) whose
/// response is peer-specific rather than something to broadcast.
async fn handle_client_message(
    state: &AppState,
    session_id: &str,
    user_id: &str,
    text: &str,
) -> Option<serde_json::Value> {
    let Ok(message) = serde_json::from_str::<ClientMessage>(text) else {
        return None;
    };

    match message {
        ClientMessage::Chat { text } => {
            handle_chat_message(state, session_id, user_id, text).await;
            None
        }
        ClientMessage::AiReply { label, text } => {
            handle_ai_reply(state, session_id, user_id, label, text).await;
            None
        }
        // The payload's `sender` is ignored in favor of the authenticated
        // user, same as chat, so presence can't be spoofed and every client
        // agrees on one id per person.
        ClientMessage::FileOpen { path } => {
            state
                .hub
                .set_presence(session_id, user_id, path.clone())
                .await;
            state
                .hub
                .broadcast(
                    session_id,
                    &json!({"type": "file_open", "sender": user_id, "path": path}),
                )
                .await;
            None
        }
        ClientMessage::FileSave { path, content } => {
            handle_file_save_message(state, session_id, user_id, path, content).await;
            None
        }
        ClientMessage::CursorMoved { path, row } => {
            state
                .hub
                .broadcast(
                    session_id,
                    &json!({
                        "type": "cursor_moved",
                        "sender": user_id,
                        "path": path,
                        "row": row,
                    }),
                )
                .await;
            None
        }
        ClientMessage::BufferJoin { path } => {
            match state.hub.join_buffer(session_id, user_id, &path).await {
                Ok(join) => Some(json!({
                    "type": "buffer_joined",
                    "path": path,
                    "replica_id": join.replica_id.as_u16(),
                    "text": join.text,
                    "base_text": join.base_text,
                    "operations": join.operations,
                })),
                Err(error) => {
                    eprintln!("ws: buffer_join failed for {path} in session {session_id}: {error}");
                    None
                }
            }
        }
        ClientMessage::BufferOp { path, op } => {
            let previous_text = state
                .hub
                .get_file(session_id, &path)
                .await
                .unwrap_or_default();
            let operation: text::Operation = op.clone().into();
            if state
                .hub
                .apply_buffer_op(session_id, &path, operation)
                .await
            {
                state
                    .hub
                    .broadcast(
                        session_id,
                        &json!({"type": "buffer_op", "path": path, "sender": user_id, "op": op}),
                    )
                    .await;
                let new_text = state
                    .hub
                    .get_file(session_id, &path)
                    .await
                    .unwrap_or_default();
                check_work_lane(state, session_id, &path, user_id).await;
                trigger_watcher(state, session_id, &path, &previous_text, &new_text).await;
            }
            None
        }
    }
}

/// Rehydrates a freshly-created, still-empty session channel from its most
/// recent automatic checkpoint (see `checkpoint.rs`) — the piece that lets a
/// session's files/blueprint survive a *server* restart, as opposed to
/// `Hub::remove_peer`'s doc comment, which covers everyone merely
/// disconnecting while the process stays up. A no-op whenever the channel
/// already has files, so a slow DB round trip never races a channel that's
/// already live. Chat history has no durable copy and isn't recovered here
/// — only `files`/`blueprint` are checkpointed today.
async fn rehydrate_session_from_checkpoint(state: &AppState, session_id: &str) {
    if !state.hub.get_all_files(session_id).await.is_empty() {
        return;
    }
    let row: Option<(String, Option<String>)> = match sqlx::query_as(
        "SELECT content, files FROM execution_plans WHERE session_id = ? AND status = 'checkpoint' ORDER BY created_at DESC LIMIT 1",
    )
    .bind(session_id)
    .fetch_optional(&state.pool)
    .await
    {
        Ok(row) => row,
        Err(error) => {
            eprintln!("ws: checkpoint lookup failed for session {session_id}: {error}");
            return;
        }
    };

    let Some((content, files_json)) = row else {
        return;
    };
    let files: HashMap<String, String> = match files_json.as_deref().map(serde_json::from_str) {
        Some(Ok(files)) => files,
        Some(Err(error)) => {
            eprintln!("ws: corrupt checkpoint snapshot for session {session_id}: {error}");
            return;
        }
        None => return,
    };
    if files.is_empty() {
        return;
    }
    // `content` is the sentinel string when the blueprint was empty at
    // checkpoint time (see `checkpoint::write_checkpoint`) — treat that as
    // "no blueprint" rather than literally restoring that placeholder text.
    let blueprint = if content == "Automatic checkpoint" {
        String::new()
    } else {
        content
    };
    state
        .hub
        .seed_files_if_empty(session_id, files, blueprint)
        .await;
}

/// True if `user_id` owns `session_id` — the same owner-authority check
/// `handlers::require_session_owner` applies to the HTTP plan endpoints,
/// duplicated here rather than shared because that function lives in a
/// different module and returns an HTTP-flavored `AppError` on failure,
/// not a plain bool a chat-trigger gate can branch on directly.
async fn is_session_owner(state: &AppState, session_id: &str, user_id: &str) -> bool {
    let owner_id: Option<String> = sqlx::query_scalar("SELECT owner_id FROM sessions WHERE id = ?")
        .bind(session_id)
        .fetch_optional(&state.pool)
        .await
        .unwrap_or_default();
    owner_id.as_deref() == Some(user_id)
}

async fn handle_chat_message(state: &AppState, session_id: &str, user_id: &str, text: String) {
    let message = ChatMessage {
        sender: user_id.to_string(),
        text,
    };

    state
        .hub
        .push_chat_message(session_id, message.clone())
        .await;
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
        // This path spends the server's own shared key and writes the
        // reference live with no approval step — reserved for the session
        // owner, who's the one actually spending that money and the one
        // with commit authority anyway. Anyone else gets routed to the
        // client's own BYOK generate-reference flow (create_plan +
        // owner approve), which is the one place non-owners can propose a
        // reference update at all.
        if !is_session_owner(state, session_id, user_id).await {
            state
                .hub
                .broadcast(
                    session_id,
                    &json!({
                        "type": "chat",
                        "sender": AGENT_USER_ID,
                        "text": "Only the session owner can regenerate the reference from chat. Use the Generate Reference button to propose one with your own key for the owner to approve.",
                    }),
                )
                .await;
            return;
        }
        let state = state.clone();
        let session_id = session_id.to_string();
        tokio::spawn(async move {
            agent::run_blueprint_job(state, session_id).await;
        });
    } else if lowered.contains("@agent triage") {
        // Triage reviews every synced file in the session at once (not
        // just whatever the requester has open), which only the server
        // can assemble today — see agent::handle_triage_request. That
        // means this spends the shared key the same way "@agent
        // blueprint" does, so it gets the same owner-only gate and for
        // the same reason: whoever's about to decide what the group
        // commits to is the one who should be triggering (and paying
        // for) the review that informs that decision.
        if !is_session_owner(state, session_id, user_id).await {
            state
                .hub
                .broadcast(
                    session_id,
                    &json!({
                        "type": "chat",
                        "sender": AGENT_USER_ID,
                        "text": "Only the session owner can run a triage review.",
                    }),
                )
                .await;
            return;
        }
        let state = state.clone();
        let session_id = session_id.to_string();
        tokio::spawn(async move {
            agent::run_triage_job(state, session_id).await;
        });
    } else if lowered.contains("@agent") {
        let state = state.clone();
        let session_id = session_id.to_string();
        tokio::spawn(async move {
            agent::handle_mention(state, session_id).await;
        });
    }
}

/// Retains the saved content for later blueprint generation, then defers to
/// `trigger_watcher` for the cost-tiered check. This is the legacy
/// whole-file path, kept for a path's first sync before any CRDT buffer
/// exists for it (see `ClientMessage::BufferJoin`/`BufferOp`); once a buffer
/// exists, the client switches to sending `BufferOp` instead, so this and
/// the CRDT path never both overwrite `files` for the same path at once —
/// that double-write would silently reintroduce last-write-wins underneath
/// the CRDT merge. The file content is never broadcast to peers.
/// Only the client's own fixed AI labels are accepted as the sender, so a
/// peer can't use this path to post chat under an arbitrary name. `author`
/// lets the asking client skip the echo of a reply it already shows.
async fn handle_ai_reply(state: &AppState, session_id: &str, user_id: &str, label: String, text: String) {
    if !AI_REPLY_LABELS.contains(&label.as_str()) || text.trim().is_empty() {
        return;
    }
    let message = ChatMessage {
        sender: label,
        text,
    };
    state
        .hub
        .push_chat_message(session_id, message.clone())
        .await;
    state
        .hub
        .broadcast(
            session_id,
            &json!({"type": "chat", "sender": message.sender, "text": message.text, "author": user_id}),
        )
        .await;
}

const AI_REPLY_LABELS: [&str; 3] = ["AI (Ask)", "AI (Plan)", "AI (Agent)"];

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

    let previous_content = state
        .hub
        .get_file(session_id, &path)
        .await
        .unwrap_or_default();
    state
        .hub
        .save_file(session_id, path.clone(), content.clone())
        .await;
    // Peers decide locally whether to write it (only when they don't already
    // have a different copy), so a share never clobbers someone's work.
    state
        .hub
        .broadcast(
            session_id,
            &json!({"type": "file_saved", "path": path, "sender": user_id, "content": content}),
        )
        .await;
    check_work_lane(state, session_id, &path, user_id).await;
    trigger_watcher(state, session_id, &path, &previous_content, &content).await;
}

/// Phase 6's work-lane warning: if the blueprint's "## Work Split" section
/// assigns `path` to a participant other than whoever just saved it, warns
/// the session instead of silently letting the edit stand — advisory only,
/// since the CRDT layer already guarantees nothing is lost; this is about
/// surfacing an *ownership* drift, not a data-loss risk. A path the
/// blueprint doesn't mention, or assigns to `agent` or to the saver
/// themself, is not a warning case.
async fn check_work_lane(state: &AppState, session_id: &str, path: &str, user_id: &str) {
    let blueprint = state.hub.get_blueprint(session_id).await;
    if blueprint.is_empty() {
        return;
    }
    let owners = agent::parse_work_split(&blueprint);
    let Some(owner) = owners.get(path) else {
        return;
    };
    if owner == user_id || owner == AGENT_USER_ID {
        return;
    }
    state.hub.record_lane_warning(session_id).await;
    state
        .hub
        .broadcast(
            session_id,
            &json!({"type": "lane_warning", "path": path, "editor": user_id, "owner": owner}),
        )
        .await;
}

/// Shared by both `handle_file_save_message` (the legacy whole-file path)
/// and the CRDT `BufferOp` path: runs the free gatekeeper check against the
/// post-change text and, only if it matches a blueprint symbol, debounces
/// and spawns the paid watcher call with a diff of what changed.
async fn trigger_watcher(
    state: &AppState,
    session_id: &str,
    path: &str,
    previous_text: &str,
    new_text: &str,
) {
    let blueprint = state.hub.get_blueprint(session_id).await;
    let matched_symbols = agent::matching_blueprint_symbols(&blueprint, new_text);
    if matched_symbols.is_empty() {
        return;
    }

    let diff = agent::diff_content(previous_text, new_text);
    // Rank by the matched symbols themselves rather than recency, so a file
    // that calls `calculate_total` gets surfaced even if nobody's touched it
    // in this session for an hour — the whole point of cross-file awareness
    // is catching exactly that case, which a recency cutoff would miss.
    let other_files = state
        .hub
        .relevant_files(
            session_id,
            &matched_symbols,
            Some(path),
            agent::CROSS_FILE_SCAN_BUDGET_CHARS,
        )
        .await;
    let cross_file = agent::cross_file_context(&other_files, &matched_symbols);
    let generation = state.hub.bump_watcher_generation(session_id, path).await;

    let state = state.clone();
    let session_id = session_id.to_string();
    let path = path.to_string();
    tokio::spawn(async move {
        tokio::time::sleep(WATCHER_DEBOUNCE).await;
        if !state
            .hub
            .is_current_watcher_generation(&session_id, &path, generation)
            .await
        {
            // A newer change to this same path arrived during the debounce
            // window; that change's own task will run the check instead.
            return;
        }
        agent::check_file_save(
            state,
            session_id,
            path,
            diff,
            cross_file,
            blueprint,
            matched_symbols,
        )
        .await;
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn presence_tracks_most_recent_open_path() {
        let hub = Hub::new();
        hub.add_peer("s1", "alice").await;
        assert!(hub.get_presence("s1").await.is_empty());

        hub.set_presence("s1", "alice", "a.rs".to_string()).await;
        hub.set_presence("s1", "alice", "b.rs".to_string()).await;
        assert_eq!(
            hub.get_presence("s1").await.get("alice"),
            Some(&"b.rs".to_string())
        );
    }

    #[tokio::test]
    async fn remove_peer_keeps_channel_state_for_resume() {
        let hub = Hub::new();
        hub.add_peer("s1", "alice").await;
        hub.save_file("s1", "a.rs".to_string(), "fn a() {}".to_string())
            .await;
        hub.remove_peer("s1", "alice").await;

        // Phase 7: disconnecting must not drop session state — only a
        // server restart (handled separately, via checkpoint rehydration)
        // should ever reset it.
        assert_eq!(
            hub.get_file("s1", "a.rs").await,
            Some("fn a() {}".to_string())
        );
    }

    #[tokio::test]
    async fn catch_up_digest_is_none_for_a_first_time_join() {
        let hub = Hub::new();
        hub.add_peer("s1", "alice").await;
        hub.push_chat_message(
            "s1",
            ChatMessage {
                sender: "bob".to_string(),
                text: "hi".to_string(),
            },
        )
        .await;
        assert_eq!(hub.catch_up_digest("s1", "alice").await, None);
    }

    #[tokio::test]
    async fn catch_up_digest_reports_messages_missed_since_disconnect() {
        let hub = Hub::new();
        hub.add_peer("s1", "alice").await;
        hub.remove_peer("s1", "alice").await; // records last_seen_total_messages = 0
        hub.push_chat_message(
            "s1",
            ChatMessage {
                sender: "bob".to_string(),
                text: "fixed the bug".to_string(),
            },
        )
        .await;

        let digest = hub
            .catch_up_digest("s1", "alice")
            .await
            .expect("should have a digest");
        assert!(digest.contains("1 message"));
        assert!(digest.contains("fixed the bug"));

        assert_eq!(
            hub.catch_up_digest("s1", "bob").await,
            None,
            "bob never left, so nothing to catch up on"
        );
    }

    /// Regression test for a real bug caught in review: `catch_up_digest`
    /// used to store a `history` vec *index* at disconnect time, but
    /// `push_chat_message` left-drains `history` once it exceeds
    /// `CHAT_HISTORY_CAP` — so a stored index silently pointed at the wrong
    /// messages after any drain happened while the user was away. This
    /// drives the drain hard enough to prove the counter-based fix doesn't
    /// have the same problem: it must still report the right count and the
    /// right (most recent, still-retained) message text, not garbage.
    #[tokio::test]
    async fn catch_up_digest_survives_history_cap_eviction() {
        let hub = Hub::new();
        hub.add_peer("s1", "alice").await;
        hub.remove_peer("s1", "alice").await; // last_seen_total_messages = 0

        // Push well past CHAT_HISTORY_CAP (50) while alice is away, forcing
        // `push_chat_message`'s drain to run repeatedly.
        for i in 0..70 {
            hub.push_chat_message(
                "s1",
                ChatMessage { sender: "bob".to_string(), text: format!("message {i}") },
            )
            .await;
        }

        let digest = hub.catch_up_digest("s1", "alice").await.expect("should have a digest");
        // The true count (70) must be reported even though only 50 messages
        // physically survived the cap — this is the honest-degradation path
        // documented on `catch_up_digest`, not silent wrong data.
        assert!(digest.contains("70 messages"), "digest was: {digest}");
        // The most recent message must still be the correct tail content,
        // not whatever happened to sit at a stale vec index.
        assert!(digest.contains("message 69"), "digest was: {digest}");
    }

    #[tokio::test]
    async fn relevant_files_ranks_symbol_matches_above_recency() {
        let hub = Hub::new();
        hub.add_peer("s1", "alice").await;
        // Saved first (so it's the older entry in file_order) but the one
        // whose structural index actually defines `calculate_total`.
        hub.save_file(
            "s1",
            "invoice.rs".to_string(),
            "fn calculate_total() -> u32 { 0 }".to_string(),
        )
        .await;
        // Saved most recently, but unrelated — recency alone would rank it first.
        hub.save_file(
            "s1",
            "unrelated.rs".to_string(),
            "fn unrelated() {}".to_string(),
        )
        .await;

        let results = hub
            .relevant_files("s1", &["calculate_total".to_string()], None, 10_000)
            .await;
        assert_eq!(
            results.first().map(|(path, _)| path.as_str()),
            Some("invoice.rs")
        );
    }

    #[tokio::test]
    async fn rejoining_replays_history_and_gets_a_fresh_replica() {
        let hub = Hub::new();
        hub.add_peer("s1", "alice").await;
        hub.save_file("s1", "a.rs".to_string(), "hello world".to_string()).await;

        let first = hub.join_buffer("s1", "alice", "a.rs").await.unwrap();
        let mut alice = text::Buffer::new(first.replica_id, text::BufferId::new(1).unwrap(), first.base_text);
        // A deletion leaves a tombstone, which is exactly what a plain-text
        // join used to lose.
        let delete = alice.edit([(0..6, "")]);
        assert!(hub.apply_buffer_op("s1", "a.rs", delete).await);

        let second = hub.join_buffer("s1", "alice", "a.rs").await.unwrap();
        assert_ne!(first.replica_id, second.replica_id);
        let mut rejoined =
            text::Buffer::new(second.replica_id, text::BufferId::new(1).unwrap(), second.base_text);
        rejoined.apply_ops(second.operations.into_iter().map(text::Operation::from));
        assert_eq!(rejoined.text(), "world");
        assert_eq!(rejoined.text(), second.text);

        let append = rejoined.edit([(5..5, "!")]);
        assert!(hub.apply_buffer_op("s1", "a.rs", append).await);
        assert_eq!(hub.get_file("s1", "a.rs").await.as_deref(), Some("world!"));
    }

    #[tokio::test]
    async fn apply_buffer_op_increments_merge_metric() {
        let hub = Hub::new();
        hub.add_peer("s1", "alice").await;
        hub
            .join_buffer("s1", "alice", "a.rs")
            .await
            .expect("join_buffer should succeed");

        assert_eq!(hub.get_metrics("s1").await.merges_applied, 0);

        let buffer_id = text::BufferId::new(1).unwrap();
        let mut buffer = text::Buffer::new(clock::ReplicaId::new(5), buffer_id, String::new());
        let operation = buffer.edit([(0..0, "hello")]);
        hub.apply_buffer_op("s1", "a.rs", operation).await;

        assert_eq!(hub.get_metrics("s1").await.merges_applied, 1);
    }

    #[tokio::test]
    async fn evict_idle_sessions_only_removes_empty_and_expired() {
        let hub = Hub::new();

        hub.add_peer("idle", "alice").await;
        hub.save_file("idle", "a.rs".to_string(), "fn a() {}".to_string()).await;
        hub.remove_peer("idle", "alice").await; // zero peers, will be "idle" long enough

        hub.add_peer("occupied", "bob").await; // zero-idle-time, but still has a peer

        // An idle threshold of zero means "idle for any amount of time
        // counts" — lets the test not need a real sleep.
        let evicted = hub.evict_idle_sessions(Duration::from_secs(0)).await;

        assert_eq!(evicted, vec!["idle".to_string()]);
        assert_eq!(hub.get_file("idle", "a.rs").await, None, "the whole channel, including its files, must be gone");
        assert_eq!(
            hub.peer_ids("occupied").await,
            vec!["bob".to_string()],
            "a session with a connected peer must never be evicted"
        );
    }

    #[tokio::test]
    async fn evict_idle_sessions_respects_the_ttl() {
        let hub = Hub::new();
        hub.add_peer("fresh", "alice").await;
        hub.save_file("fresh", "a.rs".to_string(), "fn a() {}".to_string()).await;
        hub.remove_peer("fresh", "alice").await;

        // A long TTL means "not idle long enough yet" — must survive.
        let evicted = hub.evict_idle_sessions(Duration::from_secs(3600)).await;
        assert!(evicted.is_empty());
        assert_eq!(
            hub.get_file("fresh", "a.rs").await,
            Some("fn a() {}".to_string()),
            "channel and its files must still be there — not evicted yet"
        );
    }
}
