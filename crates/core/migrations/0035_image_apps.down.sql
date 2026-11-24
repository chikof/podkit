DELETE FROM applications WHERE source_kind = 'image';

ALTER TABLE applications
	DROP CONSTRAINT applications_source_check,
	DROP COLUMN registry_auth,
	DROP COLUMN image,
	DROP COLUMN source_kind,
	ALTER COLUMN repo_url SET NOT NULL;
