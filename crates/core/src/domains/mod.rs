//! Custom hostnames routed to an application (table `custom_domains`).

use crate::ids::{ApplicationId, DomainId};

pub mod routes;
pub mod service;

pub use routes::router;

/// Row from the `custom_domains` table.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Domain {
	pub id: DomainId,
	pub application_id: ApplicationId,
	/// Unique across the whole install, since a hostname can only point at
	/// one place at a time.
	pub hostname: String,
}

/// Unique constraint on `custom_domains.hostname`.
pub const HOSTNAME_CONSTRAINT: &str = "custom_domains_hostname_key";

pub async fn insert(
	db: impl sqlx::PgExecutor<'_>,
	application_id: ApplicationId,
	hostname: &str,
) -> Result<Domain, sqlx::Error> {
	sqlx::query_as!(
		Domain,
		r#"
			INSERT INTO custom_domains (id, application_id, hostname)
			VALUES ($1, $2, $3)
			RETURNING id AS "id: DomainId", application_id AS "application_id: ApplicationId",
				hostname
		"#,
		DomainId::generate() as DomainId,
		application_id as ApplicationId,
		hostname,
	)
	.fetch_one(db)
	.await
}

/// Every custom domain attached to an application, ordered by hostname.
pub async fn list(
	db: impl sqlx::PgExecutor<'_>,
	application_id: ApplicationId,
) -> Result<Vec<Domain>, sqlx::Error> {
	sqlx::query_as!(
		Domain,
		r#"
			SELECT id AS "id: DomainId", application_id AS "application_id: ApplicationId",
				hostname
			FROM custom_domains WHERE application_id = $1 ORDER BY hostname
		"#,
		application_id as ApplicationId,
	)
	.fetch_all(db)
	.await
}

/// Deletes a domain of `application_id`. Returns whether a row was removed.
pub async fn delete(
	db: impl sqlx::PgExecutor<'_>,
	application_id: ApplicationId,
	id: DomainId,
) -> Result<bool, sqlx::Error> {
	let done = sqlx::query!(
		"DELETE FROM custom_domains WHERE id = $2 AND application_id = $1",
		application_id as ApplicationId,
		id as DomainId,
	)
	.execute(db)
	.await?;
	Ok(done.rows_affected() > 0)
}
