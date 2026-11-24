-- Restore drills: on a schedule or on demand, the newest succeeded backup of a database is
-- restored into a throwaway container of the same engine and version on the same server,
-- and its tables are counted (see `backups::drills`).
--
-- The schedule is a five-field UTC cron like `backup_schedule`; null means no scheduled
-- drills, which is the default.
ALTER TABLE databases
	ADD COLUMN drill_schedule VARCHAR(100),
	ADD COLUMN drill_next_run_at TIMESTAMPTZ;
CREATE INDEX idx_databases_drill_next_run_at ON databases (drill_next_run_at)
	WHERE drill_next_run_at IS NOT NULL;

-- Tables (collections for MongoDB) the database had when the backup was taken; null when
-- the count was not possible, such as with an older agent.
ALTER TABLE backups ADD COLUMN source_tables BIGINT;

CREATE TABLE restore_drills (
	id BIGINT PRIMARY KEY,
	database_id BIGINT NOT NULL REFERENCES databases(id) ON DELETE CASCADE,
	-- Null once retention removed the backup; `backup_taken_at` still says which it was.
	backup_id BIGINT REFERENCES backups(id) ON DELETE SET NULL,
	backup_taken_at TIMESTAMPTZ,
	status VARCHAR(20) NOT NULL DEFAULT 'queued'
		CHECK (status IN ('queued', 'running', 'passed', 'failed')),
	-- What the copy held after the restore, and what the backup's source held.
	tables_found BIGINT,
	source_tables BIGINT,
	error TEXT,
	-- Null for scheduled drills.
	triggered_by BIGINT REFERENCES users(id) ON DELETE SET NULL,
	created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
	started_at TIMESTAMPTZ,
	finished_at TIMESTAMPTZ,
	duration_ms BIGINT
);
CREATE INDEX idx_restore_drills_database_id_id ON restore_drills (database_id, id DESC);
