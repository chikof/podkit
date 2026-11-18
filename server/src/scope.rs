//! The only place a team-owned resource is fetched by its own id. Each fn
//! joins up to the owning team, so another team's id behaves exactly like a
//! missing one: the slice's `NotFound`, never `Forbidden`.

use sqlx::PgPool;

use crate::applications::{Application, BuildStrategy, SourceKind};
use crate::backups::BackupTarget;
use crate::databases::{Database, DatabaseStatus, EngineKind};
use crate::environments::Environment;
use crate::error::{AppResult, ServerError};
use crate::ids::{
	ApplicationId, BackupTargetId, DatabaseId, EnvironmentId, ProjectId, ServerId, TeamId,
};
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

pub async fn environment(
	db: &PgPool,
	team: TeamId,
	project: ProjectId,
	id: EnvironmentId,
) -> AppResult<Environment> {
	sqlx::query_as!(
		Environment,
		r#"
			SELECT e.id AS "id: EnvironmentId", e.project_id AS "project_id: ProjectId",
				e.name, e.slug, e.position, e.created_at
			FROM environments e
			JOIN projects p ON p.id = e.project_id
			WHERE e.id = $3 AND e.project_id = $2 AND p.team_id = $1
		"#,
		team as TeamId,
		project as ProjectId,
		id as EnvironmentId
	)
	.fetch_optional(db)
	.await?
	.ok_or(ServerError::EnvironmentNotFound)
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
			SELECT a.id AS "id: ApplicationId", a.project_id AS "project_id: ProjectId", a.environment_id AS "environment_id: EnvironmentId",
				a.server_id AS "server_id: ServerId", a.name, a.slug, a.repo_url, a.git_ref,
				a.source_kind AS "source_kind: SourceKind", a.image, a.registry_auth,
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
				status AS "status: ServerStatus", host_key, agent_socket_path, agent_version,
				arch, status_message,
				hardware AS "hardware?: sqlx::types::Json<proto::Hardware>", hardware_at
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

pub async fn database(
	db: &PgPool,
	team: TeamId,
	project: ProjectId,
	id: DatabaseId,
) -> AppResult<Database> {
	sqlx::query_as!(
		Database,
		r#"
			SELECT d.id AS "id: DatabaseId", d.project_id AS "project_id: ProjectId", d.environment_id AS "environment_id: EnvironmentId",
				d.server_id AS "server_id: ServerId", d.name, d.engine AS "engine: EngineKind",
				d.version, d.credentials, d.public_port, d.memory_limit_mb, d.cpu_limit,
				d.status AS "status: DatabaseStatus", d.status_message, d.container_id,
				d.backup_target_id AS "backup_target_id: BackupTargetId", d.backup_schedule,
				d.backup_keep, d.backup_next_run_at
			FROM databases d
			JOIN projects p ON p.id = d.project_id
			WHERE d.id = $3 AND d.project_id = $2 AND p.team_id = $1
		"#,
		team as TeamId,
		project as ProjectId,
		id as DatabaseId
	)
	.fetch_optional(db)
	.await?
	.ok_or(ServerError::DatabaseNotFound)
}

pub async fn backup_target(
	db: &PgPool,
	team: TeamId,
	id: BackupTargetId,
) -> AppResult<BackupTarget> {
	sqlx::query_as!(
		BackupTarget,
		r#"
			SELECT id AS "id: BackupTargetId", team_id AS "team_id: TeamId", name, endpoint,
				region, bucket, path_style, prefix, access_key_id, secret_access_key, created_at
			FROM backup_targets
			WHERE id = $2 AND team_id = $1
		"#,
		team as TeamId,
		id as BackupTargetId
	)
	.fetch_optional(db)
	.await?
	.ok_or(ServerError::BackupTargetNotFound)
}

pub async fn model(
	db: &PgPool,
	team: TeamId,
	project: ProjectId,
	id: crate::ids::ModelId,
) -> AppResult<crate::models::Model> {
	crate::models::find_scoped(db, team, project, id)
		.await?
		.ok_or(ServerError::ModelNotFound)
}
