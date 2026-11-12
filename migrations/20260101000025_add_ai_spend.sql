-- What each session (and the whole server, scope '*') has spent on the
-- shared AI key per day, so the daily caps survive a restart or redeploy.
CREATE TABLE IF NOT EXISTS ai_spend (
    scope TEXT NOT NULL,
    day TEXT NOT NULL,
    usd REAL NOT NULL DEFAULT 0,
    PRIMARY KEY (scope, day)
);
