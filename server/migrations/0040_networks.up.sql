-- Networks: every environment has a private network on each server it runs
-- resources on, and users add networks that connect resources on purpose.
-- Podman's `podkit` network stays for system containers (Traefik, the gateway).
CREATE TABLE networks (
	id BIGINT PRIMARY KEY,
	team_id BIGINT NOT NULL REFERENCES teams(id) ON DELETE CASCADE,
	server_id BIGINT NOT NULL REFERENCES servers(id) ON DELETE CASCADE,
	name VARCHAR(60) NOT NULL,
	slug VARCHAR(60) NOT NULL,
	-- `environment` (made by podkit, one per environment and server), `user`
	-- (made or imported by a user) or `system`.
	kind VARCHAR(20) NOT NULL CHECK (kind IN ('environment', 'user', 'system')),
	environment_id BIGINT REFERENCES environments(id) ON DELETE CASCADE,
	-- The name Podman knows it by: `podkit-env-<environment id>`,
	-- `podkit-net-<id>`, or the imported network's own name.
	podman_name VARCHAR(128) NOT NULL,
	-- No outbound access from the network.
	internal BOOLEAN NOT NULL DEFAULT FALSE,
	-- Found on the server rather than created by podkit; never removed from Podman.
	imported BOOLEAN NOT NULL DEFAULT FALSE,
	created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
	UNIQUE (server_id, slug),
	UNIQUE (server_id, podman_name),
	CONSTRAINT networks_environment_kind CHECK ((kind = 'environment') = (environment_id IS NOT NULL))
);
CREATE UNIQUE INDEX idx_networks_environment ON networks (environment_id, server_id)
	WHERE environment_id IS NOT NULL;
CREATE INDEX idx_networks_team_id ON networks (team_id);

-- Which resources sit on which network, and the DNS aliases they answer to there.
CREATE TABLE resource_networks (
	network_id BIGINT NOT NULL REFERENCES networks(id) ON DELETE CASCADE,
	resource_kind VARCHAR(20) NOT NULL
		CHECK (resource_kind IN ('application', 'database', 'model', 'stack')),
	resource_id BIGINT NOT NULL,
	aliases TEXT[] NOT NULL DEFAULT '{}',
	created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
	PRIMARY KEY (network_id, resource_kind, resource_id)
);
CREATE INDEX idx_resource_networks_resource ON resource_networks (resource_kind, resource_id);

-- A resource takes its memberships with it.
CREATE FUNCTION delete_resource_networks() RETURNS trigger AS $$
BEGIN
	DELETE FROM resource_networks
	WHERE resource_kind = TG_ARGV[0] AND resource_id = OLD.id;
	RETURN OLD;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER applications_networks AFTER DELETE ON applications
	FOR EACH ROW EXECUTE FUNCTION delete_resource_networks('application');
CREATE TRIGGER databases_networks AFTER DELETE ON databases
	FOR EACH ROW EXECUTE FUNCTION delete_resource_networks('database');
CREATE TRIGGER models_networks AFTER DELETE ON models
	FOR EACH ROW EXECUTE FUNCTION delete_resource_networks('model');
CREATE TRIGGER stacks_networks AFTER DELETE ON stacks
	FOR EACH ROW EXECUTE FUNCTION delete_resource_networks('stack');

-- Backfill: one environment network per environment and server that has
-- resources, and every existing resource a member of it. Ids here are small
-- numbers; generated ids are snowflakes, so they never meet.
CREATE TEMP TABLE resource_homes ON COMMIT DROP AS
	SELECT 'application'::varchar AS kind, id, environment_id, server_id FROM applications
	UNION ALL SELECT 'database', id, environment_id, server_id FROM databases
	UNION ALL SELECT 'model', id, environment_id, server_id FROM models
	UNION ALL SELECT 'stack', id, environment_id, server_id FROM stacks;

INSERT INTO networks (id, team_id, server_id, name, slug, kind, environment_id, podman_name)
SELECT
	row_number() OVER (ORDER BY h.environment_id, h.server_id),
	p.team_id,
	h.server_id,
	left(e.name || ' (' || p.name || ')', 60),
	'env-' || h.environment_id,
	'environment',
	h.environment_id,
	'podkit-env-' || h.environment_id
FROM (SELECT DISTINCT environment_id, server_id FROM resource_homes) h
JOIN environments e ON e.id = h.environment_id
JOIN projects p ON p.id = e.project_id;

INSERT INTO resource_networks (network_id, resource_kind, resource_id)
SELECT n.id, h.kind, h.id
FROM resource_homes h
JOIN networks n ON n.environment_id = h.environment_id AND n.server_id = h.server_id;
