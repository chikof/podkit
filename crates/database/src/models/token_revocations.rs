//! Denylist for JWTs that got invalidated before their natural expiry
//! (logout, password change, etc). Postgres is the source of truth here,
//! not an in-memory cache, since revocations need to survive restarts.

use time::OffsetDateTime;

use crate::{DatabaseError, DbExecutor};

/// Namespace for revocation queries; holds no state of its own.
pub struct TokenRevocation;

impl TokenRevocation {
	/// Marks a token's `jti` as revoked until `expires_at`. A no-op if it's
	/// already revoked.
	///
	/// # Errors
	/// Returns an error if the insert fails.
	pub async fn revoke<'e>(
		exec: impl DbExecutor<'e>,
		jti: i64,
		expires_at: OffsetDateTime,
	) -> Result<(), DatabaseError> {
		sqlx::query!(
			r#"
				INSERT INTO token_revocations (jti, expires_at)
				VALUES ($1, $2)
				ON CONFLICT (jti) DO NOTHING
			"#,
			jti,
			expires_at
		)
		.execute(exec)
		.await?;

		Ok(())
	}

	/// Checks whether a token's `jti` has been revoked.
	///
	/// # Errors
	/// Returns an error if the query fails.
	pub async fn is_revoked<'e>(
		exec: impl DbExecutor<'e>,
		jti: i64,
	) -> Result<bool, DatabaseError> {
		let row = sqlx::query!(
			"SELECT 1 AS exists FROM token_revocations WHERE jti = $1",
			jti,
		)
		.fetch_optional(exec)
		.await?;

		Ok(row.is_some())
	}

	/// Deletes revocation rows whose `expires_at` has already passed, since
	/// the underlying token is expired anyway and no longer needs denylisting.
	/// Returns the number of rows removed.
	///
	/// # Errors
	/// Returns an error if the delete fails.
	pub async fn purge_expired<'e>(exec: impl DbExecutor<'e>) -> Result<u64, DatabaseError> {
		let result = sqlx::query!("DELETE FROM token_revocations WHERE expires_at < now()")
			.execute(exec)
			.await?;

		Ok(result.rows_affected())
	}
}
