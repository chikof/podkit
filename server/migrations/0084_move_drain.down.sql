DROP TABLE IF EXISTS application_moves;
DROP TABLE IF EXISTS server_drains;
ALTER TABLE servers DROP COLUMN IF EXISTS draining_since;
