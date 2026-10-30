-- Every file changed in a session and who changed it, so the change list
-- and file-tree markers survive server restarts. One row per session.
CREATE TABLE IF NOT EXISTS session_changes (
    session_id TEXT PRIMARY KEY,
    changes TEXT NOT NULL,
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);
