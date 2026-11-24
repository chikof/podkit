DROP TRIGGER models_default_environment ON models;
DROP TRIGGER databases_default_environment ON databases;
DROP TRIGGER applications_default_environment ON applications;
DROP TRIGGER projects_default_environment ON projects;

-- The composite foreign keys and indexes go with the columns.
ALTER TABLE models DROP COLUMN environment_id;
ALTER TABLE databases DROP COLUMN environment_id;
ALTER TABLE applications DROP COLUMN environment_id;

DROP TABLE environments;

DROP FUNCTION default_resource_environment();
DROP FUNCTION create_default_environment();
