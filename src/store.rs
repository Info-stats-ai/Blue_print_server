//! Encrypted, content-addressed storage for file versions.
//!
//! A file is cut into chunks at content-defined boundaries: FastCDC's Gear
//! rolling hash picks a cut, which then moves to the end of that line. An edit
//! therefore changes only the chunks it touches, and every other chunk is the
//! same bytes as before. Each chunk is stored once per session, compressed with
//! zstd and encrypted with that session's own key (XChaCha20-Poly1305). A
//! version is just the list of its chunk ids, so any version reads back in one
//! query with no chain of patches to replay.
//!
//! Chunk ids are BLAKE3 hashes keyed with the session's key, so the database
//! never shows whether two sessions hold the same code, and nobody holding the
//! database can test a guess against it. Session keys are stored wrapped by the
//! server's master key; deleting a session's key makes all of its chunks
//! unreadable at once, backups included.

use chacha20poly1305::aead::{Aead, AeadCore, KeyInit, OsRng, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, OnceLock};

/// Chunks are never cut shorter than this, except at the end of a file.
pub const MIN_CHUNK: usize = 1024;
/// The size cuts aim for.
pub const AVG_CHUNK: usize = 4096;
/// The longest a chunk gets, for a line that never ends (minified code).
pub const MAX_CHUNK: usize = 32 * 1024;
const ID_LEN: usize = 32;
const NONCE_LEN: usize = 24;
const FORMAT: u8 = 1;
const COMPRESSED: u8 = 1;
const ZSTD_LEVEL: i32 = 3;

type Result<T> = std::result::Result<T, sqlx::Error>;

fn fail(message: impl Into<String>) -> sqlx::Error {
    sqlx::Error::Protocol(message.into())
}

// --- Chunking ---------------------------------------------------------------

/// The Gear table, generated rather than pasted so every copy of this
/// chunker (the app has one too) gets the same 256 numbers.
fn gear() -> &'static [u64; 256] {
    static GEAR: OnceLock<[u64; 256]> = OnceLock::new();
    GEAR.get_or_init(|| {
        let mut state: u64 = 0x1D0E_C0DE_5EED_2026;
        let mut table = [0u64; 256];
        for entry in &mut table {
            // splitmix64
            state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = state;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            *entry = z ^ (z >> 31);
        }
        table
    })
}

// Normalized chunking (FastCDC): a harder mask before the target size and an
// easier one after it pulls chunk sizes towards `AVG_CHUNK`. The masks use the
// hash's high bits, which depend on the last 64 bytes rather than the last few.
const MASK_HARD: u64 = !0u64 << (64 - 14);
const MASK_EASY: u64 = !0u64 << (64 - 10);

/// Where the first chunk of `data` ends.
fn next_cut(data: &[u8]) -> usize {
    let len = data.len();
    if len <= MIN_CHUNK {
        return len;
    }
    let gear = gear();
    let limit = len.min(MAX_CHUNK);
    let mut hash = 0u64;
    let mut want_line_end = false;
    for (offset, &byte) in data.iter().enumerate().take(limit).skip(MIN_CHUNK) {
        if !want_line_end {
            hash = (hash << 1).wrapping_add(gear[byte as usize]);
            let mask = if offset < AVG_CHUNK { MASK_HARD } else { MASK_EASY };
            want_line_end = hash & mask == 0;
        }
        if want_line_end && byte == b'\n' {
            return offset + 1;
        }
    }
    if limit == len {
        return len;
    }
    // No cut by `MAX_CHUNK`: end after the chunk's last line, or, with no line
    // break at all, at the last character boundary.
    match data[MIN_CHUNK..limit].iter().rposition(|&byte| byte == b'\n') {
        Some(position) => MIN_CHUNK + position + 1,
        None => {
            let mut cut = limit;
            while cut > MIN_CHUNK && data[cut] & 0xC0 == 0x80 {
                cut -= 1;
            }
            cut
        }
    }
}

