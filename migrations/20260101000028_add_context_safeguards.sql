-- Safeguards against a wrong claim spreading through every agent.

-- A decision a newer one replaced: no longer given to agents or found in
-- team memory, but kept so anyone can see what changed.
ALTER TABLE decisions ADD COLUMN superseded_by INTEGER;

-- Things a person marked wrong: an agent run, an agent step, or the
-- discussion summary. Flagged items are withheld from every agent until the
-- flag is lifted (or, for the summary, until it's rewritten with the fix).
CREATE TABLE IF NOT EXISTS context_flags (
    session_id TEXT NOT NULL,
    item_key TEXT NOT NULL,
    flagged_by TEXT NOT NULL,
    note TEXT,
    created_at TEXT NOT NULL,
    PRIMARY KEY (session_id, item_key)
);
