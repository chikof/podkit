use crate::domain::project::entity::Project;
use crate::domain::shared::errors::DomainResult;
use crate::domain::shared::ids::{ProjectId, TeamId};
use async_trait::async_trait;

#[async_trait]
pub trait ProjectRepository: Send + Sync {
	async fn find_by_id(&self, id: ProjectId) -> DomainResult<Option<Project>>;

	async fn list_by_team(&self, team_id: TeamId) -> DomainResult<Vec<Project>>;

	async fn save(&self, project: &Project) -> DomainResult<()>;

	async fn delete(&self, id: ProjectId) -> DomainResult<()>;
}
