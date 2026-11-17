//! Team memory: pinned decisions, and search over what a team has said and
//! done (decisions, the task board, team chat, and the agent runs and steps
//! people chose to keep).
//!
//! Search ranks items by keyword relevance (BM25, with word endings and
//! `snake_case`/`camelCase` names split, so "fees" finds `late_fee`). When
//! `VOYAGE_API_KEY` is set it also embeds the query and the items (cached in
//! `memory_embeddings`) and blends in semantic similarity, so "fines" can
//! find "late fees". Without the key, keyword search still works.
//!
//! Nothing is embedded before keys and passwords are masked, and embedding
//! counts against the session's daily AI budget.
//!
//! Safeguards keep one wrong claim from spreading to every agent:
//! - a decision can replace an older one, which then stops being served;
//! - an agent run (and its steps) is only remembered once a teammate accepts
//!   it, and never once someone sent it back (in a one-person session the
//!   owner's own runs count, since nobody else could review them);
//! - anything a person flags as wrong is withheld;
//! - every hit says where it came from, so agents can weigh it.

use std::collections::{HashMap, HashSet};

use axum::extract::{Path, Query, State};
use axum::{Extension, Json};
use serde::{Deserialize, Serialize};

use crate::AppState;
use crate::auth::AuthUser;
use crate::errors::AppError;

