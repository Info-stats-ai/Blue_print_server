//! The "conflicts avoided" dashboard: every collision-prevention event the
//! hub records is also counted per session and day in `conflict_daily`, so a
//! team lead can see a week's numbers across all their sessions, after
//! restarts too.

use axum::Json;
use axum::extract::{Extension, Query, State};
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use std::collections::BTreeMap;
use tokio::sync::mpsc;

use crate::AppState;
use crate::auth::AuthUser;
use crate::errors::AppError;

pub const MERGE: &str = "merge";
pub const DIVERGENCE: &str = "divergence";
pub const LANE_WARNING: &str = "lane_warning";
pub const AGENT_COLLISION: &str = "agent_collision";

/// Kinds that are a conflict caught before it landed. Live merges are counted
/// separately: every concurrent edit is one, so they'd swamp the headline.
const CONFLICT_KINDS: [&str; 3] = [DIVERGENCE, LANE_WARNING, AGENT_COLLISION];

pub type EventSink = mpsc::UnboundedSender<(String, &'static str)>;

/// Starts the task that writes counted events, returning where to send them.
pub fn spawn_writer(pool: SqlitePool) -> EventSink {
    let (tx, mut rx) = mpsc::unbounded_channel::<(String, &'static str)>();
    tokio::spawn(async move {
        while let Some((session_id, kind)) = rx.recv().await {
            if let Err(error) = sqlx::query(
                "INSERT INTO conflict_daily (session_id, day, kind, count) \
                 VALUES (?, strftime('%Y-%m-%d', 'now'), ?, 1) \
                 ON CONFLICT(session_id, day, kind) DO UPDATE SET count = count + 1",
            )
            .bind(&session_id)
            .bind(kind)
            .execute(&pool)
            .await
            {
                eprintln!("stats: counting {kind} for {session_id} failed: {error}");
            }
        }
    });
    tx
}

#[derive(Debug, Deserialize)]
pub struct WeeklyQuery {
    #[serde(default)]
    pub days: Option<i64>,
}

#[derive(Debug, Default, Serialize, PartialEq)]
pub struct Summary {
    pub days: i64,
    /// Conflicts caught before they landed: divergences + lane warnings +
    /// agent collisions.
    pub conflicts_caught: i64,
    pub divergences: i64,
    pub lane_warnings: i64,
    pub agent_collisions: i64,
    /// Concurrent edits merged live without overwriting each other.
    pub live_merges: i64,
    /// Conflicts caught per day, oldest first, every day in the window.
    pub by_day: Vec<DayCount>,
    /// The sessions with the most conflicts caught.
    pub sessions: Vec<SessionCount>,
}

#[derive(Debug, Serialize, PartialEq)]
pub struct DayCount {
    pub day: String,
    pub count: i64,
}

#[derive(Debug, Serialize, PartialEq)]
pub struct SessionCount {
    pub name: String,
    pub count: i64,
}

/// `GET /metrics/weekly?days=7`: the caller's sessions, summed.
pub async fn weekly(
    State(state): State<AppState>,
    Extension(AuthUser(user_id)): Extension<AuthUser>,
    Query(query): Query<WeeklyQuery>,
) -> Result<Json<Summary>, AppError> {
    let days = query.days.unwrap_or(7).clamp(1, 90);
    Ok(Json(summarize(&state.pool, &user_id, days).await?))
}

pub async fn summarize(pool: &SqlitePool, user_id: &str, days: i64) -> Result<Summary, sqlx::Error> {
    let since = format!("-{} days", days - 1);
    let rows: Vec<(String, String, String, i64)> = sqlx::query_as(
        "SELECT c.session_id, c.day, c.kind, c.count FROM conflict_daily c \
         JOIN session_members m ON m.session_id = c.session_id \
         WHERE m.user_id = ? AND c.day >= strftime('%Y-%m-%d', 'now', ?)",
    )
    .bind(user_id)
    .bind(&since)
    .fetch_all(pool)
    .await?;
    let window: Vec<(String,)> = sqlx::query_as(
        "WITH RECURSIVE d(n) AS (SELECT 0 UNION ALL SELECT n + 1 FROM d WHERE n + 1 < ?) \
         SELECT strftime('%Y-%m-%d', 'now', '-' || (? - 1 - n) || ' days') FROM d",
    )
    .bind(days)
    .bind(days)
    .fetch_all(pool)
    .await?;

    let mut summary = Summary {
        days,
        ..Summary::default()
    };
    let mut per_day: BTreeMap<String, i64> = window.into_iter().map(|(day,)| (day, 0)).collect();
    let mut per_session: BTreeMap<String, i64> = BTreeMap::new();
    for (session_id, day, kind, count) in rows {
        match kind.as_str() {
            DIVERGENCE => summary.divergences += count,
            LANE_WARNING => summary.lane_warnings += count,
            AGENT_COLLISION => summary.agent_collisions += count,
            MERGE => summary.live_merges += count,
            _ => {}
        }
        if CONFLICT_KINDS.contains(&kind.as_str()) {
            *per_day.entry(day).or_default() += count;
            *per_session.entry(session_id).or_default() += count;
        }
    }
    summary.conflicts_caught = summary.divergences + summary.lane_warnings + summary.agent_collisions;
    summary.by_day = per_day.into_iter().map(|(day, count)| DayCount { day, count }).collect();

    let mut sessions = Vec::new();
    for (session_id, count) in per_session {
        let name: Option<(String,)> = sqlx::query_as("SELECT name FROM sessions WHERE id = ?")
            .bind(&session_id)
            .fetch_optional(pool)
            .await?;
        sessions.push(SessionCount {
            name: name.map(|(name,)| name).unwrap_or(session_id),
            count,
        });
    }
    sessions.sort_by(|a, b| b.count.cmp(&a.count).then(a.name.cmp(&b.name)));
    sessions.truncate(5);
    summary.sessions = sessions;
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn pool() -> SqlitePool {
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        pool
    }

    #[tokio::test]
    async fn the_week_sums_only_the_users_sessions() {
        let pool = pool().await;
        for (id, email) in [("u1", "a@example.test"), ("u2", "b@example.test")] {
            sqlx::query("INSERT INTO users (id, email, password_hash, created_at) VALUES (?, ?, 'x', '2026-01-01T00:00:00Z')")
                .bind(id)
                .bind(email)
                .execute(&pool)
                .await
                .unwrap();
        }
        for (id, owner, name) in [("s1", "u1", "Tiny Tasks"), ("s2", "u2", "Other team")] {
            sqlx::query("INSERT INTO sessions (id, name, owner_id, created_at) VALUES (?, ?, ?, '2026-01-01T00:00:00Z')")
                .bind(id)
                .bind(name)
                .bind(owner)
                .execute(&pool)
                .await
                .unwrap();
            sqlx::query("INSERT INTO session_members (session_id, user_id, role, joined_at) VALUES (?, ?, 'owner', '2026-01-01T00:00:00Z')")
                .bind(id)
                .bind(owner)
                .execute(&pool)
                .await
                .unwrap();
        }
        let insert = |session: &'static str, day_offset: &'static str, kind: &'static str, count: i64| {
            let pool = pool.clone();
            async move {
                sqlx::query("INSERT INTO conflict_daily (session_id, day, kind, count) VALUES (?, strftime('%Y-%m-%d', 'now', ?), ?, ?)")
                    .bind(session)
                    .bind(day_offset)
                    .bind(kind)
                    .bind(count)
                    .execute(&pool)
                    .await
                    .unwrap();
            }
        };
        insert("s1", "0 days", AGENT_COLLISION, 2).await;
        insert("s1", "-1 days", DIVERGENCE, 3).await;
        insert("s1", "0 days", MERGE, 40).await;
        // Outside the 7-day window, and someone else's session.
        insert("s1", "-9 days", LANE_WARNING, 5).await;
        insert("s2", "0 days", AGENT_COLLISION, 7).await;

        let summary = summarize(&pool, "u1", 7).await.unwrap();
        assert_eq!(summary.conflicts_caught, 5);
        assert_eq!(summary.agent_collisions, 2);
        assert_eq!(summary.divergences, 3);
        assert_eq!(summary.lane_warnings, 0);
        assert_eq!(summary.live_merges, 40);
        assert_eq!(summary.by_day.len(), 7);
        assert_eq!(summary.by_day.last().unwrap().count, 2);
        assert_eq!(summary.by_day[5].count, 3);
        assert_eq!(summary.sessions, vec![SessionCount { name: "Tiny Tasks".into(), count: 5 }]);
    }

    #[tokio::test]
    async fn the_writer_counts_events_per_day() {
        let pool = pool().await;
        let sink = spawn_writer(pool.clone());
        for _ in 0..3 {
            sink.send(("s1".to_string(), AGENT_COLLISION)).unwrap();
        }
        sink.send(("s1".to_string(), MERGE)).unwrap();
        let mut count = 0;
        for _ in 0..50 {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            let row: Option<(i64,)> =
                sqlx::query_as("SELECT count FROM conflict_daily WHERE session_id = 's1' AND kind = ?")
                    .bind(AGENT_COLLISION)
                    .fetch_optional(&pool)
                    .await
                    .unwrap();
            count = row.map(|(count,)| count).unwrap_or(0);
            if count == 3 {
                break;
            }
        }
        assert_eq!(count, 3);
    }
}
