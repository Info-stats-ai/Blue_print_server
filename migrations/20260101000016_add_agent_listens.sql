-- Whether the team agent follows the session's chat and speaks up on its own
-- (see listen.rs), rather than only when someone writes "@agent". Off by
-- default: it spends the shared key.
ALTER TABLE sessions ADD COLUMN agent_listens INTEGER NOT NULL DEFAULT 0;
