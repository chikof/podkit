//! The only place a team-owned resource is fetched by its own id. Each fn
//! joins up to the owning team, so another team's id behaves exactly like a
//! missing one: the slice's `NotFound`, never `Forbidden`.

use sqlx::PgPool;

use crate::applications::{Application, BuildStrategy};
use crate::error::{AppResult, ServerError};
use crate::ids::{ApplicationId, ProjectId, ServerId, TeamId};
use crate::projects::Project;
use crate::servers::{Server, ServerStatus};

pub async fn project(db: &PgPool, team: TeamId, id: ProjectId) -> AppResult<Project> {
	sqlx::query_as!(
		Project,
		r#"
			SELECT id AS "id: ProjectId", team_id AS "team_id: TeamId", name, slug
			FROM projects
			WHERE id = $2 AND team_id = $1
		"#,
		team as TeamId,
		id as ProjectId
	)
	.fetch_optional(db)
	.await?
	.ok_or(ServerError::ProjectNotFound)
}

pub async fn application(
	db: &PgPool,
	team: TeamId,
	project: ProjectId,
	id: ApplicationId,
) -> AppResult<Application> {
	sqlx::query_as!(
		Application,
		r#"
			SELECT a.id AS "id: ApplicationId", a.project_id AS "project_id: ProjectId",
				a.server_id AS "server_id: ServerId", a.name, a.slug, a.repo_url, a.git_ref,
				a.deploy_key, a.git_token, a.build_strategy AS "build_strategy: BuildStrategy",
				a.dockerfile_path, a.container_port, a.webhook_secret, a.memory_limit_mb,
				a.cpu_limit
			FROM applications a
			JOIN projects p ON p.id = a.project_id
			WHERE a.id = $3 AND a.project_id = $2 AND p.team_id = $1
		"#,
		team as TeamId,
		project as ProjectId,
		id as ApplicationId
	)
	.fetch_optional(db)
	.await?
	.ok_or(ServerError::ApplicationNotFound)
}

pub async fn server(db: &PgPool, team: TeamId, id: ServerId) -> AppResult<Server> {
	sqlx::query_as!(
		Server,
		r#"
			SELECT id AS "id: ServerId", team_id AS "team_id: TeamId", name, hostname, ssh_port,
				ssh_user, ssh_private_key, podman_socket_path, is_local,
				status AS "status: ServerStatus"
			FROM servers
			WHERE id = $2 AND team_id = $1
		"#,
		team as TeamId,
		id as ServerId
	)
	.fetch_optional(db)
	.await?
	.ok_or(ServerError::ServerNotFound)
}
