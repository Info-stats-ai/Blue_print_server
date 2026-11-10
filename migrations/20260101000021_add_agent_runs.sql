-- Agent runs a user chose to keep in team memory: the goal, how it ended, the
-- files it touched, a summary, and teammates' reviews. Runs shared only live
-- never reach this table.
CREATE TABLE IF NOT EXISTS agent_runs (
    session_id TEXT NOT NULL,
    user_id TEXT NOT NULL,
    run_id TEXT NOT NULL,
    agent TEXT NOT NULL,
    goal TEXT NOT NULL,
    status TEXT NOT NULL,
    files TEXT NOT NULL,
    summary TEXT,
    reviews TEXT NOT NULL,
    started_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    PRIMARY KEY (session_id, user_id, run_id)
);
CREATE INDEX IF NOT EXISTS agent_runs_recent ON agent_runs (session_id, updated_at);
