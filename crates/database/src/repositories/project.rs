use async_trait::async_trait;
use podkit_core::domain::project::Project;
use podkit_core::domain::project::repository::ProjectRepository;
use podkit_core::domain::shared::errors::{DomainError, DomainResult};
use podkit_core::domain::shared::ids::{ProjectId, TeamId};

use crate::PgPool;
use crate::models::project::{NewProject, ProjectModel};

/// Postgres-backed `ProjectRepository`.
pub struct PgProjectRepository(pub &'static PgPool);

fn to_infra(e: &crate::DatabaseError) -> DomainError {
	DomainError::Infrastructure(e.to_string())
}

fn map_project(row: ProjectModel) -> Project {
	Project {
		id: ProjectId(row.id),
		team_id: TeamId(row.team_id),
		name: row.name,
		slug: row.slug,
		created_at: row.created_at,
		updated_at: row.updated_at,
	}
}

#[async_trait]
impl ProjectRepository for PgProjectRepository {
	async fn find_by_id(&self, id: ProjectId) -> DomainResult<Option<Project>> {
		Ok(ProjectModel::find_by_id(self.0, id.0)
			.await
			.map_err(|e| to_infra(&e))?
			.map(map_project))
	}

	async fn list_by_team(&self, team_id: TeamId) -> DomainResult<Vec<Project>> {
		Ok(ProjectModel::list_by_team(self.0, team_id.0)
			.await
			.map_err(|e| to_infra(&e))?
			.into_iter()
			.map(map_project)
			.collect())
	}

	async fn save(&self, project: &Project) -> DomainResult<()> {
		ProjectModel::create(
			self.0,
			NewProject {
				id: project.id.0,
				team_id: project.team_id.0,
				name: project.name.clone(),
				slug: project.slug.clone(),
			},
		)
		.await
		.map_err(|e| to_infra(&e))?;
		Ok(())
	}

	async fn delete(&self, id: ProjectId) -> DomainResult<()> {
		ProjectModel::delete(self.0, id.0)
			.await
			.map_err(|e| to_infra(&e))
	}
}
