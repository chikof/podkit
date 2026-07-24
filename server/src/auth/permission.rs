use podkit_core::domain::shared::authorization::Authorizer;
use podkit_core::domain::shared::ids::{TeamId, UserId};

use crate::error::ServerError;

pub async fn require_permission(
	authorizer: &dyn Authorizer,
	user_id: i64,
	team_id: i64,
	action: &str,
	resource: &str,
) -> Result<(), ServerError> {
	let allowed = authorizer
		.can(UserId(user_id), TeamId(team_id), action, resource)
		.await?;

	if !allowed {
		return Err(ServerError::Forbidden);
	}

	Ok(())
}

#[cfg(test)]
mod tests {
	use super::*;
	use async_trait::async_trait;
	use podkit_core::domain::shared::errors::{DomainError, DomainResult};

	struct FakeAuthorizer(Result<bool, ()>);

	#[async_trait]
	impl Authorizer for FakeAuthorizer {
		async fn can(&self, _: UserId, _: TeamId, _: &str, _: &str) -> DomainResult<bool> {
			self.0
				.map_err(|()| DomainError::Infrastructure("boom".to_string()))
		}
	}

	#[tokio::test]
	async fn allowed_returns_ok() {
		let authorizer = FakeAuthorizer(Ok(true));
		assert!(
			require_permission(&authorizer, 1, 1, "read", "team")
				.await
				.is_ok()
		);
	}

	#[tokio::test]
	async fn denied_returns_forbidden() {
		let authorizer = FakeAuthorizer(Ok(false));
		let err = require_permission(&authorizer, 1, 1, "read", "team")
			.await
			.unwrap_err();
		assert!(matches!(err, ServerError::Forbidden));
	}

	#[tokio::test]
	async fn repository_error_propagates() {
		let authorizer = FakeAuthorizer(Err(()));
		let err = require_permission(&authorizer, 1, 1, "read", "team")
			.await
			.unwrap_err();
		assert!(matches!(err, ServerError::Domain(_)));
	}
}
