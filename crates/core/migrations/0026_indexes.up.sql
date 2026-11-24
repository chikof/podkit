DROP INDEX IF EXISTS idx_projects_team_id;
DROP INDEX IF EXISTS idx_servers_team_id;
DROP INDEX IF EXISTS idx_team_members_team_id;
DROP INDEX IF EXISTS idx_env_vars_application_id;
DROP INDEX IF EXISTS idx_deployments_application_id;
CREATE INDEX idx_deployments_application_id_id ON deployments (application_id, id DESC);
CREATE INDEX idx_team_members_user_id ON team_members (user_id);
ALTER TABLE deployments DROP CONSTRAINT deployments_triggered_by_fkey,
	ADD CONSTRAINT deployments_triggered_by_fkey FOREIGN KEY (triggered_by) REFERENCES users(id) ON DELETE SET NULL;
