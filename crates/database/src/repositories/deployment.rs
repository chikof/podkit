use async_trait::async_trait;
use podkit_core::domain::deployment::entity::{Deployment, DeploymentStatus};
use podkit_core::domain::deployment::repository::DeploymentRepository;
use podkit_core::domain::shared::errors::{DomainError, DomainResult};
use podkit_core::domain::shared::ids::{ApplicationId, DeploymentId, UserId};

use crate::PgPool;
use crate::models::deployment::{DeploymentModel, DeploymentUpdate, NewDeployment};

/// Postgres-backed `DeploymentRepository`.
pub struct PgDeploymentRepository(pub &'static PgPool);

fn to_infra(e: &crate::DatabaseError) -> DomainError {
	DomainError::Infrastructure(e.to_string())
}

fn map_deployment(row: DeploymentModel) -> Deployment {
	Deployment {
		id: DeploymentId(row.id),
		application_id: ApplicationId(row.application_id),
		status: DeploymentStatus::parse(&row.status),
		commit_sha: row.commit_sha,
		image_tag: row.image_tag,
		container_id: row.container_id,
		error_message: row.error_message,
		triggered_by: row.triggered_by.map(UserId),
		created_at: row.created_at,
		started_at: row.started_at,
		finished_at: row.finished_at,
	}
}

#[async_trait]
impl DeploymentRepository for PgDeploymentRepository {
	async fn find_by_id(&self, id: DeploymentId) -> DomainResult<Option<Deployment>> {
		Ok(DeploymentModel::find_by_id(self.0, id.0)
			.await
			.map_err(|e| to_infra(&e))?
			.map(map_deployment))
	}

	async fn list_by_application(
		&self,
		application_id: ApplicationId,
	) -> DomainResult<Vec<Deployment>> {
		Ok(
			DeploymentModel::list_by_application(self.0, application_id.0)
				.await
				.map_err(|e| to_infra(&e))?
				.into_iter()
				.map(map_deployment)
				.collect(),
		)
	}

	async fn list_running(&self) -> DomainResult<Vec<Deployment>> {
		Ok(DeploymentModel::list_running(self.0)
			.await
			.map_err(|e| to_infra(&e))?
			.into_iter()
			.map(map_deployment)
			.collect())
	}

	async fn save(&self, deployment: &Deployment) -> DomainResult<()> {
		DeploymentModel::create(
			self.0,
			NewDeployment {
				id: deployment.id.0,
				application_id: deployment.application_id.0,
				triggered_by: deployment.triggered_by.map(|u| u.0),
			},
		)
		.await
		.map_err(|e| to_infra(&e))?;
		Ok(())
	}

	async fn update(&self, deployment: &Deployment) -> DomainResult<()> {
		DeploymentModel::update(
			self.0,
			DeploymentUpdate {
				id: deployment.id.0,
				status: deployment.status.as_str().to_string(),
				commit_sha: deployment.commit_sha.clone(),
				image_tag: deployment.image_tag.clone(),
				container_id: deployment.container_id.clone(),
				error_message: deployment.error_message.clone(),
				started_at: deployment.started_at,
				finished_at: deployment.finished_at,
			},
		)
		.await
		.map_err(|e| to_infra(&e))?;
		Ok(())
	}
}
