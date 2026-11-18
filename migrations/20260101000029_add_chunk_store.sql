-- Encrypted, content-addressed storage for file versions (see src/store.rs).
-- Each session has its own key, kept wrapped by the server's master key.
CREATE TABLE IF NOT EXISTS session_keys (
    session_id TEXT PRIMARY KEY,
    wrapped BLOB NOT NULL,
    created_at TEXT NOT NULL
);
-- A chunk of file text: compressed, then encrypted with the session's key.
-- `id` is a keyed hash of the text, so it says nothing across sessions.
CREATE TABLE IF NOT EXISTS chunks (
    session_id TEXT NOT NULL,
    id BLOB NOT NULL,
    data BLOB NOT NULL,
    size INTEGER NOT NULL,
    PRIMARY KEY (session_id, id)
) WITHOUT ROWID;
-- A version is its chunk ids in order (32 bytes each). Versions from before
-- this still point at `file_blobs` until they're moved over at startup.
ALTER TABLE file_versions ADD COLUMN chunks BLOB;
ALTER TABLE file_versions ADD COLUMN size INTEGER;
