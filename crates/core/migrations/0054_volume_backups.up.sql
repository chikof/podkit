-- Backups of application and stack volumes to the team's S3 destinations.
--
-- A policy is the backup settings of one volume: where it goes, when, how many to
-- keep and whether the owner is stopped while it is read. It is created the first
-- time the volume is configured or backed up. The volume is named by its Podman
-- name, which is unique across the install; `volume_id` (application volumes) or
-- `stack_id` (compose volumes, which have no row of their own) says who owns it.
CREATE TABLE volume_backup_policies (
	id BIGINT PRIMARY KEY,
	volume_id BIGINT REFERENCES volumes(id) ON DELETE CASCADE,
	stack_id BIGINT REFERENCES stacks(id) ON DELETE CASCADE,
	volume_name VARCHAR(160) NOT NULL UNIQUE,
	-- The mount path of an application volume, the compose name of a stack volume.
	label VARCHAR(255) NOT NULL,
	target_id BIGINT REFERENCES backup_targets(id) ON DELETE SET NULL,
	-- Five-field UTC cron; null for manual backups only.
	schedule VARCHAR(100),
	next_run_at TIMESTAMPTZ,
	keep INTEGER NOT NULL DEFAULT 7 CHECK (keep BETWEEN 1 AND 365),
	-- `live` reads the volume as it is; `stop` stops its owner for the read.
	consistency VARCHAR(10) NOT NULL DEFAULT 'live' CHECK (consistency IN ('live', 'stop')),
	created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
	CONSTRAINT volume_backup_policies_owner CHECK ((volume_id IS NULL) <> (stack_id IS NULL)),
	CONSTRAINT volume_backup_policies_schedule CHECK ((schedule IS NULL) = (next_run_at IS NULL))
);
CREATE INDEX idx_volume_backup_policies_due ON volume_backup_policies (next_run_at)
	WHERE next_run_at IS NOT NULL;
CREATE INDEX idx_volume_backup_policies_stack ON volume_backup_policies (stack_id)
	WHERE stack_id IS NOT NULL;

CREATE TABLE volume_backups (
	id BIGINT PRIMARY KEY,
	policy_id BIGINT NOT NULL REFERENCES volume_backup_policies(id) ON DELETE CASCADE,
	target_id BIGINT REFERENCES backup_targets(id) ON DELETE SET NULL,
	status VARCHAR(20) NOT NULL DEFAULT 'queued',
	-- What the policy said when this backup started.
	consistency VARCHAR(10) NOT NULL,
	object_key TEXT,
	-- Compressed size, and the size of the tar stream before compression.
	size_bytes BIGINT,
	raw_bytes BIGINT,
	-- Files, directories and links in the archive.
	entries BIGINT,
	sha256 VARCHAR(64),
	-- Something the user should know about a backup that did succeed.
	warning TEXT,
	error TEXT,
	triggered_by BIGINT REFERENCES users(id) ON DELETE SET NULL,
	created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
	finished_at TIMESTAMPTZ
);
CREATE INDEX idx_volume_backups_policy_id_id ON volume_backups (policy_id, id DESC);

-- One restore of one backup: the flow queued, stopping, restoring, starting, then
-- succeeded or failed (see `volume_backups::RestoreState`).
CREATE TABLE volume_restores (
	id BIGINT PRIMARY KEY,
	backup_id BIGINT NOT NULL REFERENCES volume_backups(id) ON DELETE CASCADE,
	status VARCHAR(20) NOT NULL DEFAULT 'queued',
	-- `same` empties the backup's own volume first; `new` fills another one.
	mode VARCHAR(10) NOT NULL CHECK (mode IN ('same', 'new')),
	-- Podman name of the volume the archive is written to.
	dest_volume VARCHAR(160) NOT NULL,
	error TEXT,
	triggered_by BIGINT REFERENCES users(id) ON DELETE SET NULL,
	created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
	finished_at TIMESTAMPTZ
);
CREATE INDEX idx_volume_restores_backup_id ON volume_restores (backup_id, id DESC);
