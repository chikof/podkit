DELETE FROM deployments WHERE triggered_by IS NULL;
ALTER TABLE deployments ALTER COLUMN triggered_by SET NOT NULL;
