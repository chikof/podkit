-- Image vulnerability scanning (docs/spikes/scanning.md): every deployed image is
-- scanned with Syft and Grype, results hang off the resource and its deployment,
-- and findings can be ignored for a while with a reason.

-- One row per scan of one image on one server. `sbom` is the gzipped CycloneDX
-- JSON, kept so a daily rescan can run Grype again without reading the image.
-- `image_digest` is the image's content id and stays empty until the agent has
-- resolved it; scans of the same digest on the same server are reused for 24 h.
CREATE TABLE image_scans (
	id BIGINT PRIMARY KEY,
	team_id BIGINT NOT NULL REFERENCES teams(id) ON DELETE CASCADE,
	server_id BIGINT NOT NULL REFERENCES servers(id) ON DELETE CASCADE,
	image_digest VARCHAR(160) NOT NULL DEFAULT '',
	image_ref TEXT NOT NULL,
	status VARCHAR(20) NOT NULL DEFAULT 'queued',
	started_at TIMESTAMPTZ,
	finished_at TIMESTAMPTZ,
	-- When Grype last ran over the stored SBOM (the daily rescan), if it did.
	rescanned_at TIMESTAMPTZ,
	sbom BYTEA,
	sbom_bytes INTEGER,
	-- A fresh scan skips the 24 h reuse of an earlier scan of the same image.
	force BOOLEAN NOT NULL DEFAULT FALSE,
	-- Raw counts, before the team's ignores.
	critical INTEGER NOT NULL DEFAULT 0,
	high INTEGER NOT NULL DEFAULT 0,
	medium INTEGER NOT NULL DEFAULT 0,
	low INTEGER NOT NULL DEFAULT 0,
	unknown INTEGER NOT NULL DEFAULT 0,
	act_now INTEGER NOT NULL DEFAULT 0,
	-- When the vulnerability database behind the findings was built, and the tools.
	db_built VARCHAR(40),
	tools VARCHAR(80),
	error TEXT,
	created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX idx_image_scans_digest ON image_scans (server_id, image_digest, finished_at DESC);
CREATE INDEX idx_image_scans_team ON image_scans (team_id);

CREATE TABLE scan_findings (
	scan_id BIGINT NOT NULL REFERENCES image_scans(id) ON DELETE CASCADE,
	vuln_id VARCHAR(100) NOT NULL,
	package VARCHAR(255) NOT NULL,
	version VARCHAR(255) NOT NULL,
	fixed_version TEXT,
	-- critical, high, medium, low or unknown.
	severity VARCHAR(10) NOT NULL,
	cvss DOUBLE PRECISION,
	epss DOUBLE PRECISION,
	kev BOOLEAN NOT NULL DEFAULT FALSE,
	-- KEV-listed, or EPSS >= 0.10 with CVSS >= 7: worked out once, when stored.
	act_now BOOLEAN NOT NULL DEFAULT FALSE,
	url TEXT,
	PRIMARY KEY (scan_id, vuln_id, package, version)
);

-- A team's decision to live with a finding until `expires_at`. `package` empty
-- means every package.
CREATE TABLE scan_ignores (
	id BIGINT PRIMARY KEY,
	team_id BIGINT NOT NULL REFERENCES teams(id) ON DELETE CASCADE,
	vuln_id VARCHAR(100) NOT NULL,
	package VARCHAR(255),
	reason TEXT NOT NULL,
	expires_at TIMESTAMPTZ NOT NULL,
	created_by BIGINT REFERENCES users(id) ON DELETE SET NULL,
	created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX idx_scan_ignores_team ON scan_ignores (team_id, vuln_id);

-- Which scan a resource shows (the newest link), and the deployment that queued it.
-- `resource_kind` is application, stack_service, database or model.
CREATE TABLE scan_links (
	resource_kind VARCHAR(20) NOT NULL,
	resource_id BIGINT NOT NULL,
	scan_id BIGINT NOT NULL REFERENCES image_scans(id) ON DELETE CASCADE,
	deployment_id BIGINT,
	created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
	PRIMARY KEY (resource_kind, resource_id, scan_id)
);
CREATE INDEX idx_scan_links_resource ON scan_links (resource_kind, resource_id, created_at DESC);
CREATE INDEX idx_scan_links_deployment ON scan_links (deployment_id) WHERE deployment_id IS NOT NULL;

-- Per server: scanning on or off, the scanner containers' caps, and when the
-- daily rescan last ran (it runs at most once a day).
ALTER TABLE servers ADD COLUMN scan_enabled BOOLEAN NOT NULL DEFAULT TRUE;
ALTER TABLE servers ADD COLUMN scan_cpus DOUBLE PRECISION NOT NULL DEFAULT 1;
ALTER TABLE servers ADD COLUMN scan_memory_mb INTEGER NOT NULL DEFAULT 1024;
ALTER TABLE servers ADD COLUMN scan_rescanned_at TIMESTAMPTZ;

-- `scan`: read sees results and downloads the SBOM, create scans again,
-- update ignores findings and changes the settings, delete lifts an ignore.
INSERT INTO permissions (id, action, resource)
SELECT (SELECT COALESCE(MAX(id), 0) FROM permissions) + n, action, 'scan'
FROM (VALUES (1, 'create'), (2, 'read'), (3, 'update'), (4, 'delete')) AS v (n, action)
ON CONFLICT DO NOTHING;
INSERT INTO role_permissions (role_id, permission_id)
	SELECT 1, id FROM permissions WHERE resource = 'scan' ON CONFLICT DO NOTHING;
INSERT INTO role_permissions (role_id, permission_id)
	SELECT 2, id FROM permissions WHERE resource = 'scan' AND action = 'read' ON CONFLICT DO NOTHING;
