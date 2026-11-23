//! API token use cases shared by the JSON API and the web UI.

use sha2::{Digest, Sha256};
use sqlx::PgPool;
use time::{Duration, OffsetDateTime};

use super::{ApiToken, Grant, PREFIX, Scope};
use crate::audit::{self, Diff, Target};
use crate::auth::permission;
use crate::db::is_unique_violation;
use crate::error::{AppResult, ServerError};
use crate::ids::{ApiTokenId, ProjectId, TeamId, UserId};
use crate::scope;

const MAX_NAME_LEN: usize = 60;
const MAX_PROJECTS: usize = 100;
/// `last_used_at` is written at most this often per token.
const LAST_USED_EVERY: Duration = Duration::minutes(1);
const NAME_CONSTRAINT: &str = "api_tokens_user_id_team_id_name_key";
/// Hex characters of the public prefix and of the 32-byte secret.
const PREFIX_LEN: usize = 8;
const SECRET_LEN: usize = 64;

/// What a new token is made from.
pub struct CreateInput {
	pub name: String,
	pub scopes: Vec<Scope>,
	/// Empty means every project of the team.
	pub project_ids: Vec<ProjectId>,
	pub expires_at: Option<OffsetDateTime>,
}

/// A new token and the only copy of its secret.
pub struct Created {
	pub token: ApiToken,
	/// The whole `pk_<prefix>_<secret>` string.
	pub secret: String,
}

fn digest(secret: &str) -> Vec<u8> {
	Sha256::digest(secret.as_bytes()).to_vec()
}

fn parse_scopes(raw: &[String]) -> Vec<Scope> {
	raw.iter().filter_map(|s| Scope::parse(s)).collect()
}

struct Row {
	id: ApiTokenId,
	name: String,
	prefix: String,
	scopes: Vec<String>,
	project_ids: Vec<i64>,
	expires_at: Option<OffsetDateTime>,
	last_used_at: Option<OffsetDateTime>,
	created_at: OffsetDateTime,
}

impl From<Row> for ApiToken {
	fn from(r: Row) -> Self {
		Self {
			id: r.id,
			name: r.name,
			prefix: r.prefix,
			scopes: parse_scopes(&r.scopes),
			project_ids: r.project_ids.into_iter().map(ProjectId).collect(),
			expires_at: r.expires_at,
			last_used_at: r.last_used_at,
			created_at: r.created_at,
		}
	}
}

fn validate(body: &CreateInput) -> AppResult<()> {
	let name = body.name.trim();
	if name.is_empty() || name.chars().count() > MAX_NAME_LEN || name.chars().any(char::is_control)
	{
		return Err(ServerError::Validation(format!(
			"Name must be 1 to {MAX_NAME_LEN} characters."
		)));
	}
	if body.scopes.is_empty() {
		return Err(ServerError::Validation(
			"Pick at least one scope.".to_owned(),
		));
	}
	if body.project_ids.len() > MAX_PROJECTS {
		return Err(ServerError::Validation(format!(
			"A token can be limited to at most {MAX_PROJECTS} projects."
		)));
	}
	if body
		.expires_at
		.is_some_and(|t| t <= OffsetDateTime::now_utc())
	{
		return Err(ServerError::Validation(
			"The expiry must be in the future.".to_owned(),
		));
	}
	Ok(())
}

/// Makes a token for `user` in `team`. Any member may; the token can never do more than
/// the user can. Projects it is limited to must be the team's own (another team's is a
/// 404).
pub async fn create(
	db: &PgPool,
	user: UserId,
	team: TeamId,
	mut body: CreateInput,
) -> AppResult<Created> {
	if !permission::granted(db, user, team).await?.is_member() {
		return Err(ServerError::Forbidden);
	}
	validate(&body)?;
	body.scopes
		.sort_by_key(|s| Scope::ALL.iter().position(|a| a == s));
	body.scopes.dedup();
	body.project_ids.sort_by_key(|p| p.0);
	body.project_ids.dedup();
	for project in &body.project_ids {
		scope::project(db, team, *project).await?;
	}

	let prefix = crypto::generate_token(PREFIX_LEN / 2);
	let secret = crypto::generate_token(SECRET_LEN / 2);
	let scopes: Vec<String> = body.scopes.iter().map(|s| s.as_str().to_owned()).collect();
	let projects: Vec<i64> = body.project_ids.iter().map(|p| p.0).collect();
	let row = sqlx::query_as!(
		Row,
		r#"
			INSERT INTO api_tokens
				(id, team_id, user_id, name, prefix, hash, scopes, project_ids, expires_at)
			VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
			RETURNING id AS "id: ApiTokenId",
				name, prefix, scopes, project_ids, expires_at,
				last_used_at, created_at
		"#,
		ApiTokenId::generate() as ApiTokenId,
		team as TeamId,
		user as UserId,
		body.name.trim(),
		prefix,
		digest(&secret),
		&scopes,
		&projects,
		body.expires_at,
	)
	.fetch_one(db)
	.await
	.map_err(|e| {
		if is_unique_violation(&e, NAME_CONSTRAINT) {
			ServerError::Conflict("you already have a token with that name".to_owned())
		} else {
			e.into()
		}
	})?;
	let token: ApiToken = row.into();
	audit::log(
		db,
		user,
		team,
		"api_token.create",
		Target::new("api_token", token.id).named(&token.name),
		Diff::new()
			.set("name", token.name.clone())
			.set(
				"scopes",
				token.scopes.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
			)
			.set(
				"project_ids",
				token.project_ids.iter().map(|p| p.0).collect::<Vec<_>>(),
			)
			.set(
				"expires_at",
				token.expires_at.map(OffsetDateTime::unix_timestamp),
			)
			.secret("secret"),
	)
	.await;
	Ok(Created {
		token,
		secret: format!("{PREFIX}{prefix}_{secret}"),
	})
}

