use crate::domain::shared::errors::DomainResult;
use crate::domain::shared::ids::{TeamId, UserId};
use crate::domain::team_member::entity::TeamMember;
use async_trait::async_trait;

#[async_trait]
pub trait TeamMemberRepository: Send + Sync {
	async fn find_by_team_and_user(
		&self,
		team_id: TeamId,
		user_id: UserId,
	) -> DomainResult<Option<TeamMember>>;

	async fn list_by_team(&self, team_id: TeamId) -> DomainResult<Vec<TeamMember>>;

	async fn save(&self, member: &TeamMember) -> DomainResult<()>;

	async fn delete(&self, team_id: TeamId, user_id: UserId) -> DomainResult<()>;
}