/// `text` cut into chunks. Each chunk is whole lines (but for a line longer
/// than `MAX_CHUNK`), so each is valid text on its own.
pub fn chunk_text(text: &str) -> Vec<&str> {
    let bytes = text.as_bytes();
    let mut chunks = Vec::new();
    let mut start = 0;
    while start < bytes.len() {
        let end = start + next_cut(&bytes[start..]);
        chunks.push(&text[start..end]);
        start = end;
    }
    chunks
}

// --- Keys -------------------------------------------------------------------

static MASTER: OnceLock<[u8; 32]> = OnceLock::new();

const MASTER_ENV: &str = "FILE_ENCRYPTION_KEY";
const MASTER_FILE: &str = "file_encryption.key";

fn master() -> &'static [u8; 32] {
    // Tests and in-memory databases get a fresh key for the process.
    MASTER.get_or_init(random_key)
}

fn random_key() -> [u8; 32] {
    XChaCha20Poly1305::generate_key(&mut OsRng).into()
}

fn parse_hex_key(text: &str) -> Option<[u8; 32]> {
    let text = text.trim();
    if text.len() != 64 {
        return None;
    }
    let mut key = [0u8; 32];
    for (index, byte) in key.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&text[index * 2..index * 2 + 2], 16).ok()?;
    }
    Some(key)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// The folder the database file is in, for a file-backed SQLite URL.
fn database_dir(database_url: &str) -> Option<std::path::PathBuf> {
    let path = database_url.strip_prefix("sqlite://").or_else(|| database_url.strip_prefix("sqlite:"))?;
    let path = path.split('?').next()?;
    if path.is_empty() || path.starts_with(":memory:") {
        return None;
    }
    Some(std::path::Path::new(path).parent()?.to_path_buf())
}

/// Sets up the master key, before anything is stored. `FILE_ENCRYPTION_KEY`
/// (64 hex characters) is the key. Without it, a key file next to the
/// database is used, made once; when the variable is set later, every
/// session key is re-wrapped with it and the file is deleted.
pub async fn init_master_key(pool: &sqlx::SqlitePool, database_url: &str) -> anyhow::Result<&'static str> {
    let from_env = std::env::var(MASTER_ENV).ok().filter(|value| !value.trim().is_empty());
    let key_file = database_dir(database_url).map(|dir| dir.join(MASTER_FILE));
    let file_key = match &key_file {
        Some(path) if path.exists() => Some(
            parse_hex_key(&std::fs::read_to_string(path)?)
                .ok_or_else(|| anyhow::anyhow!("{} isn't a 64-character hex key", path.display()))?,
        ),
        _ => None,
    };
    let (key, source) = match (&from_env, file_key) {
        (Some(value), file_key) => {
            let key = parse_hex_key(value).ok_or_else(|| anyhow::anyhow!("{MASTER_ENV} must be 64 hex characters"))?;
            if let (Some(old), Some(path)) = (file_key, &key_file) {
                let moved = rewrap_all(pool, &old, &key).await?;
                std::fs::remove_file(path)?;
                println!("store: moved {moved} session key(s) to {MASTER_ENV} and removed {}", path.display());
            }
            (key, "from FILE_ENCRYPTION_KEY")
        }
        (None, Some(key)) => (key, "from the key file next to the database (set FILE_ENCRYPTION_KEY to move it out)"),
        (None, None) => match &key_file {
            Some(path) => {
                let key = random_key();
                write_private_file(path, &hex(&key))?;
                (key, "new key file next to the database (set FILE_ENCRYPTION_KEY to move it out)")
            }
            None => (random_key(), "for this process only (in-memory database)"),
        },
    };
    MASTER.set(key).map_err(|_| anyhow::anyhow!("the master key was already set"))?;
    Ok(source)
}

fn write_private_file(path: &std::path::Path, text: &str) -> std::io::Result<()> {
    use std::io::Write as _;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    options.open(path)?.write_all(text.as_bytes())
}

