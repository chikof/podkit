-- Audit log (roadmap A3): who did what, to which resource, from where. One row per
-- successful mutation, written by `audit::record`. Append-only: rows are never updated
-- (a trigger refuses it); they leave only through the per-team retention purge, or with
-- their team.
CREATE TABLE audit_events (
	id BIGINT PRIMARY KEY NOT NULL,
	team_id BIGINT NOT NULL REFERENCES teams(id) ON DELETE CASCADE,
	-- Who acted. `user` is a signed-in person; `token` an API token acting as the user
	-- who made it (both ids are kept); `system` is podkit itself (a push webhook, a
	-- schedule); `mcp` is the MCP server (roadmap A7). No foreign keys: the log must
	-- outlive a revoked token and a removed user.
	actor_kind TEXT NOT NULL CHECK (actor_kind IN ('user', 'token', 'system', 'mcp')),
	actor_user_id BIGINT,
	actor_token_id BIGINT,
	-- `app.deploy`, `env_var.set`, `ssh_key.rotate`, ...: `<target kind>.<verb>`.
	action TEXT NOT NULL,
	target_kind TEXT NOT NULL,
	target_id BIGINT,
	-- The project a resource lives in, so the log can link to it.
	project_id BIGINT,
	summary TEXT NOT NULL,
	-- What changed. Secrets are never put here; any that slip through are masked to
	-- `•••` by `audit::Diff`.
	diff JSONB NOT NULL DEFAULT '{}'::jsonb,
	ip TEXT,
	user_agent TEXT,
	created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_audit_events_team_created ON audit_events (team_id, created_at DESC, id DESC);
CREATE INDEX idx_audit_events_target ON audit_events (team_id, target_kind, target_id, created_at DESC);
-- The retention purge scans by age across teams.
CREATE INDEX idx_audit_events_created ON audit_events (created_at);

CREATE FUNCTION audit_events_refuse_update() RETURNS trigger AS $$
BEGIN
	RAISE EXCEPTION 'audit_events is append-only';
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER audit_events_append_only
	BEFORE UPDATE ON audit_events
	FOR EACH ROW EXECUTE FUNCTION audit_events_refuse_update();

-- How long a team keeps its audit events, in days.
ALTER TABLE teams ADD COLUMN audit_retention_days INTEGER NOT NULL DEFAULT 365
	CHECK (audit_retention_days BETWEEN 1 AND 3650);

-- `audit`: only `read` is ever asked (the log is written by the server, never through a
-- permission). The Owner role has it; Member does not.
INSERT INTO permissions (id, action, resource)
	SELECT (SELECT COALESCE(MAX(id), 0) FROM permissions) + 1, 'read', 'audit'
	ON CONFLICT DO NOTHING;
INSERT INTO role_permissions (role_id, permission_id)
	SELECT 1, id FROM permissions WHERE resource = 'audit' ON CONFLICT DO NOTHING;
