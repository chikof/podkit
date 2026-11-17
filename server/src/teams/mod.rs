//! Teams: the tenancy root. Every other resource belongs to one.

mod routes;

pub use routes::router;

use sqlx::PgPool;

use crate::ids::{RoleId, ServerId, TeamId, TeamMemberId, UserId};
use crate::servers;

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Team {
	pub id: TeamId,
	pub name: String,
	pub slug: String,
	pub logo: String,
}

/// Lists every team a user is a member of.
pub async fn list_for_user(
	db: impl sqlx::PgExecutor<'_>,
	user: UserId,
) -> Result<Vec<Team>, sqlx::Error> {
	sqlx::query_as!(
		Team,
		r#"
			SELECT teams.id AS "id: TeamId", teams.name, teams.slug, teams.logo
			FROM teams
			JOIN team_members ON teams.id = team_members.team_id
			WHERE team_members.user_id = $1
		"#,
		user as UserId
	)
	.fetch_all(db)
	.await
}

/// Creates a team and assigns `owner` the global `Owner` role, in one transaction.
///
/// # Errors
/// Fails if the transaction can't be committed, or if the `Owner` role is missing
/// (it's seeded by migration `0009_seed_rbac_defaults` and should always exist).
pub async fn create_with_owner(
	db: &PgPool,
	name: &str,
	slug: &str,
	logo: &str,
	owner: UserId,
) -> Result<Team, sqlx::Error> {
	let mut tx = db.begin().await?;

	let team = sqlx::query_as!(
		Team,
		r#"
			INSERT INTO teams (id, name, slug, logo)
			VALUES ($1, $2, $3, $4)
			RETURNING id AS "id: TeamId", name, slug, logo
		"#,
		TeamId::generate() as TeamId,
		name,
		slug,
		logo
	)
	.fetch_one(&mut *tx)
	.await?;

	let owner_role_id = sqlx::query_scalar!(
		r#"SELECT id AS "id: RoleId" FROM roles WHERE team_id IS NULL AND name = 'Owner'"#
	)
	.fetch_one(&mut *tx)
	.await?;

	sqlx::query!(
		"INSERT INTO team_members (id, team_id, user_id, role_id) VALUES ($1, $2, $3, $4)",
		TeamMemberId::generate() as TeamMemberId,
		team.id as TeamId,
		owner as UserId,
		owner_role_id as RoleId
	)
	.execute(&mut *tx)
	.await?;

	// Every team gets its local (podkit-host) server auto-provisioned.
	// Local servers hold no ssh credentials and are active immediately,
	// no connectivity probe needed.
	sqlx::query!(
		r#"
			INSERT INTO servers (id, team_id, name, hostname, podman_socket_path, is_local, status)
			VALUES ($1, $2, 'local', 'localhost', $3, TRUE, 'active')
		"#,
		ServerId::generate() as ServerId,
		team.id as TeamId,
		servers::local_podman_socket()
	)
	.execute(&mut *tx)
	.await?;

	tx.commit().await?;

	Ok(team)
}
