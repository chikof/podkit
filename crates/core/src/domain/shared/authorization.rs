use crate::domain::shared::errors::DomainResult;
use crate::domain::shared::ids::{TeamId, UserId};
use async_trait::async_trait;

/// Checks whether a user is allowed to perform an action on a resource within a team.
#[async_trait]
pub trait Authorizer: Send + Sync {
	/// Returns `true` if `user_id` may perform `action` on `resource` within `team_id`.
	async fn can(
		&self,
		user_id: UserId,
		team_id: TeamId,
		action: &str,
		resource: &str,
	) -> DomainResult<bool>;
}