async fn rewrap_all(pool: &sqlx::SqlitePool, old: &[u8; 32], new: &[u8; 32]) -> anyhow::Result<usize> {
    let rows: Vec<(String, Vec<u8>)> = sqlx::query_as("SELECT session_id, wrapped FROM session_keys").fetch_all(pool).await?;
    let mut tx = pool.begin().await?;
    for (session_id, wrapped) in &rows {
        let key = unwrap_key(old, session_id, wrapped)?;
        sqlx::query("UPDATE session_keys SET wrapped = ? WHERE session_id = ?")
            .bind(wrap_key(new, session_id, &key))
            .bind(session_id)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(rows.len())
}

fn wrap_key(master: &[u8; 32], session_id: &str, key: &[u8; 32]) -> Vec<u8> {
    let cipher = XChaCha20Poly1305::new(master.into());
    let nonce = XChaCha20Poly1305::generate_nonce(&mut OsRng);
    let sealed = cipher
        .encrypt(&nonce, Payload { msg: key, aad: session_id.as_bytes() })
        .expect("encrypting 32 bytes can't fail");
    [&[FORMAT][..], nonce.as_slice(), &sealed].concat()
}

fn unwrap_key(master: &[u8; 32], session_id: &str, wrapped: &[u8]) -> Result<[u8; 32]> {
    if wrapped.len() < 1 + NONCE_LEN || wrapped[0] != FORMAT {
        return Err(fail("unknown session key format"));
    }
    let cipher = XChaCha20Poly1305::new(master.into());
    let nonce = XNonce::from_slice(&wrapped[1..1 + NONCE_LEN]);
    let key = cipher
        .decrypt(nonce, Payload { msg: &wrapped[1 + NONCE_LEN..], aad: session_id.as_bytes() })
        .map_err(|_| fail("a session key couldn't be unlocked with this server's master key"))?;
    key.try_into().map_err(|_| fail("a session key has the wrong length"))
}

/// What a session's key opens: chunk ids and chunk encryption, each from its
/// own derived key.
pub struct SessionKeys {
    id_key: [u8; 32],
    cipher: XChaCha20Poly1305,
}

impl SessionKeys {
    fn new(key: &[u8; 32]) -> Self {
        let id_key = blake3::derive_key("i-ide 2026 chunk id", key);
        let encryption = blake3::derive_key("i-ide 2026 chunk encryption", key);
        Self { id_key, cipher: XChaCha20Poly1305::new((&encryption).into()) }
    }

    fn chunk_id(&self, chunk: &[u8]) -> [u8; ID_LEN] {
        *blake3::keyed_hash(&self.id_key, chunk).as_bytes()
    }

    /// The id a whole file's text goes by in this session.
    pub fn content_id(&self, content: &str) -> String {
        let mut hasher = blake3::Hasher::new_keyed(&self.id_key);
        hasher.update(b"file:");
        hasher.update(content.as_bytes());
        hasher.finalize().to_hex().to_string()
    }

    fn seal(&self, session_id: &str, id: &[u8; ID_LEN], plain: &[u8]) -> Vec<u8> {
        let compressed = zstd::bulk::compress(plain, ZSTD_LEVEL).ok().filter(|compressed| compressed.len() < plain.len());
        let (flags, body) = match &compressed {
            Some(compressed) => (COMPRESSED, compressed.as_slice()),
            None => (0, plain),
        };
        let nonce = XChaCha20Poly1305::generate_nonce(&mut OsRng);
        let aad = [session_id.as_bytes(), id].concat();
        let sealed = self.cipher.encrypt(&nonce, Payload { msg: body, aad: &aad }).expect("encryption can't fail");
        [&[FORMAT, flags][..], nonce.as_slice(), &sealed].concat()
    }

    fn open(&self, session_id: &str, id: &[u8], stored: &[u8], size: usize) -> Result<Vec<u8>> {
        if stored.len() < 2 + NONCE_LEN || stored[0] != FORMAT {
            return Err(fail("unknown chunk format"));
        }
        let nonce = XNonce::from_slice(&stored[2..2 + NONCE_LEN]);
        let aad = [session_id.as_bytes(), id].concat();
        let body = self
            .cipher
            .decrypt(nonce, Payload { msg: &stored[2 + NONCE_LEN..], aad: &aad })
            .map_err(|_| fail("a stored chunk failed its integrity check"))?;
        if stored[1] & COMPRESSED == 0 {
            return Ok(body);
        }
        zstd::bulk::decompress(&body, size).map_err(|error| fail(format!("a stored chunk didn't decompress: {error}")))
    }
}

/// Unwrapped keys, by their wrapped bytes: unwrapping is done once per key,
/// and a key from one database can never be used for another.
fn unwrapped_cache() -> &'static Mutex<HashMap<Vec<u8>, Arc<SessionKeys>>> {
    static CACHE: OnceLock<Mutex<HashMap<Vec<u8>, Arc<SessionKeys>>>> = OnceLock::new();
    CACHE.get_or_init(Default::default)
}

