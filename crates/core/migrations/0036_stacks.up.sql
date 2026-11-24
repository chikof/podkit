-- Compose stacks: a docker-compose file translated into one deployment per
-- service on a private per-stack network (docs/spikes/compose.md).
CREATE TABLE stacks (
	id BIGINT PRIMARY KEY,
	project_id BIGINT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
	environment_id BIGINT NOT NULL,
	server_id BIGINT NOT NULL REFERENCES servers(id) ON DELETE RESTRICT,
	name VARCHAR(60) NOT NULL,
	slug VARCHAR(60) NOT NULL,
	compose_yaml TEXT NOT NULL,
	-- age-encrypted JSON object of the variables the compose file interpolates.
	env BYTEA NOT NULL,
	-- `raw`, or `template:<id>@<version>`.
	source VARCHAR(120) NOT NULL DEFAULT 'raw',
	status VARCHAR(20) NOT NULL DEFAULT 'pending',
	status_message TEXT,
	created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
	updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
	UNIQUE (project_id, name),
	UNIQUE (project_id, slug),
	CONSTRAINT stacks_environment_fk
		FOREIGN KEY (environment_id, project_id) REFERENCES environments (id, project_id)
);
CREATE INDEX idx_stacks_environment_id ON stacks (environment_id);
CREATE INDEX idx_stacks_server_id ON stacks (server_id);

CREATE TRIGGER stacks_default_environment
	BEFORE INSERT ON stacks
	FOR EACH ROW EXECUTE FUNCTION default_resource_environment();

CREATE TABLE stack_services (
	id BIGINT PRIMARY KEY,
	stack_id BIGINT NOT NULL REFERENCES stacks(id) ON DELETE CASCADE,
	name VARCHAR(100) NOT NULL,
	image TEXT NOT NULL DEFAULT '',
	container_id VARCHAR(255),
	status VARCHAR(20) NOT NULL DEFAULT 'pending',
	status_message TEXT,
	-- starting, healthy or unhealthy; null without a health check.
	health VARCHAR(20),
	-- Public hostname routed to this service, and the container port behind it.
	domain VARCHAR(255),
	domain_port INTEGER,
	-- Hash of the DeploySpec last deployed; an unchanged service is skipped.
	spec_hash VARCHAR(64),
	created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
	updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
	UNIQUE (stack_id, name)
);
CREATE UNIQUE INDEX idx_stack_services_domain ON stack_services (domain) WHERE domain IS NOT NULL;

INSERT INTO permissions (id, action, resource)
SELECT (SELECT COALESCE(MAX(id), 0) FROM permissions) + n, action, 'stack'
FROM (VALUES (1, 'create'), (2, 'read'), (3, 'update'), (4, 'delete')) AS v (n, action)
ON CONFLICT DO NOTHING;
INSERT INTO role_permissions (role_id, permission_id)
	SELECT 1, id FROM permissions WHERE resource = 'stack' ON CONFLICT DO NOTHING;
INSERT INTO role_permissions (role_id, permission_id)
	SELECT 2, id FROM permissions WHERE resource = 'stack' AND action = 'read' ON CONFLICT DO NOTHING;
