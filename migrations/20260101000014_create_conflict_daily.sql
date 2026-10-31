-- Daily totals behind the "conflicts avoided" dashboard: one row per session,
-- day and kind (merge, divergence, lane_warning, agent_collision), so the
-- numbers outlive the in-memory session metrics and can be summed by week.
CREATE TABLE IF NOT EXISTS conflict_daily (
    session_id TEXT NOT NULL,
    day TEXT NOT NULL,
    kind TEXT NOT NULL,
    count INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (session_id, day, kind)
);
CREATE INDEX IF NOT EXISTS idx_conflict_daily_day ON conflict_daily(day);
