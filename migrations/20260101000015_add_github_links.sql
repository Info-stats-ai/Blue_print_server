-- The GitHub repo ("owner/name") a session's work lands in, so the GitHub App
-- can comment on that repo's pull requests with the session's numbers.
ALTER TABLE sessions ADD COLUMN github_repo TEXT;

-- The comment the app posted on each pull request, so later pushes update it
-- instead of adding another.
CREATE TABLE IF NOT EXISTS pr_comments (
    repo TEXT NOT NULL,
    pr_number INTEGER NOT NULL,
    comment_id INTEGER NOT NULL,
    session_id TEXT NOT NULL,
    PRIMARY KEY (repo, pr_number)
);
