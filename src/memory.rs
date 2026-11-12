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
/// short enough to give every agent.
pub async fn pin(pool: &sqlx::SqlitePool, session_id: &str, user_id: &str, text: &str) -> Result<(), &'static str> {
    let text = clean_decision(text).ok_or("a decision needs some text")?;
    let (count,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM decisions WHERE session_id = ? AND removed = 0")
        .bind(session_id)
        .fetch_one(pool)
        .await
        .map_err(|_| "couldn't save the decision")?;
    if count >= MAX_DECISIONS {
        return Err("this session already has 30 pinned decisions; unpin one first");
    }
    sqlx::query("INSERT INTO decisions (session_id, text, pinned_by, created_at) VALUES (?, ?, ?, ?)")
        .bind(session_id)
        .bind(&text)
        .bind(user_id)
        .bind(chrono::Utc::now().to_rfc3339())
        .execute(pool)
        .await
        .map_err(|_| "couldn't save the decision")?;
    Ok(())
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
    pub text: String,
    pub score: f64,
    pub at: String,
}

struct Item {
    key: String,
    kind: &'static str,
    text: String,
    at: String,
}

async fn corpus(pool: &sqlx::SqlitePool, session_id: &str) -> Vec<Item> {
    let mut items = Vec::new();
    for decision in decisions(pool, session_id).await {
        items.push(Item {
            key: format!("decision:{}", decision.id),
            kind: "decision",
            text: decision.text,
            at: decision.created_at,
        });
    }
    let runs: Vec<(String, String, String, String, Option<String>, String, String)> = sqlx::query_as(
        "SELECT user_id, run_id, goal, files, summary, reviews, updated_at FROM agent_runs \
         WHERE session_id = ? ORDER BY updated_at DESC LIMIT ?",
    )
    .bind(session_id)
    .bind(SEARCH_CORPUS_LIMIT)
    .fetch_all(pool)
    .await
    .unwrap_or_default();
    for (user_id, run_id, goal, files, summary, reviews, at) in runs {
        let files: Vec<String> = serde_json::from_str(&files).unwrap_or_default();
        let notes: Vec<String> = serde_json::from_str::<Vec<serde_json::Value>>(&reviews)
            .unwrap_or_default()
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
        items.push(Item { key: format!("run:{user_id}:{run_id}"), kind: "run", text, at });
    }
    let steps: Vec<(i64, String, String, String)> = sqlx::query_as(
        "SELECT id, agent, text, created_at FROM agent_steps WHERE session_id = ? ORDER BY id DESC LIMIT ?",
    )
    .bind(session_id)
    .bind(SEARCH_CORPUS_LIMIT)
    .fetch_all(pool)
    .await
    .unwrap_or_default();
    for (id, agent, text, at) in steps {
        items.push(Item { key: format!("step:{id}"), kind: "step", text: format!("{agent}: {text}"), at });
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
            items.push(Item { key: format!("task:{}:{:x}", task.id, fingerprint(&text)), kind: "task", text, at: at.clone() });
        }
    }
    let chat: Vec<(i64, String, String, String)> = sqlx::query_as(
        "SELECT id, sender, text, created_at FROM chat_messages WHERE session_id = ? ORDER BY id DESC LIMIT ?",
    )
    .bind(session_id)
    .bind(CHAT_CORPUS_LIMIT)
    .fetch_all(pool)
    .await
    .unwrap_or_default();
    for (id, sender, text, at) in chat {
        if text.chars().count() >= MIN_CHAT_CHARS {
            let text: String = text.chars().take(1_500).collect();
            items.push(Item { key: format!("chat:{id}"), kind: "chat", text: format!("{sender}: {text}"), at });
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
            MemoryHit { kind: item.kind, text: item.text, score, at: item.at }
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

    #[test]
    fn decisions_are_trimmed_and_never_empty() {
        assert_eq!(clean_decision("   "), None);
        assert_eq!(clean_decision(&"x".repeat(900)).unwrap().len(), MAX_DECISION_CHARS);
    }
}
