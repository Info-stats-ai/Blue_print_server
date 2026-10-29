-- Automatic checkpoints and the server's own agent write rows whose
-- author_id must reference a user. Without these accounts every checkpoint
-- and agent-authored plan failed its foreign key. The password hash isn't a
-- valid bcrypt hash, so nobody can log in as either.
INSERT OR IGNORE INTO users (id, email, password_hash, created_at) VALUES
    ('agent', 'agent@blueprint.invalid', '!disabled', strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    ('system:checkpoint', 'checkpoint@blueprint.invalid', '!disabled', strftime('%Y-%m-%dT%H:%M:%fZ', 'now'));