pub const MAX_DECISIONS: i64 = 30;
const MAX_DECISION_CHARS: usize = 500;
const SEARCH_CORPUS_LIMIT: i64 = 1_000;
const CHAT_CORPUS_LIMIT: i64 = 300;
/// Shorter chat lines ("ok", "thanks") aren't worth remembering.
const MIN_CHAT_CHARS: usize = 25;
/// Rough price of the embedding model per million tokens, for the budget.
const EMBEDDING_USD_PER_MILLION_TOKENS: f64 = 0.18;
/// Override with `VOYAGE_MODEL`.
fn embedding_model() -> String {
    std::env::var("VOYAGE_MODEL").ok().filter(|model| !model.is_empty()).unwrap_or_else(|| "voyage-code-4".to_string())
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Decision {
    pub id: i64,
    pub text: String,
    pub pinned_by: String,
    pub created_at: String,
}

pub fn clean_decision(text: &str) -> Option<String> {
    let text: String = text.trim().chars().take(MAX_DECISION_CHARS).collect();
    (!text.is_empty()).then(|| crate::guard::redact_secrets(&text))
}

pub async fn decisions(pool: &sqlx::SqlitePool, session_id: &str) -> Vec<Decision> {
    let rows: Vec<(i64, String, String, String)> = sqlx::query_as(
        "SELECT id, text, pinned_by, created_at FROM decisions WHERE session_id = ? AND removed = 0 ORDER BY id LIMIT ?",
    )
    .bind(session_id)
    .bind(MAX_DECISIONS)
    .fetch_all(pool)
    .await
    .unwrap_or_default();
    rows.into_iter()
        .map(|(id, text, pinned_by, created_at)| Decision { id, text, pinned_by, created_at })
        .collect()
}

/// Pins a decision; refuses past the per-session limit so the list stays
/// short enough to give every agent. With `replaces`, the new decision takes
/// the old one's place: the old one stops being served (to agents and in
/// memory search) but is kept with a pointer to what replaced it.
pub async fn pin(
    pool: &sqlx::SqlitePool,
    session_id: &str,
    user_id: &str,
    text: &str,
    replaces: Option<i64>,
) -> Result<i64, &'static str> {
    let text = clean_decision(text).ok_or("a decision needs some text")?;
    if let Some(old) = replaces {
        let live: Option<(i64,)> = sqlx::query_as("SELECT id FROM decisions WHERE session_id = ? AND id = ? AND removed = 0")
            .bind(session_id)
            .bind(old)
            .fetch_optional(pool)
            .await
            .map_err(|_| "couldn't save the decision")?;
        if live.is_none() {
            return Err("the decision you're replacing was already unpinned or replaced");
        }
    } else {
        let (count,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM decisions WHERE session_id = ? AND removed = 0")
            .bind(session_id)
            .fetch_one(pool)
            .await
            .map_err(|_| "couldn't save the decision")?;
        if count >= MAX_DECISIONS {
            return Err("this session already has 30 pinned decisions; unpin one first");
        }
    }
    let mut transaction = pool.begin().await.map_err(|_| "couldn't save the decision")?;
    let id = sqlx::query("INSERT INTO decisions (session_id, text, pinned_by, created_at) VALUES (?, ?, ?, ?)")
        .bind(session_id)
        .bind(&text)
        .bind(user_id)
        .bind(chrono::Utc::now().to_rfc3339())
        .execute(&mut *transaction)
        .await
        .map_err(|_| "couldn't save the decision")?
        .last_insert_rowid();
    if let Some(old) = replaces {
        sqlx::query("UPDATE decisions SET removed = 1, superseded_by = ? WHERE session_id = ? AND id = ?")
            .bind(id)
            .bind(session_id)
            .bind(old)
            .execute(&mut *transaction)
            .await
            .map_err(|_| "couldn't save the decision")?;
    }
    transaction.commit().await.map_err(|_| "couldn't save the decision")?;
    Ok(id)
}

/// Pinned decisions that `text` may change or contradict: ones about the same
/// thing (they share at least two key words, and those make up at least a
/// third of the shorter one's). Shown before pinning, so the person decides
/// whether the new one replaces the old or both stand. No AI call.
pub fn related_decisions(text: &str, decisions: &[Decision]) -> Vec<Decision> {
    let new_terms: HashSet<String> = terms(text).into_iter().collect();
    decisions
        .iter()
        .filter(|decision| {
            let old_terms: HashSet<String> = terms(&decision.text).into_iter().collect();
            let shared = new_terms.intersection(&old_terms).count();
            let smaller = new_terms.len().min(old_terms.len()).max(1);
            shared >= 2 && shared * 3 >= smaller
        })
        .cloned()
        .collect()
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ContextFlag {
    /// What's flagged: `run:{user}:{run}`, `step:{user}:{hash}` or `digest`.
    pub key: String,
    pub flagged_by: String,
    pub note: Option<String>,
    pub at: String,
}

/// A flag key the clients and memory search agree on. Steps are keyed by
/// who said it and a stable hash of what they said ([`text_key`]).
pub fn clean_flag_key(key: &str) -> Option<String> {
    let key = key.trim();
    let valid = key.len() <= 200
        && (key == "digest" || key.starts_with("run:") || key.starts_with("step:"))
        && key.chars().all(|c| c.is_ascii_alphanumeric() || "-_:.@".contains(c));
    valid.then(|| key.to_string())
}

/// FNV-1a over the text, in hex: the same on every client and server build
/// (unlike Rust's `DefaultHasher`), so a step's key is stable everywhere.
pub fn text_key(text: &str) -> String {
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in text.trim().as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

pub async fn flags(pool: &sqlx::SqlitePool, session_id: &str) -> Vec<ContextFlag> {
    let rows: Vec<(String, String, Option<String>, String)> = sqlx::query_as(
        "SELECT item_key, flagged_by, note, created_at FROM context_flags WHERE session_id = ? ORDER BY created_at",
    )
    .bind(session_id)
    .fetch_all(pool)
    .await
    .unwrap_or_default();
    rows.into_iter().map(|(key, flagged_by, note, at)| ContextFlag { key, flagged_by, note, at }).collect()
}

pub async fn flag(pool: &sqlx::SqlitePool, session_id: &str, user_id: &str, key: &str, note: Option<&str>) -> Result<(), &'static str> {
    let key = clean_flag_key(key).ok_or("that can't be flagged")?;
    let note = note
        .map(|note| crate::guard::redact_secrets(&note.trim().chars().take(MAX_DECISION_CHARS).collect::<String>()))
        .filter(|note| !note.is_empty());
    sqlx::query(
        "INSERT INTO context_flags (session_id, item_key, flagged_by, note, created_at) VALUES (?, ?, ?, ?, ?) \
         ON CONFLICT(session_id, item_key) DO UPDATE SET flagged_by = excluded.flagged_by, note = excluded.note, \
         created_at = excluded.created_at",
    )
    .bind(session_id)
    .bind(&key)
    .bind(user_id)
    .bind(&note)
    .bind(chrono::Utc::now().to_rfc3339())
    .execute(pool)
    .await
    .map_err(|_| "couldn't save the flag")?;
    Ok(())
}

pub async fn unflag(pool: &sqlx::SqlitePool, session_id: &str, key: &str) {
    let _ = sqlx::query("DELETE FROM context_flags WHERE session_id = ? AND item_key = ?")
        .bind(session_id)
        .bind(key)
        .execute(pool)
        .await;
}

/// Whether team memory may serve an agent run: a teammate accepted it and
/// nobody sent it back. In a one-person session the owner's run counts.
pub fn run_is_trusted(reviews: &[serde_json::Value], solo: bool) -> bool {
    let verdicts: Vec<&str> = reviews.iter().filter_map(|review| review["verdict"].as_str()).collect();
    !verdicts.contains(&"changes") && (solo || verdicts.contains(&"accepted"))
}

pub async fn unpin(pool: &sqlx::SqlitePool, session_id: &str, id: i64) {
    let _ = sqlx::query("UPDATE decisions SET removed = 1 WHERE session_id = ? AND id = ?")
        .bind(session_id)
        .bind(id)
        .execute(pool)
        .await;
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct MemoryHit {
    /// "decision", "task", "chat", "run" or "step".
    pub kind: &'static str,
    /// Where it came from, for agents to weigh it: "decided by people",
    /// "said in chat", "task board", "agent work a teammate accepted" or
    /// "an agent said, unchecked".
    pub source: &'static str,
    pub text: String,
    pub score: f64,
    pub at: String,
}

struct Item {
    key: String,
    kind: &'static str,
    source: &'static str,
    text: String,
    at: String,
}

async fn corpus(pool: &sqlx::SqlitePool, session_id: &str) -> Vec<Item> {
    let mut items = Vec::new();
    for decision in decisions(pool, session_id).await {
        items.push(Item {
            key: format!("decision:{}", decision.id),
            kind: "decision",
            source: "decided by people",
            text: decision.text,
            at: decision.created_at,
        });
    }
    let flagged: HashSet<String> = flags(pool, session_id).await.into_iter().map(|flag| flag.key).collect();
    let (members,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM session_members WHERE session_id = ?")
        .bind(session_id)
        .fetch_one(pool)
        .await
        .unwrap_or((0,));
    let solo = members <= 1;
    let runs: Vec<(String, String, String, String, Option<String>, String, String)> = sqlx::query_as(
        "SELECT user_id, run_id, goal, files, summary, reviews, updated_at FROM agent_runs \
         WHERE session_id = ? ORDER BY updated_at DESC LIMIT ?",
    )
    .bind(session_id)
    .bind(SEARCH_CORPUS_LIMIT)
    .fetch_all(pool)
    .await
    .unwrap_or_default();
    // Runs memory may serve; their steps may be served too.
    let mut trusted_runs: HashSet<(String, String)> = HashSet::new();
    for (user_id, run_id, goal, files, summary, reviews, at) in runs {
        let key = format!("run:{user_id}:{run_id}");
        let reviews: Vec<serde_json::Value> = serde_json::from_str(&reviews).unwrap_or_default();
        if !run_is_trusted(&reviews, solo) || flagged.contains(&key) {
            continue;
        }
        trusted_runs.insert((user_id.clone(), run_id.clone()));
        let files: Vec<String> = serde_json::from_str(&files).unwrap_or_default();
        let notes: Vec<String> = reviews
            .iter()
            .filter_map(|review| {
                let verdict = review["verdict"].as_str()?;
                let note = review["note"].as_str().unwrap_or("");
                Some(format!("{verdict} {note}").trim().to_string())
            })
            .collect();
        let mut text = goal;
        if !files.is_empty() {
            text.push_str(&format!(" — files: {}", files.join(", ")));
        }
        if let Some(summary) = summary.filter(|summary| !summary.is_empty()) {
            text.push_str(&format!(" — result: {summary}"));
        }
        if !notes.is_empty() {
            text.push_str(&format!(" — reviews: {}", notes.join("; ")));
        }
        let source = if solo && notes.is_empty() { "an agent said, unchecked" } else { "agent work a teammate accepted" };
        items.push(Item { key, kind: "run", source, text, at });
    }
    let steps: Vec<(String, Option<String>, String, String, String)> = sqlx::query_as(
        "SELECT user_id, run_id, agent, text, created_at FROM agent_steps WHERE session_id = ? ORDER BY id DESC LIMIT ?",
    )
    .bind(session_id)
    .bind(SEARCH_CORPUS_LIMIT)
    .fetch_all(pool)
    .await
    .unwrap_or_default();
    for (user_id, run_id, agent, text, at) in steps {
        // A step of a run that isn't trusted waits with its run.
        if let Some(run_id) = run_id
            && !trusted_runs.contains(&(user_id.clone(), run_id))
        {
            continue;
        }
        let key = format!("step:{user_id}:{}", text_key(&text));
        if flagged.contains(&key) {
            continue;
        }
        items.push(Item { key, kind: "step", source: "an agent said, unchecked", text: format!("{agent}: {text}"), at });
    }
    let board: Option<(String, String)> =
        sqlx::query_as("SELECT board, updated_at FROM session_tasks WHERE session_id = ?")
            .bind(session_id)
            .fetch_optional(pool)
            .await
            .unwrap_or(None);
    if let Some((board, at)) = board
        && let Ok(board) = serde_json::from_str::<crate::team::TaskBoard>(&board)
    {
        for task in board.tasks {
            let mut text = format!("{} ({})", task.title, task.status);
            if let Some(owner) = &task.assignee {
                text.push_str(&format!(" — owner: {owner}"));
            }
            if !task.detail.is_empty() {
                text.push_str(&format!(" — {}", task.detail));
            }
            if !task.files.is_empty() {
                text.push_str(&format!(" — files: {}", task.files.join(", ")));
            }
            // Keyed by content too: a task's status and owner change.
            items.push(Item {
                key: format!("task:{}:{:x}", task.id, fingerprint(&text)),
                kind: "task",
                source: "task board",
                text,
                at: at.clone(),
            });
        }
    }
    // Who said it, by name (the part of their email before the @), not by
    // account id: an AI reading memory should see "alice", not a UUID.
    let chat: Vec<(i64, String, String, String)> = sqlx::query_as(
        "SELECT chat_messages.id, COALESCE(users.email, chat_messages.sender), chat_messages.text, chat_messages.created_at \
         FROM chat_messages LEFT JOIN users ON users.id = chat_messages.sender \
         WHERE chat_messages.session_id = ? ORDER BY chat_messages.id DESC LIMIT ?",
    )
    .bind(session_id)
    .bind(CHAT_CORPUS_LIMIT)
    .fetch_all(pool)
    .await
    .unwrap_or_default();
    for (id, sender, text, at) in chat {
        let sender = sender.split('@').next().unwrap_or(&sender).to_string();
        if text.chars().count() >= MIN_CHAT_CHARS {
            let text: String = text.chars().take(1_500).collect();
            let source = if sender == crate::ws::AGENT_USER_ID { "an agent said, unchecked" } else { "said in chat" };
            items.push(Item { key: format!("chat:{id}"), kind: "chat", source, text: format!("{sender}: {text}"), at });
        }
    }
    for item in &mut items {
        item.text = crate::guard::redact_secrets(&item.text);
    }
    items
}

fn fingerprint(text: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut hasher);
    hasher.finish()
}

/// Cuts common English endings so "fees", "fee" and "reserving", "reserve"
/// meet. Deliberately small: wrong cuts only make search a little looser.
fn stem(word: &str) -> String {
    let cut = |suffix: &str, min: usize| word.strip_suffix(suffix).filter(|rest| rest.len() >= min);
    if let Some(rest) = cut("ies", 2) {
        return format!("{rest}y");
    }
    for (suffix, min) in [("ing", 4), ("ed", 4), ("es", 4), ("s", 3)] {
        if let Some(rest) = cut(suffix, min) {
            if suffix == "s" && (rest.ends_with('s') || rest.ends_with('u')) {
                break;
            }
            return rest.strip_suffix('e').filter(|short| short.len() >= 4).unwrap_or(rest).to_string();
        }
    }
    word.strip_suffix('e').filter(|short| short.len() >= 4).unwrap_or(word).to_string()
}

/// `lateFee_total` → "late", "fee", "total".
fn split_identifier(word: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut previous_lower = false;
    for character in word.chars() {
        if character == '_' || (character.is_uppercase() && previous_lower) {
            if !current.is_empty() {
                parts.push(std::mem::take(&mut current));
            }
        }
        if character != '_' {
            current.extend(character.to_lowercase());
        }
        previous_lower = character.is_lowercase() || character.is_ascii_digit();
    }
    if !current.is_empty() {
        parts.push(current);
    }
    parts
}

fn terms(text: &str) -> Vec<String> {
    const STOP: &[&str] = &[
        "the", "and", "for", "with", "that", "this", "from", "into", "are", "was", "were", "you", "your",
        "has", "have", "not", "but", "all", "any", "can", "its", "our", "out", "use", "per",
    ];
    text.split(|c: char| !c.is_alphanumeric() && c != '_')
        .flat_map(split_identifier)
        .filter(|word| word.len() >= 3 && !STOP.contains(&word.as_str()))
        .map(|word| stem(&word))
        .collect()
}

/// BM25 scores for `query` over `docs`, one per doc.
pub fn bm25(query: &str, docs: &[String]) -> Vec<f64> {
    let query_terms: HashSet<String> = terms(query).into_iter().collect();
    if query_terms.is_empty() || docs.is_empty() {
        return vec![0.0; docs.len()];
    }
    let tokenized: Vec<Vec<String>> = docs.iter().map(|doc| terms(doc)).collect();
    let average_len = tokenized.iter().map(Vec::len).sum::<usize>() as f64 / docs.len() as f64;
    let mut document_frequency: HashMap<&str, usize> = HashMap::new();
    for doc in &tokenized {
        let unique: HashSet<&str> = doc.iter().map(String::as_str).collect();
        for term in unique {
            *document_frequency.entry(term).or_default() += 1;
        }
    }
    let (k1, b) = (1.2, 0.75);
    let n = docs.len() as f64;
    tokenized
        .iter()
        .map(|doc| {
            let len = doc.len() as f64;
            query_terms
                .iter()
                .map(|term| {
                    let tf = doc.iter().filter(|word| *word == term).count() as f64;
                    if tf == 0.0 {
                        return 0.0;
                    }
                    let df = *document_frequency.get(term.as_str()).unwrap_or(&0) as f64;
                    let idf = ((n - df + 0.5) / (df + 0.5) + 1.0).ln();
                    idf * tf * (k1 + 1.0) / (tf + k1 * (1.0 - b + b * len / average_len.max(1.0)))
                })
                .sum()
        })
        .collect()
}

fn cosine(a: &[f32], b: &[f32]) -> f64 {
    let dot: f64 = a.iter().zip(b).map(|(x, y)| (*x as f64) * (*y as f64)).sum();
    let norm = |v: &[f32]| v.iter().map(|x| (*x as f64) * (*x as f64)).sum::<f64>().sqrt();
    let denominator = norm(a) * norm(b);
    if denominator == 0.0 { 0.0 } else { dot / denominator }
}

async fn voyage_embed(key: &str, texts: &[String], input_type: &str) -> Option<Vec<Vec<f32>>> {
    #[derive(Deserialize)]
    struct Row {
        embedding: Vec<f32>,
    }
    #[derive(Deserialize)]
    struct Reply {
        data: Vec<Row>,
    }
    let reply: Reply = reqwest::Client::new()
        .post("https://api.voyageai.com/v1/embeddings")
        .bearer_auth(key)
        .json(&serde_json::json!({"input": texts, "model": embedding_model(), "input_type": input_type}))
        .send()
        .await
        .ok()?
        .error_for_status()
        .ok()?
        .json()
        .await
        .ok()?;
    Some(reply.data.into_iter().map(|row| row.embedding).collect())
}

/// Semantic similarity per item, or `None` without a key or on any failure
/// (search then falls back to keywords).
async fn semantic_scores(
    pool: &sqlx::SqlitePool,
    budget: Option<(&crate::ws::Hub, &str)>,
    query: &str,
    items: &[Item],
) -> Option<Vec<f64>> {
    let key = std::env::var("VOYAGE_API_KEY").ok().filter(|key| !key.is_empty())?;
    if let Some((hub, session_id)) = budget
        && !hub.ai_budget.has_room(session_id)
    {
        return None;
    }
    let mut embedded_chars = query.chars().count();
    let mut vectors: HashMap<String, Vec<f32>> = HashMap::new();
    for item in items {
        let row: Option<(Vec<u8>,)> = sqlx::query_as("SELECT vector FROM memory_embeddings WHERE item_key = ? AND model = ?")
            .bind(&item.key)
            .bind(embedding_model())
            .fetch_optional(pool)
            .await
            .ok()?;
        if let Some((bytes,)) = row {
            vectors.insert(item.key.clone(), bytes.chunks_exact(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect());
        }
    }
    let missing: Vec<&Item> = items.iter().filter(|item| !vectors.contains_key(&item.key)).collect();
    for batch in missing.chunks(100) {
        let texts: Vec<String> = batch.iter().map(|item| item.text.chars().take(4_000).collect()).collect();
        embedded_chars += texts.iter().map(|text| text.chars().count()).sum::<usize>();
        let embedded = voyage_embed(&key, &texts, "document").await?;
        for (item, vector) in batch.iter().zip(embedded) {
            let bytes: Vec<u8> = vector.iter().flat_map(|x| x.to_le_bytes()).collect();
            let _ = sqlx::query("INSERT OR REPLACE INTO memory_embeddings (item_key, model, vector) VALUES (?, ?, ?)")
                .bind(&item.key)
                .bind(embedding_model())
                .bind(bytes)
                .execute(pool)
                .await;
            vectors.insert(item.key.clone(), vector);
        }
    }
    let query_vector = voyage_embed(&key, &[crate::guard::redact_secrets(query)], "query").await?.into_iter().next()?;
    if let Some((hub, session_id)) = budget {
        // About four characters to a token.
        let cost = embedded_chars as f64 / 4.0 / 1_000_000.0 * EMBEDDING_USD_PER_MILLION_TOKENS;
        hub.record_llm_call(session_id, cost).await;
    }
    Some(items.iter().map(|item| vectors.get(&item.key).map_or(0.0, |vector| cosine(&query_vector, vector))).collect())
}

/// The `limit` most relevant items for `query`, best first.
pub async fn search(
    pool: &sqlx::SqlitePool,
    hub: Option<&crate::ws::Hub>,
    session_id: &str,
    query: &str,
    limit: usize,
) -> Vec<MemoryHit> {
    let items = corpus(pool, session_id).await;
    if items.is_empty() {
        return Vec::new();
    }
    let texts: Vec<String> = items.iter().map(|item| item.text.clone()).collect();
    let keyword = bm25(query, &texts);
    let best_keyword = keyword.iter().cloned().fold(0.0_f64, f64::max).max(1e-9);
    let semantic = semantic_scores(pool, hub.map(|hub| (hub, session_id)), query, &items).await;
    let mut hits: Vec<MemoryHit> = items
        .into_iter()
        .enumerate()
        .map(|(index, item)| {
            let keyword = keyword[index] / best_keyword;
            let score = match &semantic {
                Some(semantic) => 0.65 * semantic[index] + 0.35 * keyword,
                None => keyword,
            };
            MemoryHit { kind: item.kind, source: item.source, text: item.text, score, at: item.at }
        })
        .filter(|hit| hit.score > 0.05)
        .collect();
    hits.sort_by(|a, b| b.score.total_cmp(&a.score));
    hits.truncate(limit);
    hits
}

#[derive(Deserialize)]
pub struct SearchParams {
    q: String,
    #[serde(default)]
    limit: Option<usize>,
}

pub async fn search_handler(
    State(state): State<AppState>,
    Extension(AuthUser(user_id)): Extension<AuthUser>,
    Path(session_id): Path<String>,
    Query(params): Query<SearchParams>,
) -> Result<Json<Vec<MemoryHit>>, AppError> {
    crate::handlers::require_member(&state, &session_id, &user_id).await?;
    let limit = params.limit.unwrap_or(5).clamp(1, 20);
    let query: String = params.q.trim().chars().take(2_000).collect();
    Ok(Json(search(&state.pool, Some(&state.hub), &session_id, &query, limit).await))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn word_endings_and_code_names_match() {
        assert_eq!(terms("late_fee"), terms("late fees"));
        assert_eq!(terms("reserveStock"), terms("reserving stocks"));
        assert_eq!(terms("policies"), terms("policy"));
        assert_eq!(stem("class"), "class");
        let docs = vec![
            "Implement Inventory.reserve_stock and release".to_string(),
            "Catalog search by name and category".to_string(),
        ];
        let scores = bm25("who is reserving stock?", &docs);
        assert!(scores[0] > scores[1] && scores[0] > 0.0);
    }

    #[test]
    fn keyword_search_ranks_the_matching_item_first() {
        let docs = vec![
            "Add search by title or author — files: store.py".to_string(),
            "Due dates and fines: 21 days, 0.30 per day, capped at 7.50".to_string(),
            "Save and load the store as JSON".to_string(),
        ];
        let scores = bm25("how much are late fines per day?", &docs);
        let best = scores.iter().enumerate().max_by(|a, b| a.1.total_cmp(b.1)).unwrap().0;
        assert_eq!(best, 1);
        assert_eq!(bm25("", &docs), vec![0.0; 3]);
        assert!(cosine(&[1.0, 0.0], &[1.0, 0.0]) > 0.99 && cosine(&[1.0, 0.0], &[0.0, 1.0]).abs() < 1e-9);
    }

    fn decision(id: i64, text: &str) -> Decision {
        Decision { id, text: text.into(), pinned_by: "alice".into(), created_at: String::new() }
    }

    #[test]
    fn a_decision_about_the_same_thing_is_offered_for_replacing() {
        let pinned = vec![
            decision(1, "Late fees are 25 cents per day, capped at 5 dollars"),
            decision(2, "Use SQLite for storage"),
            decision(3, "Invoices are numbered INV-0001 upward"),
        ];
        let related = related_decisions("Late fee is 50 cents per day now", &pinned);
        assert_eq!(related.iter().map(|d| d.id).collect::<Vec<_>>(), vec![1]);
        assert!(related_decisions("Store everything in Postgres instead", &pinned).is_empty(), "one shared word isn't enough");
        assert!(related_decisions("Add a dark theme", &pinned).is_empty());
    }

    #[test]
    fn only_runs_a_teammate_accepted_are_trusted() {
        let review = |verdict: &str| serde_json::json!({"reviewer_id": "bob", "verdict": verdict});
        assert!(!run_is_trusted(&[], false), "nobody looked at it yet");
        assert!(run_is_trusted(&[review("accepted")], false));
        assert!(!run_is_trusted(&[review("accepted"), review("changes")], false), "someone sent it back");
        assert!(run_is_trusted(&[], true), "alone, nobody else could review it");
        assert!(!run_is_trusted(&[review("changes")], true));
    }

    #[test]
    fn flag_keys_and_step_keys_are_stable_and_safe() {
        assert_eq!(clean_flag_key(" digest ").as_deref(), Some("digest"));
        assert!(clean_flag_key("run:u-1:r_2").is_some());
        assert!(clean_flag_key("decision:4").is_none(), "decisions are unpinned, not flagged");
        assert!(clean_flag_key("step:u1:abc; DROP TABLE").is_none());
        assert!(clean_flag_key(&format!("run:{}", "x".repeat(300))).is_none());
        // FNV-1a test vectors; the app computes the same.
        assert_eq!(text_key(""), "cbf29ce484222325");
        assert_eq!(text_key("a"), "af63dc4c8601ec8c");
        assert_eq!(text_key("  a \n"), text_key("a"));
    }

    async fn ledger_pool(members: &[&str]) -> sqlx::SqlitePool {
        let pool = crate::db::init_pool("sqlite::memory:").await.unwrap();
        for user in members {
            sqlx::query("INSERT INTO users (id, email, password_hash, created_at) VALUES (?, ?, '', '')")
                .bind(user)
                .bind(format!("{user}@example.test"))
                .execute(&pool)
                .await
                .unwrap();
        }
        sqlx::query("INSERT INTO sessions (id, name, owner_id, created_at) VALUES ('s', 'Ledger', ?, '')")
            .bind(members[0])
            .execute(&pool)
            .await
            .unwrap();
        for user in members {
            sqlx::query("INSERT INTO session_members (session_id, user_id, role, joined_at) VALUES ('s', ?, 'member', '')")
                .bind(user)
                .execute(&pool)
                .await
                .unwrap();
        }
        pool
    }

    async fn add_run(pool: &sqlx::SqlitePool, user: &str, run: &str, goal: &str, reviews: &str) {
        sqlx::query(
            "INSERT INTO agent_runs (session_id, user_id, run_id, agent, goal, status, files, summary, reviews, started_at, updated_at) \
             VALUES ('s', ?, ?, 'I-mode', ?, 'done', '[]', NULL, ?, '', '')",
        )
        .bind(user)
        .bind(run)
        .bind(goal)
        .bind(reviews)
        .execute(pool)
        .await
        .unwrap();
        sqlx::query("INSERT INTO agent_steps (session_id, user_id, agent, kind, text, path, run_id, created_at) VALUES ('s', ?, 'I-mode', 'note', ?, NULL, ?, '')")
            .bind(user)
            .bind(format!("step of {goal}"))
            .bind(run)
            .execute(pool)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn memory_serves_accepted_work_and_never_flagged_or_sent_back_work() {
        let pool = ledger_pool(&["alice", "bob"]).await;
        add_run(&pool, "bob", "r1", "refund ledger accepted", r#"[{"reviewer_id":"alice","verdict":"accepted"}]"#).await;
        add_run(&pool, "bob", "r2", "refund ledger unreviewed", "[]").await;
        add_run(&pool, "bob", "r3", "refund ledger sent back", r#"[{"reviewer_id":"alice","verdict":"changes","note":"wrong rounding"}]"#).await;
        add_run(&pool, "bob", "r4", "refund ledger flagged", r#"[{"reviewer_id":"alice","verdict":"accepted"}]"#).await;
        sqlx::query("INSERT INTO agent_steps (session_id, user_id, agent, kind, text, path, run_id, created_at) VALUES ('s', 'bob', 'Agent', 'note', 'refund ledger loose claim', NULL, NULL, '')")
            .execute(&pool)
            .await
            .unwrap();
        flag(&pool, "s", "alice", "run:bob:r4", Some("it rounds refunds down")).await.unwrap();

        let texts: Vec<(String, &str)> = search(&pool, None, "s", "refund ledger", 20).await.into_iter().map(|hit| (hit.text, hit.source)).collect();
        let has = |needle: &str| texts.iter().any(|(text, _)| text.contains(needle));
        assert!(has("refund ledger accepted") && has("step of refund ledger accepted"), "{texts:?}");
        assert!(!has("unreviewed") && !has("sent back") && !has("flagged"), "{texts:?}");
        let (_, source) = texts.iter().find(|(text, _)| text.starts_with("refund ledger accepted")).unwrap();
        assert_eq!(*source, "agent work a teammate accepted");
        let (_, source) = texts.iter().find(|(text, _)| text.contains("loose claim")).unwrap();
        assert_eq!(*source, "an agent said, unchecked");

        // Flag the loose step by its key, as the app does: it's withdrawn.
        flag(&pool, "s", "alice", &format!("step:bob:{}", text_key("refund ledger loose claim")), None).await.unwrap();
        assert!(!search(&pool, None, "s", "refund ledger", 20).await.iter().any(|hit| hit.text.contains("loose claim")));
        // Lifting the flag on the run brings it back.
        unflag(&pool, "s", "run:bob:r4").await;
        assert!(search(&pool, None, "s", "refund ledger", 20).await.iter().any(|hit| hit.text.contains("flagged")));
    }

    #[tokio::test]
    async fn chat_in_memory_says_who_by_name() {
        let pool = ledger_pool(&["alice", "bob"]).await;
        // Real accounts have random ids, unlike the names above.
        sqlx::query("INSERT INTO users (id, email, password_hash, created_at) VALUES ('527272bc-2fdf', 'dana@example.test', '', '')")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO chat_messages (session_id, sender, text, created_at) VALUES ('s', '527272bc-2fdf', 'We agreed returns are accepted within 30 days of delivery', '')")
            .execute(&pool)
            .await
            .unwrap();
        let hits = search(&pool, None, "s", "returns accepted days", 5).await;
        let chat = hits.iter().find(|hit| hit.kind == "chat").expect("the chat message is found");
        assert!(chat.text.starts_with("dana: "), "a name, not an account id: {}", chat.text);
        assert_eq!(chat.source, "said in chat");
    }

    #[tokio::test]
    async fn alone_your_own_runs_count() {
        let pool = ledger_pool(&["alice"]).await;
        add_run(&pool, "alice", "r1", "invoice numbering solo", "[]").await;
        let hits = search(&pool, None, "s", "invoice numbering", 5).await;
        assert!(hits.iter().any(|hit| hit.text.starts_with("invoice numbering solo") && hit.source == "an agent said, unchecked"));
    }

    #[tokio::test]
    async fn a_replaced_decision_stops_being_served_but_is_kept() {
        let pool = ledger_pool(&["alice", "bob"]).await;
        let old = pin(&pool, "s", "alice", "Late fees are 25 cents per day", None).await.unwrap();
        let new = pin(&pool, "s", "bob", "Late fees are 50 cents per day", Some(old)).await.unwrap();
        let live = decisions(&pool, "s").await;
        assert_eq!(live.iter().map(|d| d.id).collect::<Vec<_>>(), vec![new]);
        let (removed, superseded_by): (i64, Option<i64>) =
            sqlx::query_as("SELECT removed, superseded_by FROM decisions WHERE id = ?").bind(old).fetch_one(&pool).await.unwrap();
        assert_eq!((removed, superseded_by), (1, Some(new)));
        assert!(!search(&pool, None, "s", "late fees", 5).await.iter().any(|hit| hit.text.contains("25 cents")));
        assert_eq!(pin(&pool, "s", "bob", "again", Some(old)).await.unwrap_err(), "the decision you're replacing was already unpinned or replaced");
    }

    #[test]
    fn decisions_are_trimmed_and_never_empty() {
        assert_eq!(clean_decision("   "), None);
        assert_eq!(clean_decision(&"x".repeat(900)).unwrap().len(), MAX_DECISION_CHARS);
    }
}
