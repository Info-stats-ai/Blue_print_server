//! Built-in version history, so a team doesn't need git to get back to an
//! earlier state of a file: every version a session shares is kept, with who
//! made it and when, and any of them can be read back and restored.
//!
//! Versions are stored through `store`: cut into chunks, each chunk kept once
//! per session, compressed and encrypted with the session's key. A version
//! row holds its chunk list. Each file keeps its newest
//! `VERSIONS_KEPT_PER_FILE` versions.

use axum::extract::{Path, Query, State};
use axum::{Extension, Json};
use serde::{Deserialize, Serialize};

use crate::AppState;
use crate::auth::AuthUser;
use crate::errors::AppError;
use crate::store;

const VERSIONS_KEPT_PER_FILE: i64 = 200;
const MAX_LISTED: i64 = 200;

fn line_counts(previous: &str, content: &str) -> (i64, i64) {
    use similar::{ChangeTag, TextDiff};
    // Only the part between the lines both share at the start and the end
    // can differ: an edit in a 20,000-line file diffs a few lines, not all.
    let (previous, content) = trim_common_lines(previous, content);
    TextDiff::from_lines(previous, content).iter_all_changes().fold((0, 0), |(added, removed), change| {
        match change.tag() {
            ChangeTag::Insert => (added + 1, removed),
            ChangeTag::Delete => (added, removed + 1),
            ChangeTag::Equal => (added, removed),
        }
    })
}

/// `a` and `b` without the whole lines they share at the start and the end.
fn trim_common_lines<'a>(a: &'a str, b: &'a str) -> (&'a str, &'a str) {
    let (x, y) = (a.as_bytes(), b.as_bytes());
    let mut start = x.iter().zip(y).take_while(|(p, q)| p == q).count();
    // Back to the start of the line the first difference is on.
    while start > 0 && x[start - 1] != b'\n' {
        start -= 1;
    }
    let max_end = x.len().min(y.len()) - start;
    let mut end = x.iter().rev().zip(y.iter().rev()).take(max_end).take_while(|(p, q)| p == q).count();
    // Forward to the end of the line the last difference is on.
    while end > 0 && x[x.len() - end] != b'\n' {
        end -= 1;
    }
    if end > 0 {
        end -= 1;
    }
    (&a[start..a.len() - end], &b[start..b.len() - end])
}

/// The text of a version row: from its chunks, or, for a version from before
/// the chunk store that hasn't been moved yet, from `file_blobs`.
async fn row_content(
    conn: &mut sqlx::SqliteConnection,
    session_id: &str,
    hash: &str,
    chunks: Option<&[u8]>,
) -> Result<String, sqlx::Error> {
    match chunks {
        Some(chunks) => store::get(conn, session_id, chunks, hash).await,
        None => sqlx::query_scalar("SELECT content FROM file_blobs WHERE hash = ?").bind(hash).fetch_one(&mut *conn).await,
    }
}

/// The text of version `version_id` of `session_id`, if it exists.
pub async fn content_of(pool: &sqlx::SqlitePool, session_id: &str, version_id: i64) -> Result<Option<String>, sqlx::Error> {
    let mut conn = pool.acquire().await?;
    let row: Option<(String, Option<Vec<u8>>)> =
        sqlx::query_as("SELECT hash, chunks FROM file_versions WHERE session_id = ? AND id = ?")
            .bind(session_id)
            .bind(version_id)
            .fetch_optional(&mut *conn)
            .await?;
    match row {
        Some((hash, chunks)) => Ok(Some(row_content(&mut conn, session_id, &hash, chunks.as_deref()).await?)),
        None => Ok(None),
    }
}

