-- Which run a remembered step belongs to, so a restored run shows its steps.
ALTER TABLE agent_steps ADD COLUMN run_id TEXT;
