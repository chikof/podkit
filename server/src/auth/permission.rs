use std::collections::HashSet;

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

/// Everything a user may do in one team, loaded once so a page can hide the
/// actions they lack. `authorize` stays the gate for every mutation.
#[derive(Debug, Clone, Default)]
pub struct Permissions {
	member: bool,
	granted: HashSet<(&'static str, &'static str)>,
}

impl Permissions {
	/// False for someone who is not in the team at all.
	pub fn is_member(&self) -> bool {
		self.member
	}

	pub fn can(&self, action: Action, resource: Resource) -> bool {
		self.granted.contains(&(action.as_str(), resource.as_str()))
	}
}

const ACTIONS: [Action; 4] = [Action::Create, Action::Read, Action::Update, Action::Delete];

const RESOURCES: [Resource; 14] = [
	Resource::Team,
	Resource::Project,
	Resource::Membership,
	Resource::Role,
	Resource::Server,
	Resource::Application,
	Resource::Deployment,
	Resource::EnvVar,
	Resource::Domain,
	Resource::Database,
	Resource::Volume,
	Resource::Backup,
	Resource::ModelFile,
	Resource::Model,
];

/// Loads `user`'s permission set in `team` with one query. A non-member gets an
/// empty set with `is_member() == false`.
///
/// # Errors
/// Database failures only.
pub async fn granted(db: &PgPool, user: UserId, team: TeamId) -> Result<Permissions, ServerError> {
	let rows = sqlx::query!(
		r#"
			SELECT p.action AS "action?", p.resource AS "resource?"
			FROM team_members tm
			LEFT JOIN role_permissions rp ON rp.role_id = tm.role_id
			LEFT JOIN permissions p ON p.id = rp.permission_id
			WHERE tm.team_id = $1 AND tm.user_id = $2
		"#,
		team.0,
		user.0
	)
	.fetch_all(db)
	.await?;

	let mut perms = Permissions {
		member: !rows.is_empty(),
		granted: HashSet::new(),
	};
	for row in rows {
		let (Some(action), Some(resource)) = (row.action, row.resource) else {
			continue;
		};
		let action = ACTIONS.into_iter().find(|a| a.as_str() == action);
		let resource = RESOURCES.into_iter().find(|r| r.as_str() == resource);
		if let (Some(action), Some(resource)) = (action, resource) {
			perms.granted.insert((action.as_str(), resource.as_str()));
		}
	}
	Ok(perms)
}

#[cfg(test)]
mod tests {
	use super::*;

	async fn seed(pool: &PgPool) {
		sqlx::query(
			"INSERT INTO users (id, name, email, password_hash) VALUES
				(1001, 'Owner', 'owner@test.dev', 'x'),
				(1002, 'Member', 'member@test.dev', 'x')",
		)
		.execute(pool)
		.await
		.unwrap();
		sqlx::query(
			"INSERT INTO teams (id, name, slug, logo) VALUES (2001, 'Test', 'test-team', '')",
		)
		.execute(pool)
		.await
		.unwrap();
		sqlx::query(
			"INSERT INTO team_members (id, team_id, user_id, role_id) VALUES
				(3001, 2001, 1001, 1),
				(3002, 2001, 1002, 2)",
		)
		.execute(pool)
		.await
		.unwrap();
	}

	#[sqlx::test]
	async fn granted_matches_authorize_for_every_pair(pool: PgPool) {
		seed(&pool).await;
		for user in [UserId(1001), UserId(1002)] {
			let perms = granted(&pool, user, TeamId(2001)).await.unwrap();
			assert!(perms.is_member());
			for action in ACTIONS {
				for resource in RESOURCES {
					let allowed = authorize(&pool, user, TeamId(2001), action, resource)
						.await
						.is_ok();
					assert_eq!(
						perms.can(action, resource),
						allowed,
						"{action:?} {resource:?}"
					);
				}
			}
		}
	}

	#[sqlx::test]
	async fn granted_owner_can_delete_but_member_cannot(pool: PgPool) {
		seed(&pool).await;
		let owner = granted(&pool, UserId(1001), TeamId(2001)).await.unwrap();
		let member = granted(&pool, UserId(1002), TeamId(2001)).await.unwrap();
		assert!(owner.can(Action::Delete, Resource::Project));
		assert!(member.can(Action::Read, Resource::Project));
		assert!(!member.can(Action::Delete, Resource::Project));
	}

	#[sqlx::test]
	async fn granted_non_member_has_nothing(pool: PgPool) {
		seed(&pool).await;
		let outsider = granted(&pool, UserId(9999), TeamId(2001)).await.unwrap();
		assert!(!outsider.is_member());
		assert!(!outsider.can(Action::Read, Resource::Project));
	}

	#[sqlx::test]
	async fn granted_member_whose_role_has_no_permissions_is_still_a_member(pool: PgPool) {
		seed(&pool).await;
		sqlx::query("INSERT INTO roles (id, team_id, name) VALUES (4001, 2001, 'Empty')")
			.execute(&pool)
			.await
			.unwrap();
		sqlx::query("UPDATE team_members SET role_id = 4001 WHERE id = 3002")
			.execute(&pool)
			.await
			.unwrap();
		let perms = granted(&pool, UserId(1002), TeamId(2001)).await.unwrap();
		assert!(perms.is_member());
		assert!(!perms.can(Action::Read, Resource::Project));
	}

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
