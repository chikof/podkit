-- Health-gated deploys. An application without a row here deploys the way it always did.
-- `kind` is `http` (the agent asks the container's port for `path` and wants
-- `expect_status`) or `command` (a Podman healthcheck running `command` in the container).
CREATE TABLE application_health (
	application_id BIGINT PRIMARY KEY REFERENCES applications(id) ON DELETE CASCADE,
	kind VARCHAR(10) NOT NULL CHECK (kind IN ('http', 'command')),
	path VARCHAR(512) NOT NULL DEFAULT '/',
	expect_status INTEGER NOT NULL DEFAULT 200 CHECK (expect_status BETWEEN 100 AND 599),
	command VARCHAR(1024),
	interval_s INTEGER NOT NULL DEFAULT 5 CHECK (interval_s BETWEEN 1 AND 300),
	timeout_s INTEGER NOT NULL DEFAULT 3 CHECK (timeout_s BETWEEN 1 AND 120),
	retries INTEGER NOT NULL DEFAULT 3 CHECK (retries BETWEEN 1 AND 20),
	start_period_s INTEGER NOT NULL DEFAULT 10 CHECK (start_period_s BETWEEN 0 AND 600),
	-- How long a deploy waits for the new container to be healthy before it fails.
	gate_timeout_s INTEGER NOT NULL DEFAULT 90 CHECK (gate_timeout_s BETWEEN 5 AND 1800),
	-- How long the old container keeps running once traffic has moved.
	drain_s INTEGER NOT NULL DEFAULT 5 CHECK (drain_s BETWEEN 0 AND 300),
	-- Automatic rollback: for this many minutes after a deploy, this many failed checks (or a
	-- crash) put the previous image back. 0 minutes turns it off.
	rollback_window_min INTEGER NOT NULL DEFAULT 5 CHECK (rollback_window_min BETWEEN 0 AND 120),
	rollback_after INTEGER NOT NULL DEFAULT 3 CHECK (rollback_after BETWEEN 1 AND 20),
	updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
	CONSTRAINT application_health_command_set CHECK (kind <> 'command' OR command IS NOT NULL)
);

-- What the health gate did to one deployment: the state shown in the timeline and the header.
-- `state` is waiting, healthy, unhealthy (the gate timed out or the new container never
-- passed) or rolled_back (this deployment was replaced by the previous image). `rollback_of`
-- is set on the deployment an automatic rollback started, naming the one it replaced.
CREATE TABLE deployment_health (
	deployment_id BIGINT PRIMARY KEY REFERENCES deployments(id) ON DELETE CASCADE,
	state VARCHAR(12) NOT NULL CHECK (state IN ('waiting', 'healthy', 'unhealthy', 'rolled_back')),
	detail VARCHAR(500),
	-- Failed checks counted since the deploy, by the periodic watch.
	failures INTEGER NOT NULL DEFAULT 0,
	rollback_of BIGINT REFERENCES deployments(id) ON DELETE SET NULL,
	updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
