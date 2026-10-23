-- A plan already records who proposed/approved a change (content, status,
-- reviewed_by). This adds the actual code snapshot it refers to, so
-- "approve" and "restore" operate on the real session state at that moment,
-- not just a free-text description of it — the native, no-GitHub-required
-- finalize/version layer: JSON-encoded {path: content}, null for plans
-- created before this migration.
ALTER TABLE execution_plans ADD COLUMN files TEXT;
