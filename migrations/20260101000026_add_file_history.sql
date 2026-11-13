-- Built-in version history: every version of every file a session has
-- shared, with who made it. File contents are stored once per distinct
-- content (by hash), so a file saved back to an earlier state costs nothing.
CREATE TABLE IF NOT EXISTS file_blobs (
    hash TEXT PRIMARY KEY,
    content TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS file_versions (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id TEXT NOT NULL,
    path TEXT NOT NULL,
    hash TEXT NOT NULL,
    user_id TEXT NOT NULL,
    origin TEXT,
    added INTEGER NOT NULL DEFAULT 0,
    removed INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS file_versions_by_path ON file_versions (session_id, path, id);
CREATE INDEX IF NOT EXISTS file_versions_by_hash ON file_versions (hash);
