CREATE TABLE web_sessions (
    token_hash TEXT PRIMARY KEY,
    user_id BIGINT REFERENCES users(id) ON DELETE CASCADE,
    csrf_token TEXT NOT NULL,
    user_session_token TEXT,
    expires_at TIMESTAMPTZ NOT NULL DEFAULT now() + interval '14 days'
);
CREATE INDEX web_sessions_expiry ON web_sessions(expires_at);
CREATE TABLE sync_jobs (
    id BIGSERIAL PRIMARY KEY,
    plaid_item_id BIGINT NOT NULL REFERENCES plaid_items(id) ON DELETE CASCADE,
    request_token TEXT NOT NULL,
    attempts INTEGER NOT NULL DEFAULT 0,
    available_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    locked_at TIMESTAMPTZ,
    last_error TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX sync_jobs_item ON sync_jobs(plaid_item_id);
