use crate::domain::shared::errors::DomainResult;
use crate::domain::shared::ids::{TeamId, UserId};
use async_trait::async_trait;

#[async_trait]
pub trait Authorizer: Send + Sync {
	async fn can(
		&self,
		user_id: UserId,
		team_id: TeamId,
		action: &str,
		resource: &str,
	) -> DomainResult<bool>;
}
