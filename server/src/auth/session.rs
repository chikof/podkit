//! Opaque server-side sessions. The client holds a random token; only its
//! SHA-256 digest is stored, so a database leak does not yield usable tokens.

use sha2::{Digest, Sha256};
use sqlx::PgExecutor;
use time::{Duration, OffsetDateTime};

use crate::ids::UserId;

pub const SESSION_TTL: Duration = Duration::hours(24);

fn digest(token: &str) -> Vec<u8> {
	Sha256::digest(token.as_bytes()).to_vec()
}

/// How a session began, for the session list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
	/// Email and password (and a second factor when the account has one).
	Password,
	/// An identity provider.
	Sso,
	/// A passkey, with no password.
	Passkey,
}

impl Method {
	pub const fn as_str(self) -> &'static str {
		match self {
			Self::Password => "password",
			Self::Sso => "sso",
			Self::Passkey => "passkey",
		}
	}
}

/// Starts a session for `user` and returns the plaintext token (shown once).
pub async fn create(db: impl PgExecutor<'_>, user: UserId) -> Result<String, sqlx::Error> {
	create_with(db, user, Method::Password).await
}

/// [`create`], recording how the person signed in and where from (the address and browser
/// of the request being served).
pub async fn create_with(
	db: impl PgExecutor<'_>,
	user: UserId,
	method: Method,
) -> Result<String, sqlx::Error> {
	let token = crypto::generate_token(32);
	let expires_at = OffsetDateTime::now_utc() + SESSION_TTL;
	let (ip, user_agent) = crate::audit::origin();
	sqlx::query!(
		"INSERT INTO sessions (token_hash, user_id, expires_at, method, ip, user_agent)
		VALUES ($1, $2, $3, $4, $5, $6)",
		digest(&token),
		user.0,
		expires_at,
		method.as_str(),
		ip,
		user_agent
	)
	.execute(db)
	.await?;
	Ok(token)
}

/// One of a person's open sessions.
#[derive(Debug, Clone)]
pub struct Entry {
	pub id: i64,
	pub created_at: OffsetDateTime,
	pub method: String,
	pub ip: Option<String>,
	pub user_agent: Option<String>,
	/// The session the list is being read from.
	pub current: bool,
}

/// `user`'s unexpired sessions, newest first; `current` is the token making the request.
pub async fn list(
	db: impl PgExecutor<'_>,
	user: UserId,
	current: &str,
) -> Result<Vec<Entry>, sqlx::Error> {
	sqlx::query_as!(
		Entry,
		r#"
			SELECT id AS "id!", created_at, method, ip, user_agent,
				token_hash = $2 AS "current!"
			FROM sessions
			WHERE user_id = $1 AND expires_at > NOW()
			ORDER BY created_at DESC, id DESC
		"#,
		user.0,
		digest(current)
	)
	.fetch_all(db)
	.await
}

/// Ends one of `user`'s sessions; `false` when it is not theirs (or is already gone).
pub async fn revoke_id(
	db: impl PgExecutor<'_>,
	user: UserId,
	id: i64,
) -> Result<bool, sqlx::Error> {
	Ok(sqlx::query!(
		"DELETE FROM sessions WHERE id = $1 AND user_id = $2",
		id,
		user.0
	)
	.execute(db)
	.await?
	.rows_affected()
		> 0)
}

/// Ends every session of `user` except the one making the request; returns how many.
pub async fn revoke_others(
	db: impl PgExecutor<'_>,
	user: UserId,
	keep: &str,
) -> Result<u64, sqlx::Error> {
	Ok(sqlx::query!(
		"DELETE FROM sessions WHERE user_id = $1 AND token_hash <> $2",
		user.0,
		digest(keep)
	)
	.execute(db)
	.await?
	.rows_affected())
}

/// Returns the owner of an unexpired session, or `None` for unknown/expired tokens.
pub async fn resolve(db: impl PgExecutor<'_>, token: &str) -> Result<Option<UserId>, sqlx::Error> {
	sqlx::query_scalar!(
		r#"SELECT user_id AS "user_id: UserId" FROM sessions WHERE token_hash = $1 AND expires_at > NOW()"#,
		digest(token)
	)
	.fetch_optional(db)
	.await
}

pub async fn revoke(db: impl PgExecutor<'_>, token: &str) -> Result<(), sqlx::Error> {
	sqlx::query!("DELETE FROM sessions WHERE token_hash = $1", digest(token))
		.execute(db)
		.await?;
	Ok(())
}

/// Deletes expired sessions and returns how many were removed.
pub async fn purge_expired(db: impl PgExecutor<'_>) -> Result<u64, sqlx::Error> {
	Ok(
		sqlx::query!("DELETE FROM sessions WHERE expires_at <= NOW()")
			.execute(db)
			.await?
			.rows_affected(),
	)
}

#[cfg(test)]
mod tests {
	use super::*;
	use sqlx::PgPool;

	async fn user(pool: &PgPool, id: i64) -> UserId {
		sqlx::query("INSERT INTO users (id, name, email, password_hash) VALUES ($1, 'U', $2, 'x')")
			.bind(id)
			.bind(format!("u{id}@test.dev"))
			.execute(pool)
			.await
			.unwrap();
		UserId(id)
	}

	#[sqlx::test]
	async fn create_resolve_revoke(pool: PgPool) {
		let uid = user(&pool, 1).await;
		let token = create(&pool, uid).await.unwrap();
		assert_eq!(token.len(), 64);
		assert_eq!(resolve(&pool, &token).await.unwrap(), Some(uid));
		assert_eq!(resolve(&pool, "nope").await.unwrap(), None);

		revoke(&pool, &token).await.unwrap();
		assert_eq!(resolve(&pool, &token).await.unwrap(), None);
	}

	#[sqlx::test]
	async fn expired_sessions_do_not_resolve_and_are_purged(pool: PgPool) {
		let uid = user(&pool, 1).await;
		let token = create(&pool, uid).await.unwrap();
		sqlx::query("UPDATE sessions SET expires_at = NOW() - INTERVAL '1 second'")
			.execute(&pool)
			.await
			.unwrap();

		assert_eq!(resolve(&pool, &token).await.unwrap(), None);
		assert_eq!(purge_expired(&pool).await.unwrap(), 1);
		assert_eq!(purge_expired(&pool).await.unwrap(), 0);
	}
}
