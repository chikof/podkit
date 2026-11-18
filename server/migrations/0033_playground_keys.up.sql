-- The gateway key the web UI's playground chats with, one per model. Like an application's
-- key it is stored encrypted and never returned; only its hash goes to the agent. It can use
-- exactly one model, and goes with it.
CREATE TABLE playground_keys (
	model_id BIGINT PRIMARY KEY REFERENCES models(id) ON DELETE CASCADE,
	key_hash VARCHAR(64) NOT NULL UNIQUE,
	key BYTEA NOT NULL,
	created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
