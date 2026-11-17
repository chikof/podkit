//! Per-application environment variables, encrypted at rest.

use crate::ids::{ApplicationId, EnvVarId};

pub mod routes;

pub use routes::router;

/// Row from the `env_vars` table.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct EnvVar {
	pub id: EnvVarId,
	pub application_id: ApplicationId,
	pub key: String,
	/// age-encrypted value; never stored or returned as plaintext.
	pub value: Vec<u8>,
}

/// Inserts a new env var, or updates the value if `(application_id, key)`
/// already exists. A re-set of the same key keeps its original row id.
pub async fn upsert(
	db: impl sqlx::PgExecutor<'_>,
	application_id: ApplicationId,
	key: &str,
	value: &[u8],
) -> Result<EnvVar, sqlx::Error> {
	sqlx::query_as!(
		EnvVar,
		r#"
			INSERT INTO env_vars (id, application_id, key, value)
			VALUES ($1, $2, $3, $4)
			ON CONFLICT (application_id, key)
			DO UPDATE SET value = EXCLUDED.value, updated_at = NOW()
			RETURNING id AS "id: EnvVarId", application_id AS "application_id: ApplicationId",
				key, value
		"#,
		EnvVarId::generate() as EnvVarId,
		application_id as ApplicationId,
		key,
		value,
	)
	.fetch_one(db)
	.await
}

/// Every env var set on an application, ordered by key.
pub async fn list(
	db: impl sqlx::PgExecutor<'_>,
	application_id: ApplicationId,
) -> Result<Vec<EnvVar>, sqlx::Error> {
	sqlx::query_as!(
		EnvVar,
		r#"
			SELECT id AS "id: EnvVarId", application_id AS "application_id: ApplicationId",
				key, value
			FROM env_vars WHERE application_id = $1 ORDER BY key
		"#,
		application_id as ApplicationId,
	)
	.fetch_all(db)
	.await
}

/// Deletes an env var of `application_id`. Returns whether a row was removed.
pub async fn delete(
	db: impl sqlx::PgExecutor<'_>,
	application_id: ApplicationId,
	id: EnvVarId,
) -> Result<bool, sqlx::Error> {
	let done = sqlx::query!(
		"DELETE FROM env_vars WHERE id = $2 AND application_id = $1",
		application_id as ApplicationId,
		id as EnvVarId,
	)
	.execute(db)
	.await?;
	Ok(done.rows_affected() > 0)
}
