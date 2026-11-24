ALTER TABLE deployments DROP CONSTRAINT deployments_triggered_by_fkey,
	ADD CONSTRAINT deployments_triggered_by_fkey FOREIGN KEY (triggered_by) REFERENCES users(id);
DROP INDEX IF EXISTS idx_team_members_user_id;
DROP INDEX IF EXISTS idx_deployments_application_id_id;
CREATE INDEX IF NOT EXISTS idx_deployments_application_id ON deployments (application_id);
CREATE INDEX IF NOT EXISTS idx_env_vars_application_id ON env_vars (application_id);
CREATE INDEX IF NOT EXISTS idx_team_members_team_id ON team_members (team_id);
CREATE INDEX IF NOT EXISTS idx_servers_team_id ON servers (team_id);
CREATE INDEX IF NOT EXISTS idx_projects_team_id ON projects (team_id);
