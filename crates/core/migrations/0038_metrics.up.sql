-- Time series sampled from each active server's agent. `rollup` rows are the
-- 5-minute averages kept for 7 days; the raw rows they summarise are dropped
-- after 24 hours. Neither table has a foreign key to the resources: a deleted
-- resource's rows simply age out.
CREATE TABLE server_metrics (
	server_id BIGINT NOT NULL REFERENCES servers(id) ON DELETE CASCADE,
	at TIMESTAMPTZ NOT NULL,
	rollup BOOLEAN NOT NULL DEFAULT FALSE,
	-- Percent, 0 to 100.
	cpu DOUBLE PRECISION,
	mem_used BIGINT NOT NULL,
	mem_total BIGINT NOT NULL,
	disk_used BIGINT NOT NULL,
	disk_total BIGINT NOT NULL,
	-- Bytes since boot, summed over the physical interfaces.
	net_rx BIGINT NOT NULL,
	net_tx BIGINT NOT NULL,
	-- Percent and bytes; null without an NVIDIA GPU.
	gpu_util DOUBLE PRECISION,
	vram_used BIGINT,
	PRIMARY KEY (server_id, rollup, at)
);

-- `kind` is application, database, model or stack. A stack's row sums its services.
CREATE TABLE resource_metrics (
	kind VARCHAR(20) NOT NULL,
	resource_id BIGINT NOT NULL,
	at TIMESTAMPTZ NOT NULL,
	rollup BOOLEAN NOT NULL DEFAULT FALSE,
	-- Share of the whole host's CPU, 0 to 100.
	cpu DOUBLE PRECISION,
	mem BIGINT NOT NULL,
	PRIMARY KEY (kind, resource_id, rollup, at)
);
-- The retention sweeps filter on `at` alone.
CREATE INDEX idx_server_metrics_at ON server_metrics (at);
CREATE INDEX idx_resource_metrics_at ON resource_metrics (at);
