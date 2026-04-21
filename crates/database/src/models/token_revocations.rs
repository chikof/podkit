use time::OffsetDateTime;

use crate::{DatabaseError, DbExecutor};

pub struct TokenRevocation;

impl TokenRevocation {
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

	pub async fn purge_expired<'e>(exec: impl DbExecutor<'e>) -> Result<u64, DatabaseError> {
		let result = sqlx::query!("DELETE FROM token_revocations WHERE expires_at < now()")
			.execute(exec)
			.await?;

		Ok(result.rows_affected())
	}
}
