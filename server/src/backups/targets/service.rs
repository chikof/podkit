//! Backup target use cases shared by the JSON API and the web UI: authorize,
//! then scope, then validate, then touch the database.

use std::net::{Ipv4Addr, Ipv6Addr};

use sqlx::{PgExecutor, PgPool};
use url::{Host, Url};

use crate::auth::permission::{Action, Resource, authorize};
use crate::backups::{BackupTarget, s3};
use crate::db::is_unique_violation;
use crate::error::{AppResult, ServerError};
use crate::ids::{BackupTargetId, TeamId, UserId};
use crate::state::AppState;
use crate::{scope, validate};

/// Unique constraint on `backup_targets (team_id, name)`.
const NAME_CONSTRAINT: &str = "backup_targets_team_id_name_key";

/// What a new target is made from. The secret is encrypted before it is
/// stored and never read back.
pub struct CreateInput {
	pub name: String,
	pub endpoint: String,
	pub region: String,
	pub bucket: String,
	pub path_style: bool,
	pub prefix: String,
	pub access_key_id: String,
	pub secret_access_key: String,
}

fn invalid(what: &str) -> ServerError {
	ServerError::Validation(format!("invalid {what}"))
}

/// `https`, or `http` only for the local machine, so a secret never travels
/// in clear over a network. Userinfo is refused because the endpoint is
/// stored and returned in plain text.
fn parse_endpoint(endpoint: &str) -> Result<Url, ServerError> {
	let url = Url::parse(endpoint).map_err(|_| invalid("endpoint"))?;
	if !url.username().is_empty() || url.password().is_some() {
		return Err(ServerError::Validation(
			"endpoint must not contain credentials".to_owned(),
		));
	}
	let local = match url.host() {
		Some(Host::Domain(d)) => d.eq_ignore_ascii_case("localhost"),
		Some(Host::Ipv4(ip)) => ip == Ipv4Addr::LOCALHOST,
		Some(Host::Ipv6(ip)) => ip == Ipv6Addr::LOCALHOST,
		None => return Err(invalid("endpoint")),
	};
	match url.scheme() {
		"https" => Ok(url),
		"http" if local => Ok(url),
		_ => Err(ServerError::Validation(
			"endpoint must use https (http is only allowed for localhost)".to_owned(),
		)),
	}
}

fn text(value: &str, what: &str, max: usize) -> Result<(), ServerError> {
	if value.trim().is_empty() || value.len() > max || value.chars().any(char::is_control) {
		return Err(invalid(what));
	}
	Ok(())
}

fn prefix(value: &str) -> Result<(), ServerError> {
	let ok = value.is_empty()
		|| (value.len() <= 255
			&& validate::rel_path(value)
			&& !value.chars().any(|c| c.is_control() || c.is_whitespace()));
	if ok { Ok(()) } else { Err(invalid("prefix")) }
}

async fn insert(
	db: impl PgExecutor<'_>,
	team: TeamId,
	body: &CreateInput,
	secret: &[u8],
) -> Result<BackupTarget, sqlx::Error> {
	sqlx::query_as!(
		BackupTarget,
		r#"
			INSERT INTO backup_targets
				(id, team_id, name, endpoint, region, bucket, path_style, prefix,
				access_key_id, secret_access_key)
			VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
			RETURNING id AS "id!: BackupTargetId", team_id AS "team_id!: TeamId", name AS "name!",
				endpoint AS "endpoint!", region AS "region!", bucket AS "bucket!",
				path_style AS "path_style!", prefix AS "prefix!", access_key_id AS "access_key_id!",
				secret_access_key AS "secret_access_key!", created_at AS "created_at!"
		"#,
		BackupTargetId::generate() as BackupTargetId,
		team as TeamId,
		body.name,
		body.endpoint,
		body.region,
		body.bucket,
		body.path_style,
		body.prefix,
		body.access_key_id,
		secret,
	)
	.fetch_one(db)
	.await
}

