use crate::domain::shared::errors::DomainResult;
use crate::domain::shared::ids::{TeamId, UserId};
use crate::domain::team_member::entity::TeamMember;
use async_trait::async_trait;

/// Persistence contract for [`TeamMember`], implemented by the storage adapter.
#[async_trait]
pub trait TeamMemberRepository: Send + Sync {
	/// Looks up a membership by team and user.
	async fn find_by_team_and_user(
		&self,
		team_id: TeamId,
		user_id: UserId,
	) -> DomainResult<Option<TeamMember>>;

	/// Lists all members of a team.
	async fn list_by_team(&self, team_id: TeamId) -> DomainResult<Vec<TeamMember>>;

	/// Inserts or updates a membership.
	async fn save(&self, member: &TeamMember) -> DomainResult<()>;

	/// Removes a user from a team.
	async fn delete(&self, team_id: TeamId, user_id: UserId) -> DomainResult<()>;
}
