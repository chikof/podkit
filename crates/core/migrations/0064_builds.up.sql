-- Builds: which server builds, what a build may use, and the layer cache budget.

-- The team's default: a server that builds for every application of the team that has not
-- chosen another, and the caps of a build container. No row (or no server) builds on the
-- application's own server.
CREATE TABLE build_settings (
	team_id BIGINT PRIMARY KEY REFERENCES teams(id) ON DELETE CASCADE,
	build_server_id BIGINT REFERENCES servers(id) ON DELETE SET NULL,
	-- Cores a build may use; NULL for no cap.
	cpu_limit REAL CHECK (cpu_limit IS NULL OR cpu_limit > 0),
	-- Megabytes a build may use; NULL for no cap.
	memory_limit_mb INTEGER CHECK (memory_limit_mb IS NULL OR memory_limit_mb >= 256)
);

-- Per server that builds: how many builds run at once, and how big the layer cache may stay.
CREATE TABLE server_build_settings (
	server_id BIGINT PRIMARY KEY REFERENCES servers(id) ON DELETE CASCADE,
	max_concurrent INTEGER NOT NULL DEFAULT 2 CHECK (max_concurrent BETWEEN 1 AND 16),
	-- 0 keeps no layers beyond the images in use.
	cache_max_mb INTEGER NOT NULL DEFAULT 10240 CHECK (cache_max_mb >= 0)
);

-- Per application: a server of its own to build on, and whether a build may reuse layers.
CREATE TABLE app_build_settings (
	application_id BIGINT PRIMARY KEY REFERENCES applications(id) ON DELETE CASCADE,
	build_server_id BIGINT REFERENCES servers(id) ON DELETE SET NULL,
	cache_enabled BOOLEAN NOT NULL DEFAULT TRUE
);
