-- Richer charts: HTTP, database, volume, GPU and model series, plus container events.
--
-- `resource_metrics` keeps CPU and memory. The new figures live in a table of their own, so a
-- resource without HTTP traffic or without a database engine costs no empty columns on every
-- sample and the existing queries do not change. Same keys, same retention as
-- `resource_metrics`: raw rows for 24 hours, 5-minute `rollup` rows for 7 days, no foreign key
-- (a deleted resource's rows age out).
CREATE TABLE resource_extra_metrics (
	-- application, database, model or stack.
	kind VARCHAR(20) NOT NULL,
	resource_id BIGINT NOT NULL,
	at TIMESTAMPTZ NOT NULL,
	rollup BOOLEAN NOT NULL DEFAULT FALSE,
	-- HTTP through Traefik, per second over the sampling interval.
	req_rate DOUBLE PRECISION,
	r2xx_rate DOUBLE PRECISION,
	r3xx_rate DOUBLE PRECISION,
	r4xx_rate DOUBLE PRECISION,
	r5xx_rate DOUBLE PRECISION,
	-- Milliseconds, from the latency histogram's growth over the interval.
	p50_ms DOUBLE PRECISION,
	p95_ms DOUBLE PRECISION,
	bytes_in_rate DOUBLE PRECISION,
	bytes_out_rate DOUBLE PRECISION,
	-- Databases: connections, bytes of data, transactions (or commands) per second.
	db_conns BIGINT,
	db_size BIGINT,
	db_ops_rate DOUBLE PRECISION,
	-- Bytes on the volumes of the resource, and GPU memory held by its containers.
	volume_bytes BIGINT,
	vram_bytes BIGINT,
	-- Models: tokens and requests per second the gateway served.
	tokens_rate DOUBLE PRECISION,
	model_req_rate DOUBLE PRECISION,
	PRIMARY KEY (kind, resource_id, rollup, at)
);
CREATE INDEX idx_resource_extra_metrics_at ON resource_extra_metrics (at);

-- What Podman said happened to a container: it exited with a status other than 0, was killed
-- for memory, or was restarted. Kept for 7 days.
CREATE TABLE container_events (
	kind VARCHAR(20) NOT NULL,
	resource_id BIGINT NOT NULL,
	at TIMESTAMPTZ NOT NULL,
	-- exit, oom or restart.
	event VARCHAR(10) NOT NULL,
	container VARCHAR(255) NOT NULL DEFAULT '',
	exit_code INTEGER,
	PRIMARY KEY (kind, resource_id, event, container, at)
);
CREATE INDEX idx_container_events_at ON container_events (at);

-- Models are read by id and hour for the tokens-per-second series.
CREATE INDEX idx_model_usage_model_hour ON model_usage (model_id, hour);

-- The new series are alert metrics too; their names are longer than the first five.
ALTER TABLE metric_alerts ALTER COLUMN metric TYPE VARCHAR(24);
