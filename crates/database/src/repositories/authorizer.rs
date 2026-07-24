use async_trait::async_trait;
use podkit_core::domain::shared::authorization::Authorizer;
use podkit_core::domain::shared::errors::{DomainError, DomainResult};
use podkit_core::domain::shared::ids::{TeamId, UserId};

use crate::PgPool;

pub struct PgAuthorizer(pub &'static PgPool);

#[async_trait]
impl Authorizer for PgAuthorizer {
	async fn can(
		&self,
		user_id: UserId,
		team_id: TeamId,
		action: &str,
		resource: &str,
	) -> DomainResult<bool> {
		let allowed = sqlx::query_scalar!(
			r#"
				SELECT EXISTS (
					SELECT 1 FROM team_members tm
					JOIN role_permissions rp ON rp.role_id = tm.role_id
					JOIN permissions p ON p.id = rp.permission_id
					WHERE tm.team_id = $1 AND tm.user_id = $2 AND p.action = $3 AND p.resource = $4
				) AS "exists!"
			"#,
			team_id.0,
			user_id.0,
			action,
			resource
		)
		.fetch_one(self.0)
		.await
		.map_err(|e| DomainError::Infrastructure(e.to_string()))?;

		Ok(allowed)
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[sqlx::test]
	async fn owner_can_manage_but_member_can_only_read(pool: sqlx::PgPool) {
		let pool: &'static PgPool = Box::leak(Box::new(pool));

		sqlx::query!(
			r#"INSERT INTO users (id, name, email, password_hash) VALUES
				(1001, 'Owner', 'owner@test.dev', 'x'),
				(1002, 'Member', 'member@test.dev', 'x')"#
		)
		.execute(pool)
		.await
		.unwrap();

		sqlx::query!(
			"INSERT INTO teams (id, name, slug, logo) VALUES (2001, 'Test', 'test-team', '')"
		)
		.execute(pool)
		.await
		.unwrap();

		sqlx::query!(
			r#"INSERT INTO team_members (id, team_id, user_id, role_id) VALUES
				(3001, 2001, 1001, 1),
				(3002, 2001, 1002, 2)"#
		)
		.execute(pool)
		.await
		.unwrap();

		let authorizer = PgAuthorizer(pool);

		assert!(
			authorizer
				.can(UserId(1001), TeamId(2001), "delete", "team")
				.await
				.unwrap()
		);
		assert!(
			authorizer
				.can(UserId(1002), TeamId(2001), "read", "team")
				.await
				.unwrap()
		);
		assert!(
			!authorizer
				.can(UserId(1002), TeamId(2001), "delete", "team")
				.await
				.unwrap()
		);
	}
}
