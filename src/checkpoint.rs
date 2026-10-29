//! Automatic background checkpointing — the "Finalize/restore" layer's
//! unattended half. An explicit `create_plan` call already snapshots a
//! session's files for deliberate review/approval; this adds the other
//! piece the design called for: a snapshot written on its own clock, with
//! no one having to remember to ask for one, so a session that's been live
//! for hours always has *some* recent restorable state even if nobody ever
//! clicks "create plan." It reuses `execution_plans`/`restore_plan` as-is
//! (status `"checkpoint"` instead of `"pending"`) rather than a parallel
//! table, since restoring doesn't care how a snapshot came to exist.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::time::Duration;

use chrono::Utc;
use uuid::Uuid;

use crate::AppState;

const CHECKPOINT_INTERVAL: Duration = Duration::from_secs(300);

// Each checkpoint is a full copy of every file in the session, so keeping
// them all grows the database without bound (a shared codebase is easily
// megabytes per copy, every five minutes) until the volume is full and every
// query stalls. Restoring only ever reads the newest one.
const CHECKPOINTS_KEPT_PER_SESSION: i64 = 3;

// Caps how many checkpoint writes are in flight at once. Bounded, not
// unbounded concurrency: at thousands of active sessions, firing every
// changed session's write at once would open thousands of simultaneous
// connections against the pool for no benefit — SQLite serializes the
// actual writes at the file level regardless, so this just keeps the sweep
// from queueing everything behind a single slow one without also
// overwhelming the pool. A deployment that outgrows SQLite's single-writer
// ceiling here needs a real multi-writer database (e.g. Postgres), not a
// different loop shape — this bounds the symptom, it doesn't remove the
// underlying limit.
const MAX_CONCURRENT_CHECKPOINTS: usize = 16;

// How long a session may sit with zero connected peers before its
// in-memory state is evicted (see `ws::Hub::evict_idle_sessions`). Long
// enough that someone stepping away for a meeting and coming back doesn't
// lose their session's live chat history (which has no durable copy);
// short enough that a server running for weeks under real traffic doesn't
// accumulate every session anyone has ever opened.
const IDLE_SESSION_TTL: Duration = Duration::from_secs(2 * 60 * 60);

/// The `author_id` recorded on an automatic checkpoint row, reserved the
/// same way `AGENT_USER_ID` is for chat — so a checkpoint is never
/// mistaken for (or spoofable as) a human-authored plan.
pub const CHECKPOINT_AUTHOR_ID: &str = "system:checkpoint";

// (session_id, its pre-write snapshot hash, the write's outcome) — what
// each spawned checkpoint-write task hands back to `record_one_result`.
type CheckpointTaskResult = (String, u64, anyhow::Result<()>);

/// Runs forever: every `CHECKPOINT_INTERVAL`, snapshots every active
/// session whose files have changed since its last checkpoint (tracked
/// in-memory, local to this task — losing that tracking on a restart just
/// means the next tick checkpoints once more than strictly necessary, not
/// a correctness problem), writes one `execution_plans` row per changed
/// session with bounded concurrency, then evicts any session that's been
/// idle (zero peers) past `IDLE_SESSION_TTL`. Meant to be spawned once at
/// startup and left running; intended to never be awaited on, so a failure
/// in one sweep (e.g. a transient DB error on one session) is logged and
/// the loop continues rather than taking the whole background task down.
pub async fn run_checkpoint_sweep(state: AppState) {
    let mut last_hash: HashMap<String, u64> = HashMap::new();
    let mut interval = tokio::time::interval(CHECKPOINT_INTERVAL);
    loop {
        interval.tick().await;

        let mut tasks: tokio::task::JoinSet<CheckpointTaskResult> = tokio::task::JoinSet::new();
        for (session_id, files, blueprint) in state.hub.active_session_snapshots().await {
            let hash = snapshot_hash(&files);
            if last_hash.get(&session_id) == Some(&hash) {
                continue;
            }
            let state = state.clone();
            let task_session_id = session_id.clone();
            tasks.spawn(async move {
                let result = write_checkpoint(&state, &task_session_id, &files, &blueprint).await;
                (task_session_id, hash, result)
            });
            while tasks.len() >= MAX_CONCURRENT_CHECKPOINTS {
                record_one_result(tasks.join_next().await, &mut last_hash);
            }
        }
        while !tasks.is_empty() {
            record_one_result(tasks.join_next().await, &mut last_hash);
        }

        let evicted = state.hub.evict_idle_sessions(IDLE_SESSION_TTL).await;
        for session_id in &evicted {
            last_hash.remove(session_id);
        }
        if !evicted.is_empty() {
            println!("checkpoint: evicted {} idle session(s) from memory", evicted.len());
        }
    }
}

/// Applies one finished checkpoint task's outcome to `last_hash` — on
/// success, so the next tick skips it if nothing's changed since; on
/// failure (DB error or a panicked task), logged and left out of
/// `last_hash`, so the next tick retries it rather than wrongly treating a
/// failed write as if it had succeeded.
fn record_one_result(
    joined: Option<Result<CheckpointTaskResult, tokio::task::JoinError>>,
    last_hash: &mut HashMap<String, u64>,
) {
    match joined {
        Some(Ok((session_id, hash, Ok(())))) => {
            last_hash.insert(session_id, hash);
        }
        Some(Ok((session_id, _, Err(error)))) => {
            eprintln!("checkpoint: failed to snapshot session {session_id}: {error}");
        }
        Some(Err(join_error)) => {
            eprintln!("checkpoint: a checkpoint task panicked: {join_error}");
        }
        None => {}
    }
}

