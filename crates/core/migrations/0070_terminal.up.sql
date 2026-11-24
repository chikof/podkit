-- Web terminal (roadmap B5): a shell in a running container, over a WebSocket.
--
-- `terminal`: only `create` is ever asked ("open a terminal"). Owners hold it; Member
-- does not. A team that wants another role to have it grants it in the role editor.
INSERT INTO permissions (id, action, resource)
	SELECT (SELECT COALESCE(MAX(id), 0) FROM permissions) + 1, 'create', 'terminal'
	ON CONFLICT DO NOTHING;
INSERT INTO role_permissions (role_id, permission_id)
	SELECT 1, id FROM permissions WHERE resource = 'terminal' ON CONFLICT DO NOTHING;

-- Per-team option: keep what the terminal printed, so a session can be read back later.
-- Off by default. Keystrokes are never recorded; the transcript is the output only.
ALTER TABLE teams ADD COLUMN terminal_record BOOLEAN NOT NULL DEFAULT FALSE;

-- One row per opened terminal. `transcript` is filled only when the team records and is
-- capped by the server; `transcript_truncated` says the cap was hit. The start and end are
-- also in the audit log (`terminal.start`, `terminal.end`), which carries this row's id.
CREATE TABLE terminal_sessions (
	id BIGINT PRIMARY KEY NOT NULL,
	team_id BIGINT NOT NULL REFERENCES teams(id) ON DELETE CASCADE,
	user_id BIGINT NOT NULL,
	-- application, database or stack.
	target_kind TEXT NOT NULL,
	target_id BIGINT NOT NULL,
	-- The service of a stack, else NULL.
	service TEXT,
	started_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
	ended_at TIMESTAMPTZ,
	-- `closed` (the browser left), `exited` (the shell ended), `idle` or `error`.
	end_reason TEXT,
	transcript BYTEA,
	transcript_truncated BOOLEAN NOT NULL DEFAULT FALSE
);

CREATE INDEX idx_terminal_sessions_team ON terminal_sessions (team_id, started_at DESC);