const CACHED_KEYS: usize = 4096;

/// The session's keys; made the first time something of the session is
/// stored when `create` is set.
pub async fn session_keys(conn: &mut sqlx::SqliteConnection, session_id: &str, create: bool) -> Result<Option<Arc<SessionKeys>>> {
    let mut wrapped: Option<Vec<u8>> = sqlx::query_scalar("SELECT wrapped FROM session_keys WHERE session_id = ?")
        .bind(session_id)
        .fetch_optional(&mut *conn)
        .await?;
    if wrapped.is_none() && create {
        sqlx::query("INSERT OR IGNORE INTO session_keys (session_id, wrapped, created_at) VALUES (?, ?, ?)")
            .bind(session_id)
            .bind(wrap_key(master(), session_id, &random_key()))
            .bind(chrono::Utc::now().to_rfc3339())
            .execute(&mut *conn)
            .await?;
        wrapped = sqlx::query_scalar("SELECT wrapped FROM session_keys WHERE session_id = ?")
            .bind(session_id)
            .fetch_optional(&mut *conn)
            .await?;
    }
    let Some(wrapped) = wrapped else {
        return Ok(None);
    };
    if let Some(keys) = unwrapped_cache().lock().expect("key cache").get(&wrapped) {
        return Ok(Some(keys.clone()));
    }
    let keys = Arc::new(SessionKeys::new(&unwrap_key(master(), session_id, &wrapped)?));
    let mut cache = unwrapped_cache().lock().expect("key cache");
    if cache.len() >= CACHED_KEYS {
        cache.clear();
    }
    cache.insert(wrapped, keys.clone());
    Ok(Some(keys))
}

// --- Storing and reading ----------------------------------------------------

/// A file's text as stored: its chunk list and what it cost.
pub struct Stored {
    /// The chunk ids, 32 bytes each, in order.
    pub chunks: Vec<u8>,
    pub content_id: String,
    pub size: i64,
    /// Bytes of chunks that weren't stored before.
    pub new_bytes: i64,
}

/// Stores `content` for `session_id`: only chunks the session doesn't have yet
/// are compressed, encrypted and written.
pub async fn put(conn: &mut sqlx::SqliteConnection, session_id: &str, content: &str) -> Result<Stored> {
    let keys = session_keys(conn, session_id, true).await?.ok_or_else(|| fail("no session key"))?;
    let pieces: Vec<(&str, [u8; ID_LEN])> =
        chunk_text(content).into_iter().map(|piece| (piece, keys.chunk_id(piece.as_bytes()))).collect();
    let mut existing: HashSet<[u8; ID_LEN]> = HashSet::new();
    let unique: Vec<[u8; ID_LEN]> = pieces.iter().map(|(_, id)| *id).collect::<HashSet<_>>().into_iter().collect();
    for batch in unique.chunks(400) {
        let marks = vec!["?"; batch.len()].join(",");
        let sql = format!("SELECT id FROM chunks WHERE session_id = ? AND id IN ({marks})");
        let mut query = sqlx::query_scalar::<_, Vec<u8>>(&sql).bind(session_id);
        for id in batch {
            query = query.bind(id.as_slice());
        }
        for id in query.fetch_all(&mut *conn).await? {
            if let Ok(id) = <[u8; ID_LEN]>::try_from(id.as_slice()) {
                existing.insert(id);
            }
        }
    }
    let mut new_bytes = 0i64;
    for (piece, id) in &pieces {
        if !existing.insert(*id) {
            continue;
        }
        let sealed = keys.seal(session_id, id, piece.as_bytes());
        new_bytes += sealed.len() as i64;
        sqlx::query("INSERT OR IGNORE INTO chunks (session_id, id, data, size) VALUES (?, ?, ?, ?)")
            .bind(session_id)
            .bind(id.as_slice())
            .bind(sealed)
            .bind(piece.len() as i64)
            .execute(&mut *conn)
            .await?;
    }
    Ok(Stored {
        chunks: pieces.iter().flat_map(|(_, id)| *id).collect(),
        content_id: keys.content_id(content),
        size: content.len() as i64,
        new_bytes,
    })
}

