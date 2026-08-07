use crate::domain::project::entity::Project;
use crate::domain::shared::errors::DomainResult;
use crate::domain::shared::ids::{ProjectId, TeamId};
use async_trait::async_trait;

/// Persistence contract for [`Project`], implemented by the storage adapter.
#[async_trait]
pub trait ProjectRepository: Send + Sync {
	/// Looks up a project by id.
	async fn find_by_id(&self, id: ProjectId) -> DomainResult<Option<Project>>;

	/// Lists all projects belonging to a team.
	async fn list_by_team(&self, team_id: TeamId) -> DomainResult<Vec<Project>>;

	/// Inserts or updates a project.
	async fn save(&self, project: &Project) -> DomainResult<()>;

	/// Deletes a project by id.
	async fn delete(&self, id: ProjectId) -> DomainResult<()>;
}
