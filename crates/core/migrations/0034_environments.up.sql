-- Project -> Environment -> Resources. Every application, database and model
-- now lives in one environment of its project.
CREATE TABLE environments (
	id BIGINT PRIMARY KEY,
	project_id BIGINT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
	name VARCHAR(60) NOT NULL,
	slug VARCHAR(60) NOT NULL,
	position INTEGER NOT NULL DEFAULT 0,
	created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
	UNIQUE (project_id, slug),
	-- Target of the composite foreign keys below.
	UNIQUE (id, project_id)
);
CREATE INDEX idx_environments_project_id ON environments (project_id, position, id);

-- The default environment of a project reuses the project's id. A trigger
-- makes it, so a project made by any path (the service, a test, a manual
-- INSERT) is born with its `production` environment in the same transaction.
CREATE FUNCTION create_default_environment() RETURNS trigger AS $$
BEGIN
	INSERT INTO environments (id, project_id, name, slug, position)
	VALUES (NEW.id, NEW.id, 'production', 'production', 0);
	RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER projects_default_environment
	AFTER INSERT ON projects
	FOR EACH ROW EXECUTE FUNCTION create_default_environment();

INSERT INTO environments (id, project_id, name, slug, position)
SELECT id, id, 'production', 'production', 0 FROM projects;

-- A resource inserted without an environment lands in the project's first one.
-- The column stays NOT NULL; this only fills the default before the check.
CREATE FUNCTION default_resource_environment() RETURNS trigger AS $$
BEGIN
	IF NEW.environment_id IS NULL THEN
		SELECT id INTO NEW.environment_id FROM environments
		WHERE project_id = NEW.project_id
		ORDER BY position, id
		LIMIT 1;
	END IF;
	RETURN NEW;
END;
$$ LANGUAGE plpgsql;

ALTER TABLE applications ADD COLUMN environment_id BIGINT;
ALTER TABLE databases ADD COLUMN environment_id BIGINT;
ALTER TABLE models ADD COLUMN environment_id BIGINT;

UPDATE applications SET environment_id = project_id;
UPDATE databases SET environment_id = project_id;
UPDATE models SET environment_id = project_id;

ALTER TABLE applications ALTER COLUMN environment_id SET NOT NULL;
ALTER TABLE databases ALTER COLUMN environment_id SET NOT NULL;
ALTER TABLE models ALTER COLUMN environment_id SET NOT NULL;

-- (environment_id, project_id) must name an environment of that same project.
ALTER TABLE applications ADD CONSTRAINT applications_environment_fk
	FOREIGN KEY (environment_id, project_id) REFERENCES environments (id, project_id);
ALTER TABLE databases ADD CONSTRAINT databases_environment_fk
	FOREIGN KEY (environment_id, project_id) REFERENCES environments (id, project_id);
ALTER TABLE models ADD CONSTRAINT models_environment_fk
	FOREIGN KEY (environment_id, project_id) REFERENCES environments (id, project_id);

CREATE INDEX idx_applications_environment_id ON applications (environment_id);
CREATE INDEX idx_databases_environment_id ON databases (environment_id);
CREATE INDEX idx_models_environment_id ON models (environment_id);

CREATE TRIGGER applications_default_environment
	BEFORE INSERT ON applications
	FOR EACH ROW EXECUTE FUNCTION default_resource_environment();
CREATE TRIGGER databases_default_environment
	BEFORE INSERT ON databases
	FOR EACH ROW EXECUTE FUNCTION default_resource_environment();
CREATE TRIGGER models_default_environment
	BEFORE INSERT ON models
	FOR EACH ROW EXECUTE FUNCTION default_resource_environment();