/// The text stored as `chunks`, checked against `content_id`: a missing,
/// damaged or reordered chunk is an error, never a wrong file.
pub async fn get(conn: &mut sqlx::SqliteConnection, session_id: &str, chunks: &[u8], content_id: &str) -> Result<String> {
    let keys = session_keys(conn, session_id, false).await?.ok_or_else(|| fail("this session's stored files were deleted"))?;
    if chunks.len() % ID_LEN != 0 {
        return Err(fail("a version's chunk list is damaged"));
    }
    let ids: Vec<&[u8]> = chunks.chunks(ID_LEN).collect();
    let unique: Vec<&[u8]> = ids.iter().copied().collect::<HashSet<_>>().into_iter().collect();
    let mut found: HashMap<Vec<u8>, Vec<u8>> = HashMap::with_capacity(unique.len());
    for batch in unique.chunks(400) {
        let marks = vec!["?"; batch.len()].join(",");
        let sql = format!("SELECT id, data, size FROM chunks WHERE session_id = ? AND id IN ({marks})");
        let mut query = sqlx::query_as::<_, (Vec<u8>, Vec<u8>, i64)>(&sql).bind(session_id);
        for id in batch {
            query = query.bind(*id);
        }
        for (id, data, size) in query.fetch_all(&mut *conn).await? {
            let plain = keys.open(session_id, &id, &data, size.max(0) as usize)?;
            found.insert(id, plain);
        }
    }
    let mut bytes = Vec::with_capacity(found.values().map(Vec::len).sum());
    for id in &ids {
        bytes.extend_from_slice(found.get(*id).ok_or_else(|| fail("a version's chunk is missing"))?);
    }
    let text = String::from_utf8(bytes).map_err(|_| fail("a stored version isn't valid text"))?;
    if keys.content_id(&text) != content_id {
        return Err(fail("a stored version didn't match its fingerprint"));
    }
    Ok(text)
}

/// The id `content` would be stored under in `session_id`.
pub async fn content_id(conn: &mut sqlx::SqliteConnection, session_id: &str, content: &str) -> Result<String> {
    let keys = session_keys(conn, session_id, true).await?.ok_or_else(|| fail("no session key"))?;
    Ok(keys.content_id(content))
}

/// Deletes the session's chunks no version uses any more.
pub async fn collect_garbage(pool: &sqlx::SqlitePool, session_id: &str) -> Result<u64> {
    let lists: Vec<Option<Vec<u8>>> = sqlx::query_scalar("SELECT chunks FROM file_versions WHERE session_id = ?")
        .bind(session_id)
        .fetch_all(pool)
        .await?;
    let pinned = crate::history::snapshot_chunk_ids(pool, session_id).await?;
    let used: HashSet<&[u8]> = lists
        .iter()
        .flatten()
        .flat_map(|list| list.chunks(ID_LEN))
        .chain(pinned.iter().map(Vec::as_slice))
        .collect();
    let stored: Vec<Vec<u8>> = sqlx::query_scalar("SELECT id FROM chunks WHERE session_id = ?").bind(session_id).fetch_all(pool).await?;
    let unused: Vec<&Vec<u8>> = stored.iter().filter(|id| !used.contains(id.as_slice())).collect();
    let mut removed = 0;
    let mut tx = pool.begin().await?;
    for id in unused {
        removed += sqlx::query("DELETE FROM chunks WHERE session_id = ? AND id = ?")
            .bind(session_id)
            .bind(id.as_slice())
            .execute(&mut *tx)
            .await?
            .rows_affected();
    }
    tx.commit().await?;
    Ok(removed)
}

