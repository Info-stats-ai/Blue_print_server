use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};
use sqlx::SqlitePool;
use std::path::Path;
use std::str::FromStr;
use std::time::Duration;

pub async fn init_pool(database_url: &str) -> Result<SqlitePool, sqlx::Error> {
    create_parent_dir(database_url)?;

    let options = SqliteConnectOptions::from_str(database_url)?
        .create_if_missing(true)
        .foreign_keys(true)
        // Without WAL, SQLite's default rollback-journal mode takes an
        // exclusive lock for the duration of every write — every read and
        // every other write blocks behind it. At real concurrent load
        // (checkpoints, plan creates/restores, auth/session queries all
        // landing around the same time) that turns into exactly the
        // `SQLITE_BUSY` errors `busy_timeout` below is also there to
        // smooth over. WAL lets readers proceed while a single writer
        // writes, which is the whole reason the checkpoint sweep's
        // bounded-concurrency rewrite (checkpoint.rs) is worth anything —
        // without this, that change mostly just moved where contention
        // happens rather than reducing it.
        .journal_mode(SqliteJournalMode::Wal)
        // NORMAL is the standard, well-tested pairing with WAL (not FULL,
        // which exists for the rollback-journal mode this isn't using) —
        // WAL's own log already provides the durability guarantee FULL
        // sync exists to add, so pairing them is wasted fsync overhead,
        // not extra safety.
        .synchronous(SqliteSynchronous::Normal)
        // If a writer briefly holds the lock, a second writer waits up to
        // this long for it to clear instead of failing immediately with
        // `SQLITE_BUSY` — turns a real but short-lived contention moment
        // into added latency, not a hard error surfaced to a user action.
        .busy_timeout(Duration::from_secs(10));

    let pool = SqlitePoolOptions::new()
        .max_connections(10)
        .connect_with(options)
        .await?;

    sqlx::migrate!("./migrations").run(&pool).await?;

    Ok(pool)
}

/// Creates the parent directory of a sqlite:// URL's filesystem path if it's
/// missing, so a fresh Railway volume mount has somewhere to put the DB file
/// on first boot (create_if_missing only creates the file, not its directory).
/// Strips the scheme and any query string before touching the filesystem —
/// create_dir_all must never see the raw URL. A no-op for in-memory URLs or
/// any path with no parent component (e.g. a bare relative filename).
fn create_parent_dir(database_url: &str) -> Result<(), sqlx::Error> {
    let without_scheme = database_url
        .strip_prefix("sqlite://")
        .or_else(|| database_url.strip_prefix("sqlite:"))
        .unwrap_or(database_url);
    let path_str = without_scheme.split('?').next().unwrap_or(without_scheme);

    if path_str.is_empty() || path_str == ":memory:" {
        return Ok(());
    }

    let path = Path::new(path_str);
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(sqlx::Error::Io)?;
        }
    }

    Ok(())
}
