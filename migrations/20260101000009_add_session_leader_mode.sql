-- Whether this session has a designated leader (the creator, via owner_id)
-- with final say on sharing/reference-approval/triage, or is "flat" — every
-- peer has equal authority. Chosen once at creation; defaults to true so
-- every session created before this column existed keeps behaving exactly
-- as it always did (owner-gated).
ALTER TABLE sessions ADD COLUMN has_leader BOOLEAN NOT NULL DEFAULT 1;
