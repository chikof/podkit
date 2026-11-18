-- Applications can deploy a registry image instead of building from git.
ALTER TABLE applications
	ADD COLUMN source_kind TEXT NOT NULL DEFAULT 'git',
	ADD COLUMN image TEXT,
	-- age-encrypted JSON {"username": ..., "password": ...}; write-only.
	ADD COLUMN registry_auth BYTEA,
	ALTER COLUMN repo_url DROP NOT NULL;

ALTER TABLE applications
	ADD CONSTRAINT applications_source_check CHECK (
		(source_kind = 'git' AND repo_url IS NOT NULL AND image IS NULL)
		OR (source_kind = 'image' AND image IS NOT NULL AND repo_url IS NULL)
	);