async fn write_checkpoint(
    state: &AppState,
    session_id: &str,
    files: &HashMap<String, String>,
    blueprint: &str,
) -> anyhow::Result<()> {
    let id = Uuid::new_v4().to_string();
    let now = Utc::now();
    let files_json = serde_json::to_string(files)?;
    // The blueprint rides in `content` — the same column a human-authored
    // plan uses for its free-text description — distinguished from those by
    // `status = 'checkpoint'`, which `ws::rehydrate_session_from_checkpoint`
    // filters on specifically so it never mistakes a human plan's content
    // for blueprint markdown.
    let content = if blueprint.is_empty() {
        "Automatic checkpoint"
    } else {
        blueprint
    };

    sqlx::query(
        "INSERT INTO execution_plans (id, session_id, author_id, content, status, files, created_at, updated_at) \
         VALUES (?, ?, ?, ?, 'checkpoint', ?, ?, ?)",
    )
    .bind(&id)
    .bind(session_id)
    .bind(CHECKPOINT_AUTHOR_ID)
    .bind(content)
    .bind(&files_json)
    .bind(now)
    .bind(now)
    .execute(&state.pool)
    .await?;
    prune_checkpoints(&state.pool, Some(session_id)).await?;

    Ok(())
}

/// Deletes all but the newest `CHECKPOINTS_KEPT_PER_SESSION` checkpoints of
/// `session_id`, or of every session when `None` (run once at startup to
/// clear any backlog). Human-authored plans are never touched.
pub async fn prune_checkpoints(pool: &sqlx::SqlitePool, session_id: Option<&str>) -> anyhow::Result<u64> {
    let result = sqlx::query(
        "DELETE FROM execution_plans WHERE status = 'checkpoint' AND (?1 IS NULL OR session_id = ?1) \
         AND id NOT IN ( \
             SELECT id FROM ( \
                 SELECT id, ROW_NUMBER() OVER (PARTITION BY session_id ORDER BY created_at DESC) AS position \
                 FROM execution_plans WHERE status = 'checkpoint' \
             ) WHERE position <= ?2 \
         )",
    )
    .bind(session_id)
    .bind(CHECKPOINTS_KEPT_PER_SESSION)
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

/// Order-independent hash of a file snapshot, so two snapshots with the same
/// content hash equal regardless of `HashMap` iteration order.
fn snapshot_hash(files: &HashMap<String, String>) -> u64 {
    let mut entries: Vec<_> = files.iter().collect();
    entries.sort_by(|a, b| a.0.cmp(b.0));
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for (path, content) in entries {
        path.hash(&mut hasher);
        content.hash(&mut hasher);
    }
    hasher.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ws::AGENT_USER_ID;

    #[test]
    fn hash_is_order_independent() {
        let mut a = HashMap::new();
        a.insert("b.rs".to_string(), "2".to_string());
        a.insert("a.rs".to_string(), "1".to_string());

        let mut b = HashMap::new();
        b.insert("a.rs".to_string(), "1".to_string());
        b.insert("b.rs".to_string(), "2".to_string());

        assert_eq!(snapshot_hash(&a), snapshot_hash(&b));
    }

    #[test]
    fn hash_changes_with_content() {
        let mut a = HashMap::new();
        a.insert("a.rs".to_string(), "1".to_string());
        let mut b = a.clone();
        b.insert("a.rs".to_string(), "2".to_string());

        assert_ne!(snapshot_hash(&a), snapshot_hash(&b));
    }

    #[test]
    fn checkpoint_author_is_not_the_chat_agent_id() {
        assert_ne!(CHECKPOINT_AUTHOR_ID, AGENT_USER_ID);
    }

    #[tokio::test]
    async fn pruning_keeps_only_the_newest_checkpoints_per_session() {
        let directory = tempfile::tempdir().unwrap();
        let url = format!("sqlite://{}", directory.path().join("test.db").display());
        let pool = crate::db::init_pool(&url).await.unwrap();
        sqlx::query("INSERT INTO users (id, email, password_hash, created_at) VALUES ('u', 'u@x', 'h', 'now')")
            .execute(&pool)
            .await
            .unwrap();
        for session in ["s1", "s2"] {
            sqlx::query("INSERT INTO sessions (id, name, owner_id, created_at) VALUES (?, 'n', 'u', 'now')")
                .bind(session)
                .execute(&pool)
                .await
                .unwrap();
        }
        for session in ["s1", "s2"] {
            for minute in 0..5 {
                sqlx::query(
                    "INSERT INTO execution_plans (id, session_id, author_id, content, status, files, created_at, updated_at) \
                     VALUES (?, ?, 'u', 'c', 'checkpoint', '{}', ?, ?)",
                )
                .bind(format!("{session}-{minute}"))
                .bind(session)
                .bind(format!("2026-01-01T00:0{minute}:00Z"))
                .bind(format!("2026-01-01T00:0{minute}:00Z"))
                .execute(&pool)
                .await
                .unwrap();
            }
        }
        sqlx::query(
            "INSERT INTO execution_plans (id, session_id, author_id, content, status, files, created_at, updated_at) \
             VALUES ('human', 's1', 'u', 'plan', 'pending', NULL, '2020-01-01T00:00:00Z', '2020-01-01T00:00:00Z')",
        )
        .execute(&pool)
        .await
        .unwrap();

        assert_eq!(prune_checkpoints(&pool, Some("s1")).await.unwrap(), 2);
        assert_eq!(prune_checkpoints(&pool, None).await.unwrap(), 2);
        let remaining: Vec<String> = sqlx::query_scalar("SELECT id FROM execution_plans ORDER BY id")
            .fetch_all(&pool)
            .await
            .unwrap();
        assert_eq!(remaining, vec!["human", "s1-2", "s1-3", "s1-4", "s2-2", "s2-3", "s2-4"]);
    }
}