pub(crate) async fn list_for_team(
	db: impl PgExecutor<'_>,
	team: TeamId,
) -> Result<Vec<BackupTarget>, sqlx::Error> {
	sqlx::query_as!(
		BackupTarget,
		r#"
			SELECT id AS "id: BackupTargetId", team_id AS "team_id: TeamId", name, endpoint,
				region, bucket, path_style, prefix, access_key_id, secret_access_key, created_at
			FROM backup_targets WHERE team_id = $1 ORDER BY name
		"#,
		team as TeamId,
	)
	.fetch_all(db)
	.await
}

/// Registers a bucket for the team. The secret is encrypted at rest and never
/// returned. Caller must hold `create backup` on the team.
pub async fn create(
	s: &AppState,
	user: UserId,
	team: TeamId,
	mut body: CreateInput,
) -> AppResult<BackupTarget> {
	authorize(&s.db, user, team, Action::Create, Resource::Backup).await?;

	body.endpoint = body.endpoint.trim().to_owned();
	text(&body.name, "name", 60)?;
	text(&body.region, "region", 60)?;
	text(&body.bucket, "bucket", 255)?;
	text(&body.access_key_id, "access_key_id", 255)?;
	if body.secret_access_key.is_empty() {
		return Err(invalid("secret_access_key"));
	}
	prefix(&body.prefix)?;
	let endpoint = parse_endpoint(&body.endpoint)?;
	s3::bucket(endpoint, body.path_style, &body.bucket, &body.region)
		.map_err(|e| ServerError::Validation(e.to_string()))?;

	let secret = s.secrets.encrypt(&body.secret_access_key).map_err(|e| {
		tracing::error!("failed to encrypt backup target secret: {e}");
		ServerError::Internal
	})?;
	let target = insert(&s.db, team, &body, &secret).await.map_err(|e| {
		if is_unique_violation(&e, NAME_CONSTRAINT) {
			ServerError::Conflict("a backup target with that name already exists".to_owned())
		} else {
			e.into()
		}
	})?;
	Ok(target)
}

/// The team's targets. The stored secret is still encrypted in the result;
/// callers must not render it. Caller must hold `read backup` on the team.
pub async fn list(db: &PgPool, user: UserId, team: TeamId) -> AppResult<Vec<BackupTarget>> {
	authorize(db, user, team, Action::Read, Resource::Backup).await?;
	Ok(list_for_team(db, team).await?)
}

/// Deletes a target. Databases using it lose their target and backups made
/// to it keep their rows but can no longer be restored. Caller must hold
/// `delete backup` on the team.
pub async fn remove(
	db: &PgPool,
	user: UserId,
	team: TeamId,
	id: BackupTargetId,
) -> AppResult<BackupTargetId> {
	authorize(db, user, team, Action::Delete, Resource::Backup).await?;
	let target = scope::backup_target(db, team, id).await?;
	sqlx::query!(
		"DELETE FROM backup_targets WHERE id = $1",
		target.id as BackupTargetId,
	)
	.execute(db)
	.await?;
	Ok(target.id)
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn endpoint_scheme_and_host_table() {
		let cases = [
			("https://s3.eu-west-1.amazonaws.com", true),
			("https://minio.example.com:9000", true),
			("https://127.0.0.1:9000", true),
			("http://localhost:9000", true),
			("http://LOCALHOST", true),
			("http://127.0.0.1:9000", true),
			("http://[::1]:9000", true),
			("http://minio.example.com", false),
			("http://10.0.0.5:9000", false),
			("http://127.0.0.2:9000", false),
			("http://localhost.example.com", false),
			("http://[::2]:9000", false),
			("ftp://example.com", false),
			("file:///tmp/x", false),
			("s3://bucket", false),
			("https://user:pass@example.com", false),
			("https://user@example.com", false),
			("example.com", false),
			("", false),
		];
		for (endpoint, ok) in cases {
			assert_eq!(parse_endpoint(endpoint).is_ok(), ok, "{endpoint}");
		}
	}

	#[test]
	fn prefix_table() {
		for ok in ["", "podkit", "podkit/", "a/b/c", "backups.v1/prod-eu"] {
			assert!(prefix(ok).is_ok(), "{ok}");
		}
		for bad in ["/abs", "a/../b", "..", "a b", "a\nb"] {
			assert!(prefix(bad).is_err(), "{bad:?}");
		}
		assert!(prefix(&"a".repeat(256)).is_err());
	}
}
