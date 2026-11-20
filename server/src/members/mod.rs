//! Team membership and roles. Which role a member holds decides what they may do.

mod routes;
pub mod service;

pub use routes::router;

use time::OffsetDateTime;

use crate::error::ServerError;
use crate::ids::{RoleId, TeamId, TeamMemberId, UserId};

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct TeamMember {
	pub user_id: UserId,
	pub role_id: RoleId,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Role {
	pub id: RoleId,
	/// `None` for a built-in role, shared across every team.
	pub team_id: Option<TeamId>,
	pub name: String,
}

impl Role {
	/// True for built-in roles, shared across every team.
	pub fn is_builtin(&self) -> bool {
		self.team_id.is_none()
	}
}

/// A membership joined with the user and role it references.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct MemberDetail {
	pub user_id: UserId,
	pub email: String,
	pub display_name: String,
	pub role_id: RoleId,
	pub role_name: String,
	pub joined_at: OffsetDateTime,
	/// Hash of the member's picture, when they have one (see `avatars`).
	pub avatar: Option<String>,
}

pub async fn find(
	db: impl sqlx::PgExecutor<'_>,
	team: TeamId,
	user: UserId,
) -> Result<Option<TeamMember>, sqlx::Error> {
	sqlx::query_as!(
		TeamMember,
		r#"
			SELECT user_id AS "user_id: UserId", role_id AS "role_id: RoleId"
			FROM team_members
			WHERE team_id = $1 AND user_id = $2
		"#,
		team as TeamId,
		user as UserId
	)
	.fetch_optional(db)
	.await
}

pub async fn insert(
	db: impl sqlx::PgExecutor<'_>,
	team: TeamId,
	user: UserId,
	role: RoleId,
) -> Result<TeamMember, sqlx::Error> {
	sqlx::query_as!(
		TeamMember,
		r#"
			INSERT INTO team_members (id, team_id, user_id, role_id)
			VALUES ($1, $2, $3, $4)
			RETURNING user_id AS "user_id: UserId", role_id AS "role_id: RoleId"
		"#,
		TeamMemberId::generate() as TeamMemberId,
		team as TeamId,
		user as UserId,
		role as RoleId
	)
	.fetch_one(db)
	.await
}

pub async fn set_role(
	db: impl sqlx::PgExecutor<'_>,
	team: TeamId,
	user: UserId,
	role: RoleId,
) -> Result<(), sqlx::Error> {
	sqlx::query!(
		"UPDATE team_members SET role_id = $1 WHERE team_id = $2 AND user_id = $3",
		role as RoleId,
		team as TeamId,
		user as UserId
	)
	.execute(db)
	.await?;
	Ok(())
}

pub async fn list_details(
	db: impl sqlx::PgExecutor<'_>,
	team: TeamId,
) -> Result<Vec<MemberDetail>, sqlx::Error> {
	sqlx::query_as!(
		MemberDetail,
		r#"
			SELECT m.user_id AS "user_id: UserId", u.email AS "email!",
				u.name AS "display_name!", m.role_id AS "role_id: RoleId",
				r.name AS "role_name!", m.joined_at,
				(SELECT hash FROM user_avatars WHERE user_id = u.id) AS avatar
			FROM team_members m
			JOIN users u ON u.id = m.user_id
			JOIN roles r ON r.id = m.role_id
			WHERE m.team_id = $1
			ORDER BY m.joined_at, m.id
		"#,
		team as TeamId
	)
	.fetch_all(db)
	.await
}

/// Lists every role usable by a team: built-in roles plus the team's own.
pub async fn list_roles(
	db: impl sqlx::PgExecutor<'_>,
	team: TeamId,
) -> Result<Vec<Role>, sqlx::Error> {
	sqlx::query_as!(
		Role,
		r#"
			SELECT id AS "id: RoleId", team_id AS "team_id?: TeamId", name
			FROM roles
			WHERE team_id IS NULL OR team_id = $1
		"#,
		team as TeamId
	)
	.fetch_all(db)
	.await
}

/// Fails with `RoleNotAllowed` unless `role` exists and is built-in or owned by `team`.
pub async fn ensure_assignable(
	db: impl sqlx::PgExecutor<'_>,
	team: TeamId,
	role: RoleId,
) -> Result<(), ServerError> {
	let found = sqlx::query_scalar!(
		r#"
			SELECT EXISTS (
				SELECT 1 FROM roles WHERE id = $1 AND (team_id IS NULL OR team_id = $2)
			) AS "exists!"
		"#,
		role as RoleId,
		team as TeamId
	)
	.fetch_one(db)
	.await?;

	if found {
		Ok(())
	} else {
		Err(ServerError::RoleNotAllowed)
	}
}
