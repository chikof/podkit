DROP TABLE IF EXISTS restore_drills;
ALTER TABLE backups DROP COLUMN IF EXISTS source_tables;
DROP INDEX IF EXISTS idx_databases_drill_next_run_at;
ALTER TABLE databases
	DROP COLUMN IF EXISTS drill_next_run_at,
	DROP COLUMN IF EXISTS drill_schedule;
