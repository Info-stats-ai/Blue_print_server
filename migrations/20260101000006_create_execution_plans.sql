CREATE TABLE IF NOT EXISTS execution_plans (
    id TEXT PRIMARY KEY NOT NULL,
    session_id TEXT NOT NULL REFERENCES sessions(id),
    author_id TEXT NOT NULL REFERENCES users(id),
    content TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending',
    reviewed_by TEXT REFERENCES users(id),
    review_note TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_execution_plans_session_id ON execution_plans(session_id);
