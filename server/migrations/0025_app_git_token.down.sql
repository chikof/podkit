UPDATE applications SET deploy_key = git_token
	WHERE git_token IS NOT NULL AND deploy_key IS NULL;

ALTER TABLE applications DROP COLUMN git_token;
