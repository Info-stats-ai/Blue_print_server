use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::SqlitePool;
use std::path::Path;
use std::str::FromStr;

pub async fn init_pool(database_url: &str) -> Result<SqlitePool, sqlx::Error> {
    create_parent_dir(database_url)?;

    let options = SqliteConnectOptions::from_str(database_url)?
        .create_if_missing(true)
        .foreign_keys(true);

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
