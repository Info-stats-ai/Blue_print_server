-- Knowing a session's id no longer lets someone join it: a new member needs
-- this secret too, which travels in invite links and emails.
ALTER TABLE sessions ADD COLUMN invite_token TEXT;
UPDATE sessions SET invite_token = lower(hex(randomblob(16))) WHERE invite_token IS NULL;
