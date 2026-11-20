-- The team SSH key vault: keys are generated or imported once and reused by servers (to log
-- in) and applications (as their git deploy key). The private half is age-encrypted with the
-- same secret box as every other credential and is never read back out by the API or the UI.
CREATE TABLE ssh_keys (
	id BIGINT PRIMARY KEY,
	team_id BIGINT NOT NULL REFERENCES teams(id) ON DELETE CASCADE,
	name VARCHAR(120) NOT NULL,
	-- `ed25519`, `rsa`, `ecdsa`; `unknown` until the startup pass has derived it from a
	-- backfilled key (the migration cannot decrypt).
	algorithm VARCHAR(20) NOT NULL,
	-- One authorized_keys line, no private material. Empty until derived (see above).
	public_key TEXT NOT NULL DEFAULT '',
	-- `SHA256:<base64>` as `ssh-keygen -lf` prints it. Empty until derived.
	fingerprint VARCHAR(80) NOT NULL DEFAULT '',
	-- age-encrypted OpenSSH private key.
	private_key BYTEA NOT NULL,
	created_by BIGINT REFERENCES users(id) ON DELETE SET NULL,
	created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
	last_used_at TIMESTAMPTZ,
	-- Lets `servers` pin its key to its own team with a composite foreign key.
	UNIQUE (id, team_id)
);
CREATE INDEX idx_ssh_keys_team_id ON ssh_keys (team_id);

ALTER TABLE servers ADD COLUMN ssh_key_id BIGINT;
ALTER TABLE applications ADD COLUMN deploy_key_id BIGINT REFERENCES ssh_keys(id);
CREATE INDEX idx_servers_ssh_key_id ON servers (ssh_key_id);
CREATE INDEX idx_applications_deploy_key_id ON applications (deploy_key_id);

-- Backfill: one vault entry per stored key, named after its resource, holding the encrypted
-- bytes as they are. The public key and fingerprint are derived by the server at startup,
-- since only it can decrypt. The backfilled ids are small and sequential; every key made
-- later is a snowflake, far above them.
CREATE TEMPORARY TABLE ssh_key_backfill ON COMMIT DROP AS
SELECT 'server' AS kind, id AS resource_id, ROW_NUMBER() OVER (ORDER BY id) AS key_id
FROM servers WHERE ssh_private_key IS NOT NULL
UNION ALL
SELECT 'application', id,
	ROW_NUMBER() OVER (ORDER BY id) + (SELECT COUNT(*) FROM servers WHERE ssh_private_key IS NOT NULL)
FROM applications WHERE deploy_key IS NOT NULL;

INSERT INTO ssh_keys (id, team_id, name, algorithm, private_key)
SELECT b.key_id, s.team_id, LEFT('Server ' || s.name, 120), 'unknown', s.ssh_private_key
FROM ssh_key_backfill b JOIN servers s ON b.kind = 'server' AND s.id = b.resource_id;

INSERT INTO ssh_keys (id, team_id, name, algorithm, private_key)
SELECT b.key_id, p.team_id, LEFT('Deploy key for ' || a.name, 120), 'unknown', a.deploy_key
FROM ssh_key_backfill b
JOIN applications a ON b.kind = 'application' AND a.id = b.resource_id
JOIN projects p ON p.id = a.project_id;

UPDATE servers s SET ssh_key_id = b.key_id
FROM ssh_key_backfill b WHERE b.kind = 'server' AND b.resource_id = s.id;
UPDATE applications a SET deploy_key_id = b.key_id
FROM ssh_key_backfill b WHERE b.kind = 'application' AND b.resource_id = a.id;

-- A server's key belongs to the server's own team.
ALTER TABLE servers
	ADD CONSTRAINT servers_ssh_key_fk FOREIGN KEY (ssh_key_id, team_id) REFERENCES ssh_keys (id, team_id);

-- The old columns go in the same migration; the down migration restores them from the vault.
ALTER TABLE servers DROP CONSTRAINT servers_local_no_ssh;
ALTER TABLE servers DROP COLUMN ssh_private_key;
ALTER TABLE servers
	ADD CONSTRAINT servers_local_no_ssh CHECK (
		(is_local AND ssh_user IS NULL AND ssh_key_id IS NULL)
		OR
		(NOT is_local AND ssh_user IS NOT NULL AND ssh_key_id IS NOT NULL)
	);
ALTER TABLE applications DROP COLUMN deploy_key;

INSERT INTO permissions (id, action, resource)
SELECT (SELECT COALESCE(MAX(id), 0) FROM permissions) + n, action, 'ssh_key'
FROM (VALUES (1, 'create'), (2, 'read'), (3, 'update'), (4, 'delete')) AS v (n, action)
ON CONFLICT DO NOTHING;
INSERT INTO role_permissions (role_id, permission_id)
	SELECT 1, id FROM permissions WHERE resource = 'ssh_key' ON CONFLICT DO NOTHING;
INSERT INTO role_permissions (role_id, permission_id)
	SELECT 2, id FROM permissions WHERE resource = 'ssh_key' AND action = 'read' ON CONFLICT DO NOTHING;
