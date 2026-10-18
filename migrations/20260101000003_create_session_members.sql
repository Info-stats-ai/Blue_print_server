CREATE TABLE IF NOT EXISTS session_members (
    session_id TEXT NOT NULL REFERENCES sessions(id),
    user_id TEXT NOT NULL REFERENCES users(id),
    role TEXT NOT NULL,
    joined_at TEXT NOT NULL,
    PRIMARY KEY (session_id, user_id)
);
