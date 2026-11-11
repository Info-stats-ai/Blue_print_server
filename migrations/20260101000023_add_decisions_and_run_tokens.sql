-- Decisions a team pinned from its chat. Every agent in the session is given
-- them, so they're kept like the chat they came from.
CREATE TABLE IF NOT EXISTS decisions (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id TEXT NOT NULL,
    text TEXT NOT NULL,
    pinned_by TEXT NOT NULL,
    created_at TEXT NOT NULL,
    removed INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS decisions_session ON decisions (session_id, id);

-- Tokens an agent run used, when the person shares it.
ALTER TABLE agent_runs ADD COLUMN tokens INTEGER;

-- Embeddings for team memory search, cached per item and model.
CREATE TABLE IF NOT EXISTS memory_embeddings (
    item_key TEXT NOT NULL,
    model TEXT NOT NULL,
    vector BLOB NOT NULL,
    PRIMARY KEY (item_key, model)
);
