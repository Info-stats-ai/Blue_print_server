-- Version history per git branch: a teammate on another branch keeps their
-- own versions, so restoring a version never brings in another branch's code.
-- Versions from before this are the main lane's ("").
ALTER TABLE file_versions ADD COLUMN branch TEXT NOT NULL DEFAULT '';
CREATE INDEX IF NOT EXISTS file_versions_by_branch_path ON file_versions (session_id, branch, path, id);
