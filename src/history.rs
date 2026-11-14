//! Built-in version history, so a team doesn't need git to get back to an
//! earlier state of a file: every version a session shares is kept, with who
//! made it and when, and any of them can be read back and restored.
//!
//! Contents are stored once per distinct text (by SHA-256), so identical
//! versions cost one row, and each file keeps its newest
//! `VERSIONS_KEPT_PER_FILE` versions.

use axum::extract::{Path, Query, State};
use axum::{Extension, Json};
use serde::{Deserialize, Serialize};

use crate::AppState;
use crate::auth::AuthUser;
use crate::errors::AppError;

const VERSIONS_KEPT_PER_FILE: i64 = 200;
const MAX_LISTED: i64 = 200;

use crate::patch::content_hash;

fn line_counts(previous: &str, content: &str) -> (i64, i64) {
    use similar::{ChangeTag, TextDiff};
    TextDiff::from_lines(previous, content).iter_all_changes().fold((0, 0), |(added, removed), change| {
        match change.tag() {
            ChangeTag::Insert => (added + 1, removed),
            ChangeTag::Delete => (added, removed + 1),
            ChangeTag::Equal => (added, removed),
        }
    })
}

/// Records `content` as the newest version of `path`, unless it's the same
/// as the newest one already kept. `user_id` is who made it; `previous` is
/// what the file held before, for the lines-changed counts.
pub async fn record(
    pool: &sqlx::SqlitePool,
    session_id: &str,
    path: &str,
    content: &str,
    previous: Option<&str>,
    user_id: &str,
    origin: Option<&str>,
) -> Result<bool, sqlx::Error> {
    let hash = content_hash(content);
    let newest: Option<(String,)> =
        sqlx::query_as("SELECT hash FROM file_versions WHERE session_id = ? AND path = ? ORDER BY id DESC LIMIT 1")
            .bind(session_id)
            .bind(path)
            .fetch_optional(pool)
            .await?;
    if newest.is_some_and(|(newest,)| newest == hash) {
        return Ok(false);
    }
    // With no "before" given, compare against the newest version kept.
    let kept_previous: Option<(String,)> = match previous {
        Some(_) => None,
        None => {
            sqlx::query_as(
                "SELECT b.content FROM file_versions v JOIN file_blobs b ON b.hash = v.hash \
                 WHERE v.session_id = ? AND v.path = ? ORDER BY v.id DESC LIMIT 1",
            )
            .bind(session_id)
            .bind(path)
            .fetch_optional(pool)
            .await?
        }
    };
    let previous = previous.or(kept_previous.as_ref().map(|(content,)| content.as_str()));
    let (added, removed) = line_counts(previous.unwrap_or_default(), content);
    sqlx::query("INSERT OR IGNORE INTO file_blobs (hash, content) VALUES (?, ?)")
        .bind(&hash)
        .bind(content)
        .execute(pool)
        .await?;
    sqlx::query(
        "INSERT INTO file_versions (session_id, path, hash, user_id, origin, added, removed, created_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(session_id)
    .bind(path)
    .bind(&hash)
    .bind(user_id)
    .bind(origin)
    .bind(added)
    .bind(removed)
    .bind(chrono::Utc::now().to_rfc3339())
    .execute(pool)
    .await?;
    // Keep the newest versions of this file; drop the rest.
    sqlx::query(
        "DELETE FROM file_versions WHERE session_id = ? AND path = ? AND id NOT IN \
         (SELECT id FROM file_versions WHERE session_id = ? AND path = ? ORDER BY id DESC LIMIT ?)",
    )
    .bind(session_id)
    .bind(path)
    .bind(session_id)
    .bind(path)
    .bind(VERSIONS_KEPT_PER_FILE)
    .execute(pool)
    .await?;
    Ok(true)
}

/// Removes stored contents no version points at any more.
pub async fn prune_blobs(pool: &sqlx::SqlitePool) -> Result<u64, sqlx::Error> {
    Ok(sqlx::query("DELETE FROM file_blobs WHERE hash NOT IN (SELECT DISTINCT hash FROM file_versions)")
        .execute(pool)
        .await?
        .rows_affected())
}

#[derive(Debug, Serialize, sqlx::FromRow, PartialEq)]
pub struct VersionInfo {
    pub id: i64,
    pub path: String,
    pub user_id: String,
    pub origin: Option<String>,
    pub added: i64,
    pub removed: i64,
    pub created_at: String,
}

#[derive(Deserialize)]
pub struct ListParams {
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    limit: Option<i64>,
}

pub async fn versions(
    pool: &sqlx::SqlitePool,
    session_id: &str,
    path: Option<&str>,
    limit: i64,
) -> Result<Vec<VersionInfo>, sqlx::Error> {
    let limit = limit.clamp(1, MAX_LISTED);
    match path {
        Some(path) => {
            sqlx::query_as(
                "SELECT id, path, user_id, origin, added, removed, created_at FROM file_versions \
                 WHERE session_id = ? AND path = ? ORDER BY id DESC LIMIT ?",
            )
            .bind(session_id)
            .bind(path)
            .bind(limit)
            .fetch_all(pool)
            .await
        }
        None => {
            sqlx::query_as(
                "SELECT id, path, user_id, origin, added, removed, created_at FROM file_versions \
                 WHERE session_id = ? ORDER BY id DESC LIMIT ?",
            )
            .bind(session_id)
            .bind(limit)
            .fetch_all(pool)
            .await
        }
    }
}

/// `GET /sessions/{id}/history?path=...`: the versions of one file, or with
/// no path the latest versions across the whole session, newest first.
pub async fn list_handler(
    State(state): State<AppState>,
    Extension(AuthUser(user_id)): Extension<AuthUser>,
    Path(session_id): Path<String>,
    Query(params): Query<ListParams>,
) -> Result<Json<Vec<VersionInfo>>, AppError> {
    crate::handlers::require_member(&state, &session_id, &user_id).await?;
    Ok(Json(versions(&state.pool, &session_id, params.path.as_deref(), params.limit.unwrap_or(50)).await?))
}

#[derive(Serialize)]
pub struct VersionContent {
    #[serde(flatten)]
    info: VersionInfo,
    content: String,
}

/// `GET /sessions/{id}/history/{version}`: one version with its full text.
pub async fn version_handler(
    State(state): State<AppState>,
    Extension(AuthUser(user_id)): Extension<AuthUser>,
    Path((session_id, version_id)): Path<(String, i64)>,
) -> Result<Json<VersionContent>, AppError> {
    crate::handlers::require_member(&state, &session_id, &user_id).await?;
    let info: Option<VersionInfo> = sqlx::query_as(
        "SELECT id, path, user_id, origin, added, removed, created_at FROM file_versions WHERE session_id = ? AND id = ?",
    )
    .bind(&session_id)
    .bind(version_id)
    .fetch_optional(&state.pool)
    .await?;
    let info = info.ok_or_else(|| AppError::NotFound("no such version".into()))?;
    let (content,): (String,) = sqlx::query_as(
        "SELECT b.content FROM file_blobs b JOIN file_versions v ON v.hash = b.hash WHERE v.id = ?",
    )
    .bind(version_id)
    .fetch_one(&state.pool)
    .await?;
    Ok(Json(VersionContent { info, content }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn versions_are_kept_once_per_change_and_can_be_read_back() {
        let pool = crate::db::init_pool("sqlite::memory:").await.unwrap();
        assert!(record(&pool, "s", "a.py", "x = 1\n", None, "alice", None).await.unwrap());
        // Saving the same text again isn't a new version.
        assert!(!record(&pool, "s", "a.py", "x = 1\n", Some("x = 1\n"), "bob", None).await.unwrap());
        assert!(record(&pool, "s", "a.py", "x = 2\ny = 3\n", Some("x = 1\n"), "bob", Some("agent")).await.unwrap());
        // Going back to an earlier text is a version, but stores no new copy.
        assert!(record(&pool, "s", "a.py", "x = 1\n", Some("x = 2\ny = 3\n"), "alice", None).await.unwrap());
        assert!(record(&pool, "s", "b.py", "z\n", None, "alice", None).await.unwrap());
        assert!(record(&pool, "other", "a.py", "secret\n", None, "carol", None).await.unwrap());

        let listed = versions(&pool, "s", Some("a.py"), 50).await.unwrap();
        assert_eq!(listed.iter().map(|v| v.user_id.as_str()).collect::<Vec<_>>(), ["alice", "bob", "alice"]);
        assert_eq!((listed[1].added, listed[1].removed), (2, 1));
        assert_eq!(listed[1].origin.as_deref(), Some("agent"));
        assert_eq!(versions(&pool, "s", None, 50).await.unwrap().len(), 4, "one session's versions only");

        let (blobs,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM file_blobs").fetch_one(&pool).await.unwrap();
        assert_eq!(blobs, 4, "x = 1 is stored once though it's two versions");
        assert_eq!(prune_blobs(&pool).await.unwrap(), 0);
    }
}
