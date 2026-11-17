use sqlx::PgPool;

use crate::error::ServerError;
use crate::ids::{TeamId, UserId};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
	Create,
	Read,
	Update,
	Delete,
}

impl Action {
	pub fn as_str(self) -> &'static str {
		match self {
			Self::Create => "create",
			Self::Read => "read",
			Self::Update => "update",
			Self::Delete => "delete",
		}
	}
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resource {
	#[allow(dead_code, reason = "consumed by the agent and data-service slices")]
	Team,
	Project,
	Membership,
	Role,
	Server,
	Application,
	Deployment,
	EnvVar,
	Domain,
	Database,
	Volume,
	Backup,
	ModelFile,
	Model,
}

impl Resource {
	pub fn as_str(self) -> &'static str {
		match self {
			Self::Team => "team",
			Self::Project => "project",
			Self::Membership => "membership",
			Self::Role => "role",
			Self::Server => "server",
			Self::Application => "application",
			Self::Deployment => "deployment",
			Self::EnvVar => "env_var",
			Self::Domain => "domain",
			Self::Database => "database",
			Self::Volume => "volume",
			Self::Backup => "backup",
			Self::ModelFile => "model_file",
			Self::Model => "model",
		}
	}
}

/// Checks that `user` holds `action` on `resource` through their role in `team`.
///
/// # Errors
/// `Forbidden` when the user is not a member or their role lacks the permission.
pub async fn authorize(
	db: &PgPool,
	user: UserId,
	team: TeamId,
	action: Action,
	resource: Resource,
) -> Result<(), ServerError> {
	let allowed = sqlx::query_scalar!(
		r#"
			SELECT EXISTS (
				SELECT 1 FROM team_members tm
				JOIN role_permissions rp ON rp.role_id = tm.role_id
				JOIN permissions p ON p.id = rp.permission_id
				WHERE tm.team_id = $1 AND tm.user_id = $2 AND p.action = $3 AND p.resource = $4
			) AS "exists!"
		"#,
		team.0,
		user.0,
		action.as_str(),
		resource.as_str()
	)
	.fetch_one(db)
	.await?;

	if allowed {
		Ok(())
	} else {
		Err(ServerError::Forbidden)
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[sqlx::test]
	async fn owner_can_manage_but_member_can_only_read(pool: PgPool) {
		sqlx::query(
			"INSERT INTO users (id, name, email, password_hash) VALUES
				(1001, 'Owner', 'owner@test.dev', 'x'),
				(1002, 'Member', 'member@test.dev', 'x')",
		)
		.execute(&pool)
		.await
		.unwrap();

		sqlx::query(
			"INSERT INTO teams (id, name, slug, logo) VALUES (2001, 'Test', 'test-team', '')",
		)
		.execute(&pool)
		.await
		.unwrap();

		sqlx::query(
			"INSERT INTO team_members (id, team_id, user_id, role_id) VALUES
				(3001, 2001, 1001, 1),
				(3002, 2001, 1002, 2)",
		)
		.execute(&pool)
		.await
		.unwrap();

		let (owner, member, team) = (UserId(1001), UserId(1002), TeamId(2001));

		authorize(&pool, owner, team, Action::Delete, Resource::Team)
			.await
			.unwrap();
		authorize(&pool, member, team, Action::Read, Resource::Team)
			.await
			.unwrap();
		let denied = authorize(&pool, member, team, Action::Delete, Resource::Team)
			.await
			.unwrap_err();
		assert!(matches!(denied, ServerError::Forbidden));

		let outsider = authorize(&pool, UserId(9999), team, Action::Read, Resource::Team)
			.await
			.unwrap_err();
		assert!(matches!(outsider, ServerError::Forbidden));
	}
}
