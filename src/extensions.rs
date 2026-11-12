//! I-IDE's extension store, served under `/api/extensions` in the shape the
//! IDE's extension host already speaks: search, updates, versions, download.
//!
//! The packages are open-source extensions we build from their own source
//! (see `extensions/publish.py`), each keeping its own licence. They reach the
//! store two ways: committed under `extensions/packages/<id>/` (loaded at
//! startup, so no secret is needed), or published to `POST /api/extensions/{id}`
//! with `EXTENSIONS_PUBLISH_TOKEN`.

use std::cmp::Ordering;

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::Json;
use base64::Engine as _;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::AppState;
use crate::errors::AppError;

/// Packages above this are refused; the largest language extensions are a few MB.
pub const MAX_ARCHIVE_BYTES: usize = 48 * 1024 * 1024;
const MAX_LISTED: usize = 500;

#[derive(Debug, Default, Deserialize)]
pub struct VersionQuery {
    filter: Option<String>,
    provides: Option<String>,
    ids: Option<String>,
    min_schema_version: Option<i64>,
    max_schema_version: Option<i64>,
    min_wasm_api_version: Option<String>,
    max_wasm_api_version: Option<String>,
}

struct Row {
    id: String,
    version: String,
    manifest: Value,
    published_at: String,
    download_count: i64,
}

fn parse_version(text: &str) -> Vec<u64> {
    text.trim()
        .trim_start_matches('v')
        .split(['.', '-', '+'])
        .map_while(|part| part.parse().ok())
        .collect()
}

fn compare_versions(a: &str, b: &str) -> Ordering {
    let (a, b) = (parse_version(a), parse_version(b));
    for index in 0..a.len().max(b.len()) {
        let ordering = a.get(index).unwrap_or(&0).cmp(b.get(index).unwrap_or(&0));
        if ordering != Ordering::Equal {
            return ordering;
        }
    }
    Ordering::Equal
}

impl VersionQuery {
    /// Whether this IDE can run the package: its manifest schema and, for
    /// extensions with code, its wasm API, are within what the IDE asked for.
    fn compatible(&self, manifest: &Value) -> bool {
        let schema = manifest["schema_version"].as_i64().unwrap_or(0);
        if self.min_schema_version.is_some_and(|min| schema < min)
            || self.max_schema_version.is_some_and(|max| schema > max)
        {
            return false;
        }
        match manifest["wasm_api_version"].as_str() {
            None => true,
            Some(wasm) => {
                self.min_wasm_api_version
                    .as_deref()
                    .is_none_or(|min| compare_versions(wasm, min) != Ordering::Less)
                    && self
                        .max_wasm_api_version
                        .as_deref()
                        .is_none_or(|max| compare_versions(wasm, max) != Ordering::Greater)
            }
        }
    }

    fn matches(&self, row: &Row) -> bool {
        if let Some(filter) = self.filter.as_deref().map(str::trim).filter(|f| !f.is_empty()) {
            let filter = filter.to_lowercase();
            let text = format!(
                "{} {} {}",
                row.id,
                row.manifest["name"].as_str().unwrap_or_default(),
                row.manifest["description"].as_str().unwrap_or_default()
            )
            .to_lowercase();
            if !text.contains(&filter) {
                return false;
            }
        }
        if let Some(provides) = self.provides.as_deref().filter(|p| !p.is_empty()) {
            let offered: Vec<&str> = row.manifest["provides"]
                .as_array()
                .map(|items| items.iter().filter_map(Value::as_str).collect())
                .unwrap_or_default();
            if !provides.split(',').any(|wanted| offered.contains(&wanted.trim())) {
                return false;
            }
        }
        if let Some(ids) = self.ids.as_deref() {
            if !ids.split(',').any(|id| id.trim() == row.id) {
                return false;
            }
        }
        true
    }
}

impl Row {
    /// The IDE's `ExtensionMetadata`: the manifest's fields plus id,
    /// publish time and downloads.
    fn metadata(&self) -> Value {
        let mut metadata = self.manifest.clone();
        if let Some(object) = metadata.as_object_mut() {
            object.insert("id".into(), json!(self.id));
            object.insert("version".into(), json!(self.version));
            object.insert("published_at".into(), json!(self.published_at));
            object.insert("download_count".into(), json!(self.download_count.max(0)));
        }
        metadata
    }
}

async fn rows(pool: &sqlx::SqlitePool, id: Option<&str>) -> Result<Vec<Row>, AppError> {
    let raw: Vec<(String, String, String, String, i64)> = sqlx::query_as(
        "SELECT id, version, manifest, published_at, download_count FROM extension_versions \
         WHERE (?1 IS NULL OR id = ?1)",
    )
    .bind(id)
    .fetch_all(pool)
    .await?;
    Ok(raw
        .into_iter()
        .filter_map(|(id, version, manifest, published_at, download_count)| {
            Some(Row { id, version, manifest: serde_json::from_str(&manifest).ok()?, published_at, download_count })
        })
        .collect())
}

