-- age-encrypted HTTPS access token (PAT). deploy_key now only holds SSH PEMs.
ALTER TABLE applications ADD COLUMN git_token BYTEA;

UPDATE applications SET git_token = deploy_key, deploy_key = NULL
	WHERE deploy_key IS NOT NULL AND repo_url LIKE 'https://%';
