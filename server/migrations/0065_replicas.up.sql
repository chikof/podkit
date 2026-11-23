-- Replicas of one application on its server. An application without a row runs one container,
-- the way it always did. The copies share one Traefik service, so Traefik balances over them
-- round robin; `sticky` pins a client to one copy with a cookie.
CREATE TABLE application_replicas (
	application_id BIGINT PRIMARY KEY REFERENCES applications(id) ON DELETE CASCADE,
	replicas INTEGER NOT NULL DEFAULT 1 CHECK (replicas BETWEEN 1 AND 10),
	sticky BOOLEAN NOT NULL DEFAULT FALSE,
	-- A rolling update starts up to `max_surge` new copies next to the old ones, or (with no
	-- surge) takes up to `max_unavailable` old ones away first. One of them must be allowed.
	max_surge INTEGER NOT NULL DEFAULT 1 CHECK (max_surge BETWEEN 0 AND 10),
	max_unavailable INTEGER NOT NULL DEFAULT 0 CHECK (max_unavailable BETWEEN 0 AND 10),
	-- The owner says the application's volumes can be mounted by several containers at once.
	-- Without it an application with volumes cannot have more than one replica.
	shared_volumes BOOLEAN NOT NULL DEFAULT FALSE,
	updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
	CONSTRAINT application_replicas_rollout CHECK (max_surge > 0 OR max_unavailable > 0)
);