/// The caller's own tokens in the team, newest first. Never the secrets: they are not
/// stored.
pub async fn list(db: &PgPool, user: UserId, team: TeamId) -> AppResult<Vec<ApiToken>> {
	if !permission::granted(db, user, team).await?.is_member() {
		return Err(ServerError::Forbidden);
	}
	let rows = sqlx::query_as!(
		Row,
		r#"
			SELECT id AS "id: ApiTokenId",
				name, prefix, scopes, project_ids, expires_at,
				last_used_at, created_at
			FROM api_tokens WHERE team_id = $1 AND user_id = $2 ORDER BY created_at DESC, id DESC
		"#,
		team as TeamId,
		user as UserId,
	)
	.fetch_all(db)
	.await?;
	Ok(rows.into_iter().map(Into::into).collect())
}

/// Deletes one of the caller's tokens; it stops working at once. Someone else's token, or
/// one from another team, is `TokenNotFound`.
pub async fn revoke(
	db: &PgPool,
	user: UserId,
	team: TeamId,
	id: ApiTokenId,
) -> AppResult<ApiTokenId> {
	if !permission::granted(db, user, team).await?.is_member() {
		return Err(ServerError::Forbidden);
	}
	let gone = sqlx::query!(
		"DELETE FROM api_tokens WHERE id = $1 AND team_id = $2 AND user_id = $3",
		id as ApiTokenId,
		team as TeamId,
		user as UserId,
	)
	.execute(db)
	.await?
	.rows_affected();
	if gone == 0 {
		return Err(ServerError::TokenNotFound);
	}
	audit::log(
		db,
		user,
		team,
		"api_token.revoke",
		Target::new("api_token", id),
		Diff::new(),
	)
	.await;
	Ok(id)
}

/// Splits `pk_<prefix>_<secret>` and checks its shape, so a malformed string never
/// reaches the database.
fn split(raw: &str) -> Option<(&str, &str)> {
	let (prefix, secret) = raw.strip_prefix(PREFIX)?.split_once('_')?;
	let hex = |s: &str, len: usize| s.len() == len && s.bytes().all(|b| b.is_ascii_hexdigit());
	(hex(prefix, PREFIX_LEN) && hex(secret, SECRET_LEN)).then_some((prefix, secret))
}

/// The grant behind a presented token: `InvalidToken` when it is malformed, unknown or
/// revoked, `TokenExpired` past its expiry. Writes `last_used_at` at most once a minute.
pub async fn resolve(db: &PgPool, raw: &str) -> Result<Grant, ServerError> {
	let (prefix, secret) = split(raw).ok_or(ServerError::InvalidToken)?;
	let row = sqlx::query!(
		r#"
			SELECT id AS "id: ApiTokenId", team_id AS "team_id: TeamId",
				user_id AS "user_id: UserId", scopes, project_ids, expires_at, last_used_at
			FROM api_tokens WHERE prefix = $1 AND hash = $2
		"#,
		prefix,
		digest(secret),
	)
	.fetch_optional(db)
	.await?
	.ok_or(ServerError::InvalidToken)?;

	let now = OffsetDateTime::now_utc();
	if row.expires_at.is_some_and(|t| t <= now) {
		return Err(ServerError::TokenExpired);
	}
	if row.last_used_at.is_none_or(|t| now - t >= LAST_USED_EVERY)
		&& let Err(e) = sqlx::query!(
			"UPDATE api_tokens SET last_used_at = NOW()
			WHERE id = $1 AND (last_used_at IS NULL OR last_used_at < NOW() - INTERVAL '1 minute')",
			row.id as ApiTokenId,
		)
		.execute(db)
		.await
	{
		tracing::warn!("failed to record API token use: {e}");
	}
	Ok(Grant {
		token: row.id,
		team: row.team_id,
		user: row.user_id,
		scopes: parse_scopes(&row.scopes),
		projects: row.project_ids.into_iter().map(ProjectId).collect(),
	})
}

#[cfg(test)]
mod unit {
	use super::*;

	#[test]
	fn token_shape_is_checked_before_the_database() {
		let secret = "a".repeat(SECRET_LEN);
		let ok = format!("pk_12345678_{secret}");
		assert_eq!(split(&ok), Some(("12345678", secret.as_str())));
		for bad in [
			"",
			"pk_",
			"pk_12345678",
			"pk_1234567_aaaa",
			&format!("pk_12345678_{}", "a".repeat(SECRET_LEN - 1)),
			&format!("pk_1234567z_{secret}"),
			&format!("xx_12345678_{secret}"),
			&format!("pk_12345678_{}z", "a".repeat(SECRET_LEN - 1)),
		] {
			assert!(split(bad).is_none(), "{bad:?}");
		}
	}
}