/// Newest compatible version of each matching extension, most downloaded first.
fn latest(rows: Vec<Row>, query: &VersionQuery) -> Vec<Row> {
    let mut best: std::collections::HashMap<String, Row> = Default::default();
    for row in rows.into_iter().filter(|row| query.compatible(&row.manifest) && query.matches(row)) {
        match best.get(&row.id) {
            Some(current) if compare_versions(&current.version, &row.version) != Ordering::Less => {}
            _ => {
                best.insert(row.id.clone(), row);
            }
        }
    }
    let mut rows: Vec<Row> = best.into_values().collect();
    rows.sort_by(|a, b| b.download_count.cmp(&a.download_count).then_with(|| a.id.cmp(&b.id)));
    rows.truncate(MAX_LISTED);
    rows
}

fn listing(rows: Vec<Row>) -> Json<Value> {
    Json(json!({ "data": rows.iter().map(Row::metadata).collect::<Vec<_>>() }))
}

pub async fn search(State(state): State<AppState>, Query(query): Query<VersionQuery>) -> Result<Json<Value>, AppError> {
    Ok(listing(latest(rows(&state.pool, None).await?, &query)))
}

pub async fn updates(State(state): State<AppState>, Query(query): Query<VersionQuery>) -> Result<Json<Value>, AppError> {
    if query.ids.as_deref().is_none_or(|ids| ids.trim().is_empty()) {
        return Ok(listing(Vec::new()));
    }
    Ok(listing(latest(rows(&state.pool, None).await?, &query)))
}

pub async fn versions(State(state): State<AppState>, Path(id): Path<String>) -> Result<Json<Value>, AppError> {
    let mut rows = rows(&state.pool, Some(&id)).await?;
    rows.sort_by(|a, b| compare_versions(&b.version, &a.version));
    Ok(listing(rows))
}

async fn send_archive(pool: &sqlx::SqlitePool, id: &str, version: &str) -> Result<Response, AppError> {
    let (archive,): (Vec<u8>,) =
        sqlx::query_as("SELECT archive FROM extension_versions WHERE id = ? AND version = ?")
            .bind(id)
            .bind(version)
            .fetch_one(pool)
            .await?;
    sqlx::query("UPDATE extension_versions SET download_count = download_count + 1 WHERE id = ? AND version = ?")
        .bind(id)
        .bind(version)
        .execute(pool)
        .await?;
    Ok((
        [
            (header::CONTENT_TYPE, "application/gzip".to_string()),
            (header::CONTENT_DISPOSITION, format!("attachment; filename=\"{id}-{version}.tar.gz\"")),
        ],
        archive,
    )
        .into_response())
}

pub async fn download_latest(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<VersionQuery>,
) -> Result<Response, AppError> {
    let rows = latest(rows(&state.pool, Some(&id)).await?, &query);
    let row = rows.first().ok_or_else(|| AppError::NotFound(format!("no compatible version of {id}")))?;
    send_archive(&state.pool, &row.id, &row.version).await
}

pub async fn download_version(
    State(state): State<AppState>,
    Path((id, version)): Path<(String, String)>,
) -> Result<Response, AppError> {
    send_archive(&state.pool, &id, &version).await
}

/// Loads the packages committed under `dir` (default `extensions/packages`):
/// each `<id>/manifest.json` with its `<id>/archive.tar.gz`. Versions already
/// in the store are left alone, so download counts survive restarts.
pub async fn load_bundled(pool: &sqlx::SqlitePool, dir: Option<&str>) -> usize {
    let dir = std::path::PathBuf::from(dir.unwrap_or("extensions/packages"));
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return 0;
    };
    let mut loaded = 0;
    for entry in entries.flatten() {
        let id = entry.file_name().to_string_lossy().to_string();
        let (Ok(manifest), Ok(archive)) = (
            std::fs::read(entry.path().join("manifest.json")),
            std::fs::read(entry.path().join("archive.tar.gz")),
        ) else {
            continue;
        };
        let Ok(manifest) = serde_json::from_slice::<Value>(&manifest) else {
            continue;
        };
        let Some(version) = manifest["version"].as_str().map(str::to_string) else {
            continue;
        };
        if !valid_id(&id) || archive.len() > MAX_ARCHIVE_BYTES || !archive.starts_with(&[0x1f, 0x8b]) {
            continue;
        }
        let inserted = sqlx::query(
            "INSERT OR IGNORE INTO extension_versions (id, version, manifest, archive, published_at) VALUES (?, ?, ?, ?, ?)",
        )
        .bind(&id)
        .bind(&version)
        .bind(manifest.to_string())
        .bind(archive)
        .bind(chrono::Utc::now().to_rfc3339())
        .execute(pool)
        .await
        .map(|result| result.rows_affected())
        .unwrap_or(0);
        loaded += inserted as usize;
    }
    loaded
}

fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

fn tokens_match(given: &str, expected: &str) -> bool {
    use sha2::{Digest, Sha256};
    // Compare digests so the time taken says nothing about the token.
    let (a, b) = (Sha256::digest(given.as_bytes()), Sha256::digest(expected.as_bytes()));
    a.iter().zip(b.iter()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Publishes one version: the body is the package (`archive.tar.gz`), and the
/// `x-extension-manifest` header carries its `manifest.json`, base64-encoded.
pub async fn publish(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<StatusCode, AppError> {
    let expected = std::env::var("EXTENSIONS_PUBLISH_TOKEN").ok().filter(|token| token.len() >= 24);
    let Some(expected) = expected else {
        return Err(AppError::NotFound("publishing is off on this server".into()));
    };
    let given = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .unwrap_or_default();
    if !tokens_match(given, &expected) {
        return Err(AppError::Unauthorized("wrong publish token".into()));
    }
    if !valid_id(&id) {
        return Err(AppError::BadRequest("extension ids are lowercase letters, digits, - and _".into()));
    }
    if body.is_empty() || body.len() > MAX_ARCHIVE_BYTES || !body.starts_with(&[0x1f, 0x8b]) {
        return Err(AppError::BadRequest("the body must be a .tar.gz package under 48 MB".into()));
    }
    let manifest = headers
        .get("x-extension-manifest")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| base64::engine::general_purpose::STANDARD.decode(value).ok())
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        .ok_or_else(|| AppError::BadRequest("x-extension-manifest must be base64 JSON".into()))?;
    let version = manifest["version"]
        .as_str()
        .filter(|version| !parse_version(version).is_empty())
        .ok_or_else(|| AppError::BadRequest("the manifest needs a version".into()))?
        .to_string();
    if manifest["name"].as_str().is_none_or(str::is_empty) || manifest["repository"].as_str().is_none() {
        return Err(AppError::BadRequest("the manifest needs a name and repository".into()));
    }
    sqlx::query(
        "INSERT INTO extension_versions (id, version, manifest, archive, published_at) VALUES (?, ?, ?, ?, ?) \
         ON CONFLICT (id, version) DO UPDATE SET manifest = excluded.manifest, archive = excluded.archive",
    )
    .bind(&id)
    .bind(&version)
    .bind(manifest.to_string())
    .bind(body.to_vec())
    .bind(chrono::Utc::now().to_rfc3339())
    .execute(&state.pool)
    .await?;
    Ok(StatusCode::CREATED)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_numerically() {
        assert_eq!(compare_versions("0.10.0", "0.9.3"), Ordering::Greater);
        assert_eq!(compare_versions("1.0", "1.0.0"), Ordering::Equal);
        assert_eq!(compare_versions("v0.2.1", "0.2.0"), Ordering::Greater);
    }

    #[test]
    fn compatibility_uses_schema_and_wasm_ranges() {
        let query = VersionQuery {
            max_schema_version: Some(1),
            min_wasm_api_version: Some("0.1.0".into()),
            max_wasm_api_version: Some("0.7.0".into()),
            ..Default::default()
        };
        assert!(query.compatible(&json!({"schema_version": 1, "wasm_api_version": "0.6.0"})));
        assert!(query.compatible(&json!({"schema_version": 1})));
        assert!(!query.compatible(&json!({"schema_version": 2})));
        assert!(!query.compatible(&json!({"schema_version": 1, "wasm_api_version": "0.8.0"})));
    }

    #[tokio::test]
    async fn bundled_packages_load_once() {
        let pool = crate::db::init_pool("sqlite::memory:").await.unwrap();
        let dir = tempfile::tempdir().unwrap();
        let package = dir.path().join("java");
        std::fs::create_dir(&package).unwrap();
        std::fs::write(package.join("manifest.json"), r#"{"name":"Java","version":"6.1.0","repository":"https://example.test/java","schema_version":1}"#).unwrap();
        std::fs::write(package.join("archive.tar.gz"), [0x1f, 0x8b, 0, 0]).unwrap();
        let path = dir.path().to_str().unwrap();
        assert_eq!(load_bundled(&pool, Some(path)).await, 1);
        assert_eq!(load_bundled(&pool, Some(path)).await, 0);
        let listed = latest(rows(&pool, None).await.unwrap(), &VersionQuery { max_schema_version: Some(1), ..Default::default() });
        assert_eq!(listed[0].id, "java");
    }

    #[test]
    fn ids_are_checked() {
        assert!(valid_id("java"));
        assert!(valid_id("elixir-snippets"));
        assert!(!valid_id("../etc"));
        assert!(!valid_id(""));
    }
}
