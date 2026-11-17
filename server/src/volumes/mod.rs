//! Named podman volumes attached to an application (table `volumes`).

use crate::ids::{ApplicationId, VolumeId};

pub mod routes;
pub mod service;

pub use routes::router;

/// Row from the `volumes` table.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Volume {
	pub id: VolumeId,
	pub application_id: ApplicationId,
	/// Podman volume name, `podkit-vol-{id}`; unique across the install.
	pub name: String,
	pub mount_path: String,
}

/// Unique constraint on `volumes (application_id, mount_path)`.
pub const MOUNT_PATH_CONSTRAINT: &str = "volumes_application_id_mount_path_key";

/// Inserts a volume whose name is derived from its freshly generated id.
pub async fn insert(
	db: impl sqlx::PgExecutor<'_>,
	application_id: ApplicationId,
	mount_path: &str,
) -> Result<Volume, sqlx::Error> {
	let id = VolumeId::generate();
	sqlx::query_as!(
		Volume,
		r#"
			INSERT INTO volumes (id, application_id, name, mount_path)
			VALUES ($1, $2, $3, $4)
			RETURNING id AS "id: VolumeId", application_id AS "application_id: ApplicationId",
				name, mount_path
		"#,
		id as VolumeId,
		application_id as ApplicationId,
		format!("podkit-vol-{id}"),
		mount_path,
	)
	.fetch_one(db)
	.await
}

/// Every volume of an application, ordered by mount path.
pub async fn list(
	db: impl sqlx::PgExecutor<'_>,
	application_id: ApplicationId,
) -> Result<Vec<Volume>, sqlx::Error> {
	sqlx::query_as!(
		Volume,
		r#"
			SELECT id AS "id: VolumeId", application_id AS "application_id: ApplicationId",
				name, mount_path
			FROM volumes WHERE application_id = $1 ORDER BY mount_path
		"#,
		application_id as ApplicationId,
	)
	.fetch_all(db)
	.await
}

/// A volume of `application_id`, if it exists.
pub async fn find(
	db: impl sqlx::PgExecutor<'_>,
	application_id: ApplicationId,
	id: VolumeId,
) -> Result<Option<Volume>, sqlx::Error> {
	sqlx::query_as!(
		Volume,
		r#"
			SELECT id AS "id: VolumeId", application_id AS "application_id: ApplicationId",
				name, mount_path
			FROM volumes WHERE id = $2 AND application_id = $1
		"#,
		application_id as ApplicationId,
		id as VolumeId,
	)
	.fetch_optional(db)
	.await
}

/// Deletes a volume row of `application_id`. Returns whether a row was removed.
pub async fn delete(
	db: impl sqlx::PgExecutor<'_>,
	application_id: ApplicationId,
	id: VolumeId,
) -> Result<bool, sqlx::Error> {
	let done = sqlx::query!(
		"DELETE FROM volumes WHERE id = $2 AND application_id = $1",
		application_id as ApplicationId,
		id as VolumeId,
	)
	.execute(db)
	.await?;
	Ok(done.rows_affected() > 0)
}
