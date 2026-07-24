use crate::domain::shared::errors::DomainResult;
use crate::domain::shared::ids::TeamId;
use crate::domain::team::entity::Team;
use async_trait::async_trait;

#[async_trait]
pub trait TeamRepository: Send + Sync {
	async fn find_by_id(&self, id: TeamId) -> DomainResult<Option<Team>>;

	async fn find_by_slug(&self, slug: &str) -> DomainResult<Option<Team>>;

	async fn save(&self, team: &Team) -> DomainResult<()>;

	async fn delete(&self, id: TeamId) -> DomainResult<()>;
}
