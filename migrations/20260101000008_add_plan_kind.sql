-- Distinguishes a normal reference-doc proposal ('reference', the existing
-- behavior every plan created before this migration implicitly was) from a
-- 'triage' report: a structured breakdown of multiple files' current state
-- against reference.md and the chat, flagging which ones need a human's
-- judgment before committing. Approving a 'reference' plan publishes its
-- content as the live blueprint (see handlers::approve_plan); approving a
-- 'triage' plan deliberately does not, since its content is a report about
-- many files, not a document meant to replace the reference doc itself.
ALTER TABLE execution_plans ADD COLUMN kind TEXT NOT NULL DEFAULT 'reference';
