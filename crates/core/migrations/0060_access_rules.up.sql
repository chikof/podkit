-- Access rules: what Traefik does to requests for an application before they reach it, and
-- whether the application is on the public ingress at all. Written to the container's labels
-- as middlewares at the next deploy.
--
-- An application without a row has the defaults: no IP lists, no limits, no login, the
-- security headers on, and routed. The migration gives every application that already exists
-- a row with the headers off, so a deploy after the upgrade changes nothing for them; only
-- applications made later get the headers.
CREATE TABLE access_rules (
	application_id BIGINT PRIMARY KEY REFERENCES applications(id) ON DELETE CASCADE,
	-- CIDR ranges or single addresses, validated by the server. Empty allows everyone.
	allow_cidrs TEXT[] NOT NULL DEFAULT '{}',
	deny_cidrs TEXT[] NOT NULL DEFAULT '{}',
	-- Requests per second per client address, and the burst above it. NULL is no limit.
	rate_average INTEGER CHECK (rate_average BETWEEN 1 AND 100000),
	rate_burst INTEGER CHECK (rate_burst BETWEEN 1 AND 100000),
	-- Largest request body, in megabytes. NULL is no limit.
	max_body_mb INTEGER CHECK (max_body_mb BETWEEN 1 AND 10240),
	security_headers BOOLEAN NOT NULL DEFAULT TRUE,
	-- No public router at all: the application answers only on its networks.
	internal_only BOOLEAN NOT NULL DEFAULT FALSE,
	updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Basic auth users of an application. `password_hash` is a bcrypt hash and goes into the
-- Traefik label as it is; the password itself is never stored and no page shows the hash.
CREATE TABLE access_users (
	application_id BIGINT NOT NULL REFERENCES applications(id) ON DELETE CASCADE,
	username VARCHAR(64) NOT NULL,
	password_hash VARCHAR(100) NOT NULL,
	created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
	PRIMARY KEY (application_id, username)
);

INSERT INTO access_rules (application_id, security_headers)
SELECT id, FALSE FROM applications;
