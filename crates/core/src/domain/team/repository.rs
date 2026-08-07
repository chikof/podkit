use crate::domain::shared::errors::DomainResult;
use crate::domain::shared::ids::TeamId;
use crate::domain::team::entity::Team;
use async_trait::async_trait;

/// Persistence contract for [`Team`], implemented by the storage adapter.
#[async_trait]
pub trait TeamRepository: Send + Sync {
	/// Looks up a team by id.
	async fn find_by_id(&self, id: TeamId) -> DomainResult<Option<Team>>;

	/// Looks up a team by its unique slug.
	async fn find_by_slug(&self, slug: &str) -> DomainResult<Option<Team>>;

	/// Inserts or updates a team.
	async fn save(&self, team: &Team) -> DomainResult<()>;

	/// Deletes a team by id.
	async fn delete(&self, id: TeamId) -> DomainResult<()>;
}
