-- An invite link gets someone as far as asking; the session owner lets them in.
CREATE TABLE IF NOT EXISTS session_join_requests (
    session_id TEXT NOT NULL,
    user_id TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending',
    requested_at TEXT NOT NULL,
    PRIMARY KEY (session_id, user_id)
);
