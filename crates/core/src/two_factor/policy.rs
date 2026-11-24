//! The team policy "require two-step verification for members". Switching it on starts a
//! grace period; once that ends, a member with no second factor is let into nothing of the
//! team except their own Account pages, where they can set one up.

use sqlx::PgPool;
use time::{Duration, OffsetDateTime};

use crate::audit::{self, Diff, Target};
use crate::auth::permission::{Action, Resource, authorize};
use crate::error::{AppResult, ServerError};
use crate::ids::{TeamId, UserId};

/// The longest grace period.
pub const MAX_GRACE_DAYS: i32 = 90;

#[derive(Debug, Clone, Copy)]
pub struct Policy {
	pub required: bool,
	pub grace_days: i32,
	/// When members without a second factor are shut out; set while the policy is on.
	pub deadline: Option<OffsetDateTime>,
}

/// Where one member stands under the policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Standing {
	/// Nothing is asked of them: no policy, or they have a second factor.
	Fine,
	/// The policy is on and they have until `deadline`.
	Grace(OffsetDateTime),
	/// The deadline passed and they have no second factor.
	Blocked,
}

struct Row {
	required: bool,
	grace_days: i32,
	deadline: Option<OffsetDateTime>,
	enrolled: bool,
}

async fn row(db: &PgPool, user: UserId, team: TeamId) -> Result<Option<Row>, sqlx::Error> {
	sqlx::query_as!(
		Row,
		r#"
			SELECT t.require_2fa AS "required!", t.require_2fa_grace_days AS "grace_days!",
				t.require_2fa_deadline AS deadline,
				(
					EXISTS (SELECT 1 FROM totp_credentials WHERE user_id = $2 AND confirmed_at IS NOT NULL)
					OR EXISTS (SELECT 1 FROM passkeys WHERE user_id = $2)
				) AS "enrolled!"
			FROM teams t WHERE t.id = $1
		"#,
		team.0,
		user.0
	)
	.fetch_optional(db)
	.await
}

/// Where `user` stands in `team`.
pub async fn standing(db: &PgPool, user: UserId, team: TeamId) -> Result<Standing, sqlx::Error> {
	let Some(row) = row(db, user, team).await? else {
		return Ok(Standing::Fine);
	};
	if !row.required || row.enrolled {
		return Ok(Standing::Fine);
	}
	Ok(match row.deadline {
		Some(deadline) if deadline > OffsetDateTime::now_utc() => Standing::Grace(deadline),
		_ => Standing::Blocked,
	})
}

/// Whether the policy shuts `user` out of `team` right now.
pub async fn blocks(db: &PgPool, user: UserId, team: TeamId) -> Result<bool, sqlx::Error> {
	Ok(standing(db, user, team).await? == Standing::Blocked)
}

/// The team's policy; any member may read it.
pub async fn get(db: &PgPool, user: UserId, team: TeamId) -> AppResult<Policy> {
	authorize(db, user, team, Action::Read, Resource::Team).await?;
	let row = row(db, user, team).await?.ok_or(ServerError::NotFound)?;
	Ok(Policy {
		required: row.required,
		grace_days: row.grace_days,
		deadline: row.deadline,
	})
}

/// Switches the policy on or off; needs `update team`. Switching on (or changing the grace
/// period) restarts the clock: members have `grace_days` from now.
pub async fn set(
	db: &PgPool,
	user: UserId,
	team: TeamId,
	required: bool,
	grace_days: i32,
) -> AppResult<Policy> {
	authorize(db, user, team, Action::Update, Resource::Team).await?;
	if !(0..=MAX_GRACE_DAYS).contains(&grace_days) {
		return Err(ServerError::Validation(format!(
			"The grace period is 0 to {MAX_GRACE_DAYS} days."
		)));
	}
	let before = row(db, user, team).await?.ok_or(ServerError::NotFound)?;
	// Switching it on with no grace period would shut the person who does it out of their
	// own team's settings, with no way back in but another Owner.
	if required && grace_days == 0 && !before.enrolled {
		return Err(ServerError::Validation(
			"Set up your own authenticator app or passkey first, or give members a grace period."
				.to_owned(),
		));
	}
	let deadline =
		required.then(|| OffsetDateTime::now_utc() + Duration::days(i64::from(grace_days)));
	// Saving the form again without a change must not push the deadline back.
	let keep = required && before.required && before.grace_days == grace_days;
	let deadline = if keep { before.deadline } else { deadline };
	sqlx::query!(
		"UPDATE teams SET require_2fa = $2, require_2fa_grace_days = $3, require_2fa_deadline = $4
		WHERE id = $1",
		team.0,
		required,
		grace_days,
		deadline
	)
	.execute(db)
	.await?;
	audit::log(
		db,
		user,
		team,
		"team.update",
		Target::none("two_factor_policy"),
		Diff::new()
			.changed("required", before.required, required)
			.changed("grace_days", before.grace_days, grace_days),
	)
	.await;
	Ok(Policy {
		required,
		grace_days,
		deadline,
	})
}

/// Members who have no second factor yet; needs `read membership`.
pub async fn members_without(db: &PgPool, user: UserId, team: TeamId) -> AppResult<Vec<String>> {
	authorize(db, user, team, Action::Read, Resource::Membership).await?;
	Ok(sqlx::query_scalar!(
		r#"
			SELECT u.name AS "name!" FROM team_members m
			JOIN users u ON u.id = m.user_id
			WHERE m.team_id = $1
				AND NOT EXISTS (SELECT 1 FROM totp_credentials t WHERE t.user_id = u.id AND t.confirmed_at IS NOT NULL)
				AND NOT EXISTS (SELECT 1 FROM passkeys p WHERE p.user_id = u.id)
			ORDER BY u.name
		"#,
		team.0
	)
	.fetch_all(db)
	.await?)
}