/// Records `content` as the newest version of `path` on `branch` ("" for the
/// session's main lane), unless it's the same as the newest one already kept.
/// `user_id` is who made it; `previous` is what the file held before, for the
/// lines-changed counts.
#[allow(clippy::too_many_arguments)]
pub async fn record(
    pool: &sqlx::SqlitePool,
    session_id: &str,
    path: &str,
    branch: &str,
    content: &str,
    previous: Option<&str>,
    user_id: &str,
    origin: Option<&str>,
) -> Result<bool, sqlx::Error> {
    // The write lock from the start: two saves at once otherwise both read,
    // then one fails to upgrade ("database is locked") instead of waiting.
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
    let hash = store::content_id(&mut tx, session_id, content).await?;
    let newest: Option<(String, Option<Vec<u8>>)> = sqlx::query_as(
        "SELECT hash, chunks FROM file_versions WHERE session_id = ? AND path = ? AND branch = ? ORDER BY id DESC LIMIT 1",
    )
    .bind(session_id)
    .bind(path)
    .bind(branch)
    .fetch_optional(&mut *tx)
    .await?;
    if newest.as_ref().is_some_and(|(newest, _)| *newest == hash) {
        return Ok(false);
    }
    // With no "before" given, compare against the newest version kept.
    let kept_previous = match (previous, &newest) {
        (None, Some((newest_hash, chunks))) => row_content(&mut tx, session_id, newest_hash, chunks.as_deref()).await.ok(),
        _ => None,
    };
    let previous = previous.or(kept_previous.as_deref());
    let (added, removed) = line_counts(previous.unwrap_or_default(), content);
    let stored = store::put(&mut tx, session_id, content).await?;
    sqlx::query(
        "INSERT INTO file_versions (session_id, path, branch, hash, chunks, size, user_id, origin, added, removed, created_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(session_id)
    .bind(path)
    .bind(branch)
    .bind(&stored.content_id)
    .bind(&stored.chunks)
    .bind(stored.size)
    .bind(user_id)
    .bind(origin)
    .bind(added)
    .bind(removed)
    .bind(chrono::Utc::now().to_rfc3339())
    .execute(&mut *tx)
    .await?;
    // Keep the newest versions of this file; drop the rest.
    let dropped = sqlx::query(
        "DELETE FROM file_versions WHERE session_id = ? AND path = ? AND branch = ? AND id NOT IN \
         (SELECT id FROM file_versions WHERE session_id = ? AND path = ? AND branch = ? ORDER BY id DESC LIMIT ?)",
    )
    .bind(session_id)
    .bind(path)
    .bind(branch)
    .bind(session_id)
    .bind(path)
    .bind(branch)
    .bind(VERSIONS_KEPT_PER_FILE)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    tx.commit().await?;
    if dropped > 0 {
        store::mark_dirty(session_id);
    }
    Ok(true)
}

/// Removes stored contents no version points at any more.
pub async fn prune_blobs(pool: &sqlx::SqlitePool) -> Result<u64, sqlx::Error> {
    Ok(sqlx::query(
        "DELETE FROM file_blobs WHERE hash NOT IN (SELECT DISTINCT hash FROM file_versions WHERE chunks IS NULL)",
    )
    .execute(pool)
    .await?
    .rows_affected())
}

/// Moves versions kept in `file_blobs` (plain text) into the chunk store, one
/// at a time: each is read back and compared before its row points at the new
/// copy, so a version is never lost or changed. Returns how many moved.
pub async fn move_to_chunk_store(pool: &sqlx::SqlitePool) -> Result<u64, sqlx::Error> {
    let mut moved = 0;
    let mut failed: std::collections::HashSet<i64> = std::collections::HashSet::new();
    loop {
        let rows: Vec<(i64, String, String)> = sqlx::query_as(
            "SELECT id, session_id, hash FROM file_versions WHERE chunks IS NULL ORDER BY id LIMIT 200",
        )
        .fetch_all(pool)
        .await?;
        let rows: Vec<_> = rows.into_iter().filter(|(id, _, _)| !failed.contains(id)).collect();
        if rows.is_empty() {
            break;
        }
        for (id, session_id, hash) in rows {
            let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
            let content: Option<String> =
                sqlx::query_scalar("SELECT content FROM file_blobs WHERE hash = ?").bind(&hash).fetch_optional(&mut *tx).await?;
            let Some(content) = content else {
                failed.insert(id);
                eprintln!("history: version {id} has no stored text; left as it is");
                continue;
            };
            let stored = store::put(&mut tx, &session_id, &content).await?;
            if store::get(&mut tx, &session_id, &stored.chunks, &stored.content_id).await? != content {
                failed.insert(id);
                eprintln!("history: version {id} didn't read back identically; left as it is");
                continue;
            }
            sqlx::query("UPDATE file_versions SET hash = ?, chunks = ?, size = ? WHERE id = ?")
                .bind(&stored.content_id)
                .bind(&stored.chunks)
                .bind(stored.size)
                .bind(id)
                .execute(&mut *tx)
                .await?;
            tx.commit().await?;
            moved += 1;
        }
    }
    prune_blobs(pool).await?;
    Ok(moved)
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
    // The branch whose versions to list; the main lane's when left out.
    #[serde(default)]
    branch: Option<String>,
}

pub async fn versions(
    pool: &sqlx::SqlitePool,
    session_id: &str,
    path: Option<&str>,
    branch: &str,
    limit: i64,
) -> Result<Vec<VersionInfo>, sqlx::Error> {
    let limit = limit.clamp(1, MAX_LISTED);
    match path {
        Some(path) => {
            sqlx::query_as(
                "SELECT id, path, user_id, origin, added, removed, created_at FROM file_versions \
                 WHERE session_id = ? AND path = ? AND branch = ? ORDER BY id DESC LIMIT ?",
            )
            .bind(session_id)
            .bind(path)
            .bind(branch)
            .bind(limit)
            .fetch_all(pool)
            .await
        }
        None => {
            sqlx::query_as(
                "SELECT id, path, user_id, origin, added, removed, created_at FROM file_versions \
                 WHERE session_id = ? AND branch = ? ORDER BY id DESC LIMIT ?",
            )
            .bind(session_id)
            .bind(branch)
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
    let branch = crate::ws::clean_branch(params.branch.as_deref().unwrap_or_default());
    Ok(Json(versions(&state.pool, &session_id, params.path.as_deref(), &branch, params.limit.unwrap_or(50)).await?))
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
    let content = content_of(&state.pool, &session_id, version_id)
        .await?
        .ok_or_else(|| AppError::NotFound("no such version".into()))?;
    Ok(Json(VersionContent { info, content }))
}

// --- Snapshots ---------------------------------------------------------------
//
// A plan or checkpoint keeps a whole session's files. Stored as each file's
// chunk list (the chunks are shared with the versions, so a snapshot costs
// almost nothing); the chunks it lists are never cleaned up while it exists.

const SNAPSHOT_KEY: &str = "chunked";

#[derive(Serialize, Deserialize)]
struct ChunkedFile {
    id: String,
    chunks: String,
}

/// Stores `files` as a snapshot and returns what goes in `execution_plans.files`.
/// Files the server never shares (secrets, keys) are left out.
pub async fn snapshot_save(
    conn: &mut sqlx::SqliteConnection,
    session_id: &str,
    files: &std::collections::HashMap<String, String>,
) -> Result<String, sqlx::Error> {
    use base64::Engine as _;
    let mut stored = std::collections::BTreeMap::new();
    for (path, content) in files {
        if crate::guard::file_refusal(path, content).is_some() {
            continue;
        }
        let put = store::put(conn, session_id, content).await?;
        stored.insert(
            path.clone(),
            ChunkedFile { id: put.content_id, chunks: base64::engine::general_purpose::STANDARD.encode(&put.chunks) },
        );
    }
    Ok(serde_json::json!({ SNAPSHOT_KEY: stored }).to_string())
}

/// The files of a snapshot, in either form: chunk lists, or the full copies
/// stored before the chunk store.
pub async fn snapshot_load(
    conn: &mut sqlx::SqliteConnection,
    session_id: &str,
    json: &str,
) -> Result<std::collections::HashMap<String, String>, sqlx::Error> {
    use base64::Engine as _;
    let value: serde_json::Value = serde_json::from_str(json).map_err(|error| sqlx::Error::Protocol(format!("snapshot: {error}")))?;
    let Some(chunked) = snapshot_files(&value) else {
        return serde_json::from_value(value).map_err(|error| sqlx::Error::Protocol(format!("snapshot: {error}")));
    };
    let mut files = std::collections::HashMap::new();
    for (path, file) in chunked {
        let chunks = base64::engine::general_purpose::STANDARD
            .decode(&file.chunks)
            .map_err(|_| sqlx::Error::Protocol("snapshot: damaged chunk list".into()))?;
        files.insert(path, store::get(conn, session_id, &chunks, &file.id).await?);
    }
    Ok(files)
}

fn snapshot_files(value: &serde_json::Value) -> Option<std::collections::BTreeMap<String, ChunkedFile>> {
    let object = value.as_object()?;
    if object.len() != 1 {
        return None;
    }
    serde_json::from_value(object.get(SNAPSHOT_KEY)?.clone()).ok()
}

/// The chunk ids a session's snapshots still use, so clean-up keeps them.
pub async fn snapshot_chunk_ids(pool: &sqlx::SqlitePool, session_id: &str) -> Result<Vec<Vec<u8>>, sqlx::Error> {
    use base64::Engine as _;
    let rows: Vec<Option<String>> = sqlx::query_scalar(
        "SELECT files FROM execution_plans WHERE session_id = ? AND files LIKE '{\"chunked\"%'",
    )
    .bind(session_id)
    .fetch_all(pool)
    .await?;
    let mut ids = Vec::new();
    for json in rows.into_iter().flatten() {
        let Some(files) = serde_json::from_str::<serde_json::Value>(&json).ok().as_ref().and_then(snapshot_files) else {
            continue;
        };
        for file in files.into_values() {
            if let Ok(chunks) = base64::engine::general_purpose::STANDARD.decode(&file.chunks) {
                ids.extend(chunks.chunks(32).map(<[u8]>::to_vec));
            }
        }
    }
    Ok(ids)
}

/// Rewrites plans and checkpoints that hold full copies of files as chunk
/// lists, each checked to read back identically first. Returns how many.
pub async fn move_snapshots_to_chunk_store(pool: &sqlx::SqlitePool) -> Result<u64, sqlx::Error> {
    let rows: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT id, session_id, files FROM execution_plans WHERE files IS NOT NULL AND files NOT LIKE '{\"chunked\"%'",
    )
    .fetch_all(pool)
    .await?;
    let mut moved = 0;
    for (id, session_id, json) in rows {
        let Ok(files) = serde_json::from_str::<std::collections::HashMap<String, String>>(&json) else {
            eprintln!("history: snapshot {id} isn't a file map; left as it is");
            continue;
        };
        let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
        let stored = snapshot_save(&mut tx, &session_id, &files).await?;
        let read_back = snapshot_load(&mut tx, &session_id, &stored).await?;
        let kept: std::collections::HashMap<&String, &String> =
            files.iter().filter(|(path, content)| crate::guard::file_refusal(path, content).is_none()).collect();
        if read_back.len() != kept.len() || kept.iter().any(|(path, content)| read_back.get(*path) != Some(*content)) {
            eprintln!("history: snapshot {id} didn't read back identically; left as it is");
            continue;
        }
        sqlx::query("UPDATE execution_plans SET files = ? WHERE id = ?").bind(&stored).bind(&id).execute(&mut *tx).await?;
        tx.commit().await?;
        moved += 1;
    }
    Ok(moved)
}

async fn is_owner(state: &AppState, session_id: &str, user_id: &str) -> Result<bool, AppError> {
    let owner: Option<String> = sqlx::query_scalar("SELECT owner_id FROM sessions WHERE id = ?")
        .bind(session_id)
        .fetch_optional(&state.pool)
        .await?;
    Ok(owner.as_deref() == Some(user_id))
}

#[derive(Serialize, Debug, PartialEq)]
pub struct Deleted {
    versions: u64,
    chunks: u64,
}

/// `DELETE /sessions/{id}/history/{version}`: deletes one version. The
/// session's owner can delete any; others only versions they made.
pub async fn delete_version_handler(
    State(state): State<AppState>,
    Extension(AuthUser(user_id)): Extension<AuthUser>,
    Path((session_id, version_id)): Path<(String, i64)>,
) -> Result<Json<Deleted>, AppError> {
    crate::handlers::require_member(&state, &session_id, &user_id).await?;
    let author: Option<String> = sqlx::query_scalar("SELECT user_id FROM file_versions WHERE session_id = ? AND id = ?")
        .bind(&session_id)
        .bind(version_id)
        .fetch_optional(&state.pool)
        .await?;
    let author = author.ok_or_else(|| AppError::NotFound("no such version".into()))?;
    if author != user_id && !is_owner(&state, &session_id, &user_id).await? {
        return Err(AppError::Unauthorized("only the session owner, or whoever made this version, can delete it".into()));
    }
    let versions = sqlx::query("DELETE FROM file_versions WHERE session_id = ? AND id = ?")
        .bind(&session_id)
        .bind(version_id)
        .execute(&state.pool)
        .await?
        .rows_affected();
    let chunks = store::collect_garbage(&state.pool, &session_id).await?;
    prune_blobs(&state.pool).await?;
    Ok(Json(Deleted { versions, chunks }))
}

#[derive(Deserialize)]
pub struct DeleteParams {
    #[serde(default)]
    path: Option<String>,
    // Delete the versions of `path` older than this one.
    #[serde(default)]
    before: Option<i64>,
    #[serde(default)]
    branch: Option<String>,
}

/// `DELETE /sessions/{id}/history`, for the session's owner: with `path` and
/// `before`, the file's versions older than `before`; with `path` only, all of
/// the file's versions; with neither, the whole session's history, and its
/// key is destroyed so nothing of it can be read again, backups included.
pub async fn delete_many_handler(
    State(state): State<AppState>,
    Extension(AuthUser(user_id)): Extension<AuthUser>,
    Path(session_id): Path<String>,
    Query(params): Query<DeleteParams>,
) -> Result<Json<Deleted>, AppError> {
    crate::handlers::require_member(&state, &session_id, &user_id).await?;
    if !is_owner(&state, &session_id, &user_id).await? {
        return Err(AppError::Unauthorized("only the session owner can delete history".into()));
    }
    let branch = crate::ws::clean_branch(params.branch.as_deref().unwrap_or_default());
    let versions = match (&params.path, params.before) {
        (Some(path), Some(before)) => sqlx::query(
            "DELETE FROM file_versions WHERE session_id = ? AND path = ? AND branch = ? AND id < ?",
        )
        .bind(&session_id)
        .bind(path)
        .bind(&branch)
        .bind(before)
        .execute(&state.pool)
        .await?
        .rows_affected(),
        (Some(path), None) => sqlx::query("DELETE FROM file_versions WHERE session_id = ? AND path = ? AND branch = ?")
            .bind(&session_id)
            .bind(path)
            .bind(&branch)
            .execute(&state.pool)
            .await?
            .rows_affected(),
        (None, _) => {
            let mut tx = state.pool.begin().await?;
            let versions = sqlx::query("DELETE FROM file_versions WHERE session_id = ?")
                .bind(&session_id)
                .execute(&mut *tx)
                .await?
                .rows_affected();
            sqlx::query("DELETE FROM execution_plans WHERE session_id = ? AND status = 'checkpoint'")
                .bind(&session_id)
                .execute(&mut *tx)
                .await?;
            sqlx::query("UPDATE execution_plans SET files = NULL WHERE session_id = ?")
                .bind(&session_id)
                .execute(&mut *tx)
                .await?;
            let chunks: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM chunks WHERE session_id = ?")
                .bind(&session_id)
                .fetch_one(&mut *tx)
                .await?;
            store::shred_session(&mut tx, &session_id).await?;
            tx.commit().await?;
            prune_blobs(&state.pool).await?;
            return Ok(Json(Deleted { versions, chunks: chunks as u64 }));
        }
    };
    let chunks = store::collect_garbage(&state.pool, &session_id).await?;
    prune_blobs(&state.pool).await?;
    Ok(Json(Deleted { versions, chunks }))
}

#[derive(Serialize, Debug, PartialEq)]
pub struct Usage {
    pub versions: i64,
    pub files: i64,
    /// The text of every kept version, added up.
    pub text_bytes: i64,
    /// What's actually stored for them.
    pub stored_bytes: i64,
}

pub async fn usage(pool: &sqlx::SqlitePool, session_id: &str) -> Result<Usage, sqlx::Error> {
    let (versions, files, chunked_text): (i64, i64, Option<i64>) = sqlx::query_as(
        "SELECT COUNT(*), COUNT(DISTINCT path || char(0) || branch), SUM(size) FROM file_versions WHERE session_id = ?",
    )
    .bind(session_id)
    .fetch_one(pool)
    .await?;
    let (legacy_text, legacy_stored): (Option<i64>, Option<i64>) = sqlx::query_as(
        "SELECT SUM(LENGTH(CAST(b.content AS BLOB))), \
         (SELECT SUM(LENGTH(CAST(content AS BLOB))) FROM file_blobs WHERE hash IN \
            (SELECT hash FROM file_versions WHERE session_id = ? AND chunks IS NULL)) \
         FROM file_versions v JOIN file_blobs b ON b.hash = v.hash WHERE v.session_id = ? AND v.chunks IS NULL",
    )
    .bind(session_id)
    .bind(session_id)
    .fetch_one(pool)
    .await?;
    let chunk_bytes: Option<i64> = sqlx::query_scalar("SELECT SUM(LENGTH(data)) FROM chunks WHERE session_id = ?")
        .bind(session_id)
        .fetch_one(pool)
        .await?;
    Ok(Usage {
        versions,
        files,
        text_bytes: chunked_text.unwrap_or(0) + legacy_text.unwrap_or(0),
        stored_bytes: chunk_bytes.unwrap_or(0) + legacy_stored.unwrap_or(0),
    })
}

/// `GET /sessions/{id}/history/usage`: how much history the session keeps.
pub async fn usage_handler(
    State(state): State<AppState>,
    Extension(AuthUser(user_id)): Extension<AuthUser>,
    Path(session_id): Path<String>,
) -> Result<Json<Usage>, AppError> {
    crate::handlers::require_member(&state, &session_id, &user_id).await?;
    Ok(Json(usage(&state.pool, &session_id).await?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn versions_are_kept_once_per_change_and_can_be_read_back() {
        let pool = crate::db::init_pool("sqlite::memory:").await.unwrap();
        assert!(record(&pool, "s", "a.py", "", "x = 1\n", None, "alice", None).await.unwrap());
        // Saving the same text again isn't a new version.
        assert!(!record(&pool, "s", "a.py", "", "x = 1\n", Some("x = 1\n"), "bob", None).await.unwrap());
        assert!(record(&pool, "s", "a.py", "", "x = 2\ny = 3\n", Some("x = 1\n"), "bob", Some("agent")).await.unwrap());
        // Going back to an earlier text is a version, but stores no new copy.
        assert!(record(&pool, "s", "a.py", "", "x = 1\n", Some("x = 2\ny = 3\n"), "alice", None).await.unwrap());
        assert!(record(&pool, "s", "b.py", "", "z\n", None, "alice", None).await.unwrap());
        assert!(record(&pool, "other", "a.py", "", "secret\n", None, "carol", None).await.unwrap());

        let listed = versions(&pool, "s", Some("a.py"), "", 50).await.unwrap();
        assert_eq!(listed.iter().map(|v| v.user_id.as_str()).collect::<Vec<_>>(), ["alice", "bob", "alice"]);
        assert_eq!((listed[1].added, listed[1].removed), (2, 1));
        assert_eq!(listed[1].origin.as_deref(), Some("agent"));
        assert_eq!(versions(&pool, "s", None, "", 50).await.unwrap().len(), 4, "one session's versions only");

        let (chunks,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM chunks WHERE session_id = 's'").fetch_one(&pool).await.unwrap();
        assert_eq!(chunks, 3, "x = 1 is stored once though it's two versions");
        for version in &listed {
            let text = content_of(&pool, "s", version.id).await.unwrap().unwrap();
            assert!(text == "x = 1\n" || text == "x = 2\ny = 3\n");
        }
        assert_eq!(content_of(&pool, "s", listed[1].id).await.unwrap().as_deref(), Some("x = 2\ny = 3\n"));
        assert_eq!(content_of(&pool, "other", listed[1].id).await.unwrap(), None, "another session's version isn't readable");
    }

    #[test]
    fn counting_only_the_changed_middle_gives_the_same_counts() {
        use similar::{ChangeTag, TextDiff};
        let full = |a: &str, b: &str| {
            TextDiff::from_lines(a, b).iter_all_changes().fold((0, 0), |(x, y), change| match change.tag() {
                ChangeTag::Insert => (x + 1, y),
                ChangeTag::Delete => (x, y + 1),
                ChangeTag::Equal => (x, y),
            })
        };
        let base: String = (0..300).map(|n| format!("line {n}\n")).collect();
        let cases = [
            (base.clone(), base.replace("line 150\n", "line 150 changed\n")),
            (base.clone(), base.replace("line 0\n", "")),
            (base.clone(), format!("{base}line 300\n")),
            (base.clone(), base.replace("line 299\n", "line 299")),
            (base.clone(), base.replace("line 10\n", "line 10\nline 10\n")),
            (String::new(), "a\n".into()),
            ("a\n".into(), String::new()),
            ("same".into(), "same".into()),
            ("ab\n".into(), "abc\n".into()),
        ];
        for (a, b) in &cases {
            assert_eq!(line_counts(a, b), full(a, b), "{a:?} -> {b:?}");
        }
    }

    #[tokio::test]
    async fn saves_at_the_same_moment_all_get_recorded() {
        // In real-app testing two saves of one file at once failed with
        // "database is locked" and one version was never kept.
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::db::init_pool(&format!("sqlite://{}/h.db", dir.path().display())).await.unwrap();
        let saves = (0..20).map(|n| {
            let pool = pool.clone();
            tokio::spawn(async move {
                let content = format!("def refund_for(order):  # edit {n}\n    return {n}\n");
                record(&pool, "s", "returns.py", "", &content, None, &format!("u{n}"), None).await
            })
        });
        for save in futures_util::future::join_all(saves).await {
            assert!(save.unwrap().is_ok(), "no save fails for the lock");
        }
        assert_eq!(versions(&pool, "s", Some("returns.py"), "", 50).await.unwrap().len(), 20);
    }

    #[tokio::test]
    async fn old_plain_versions_move_into_the_encrypted_store_unchanged() {
        let pool = crate::db::init_pool("sqlite::memory:").await.unwrap();
        let text = "def total(cart):\n    return sum(cart)\n".repeat(200);
        let hash = crate::patch::content_hash(&text);
        sqlx::query("INSERT INTO file_blobs (hash, content) VALUES (?, ?)").bind(&hash).bind(&text).execute(&pool).await.unwrap();
        sqlx::query(
            "INSERT INTO file_versions (session_id, path, branch, hash, user_id, added, removed, created_at) \
             VALUES ('s', 'cart.py', '', ?, 'alice', 400, 0, 'then')",
        )
        .bind(&hash)
        .execute(&pool)
        .await
        .unwrap();
        assert_eq!(move_to_chunk_store(&pool).await.unwrap(), 1);
        let (id,): (i64,) = sqlx::query_as("SELECT id FROM file_versions").fetch_one(&pool).await.unwrap();
        assert_eq!(content_of(&pool, "s", id).await.unwrap().as_deref(), Some(text.as_str()));
        let (blobs,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM file_blobs").fetch_one(&pool).await.unwrap();
        assert_eq!(blobs, 0, "the plain copy is gone once it's moved");
        assert_eq!(move_to_chunk_store(&pool).await.unwrap(), 0, "nothing left to move");
        // A new save after the move is still compared with it.
        assert!(!record(&pool, "s", "cart.py", "", &text, None, "bob", None).await.unwrap());
    }

    #[tokio::test]
    async fn snapshots_read_back_and_keep_their_chunks_through_clean_up() {
        let pool = crate::db::init_pool("sqlite::memory:").await.unwrap();
        let files: std::collections::HashMap<String, String> = [
            ("a.py".to_string(), "x = 1\n".repeat(500)),
            ("b.py".to_string(), "y = 2\n".repeat(500)),
            (".env".to_string(), "KEY=sk-live-0000000000000000000000\n".to_string()),
        ]
        .into();
        let mut conn = pool.acquire().await.unwrap();
        let json = snapshot_save(&mut conn, "s", &files).await.unwrap();
        assert!(!json.contains("x = 1"), "no file text in the snapshot row");
        // Just the plan row; its session and author don't matter here.
        sqlx::query("PRAGMA foreign_keys = OFF").execute(&mut *conn).await.unwrap();
        sqlx::query("INSERT INTO execution_plans (id, session_id, author_id, content, status, files, created_at, updated_at) \
                     VALUES ('p', 's', 'u', 'c', 'pending', ?, '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')")
            .bind(&json)
            .execute(&mut *conn)
            .await
            .unwrap();
        // No version uses these chunks, but the plan does.
        assert_eq!(store::collect_garbage(&pool, "s").await.unwrap(), 0);
        let back = snapshot_load(&mut conn, "s", &json).await.unwrap();
        assert_eq!(back.len(), 2, "the secret file was left out");
        assert_eq!(back["a.py"], files["a.py"]);
        // A plan from before the chunk store still loads, and moves over.
        let legacy = serde_json::to_string(&std::collections::HashMap::from([("c.py", "z\n")])).unwrap();
        sqlx::query("UPDATE execution_plans SET files = ? WHERE id = 'p'").bind(&legacy).execute(&pool).await.unwrap();
        assert_eq!(snapshot_load(&mut conn, "s", &legacy).await.unwrap()["c.py"], "z\n");
        drop(conn);
        assert_eq!(move_snapshots_to_chunk_store(&pool).await.unwrap(), 1);
        let moved: String = sqlx::query_scalar("SELECT files FROM execution_plans WHERE id = 'p'").fetch_one(&pool).await.unwrap();
        assert!(moved.starts_with("{\"chunked\""));
    }

    #[tokio::test]
    async fn deleting_versions_frees_what_only_they_used() {
        let pool = crate::db::init_pool("sqlite::memory:").await.unwrap();
        let first = "a = 1\n".repeat(2000);
        let second = format!("{first}b = 2\n");
        record(&pool, "s", "a.py", "", &first, None, "alice", None).await.unwrap();
        record(&pool, "s", "a.py", "", &second, None, "bob", None).await.unwrap();
        let before = usage(&pool, "s").await.unwrap();
        assert_eq!(before.versions, 2);
        assert!(before.stored_bytes * 5 < before.text_bytes, "two near-identical versions cost about one, compressed: {before:?}");
        let listed = versions(&pool, "s", Some("a.py"), "", 50).await.unwrap();
        sqlx::query("DELETE FROM file_versions WHERE id = ?").bind(listed[1].id).execute(&pool).await.unwrap();
        // The older version's chunks are mostly shared with the newer one.
        store::collect_garbage(&pool, "s").await.unwrap();
        assert_eq!(content_of(&pool, "s", listed[0].id).await.unwrap().as_deref(), Some(second.as_str()));
        sqlx::query("DELETE FROM file_versions WHERE session_id = 's'").execute(&pool).await.unwrap();
        assert!(store::collect_garbage(&pool, "s").await.unwrap() > 0);
        assert_eq!(usage(&pool, "s").await.unwrap().stored_bytes, 0);
    }

    #[tokio::test]
    async fn each_branch_keeps_its_own_versions() {
        let pool = crate::db::init_pool("sqlite::memory:").await.unwrap();
        assert!(record(&pool, "s", "cart.py", "", "def total(self):\n", None, "alice", None).await.unwrap());
        // The same text on another branch is that branch's first version.
        assert!(record(&pool, "s", "cart.py", "feature/tax", "def total(self):\n", None, "bob", None).await.unwrap());
        assert!(
            record(&pool, "s", "cart.py", "feature/tax", "def total(self, tax):\n", None, "bob", None).await.unwrap()
        );

        let main = versions(&pool, "s", Some("cart.py"), "", 50).await.unwrap();
        assert_eq!(main.iter().map(|v| v.user_id.as_str()).collect::<Vec<_>>(), ["alice"]);
        let branch = versions(&pool, "s", Some("cart.py"), "feature/tax", 50).await.unwrap();
        assert_eq!(branch.iter().map(|v| v.user_id.as_str()).collect::<Vec<_>>(), ["bob", "bob"]);
        // Lines changed are counted against the branch's own previous version.
        assert_eq!((branch[0].added, branch[0].removed), (1, 1));
        assert_eq!(versions(&pool, "s", None, "", 50).await.unwrap().len(), 1, "the main lane's list has no branch versions");
    }
}

/// Replays real git history through the version store and reports what it
/// costs compared with keeping each distinct version whole (the old design).
/// `BENCH_REPO=~/i-ide BENCH_COMMITS=300 cargo test --release bench_replay -- --ignored --nocapture`
#[cfg(test)]
mod bench {
    use super::*;
    use std::process::Command;

    fn git(repo: &str, args: &[&str]) -> Option<String> {
        let output = Command::new("git").arg("-C").arg(repo).args(args).output().ok()?;
        output.status.success().then(|| String::from_utf8_lossy(&output.stdout).into_owned())
    }

    #[tokio::test]
    #[ignore]
    async fn bench_replay_git_history() {
        let repo = std::env::var("BENCH_REPO").expect("set BENCH_REPO to a git repository");
        let commits: usize = std::env::var("BENCH_COMMITS").ok().and_then(|n| n.parse().ok()).unwrap_or(300);
        let pool = crate::db::init_pool("sqlite::memory:").await.unwrap();
        let log = git(&repo, &["log", "--format=%H", "-n", &commits.to_string(), "--", "*.rs", "*.html", "*.md"]).unwrap();
        let mut hashes: Vec<&str> = log.lines().collect();
        hashes.reverse();
        let mut distinct_whole = std::collections::HashSet::new();
        // As the live save path does: the file's previous text is known.
        let mut last: std::collections::HashMap<String, String> = std::collections::HashMap::new();
        let (mut versions, mut whole_bytes, mut zstd_whole_bytes) = (0u64, 0u64, 0u64);
        let started = std::time::Instant::now();
        for commit in &hashes {
            let changed = git(&repo, &["diff-tree", "--no-commit-id", "--name-only", "-r", commit]).unwrap_or_default();
            for path in changed.lines().filter(|path| path.ends_with(".rs") || path.ends_with(".html") || path.ends_with(".md")) {
                let Some(text) = git(&repo, &["show", &format!("{commit}:{path}")]) else { continue };
                if text.len() > 2_000_000 {
                    continue;
                }
                let previous = last.insert(path.to_string(), text.clone());
                if record(&pool, "bench", path, "", &text, previous.as_deref(), "u", None).await.unwrap() {
                    versions += 1;
                    if distinct_whole.insert(blake3::hash(text.as_bytes())) {
                        whole_bytes += text.len() as u64;
                        zstd_whole_bytes += zstd::bulk::compress(text.as_bytes(), 3).unwrap().len() as u64;
                    }
                }
            }
        }
        let elapsed = started.elapsed();
        let used = usage(&pool, "bench").await.unwrap();
        let (chunks,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM chunks").fetch_one(&pool).await.unwrap();
        // Reading every version back, checked.
        let ids: Vec<i64> = sqlx::query_scalar("SELECT id FROM file_versions").fetch_all(&pool).await.unwrap();
        let read_started = std::time::Instant::now();
        for id in &ids {
            content_of(&pool, "bench", *id).await.unwrap().unwrap();
        }
        let read = read_started.elapsed();
        let mb = |bytes: u64| bytes as f64 / 1_048_576.0;
        println!("commits replayed:            {}", hashes.len());
        println!("versions kept:               {versions} ({} files)", used.files);
        println!("text of all versions:        {:.1} MB", mb(used.text_bytes as u64));
        println!("old design (whole, distinct):{:.1} MB", mb(whole_bytes));
        println!("whole + zstd only:           {:.1} MB", mb(zstd_whole_bytes));
        println!("new design (chunks):         {:.2} MB in {chunks} chunks", mb(used.stored_bytes as u64));
        println!("saving vs old design:        {:.1}x", whole_bytes as f64 / used.stored_bytes.max(1) as f64);
        println!("write: {:.2} ms per version, read: {:.2} ms per version", elapsed.as_secs_f64() * 1000.0 / versions.max(1) as f64, read.as_secs_f64() * 1000.0 / ids.len().max(1) as f64);
    }
}