/// Destroys the session's key and chunks. Anything of the session left in a
/// backup can no longer be read.
pub async fn shred_session(conn: &mut sqlx::SqliteConnection, session_id: &str) -> Result<()> {
    sqlx::query("DELETE FROM session_keys WHERE session_id = ?").bind(session_id).execute(&mut *conn).await?;
    sqlx::query("DELETE FROM chunks WHERE session_id = ?").bind(session_id).execute(&mut *conn).await?;
    Ok(())
}

/// Sessions whose versions were pruned since the last clean-up.
fn dirty() -> &'static Mutex<HashSet<String>> {
    static DIRTY: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    DIRTY.get_or_init(Default::default)
}

pub fn mark_dirty(session_id: &str) {
    dirty().lock().expect("dirty set").insert(session_id.to_string());
}

/// Every few minutes, removes chunks that pruned versions left unused.
pub fn spawn_collector(pool: sqlx::SqlitePool) {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(300)).await;
            let sessions: Vec<String> = dirty().lock().expect("dirty set").drain().collect();
            for session_id in sessions {
                match collect_garbage(&pool, &session_id).await {
                    Ok(0) => {}
                    Ok(removed) => println!("store: removed {removed} unused chunk(s) of session {session_id}"),
                    Err(error) => eprintln!("store: clean-up of session {session_id} failed: {error}"),
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_code(lines: usize, seed: u64) -> String {
        let mut state = seed;
        let mut text = String::new();
        for line in 0..lines {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            let width = (state >> 58) as usize;
            text.push_str(&format!("    let value_{line} = compute({}, {});{}\n", state % 997, state % 31, " ".repeat(width)));
        }
        text
    }

    #[test]
    fn chunks_cover_the_text_exactly_and_end_at_line_ends() {
        for text in [String::new(), "short".to_string(), sample_code(3000, 7), "x".repeat(100_000)] {
            let chunks = chunk_text(&text);
            assert_eq!(chunks.concat(), text);
            for chunk in &chunks[..chunks.len().saturating_sub(1)] {
                assert!(chunk.len() <= MAX_CHUNK);
                assert!(chunk.ends_with('\n') || !text.contains('\n'), "cut mid-line");
            }
        }
        // A line that never ends is cut at character boundaries.
        let wide = "é".repeat(40_000);
        assert_eq!(chunk_text(&wide).concat(), wide);
    }

    #[test]
    fn chunk_sizes_aim_for_the_target() {
        let text = sample_code(20_000, 11);
        let chunks = chunk_text(&text);
        let average = text.len() / chunks.len();
        assert!((AVG_CHUNK / 2..AVG_CHUNK * 3).contains(&average), "average chunk {average} bytes");
    }

    #[test]
    fn an_edit_changes_only_the_chunks_around_it() {
        let before = sample_code(4000, 3);
        let middle = before.len() / 2;
        let line_start = before[..middle].rfind('\n').unwrap() + 1;
        let after = format!("{}    // a new comment\n{}", &before[..line_start], &before[line_start..]);
        let old: HashSet<&str> = chunk_text(&before).into_iter().collect();
        let new = chunk_text(&after);
        let changed = new.iter().filter(|chunk| !old.contains(*chunk)).count();
        assert!(changed <= 2, "{changed} of {} chunks changed for a one-line insert", new.len());
    }

    #[test]
    fn this_chunker_matches_the_apps() {
        // The same text must cut at the same places here and in the app
        // (crates/blueprint_panel/src/chunks.rs); both test this vector.
        let text = sample_code(2000, 2026);
        let ends: Vec<usize> = chunk_text(&text).iter().scan(0, |at, chunk| {
            *at += chunk.len();
            Some(*at)
        }).collect();
        assert_eq!(*ends.last().unwrap(), text.len());
        let fingerprint = blake3::hash(format!("{ends:?}").as_bytes()).to_hex()[..16].to_string();
        assert_eq!(fingerprint, GOLDEN, "chunk boundaries changed: update both chunkers together");
    }

    const GOLDEN: &str = "b380f3fbcf134168";

    async fn pool() -> sqlx::SqlitePool {
        crate::db::init_pool("sqlite::memory:").await.unwrap()
    }

    #[tokio::test]
    async fn versions_read_back_byte_for_byte_and_share_chunks() {
        let pool = pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let v1 = sample_code(3000, 5);
        let first = put(&mut conn, "s", &v1).await.unwrap();
        let v2 = v1.replacen("compute(", "compute_fast(", 1);
        let second = put(&mut conn, "s", &v2).await.unwrap();
        assert!(second.new_bytes * 10 < first.new_bytes, "a one-word edit stores a sliver: {} vs {}", second.new_bytes, first.new_bytes);
        assert_eq!(get(&mut conn, "s", &first.chunks, &first.content_id).await.unwrap(), v1);
        assert_eq!(get(&mut conn, "s", &second.chunks, &second.content_id).await.unwrap(), v2);
        // Compressed and encrypted: the text isn't in the stored bytes.
        let stored: Vec<Vec<u8>> = sqlx::query_scalar("SELECT data FROM chunks").fetch_all(&pool).await.unwrap();
        let all: Vec<u8> = stored.concat();
        assert!(!all.windows(12).any(|window| window == b"let value_10"));
        assert!(all.len() * 2 < v1.len(), "stored {} bytes for {} of text", all.len(), v1.len());
    }

    #[tokio::test]
    async fn damage_is_caught_not_returned() {
        let pool = pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let text = sample_code(500, 9);
        let stored = put(&mut conn, "s", &text).await.unwrap();
        // Chunks in the wrong order.
        let mut swapped = stored.chunks.clone();
        if swapped.len() >= 2 * ID_LEN {
            let (a, b) = swapped.split_at_mut(ID_LEN);
            a.swap_with_slice(&mut b[..ID_LEN]);
            assert!(get(&mut conn, "s", &swapped, &stored.content_id).await.is_err());
        }
        // A flipped bit in a stored chunk.
        sqlx::query("UPDATE chunks SET data = substr(data, 1, 30) || x'00' || substr(data, 32)").execute(&pool).await.unwrap();
        assert!(get(&mut conn, "s", &stored.chunks, &stored.content_id).await.is_err());
    }

    #[tokio::test]
    async fn sessions_never_share_or_reveal_chunks() {
        let pool = pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let text = sample_code(200, 1);
        let a = put(&mut conn, "a", &text).await.unwrap();
        let b = put(&mut conn, "b", &text).await.unwrap();
        assert_ne!(a.chunks, b.chunks, "the same code has different ids in different sessions");
        assert_ne!(a.content_id, b.content_id);
        // One session's chunks can't be read with another's key.
        assert!(get(&mut conn, "b", &a.chunks, &a.content_id).await.is_err());
    }

    #[tokio::test]
    async fn shredding_a_session_makes_it_unreadable() {
        let pool = pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let stored = put(&mut conn, "s", "fn main() {}\n").await.unwrap();
        shred_session(&mut conn, "s").await.unwrap();
        assert!(get(&mut conn, "s", &stored.chunks, &stored.content_id).await.is_err());
        let (left,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM chunks").fetch_one(&pool).await.unwrap();
        assert_eq!(left, 0);
    }

    #[tokio::test]
    async fn a_key_file_moves_to_the_environment_key() {
        let old = random_key();
        let new = random_key();
        let pool = pool().await;
        let wrapped = wrap_key(&old, "s", &[7u8; 32]);
        sqlx::query("INSERT INTO session_keys (session_id, wrapped, created_at) VALUES ('s', ?, 'now')")
            .bind(&wrapped)
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(rewrap_all(&pool, &old, &new).await.unwrap(), 1);
        let moved: Vec<u8> = sqlx::query_scalar("SELECT wrapped FROM session_keys").fetch_one(&pool).await.unwrap();
        assert_eq!(unwrap_key(&new, "s", &moved).unwrap(), [7u8; 32]);
        assert!(unwrap_key(&old, "s", &moved).is_err());
        assert!(unwrap_key(&new, "other", &moved).is_err(), "a key only opens for its own session");
    }
}
