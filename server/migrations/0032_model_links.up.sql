-- Which models an application may use through the LLM gateway. One link is the default,
-- announced to the application as PODKIT_MODEL.
CREATE TABLE application_models (
	application_id BIGINT NOT NULL REFERENCES applications(id) ON DELETE CASCADE,
	model_id BIGINT NOT NULL REFERENCES models(id) ON DELETE RESTRICT,
	is_default BOOLEAN NOT NULL DEFAULT FALSE,
	created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
	PRIMARY KEY (application_id, model_id)
);
CREATE UNIQUE INDEX idx_application_models_one_default
	ON application_models (application_id) WHERE is_default;
CREATE INDEX idx_application_models_model_id ON application_models (model_id);

-- The gateway key of an application: issued with its first link, gone with its last.
-- Only the hash goes to the agent; the key itself is encrypted, so each deploy can hand it to the app.
CREATE TABLE gateway_keys (
	application_id BIGINT PRIMARY KEY REFERENCES applications(id) ON DELETE CASCADE,
	key_hash VARCHAR(64) NOT NULL UNIQUE,
	key BYTEA NOT NULL,
	created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Tokens served by the gateway, per key, model and hour. Kept after a link, a key or a
-- model is gone, so there are no foreign keys; application_id and team_id are filled in
-- when the key was still known.
CREATE TABLE model_usage (
	key_hash VARCHAR(64) NOT NULL,
	model_id BIGINT NOT NULL,
	hour TIMESTAMPTZ NOT NULL,
	application_id BIGINT,
	team_id BIGINT,
	requests BIGINT NOT NULL DEFAULT 0,
	input_tokens BIGINT NOT NULL DEFAULT 0,
	output_tokens BIGINT NOT NULL DEFAULT 0,
	PRIMARY KEY (key_hash, model_id, hour)
);
CREATE INDEX idx_model_usage_team_hour ON model_usage (team_id, hour);
