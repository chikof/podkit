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

/// Starts a session for `user` and returns the plaintext token (shown once).
pub async fn create(db: impl PgExecutor<'_>, user: UserId) -> Result<String, sqlx::Error> {
	let token = crypto::generate_token(32);
	let expires_at = OffsetDateTime::now_utc() + SESSION_TTL;
	sqlx::query!(
		"INSERT INTO sessions (token_hash, user_id, expires_at) VALUES ($1, $2, $3)",
		digest(&token),
		user.0,
		expires_at
	)
	.execute(db)
	.await?;
	Ok(token)
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
