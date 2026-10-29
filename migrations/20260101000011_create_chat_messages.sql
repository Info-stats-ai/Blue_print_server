-- Every chat message ever posted in a session, so a long discussion
-- survives restarts and clients can page back through all of it. The
-- in-memory Hub history stays a small recent window for agent prompts.
CREATE TABLE IF NOT EXISTS chat_messages (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id TEXT NOT NULL,
    sender TEXT NOT NULL,
    author TEXT,
    text TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);
CREATE INDEX IF NOT EXISTS idx_chat_messages_session ON chat_messages(session_id, id);

-- The observer's rolling summary of the discussion: one row per session,
-- covering every message up to and including `through_id`.
CREATE TABLE IF NOT EXISTS session_digests (
    session_id TEXT PRIMARY KEY,
    content TEXT NOT NULL,
    through_id INTEGER NOT NULL,
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);

-- The current task board, stored whole as JSON: it is always read and
-- replaced as a unit, and its shape is owned by the server code.
CREATE TABLE IF NOT EXISTS session_tasks (
    session_id TEXT PRIMARY KEY,
    board TEXT NOT NULL,
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);
