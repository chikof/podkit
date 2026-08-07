use async_trait::async_trait;
use podkit_core::domain::application::entity::{Application, BuildStrategy};
use podkit_core::domain::application::repository::ApplicationRepository;
use podkit_core::domain::shared::errors::{DomainError, DomainResult};
use podkit_core::domain::shared::ids::{ApplicationId, ProjectId, ServerId};

use crate::PgPool;
use crate::models::application::{ApplicationModel, ApplicationUpdate, NewApplication};

/// Postgres-backed `ApplicationRepository`.
pub struct PgApplicationRepository(pub &'static PgPool);

fn to_infra(e: &crate::DatabaseError) -> DomainError {
	if let crate::DatabaseError::ConnectionError(sqlx_err) = e
		&& sqlx_err
			.as_database_error()
			.is_some_and(sqlx::error::DatabaseError::is_unique_violation)
	{
		return DomainError::AlreadyExists;
	}
	DomainError::Infrastructure(e.to_string())
}

fn map_application(row: ApplicationModel) -> Application {
	Application {
		id: ApplicationId(row.id),
		project_id: ProjectId(row.project_id),
		server_id: ServerId(row.server_id),
		name: row.name,
		slug: row.slug,
		repo_url: row.repo_url,
		git_ref: row.git_ref,
		deploy_key: row.deploy_key,
		build_strategy: BuildStrategy::parse(&row.build_strategy),
		dockerfile_path: row.dockerfile_path,
		container_port: row.container_port,
		webhook_secret: row.webhook_secret,
		memory_limit_mb: row.memory_limit_mb,
		cpu_limit: row.cpu_limit.map(f64::from),
		created_at: row.created_at,
		updated_at: row.updated_at,
	}
}

#[async_trait]
impl ApplicationRepository for PgApplicationRepository {
	async fn find_by_id(&self, id: ApplicationId) -> DomainResult<Option<Application>> {
		Ok(ApplicationModel::find_by_id(self.0, id.0)
			.await
			.map_err(|e| to_infra(&e))?
			.map(map_application))
	}

	async fn list_by_project(&self, project_id: ProjectId) -> DomainResult<Vec<Application>> {
		Ok(ApplicationModel::list_by_project(self.0, project_id.0)
			.await
			.map_err(|e| to_infra(&e))?
			.into_iter()
			.map(map_application)
			.collect())
	}

	async fn list_by_server(&self, server_id: ServerId) -> DomainResult<Vec<Application>> {
		Ok(ApplicationModel::list_by_server(self.0, server_id.0)
			.await
			.map_err(|e| to_infra(&e))?
			.into_iter()
			.map(map_application)
			.collect())
	}

	async fn save(&self, application: &Application) -> DomainResult<()> {
		ApplicationModel::create(
			self.0,
			NewApplication {
				id: application.id.0,
				project_id: application.project_id.0,
				server_id: application.server_id.0,
				name: application.name.clone(),
				slug: application.slug.clone(),
				repo_url: application.repo_url.clone(),
				git_ref: application.git_ref.clone(),
				deploy_key: application.deploy_key.clone(),
				build_strategy: application.build_strategy.as_str().to_string(),
				dockerfile_path: application.dockerfile_path.clone(),
				container_port: application.container_port,
				webhook_secret: application.webhook_secret.clone(),
				memory_limit_mb: application.memory_limit_mb,
				#[allow(clippy::cast_possible_truncation)]
				cpu_limit: application.cpu_limit.map(|c| c as f32),
			},
		)
		.await
		.map_err(|e| to_infra(&e))?;
		Ok(())
	}

	async fn update(&self, application: &Application) -> DomainResult<()> {
		ApplicationModel::update(
			self.0,
			application.id.0,
			ApplicationUpdate {
				name: application.name.clone(),
				slug: application.slug.clone(),
				repo_url: application.repo_url.clone(),
				git_ref: application.git_ref.clone(),
				deploy_key: application.deploy_key.clone(),
				build_strategy: application.build_strategy.as_str().to_string(),
				dockerfile_path: application.dockerfile_path.clone(),
				container_port: application.container_port,
				memory_limit_mb: application.memory_limit_mb,
				#[allow(clippy::cast_possible_truncation)]
				cpu_limit: application.cpu_limit.map(|c| c as f32),
			},
		)
		.await
		.map_err(|e| to_infra(&e))?;
		Ok(())
	}

	async fn delete(&self, id: ApplicationId) -> DomainResult<()> {
		ApplicationModel::delete(self.0, id.0)
			.await
			.map_err(|e| to_infra(&e))
	}
}
