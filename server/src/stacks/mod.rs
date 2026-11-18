//! Compose stacks: a docker-compose file run as one deployment per service
//! on a private network of one of the team's servers.
//!
//! The compose text is stored as received and translated at deploy time
//! ([`translate`]), so what runs is always what the file and the stack's
//! variables say now. Rows live in `stacks` and `stack_services`; the work is
//! done by jobs ([`run`]).

pub mod compose;
mod routes;
pub mod run;
pub mod service;
pub mod translate;

#[cfg(test)]
mod corpus;
#[cfg(test)]
mod tests;

use std::collections::BTreeMap;

use crypto::age::SecretBox;
use sqlx::PgExecutor;
use time::OffsetDateTime;

use crate::error::ServerError;
use crate::ids::{EnvironmentId, ProjectId, ServerId, StackId, StackServiceId, TeamId};
use crate::jobs::{self, JobKind};
use crate::state::AppState;

pub use routes::router;

/// Unique constraint on `stacks (project_id, name)`.
pub const NAME_CONSTRAINT: &str = "stacks_project_id_name_key";
/// Unique constraint on `stacks (project_id, slug)`.
pub const SLUG_CONSTRAINT: &str = "stacks_project_id_slug_key";
/// Unique index on `stack_services (domain)`.
pub const DOMAIN_INDEX: &str = "idx_stack_services_domain";

/// Lifecycle state of a stack.
#[derive(Debug, Clone, Copy, PartialEq, Eq, sqlx::Type, serde::Serialize, serde::Deserialize)]
#[sqlx(type_name = "varchar", rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum StackStatus {
	/// Created, never deployed.
	Pending,
	/// A deploy job is queued or running.
	Deploying,
	/// Every service was deployed.
	Running,
	/// Stopped on purpose.
	Stopped,
	/// A job failed; see `status_message`.
	Failed,
	/// A remove job is queued or running.
	Deleting,
}

/// State of one service of a stack.
#[derive(Debug, Clone, Copy, PartialEq, Eq, sqlx::Type, serde::Serialize, serde::Deserialize)]
#[sqlx(type_name = "varchar", rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum ServiceStatus {
	/// In the compose file, not deployed yet.
	Pending,
	/// Deployed and not seen exiting since.
	Running,
	/// Stopped on purpose.
	Stopped,
	/// Failed to deploy or exited.
	Failed,
	/// No longer in the compose file; the next deploy removes it.
	Removing,
}

/// Row from the `stacks` table.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Stack {
	pub id: StackId,
	pub project_id: ProjectId,
	pub environment_id: EnvironmentId,
	pub server_id: ServerId,
	pub name: String,
	pub slug: String,
	pub compose_yaml: String,
	/// age-encrypted JSON object of the stack's variables.
	pub env: Vec<u8>,
	/// age-encrypted JSON object of the files a template mounts (path to
	/// content); empty when there are none.
	pub files: Vec<u8>,
	/// `raw`, or `template:<id>@<version>`.
	pub source: String,
	pub status: StackStatus,
	pub status_message: Option<String>,
	pub created_at: OffsetDateTime,
	pub updated_at: OffsetDateTime,
}

/// Row from the `stack_services` table.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct StackService {
	pub id: StackServiceId,
	pub stack_id: StackId,
	pub name: String,
	pub image: String,
	pub container_id: Option<String>,
	pub status: ServiceStatus,
	pub status_message: Option<String>,
	pub health: Option<String>,
	pub domain: Option<String>,
	pub domain_port: Option<i32>,
	pub spec_hash: Option<String>,
}

const STACK_COLUMNS: &str = "s.id, s.project_id, s.environment_id, s.server_id, s.name, s.slug,
	s.compose_yaml, s.env, s.files, s.source, s.status, s.status_message, s.created_at, s.updated_at";
const SERVICE_COLUMNS: &str = "id, stack_id, name, image, container_id, status, status_message,
	health, domain, domain_port, spec_hash";

/// Fields needed to insert a new stack row.
pub struct NewStack {
	pub id: StackId,
	pub project_id: ProjectId,
	pub environment_id: EnvironmentId,
	pub server_id: ServerId,
	pub name: String,
	pub slug: String,
	pub compose_yaml: String,
	pub env: Vec<u8>,
	pub files: Vec<u8>,
	pub source: String,
}

pub async fn insert(db: impl PgExecutor<'_>, new: NewStack) -> Result<Stack, sqlx::Error> {
	sqlx::query_as::<_, Stack>(sqlx::AssertSqlSafe(format!(
		"INSERT INTO stacks AS s
			(id, project_id, environment_id, server_id, name, slug, compose_yaml, env, files, source)
		VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
		RETURNING {STACK_COLUMNS}"
	)))
	.bind(new.id)
	.bind(new.project_id)
	.bind(new.environment_id)
	.bind(new.server_id)
	.bind(new.name)
	.bind(new.slug)
	.bind(new.compose_yaml)
	.bind(new.env)
	.bind(new.files)
	.bind(new.source)
	.fetch_one(db)
	.await
}

/// Lookup by id alone, with no team check. Only for jobs; handlers use
/// `scope::stack`.
pub async fn find_by_id(
	db: impl PgExecutor<'_>,
	id: StackId,
) -> Result<Option<Stack>, sqlx::Error> {
	sqlx::query_as::<_, Stack>(sqlx::AssertSqlSafe(format!(
		"SELECT {STACK_COLUMNS} FROM stacks s WHERE s.id = $1"
	)))
	.bind(id)
	.fetch_optional(db)
	.await
}

/// The stack if it belongs to `project` and the project to `team`.
pub async fn find_scoped(
	db: impl PgExecutor<'_>,
	team: TeamId,
	project: ProjectId,
	id: StackId,
) -> Result<Option<Stack>, sqlx::Error> {
	sqlx::query_as::<_, Stack>(sqlx::AssertSqlSafe(format!(
		"SELECT {STACK_COLUMNS} FROM stacks s
		JOIN projects p ON p.id = s.project_id
		WHERE s.id = $3 AND s.project_id = $2 AND p.team_id = $1"
	)))
	.bind(team)
	.bind(project)
	.bind(id)
	.fetch_optional(db)
	.await
}

pub async fn list_by_environment(
	db: impl PgExecutor<'_>,
	environment: EnvironmentId,
) -> Result<Vec<Stack>, sqlx::Error> {
	sqlx::query_as::<_, Stack>(sqlx::AssertSqlSafe(format!(
		"SELECT {STACK_COLUMNS} FROM stacks s WHERE s.environment_id = $1 ORDER BY s.id"
	)))
	.bind(environment)
	.fetch_all(db)
	.await
}

/// Replaces the editable fields. The caller resolved `id` through `scope::stack`.
pub async fn update(
	db: impl PgExecutor<'_>,
	id: StackId,
	name: &str,
	compose_yaml: &str,
	env: &[u8],
) -> Result<Stack, sqlx::Error> {
	sqlx::query_as::<_, Stack>(sqlx::AssertSqlSafe(format!(
		"UPDATE stacks AS s SET name = $2, compose_yaml = $3, env = $4, updated_at = NOW()
		WHERE s.id = $1 RETURNING {STACK_COLUMNS}"
	)))
	.bind(id)
	.bind(name)
	.bind(compose_yaml)
	.bind(env)
	.fetch_one(db)
	.await
}

pub async fn set_status(
	db: impl PgExecutor<'_>,
	id: StackId,
	status: StackStatus,
	message: Option<&str>,
) -> Result<(), sqlx::Error> {
	sqlx::query(
		"UPDATE stacks SET status = $2, status_message = $3, updated_at = NOW() WHERE id = $1",
	)
	.bind(id)
	.bind(status)
	.bind(message)
	.execute(db)
	.await?;
	Ok(())
}

pub async fn delete(db: impl PgExecutor<'_>, id: StackId) -> Result<(), sqlx::Error> {
	sqlx::query("DELETE FROM stacks WHERE id = $1")
		.bind(id)
		.execute(db)
		.await?;
	Ok(())
}

pub async fn list_services(
	db: impl PgExecutor<'_>,
	stack: StackId,
) -> Result<Vec<StackService>, sqlx::Error> {
	sqlx::query_as::<_, StackService>(sqlx::AssertSqlSafe(format!(
		"SELECT {SERVICE_COLUMNS} FROM stack_services WHERE stack_id = $1 ORDER BY name"
	)))
	.bind(stack)
	.fetch_all(db)
	.await
}

pub async fn find_service(
	db: impl PgExecutor<'_>,
	stack: StackId,
	name: &str,
) -> Result<Option<StackService>, sqlx::Error> {
	sqlx::query_as::<_, StackService>(sqlx::AssertSqlSafe(format!(
		"SELECT {SERVICE_COLUMNS} FROM stack_services WHERE stack_id = $1 AND name = $2"
	)))
	.bind(stack)
	.bind(name)
	.fetch_optional(db)
	.await
}

/// Lookup by id alone. Only for jobs.
pub async fn find_service_by_id(
	db: impl PgExecutor<'_>,
	id: StackServiceId,
) -> Result<Option<StackService>, sqlx::Error> {
	sqlx::query_as::<_, StackService>(sqlx::AssertSqlSafe(format!(
		"SELECT {SERVICE_COLUMNS} FROM stack_services WHERE id = $1"
	)))
	.bind(id)
	.fetch_optional(db)
	.await
}

/// Makes the rows match the compose file's services: new ones are added,
/// existing ones keep their state and domain, missing ones are marked
/// `removing` (the next deploy removes their containers and the row).
pub async fn sync_services(
	db: &sqlx::PgPool,
	stack: StackId,
	images: &BTreeMap<String, String>,
) -> Result<(), sqlx::Error> {
	let mut tx = db.begin().await?;
	for (name, image) in images {
		sqlx::query(
			"INSERT INTO stack_services (id, stack_id, name, image) VALUES ($1, $2, $3, $4)
			ON CONFLICT (stack_id, name) DO UPDATE SET image = EXCLUDED.image, updated_at = NOW(),
				status = CASE WHEN stack_services.status = 'removing' THEN 'pending'
					ELSE stack_services.status END",
		)
		.bind(StackServiceId::generate())
		.bind(stack)
		.bind(name)
		.bind(image)
		.execute(&mut *tx)
		.await?;
	}
	let names: Vec<&str> = images.keys().map(String::as_str).collect();
	sqlx::query(
		"UPDATE stack_services SET status = 'removing', domain = NULL, domain_port = NULL,
			updated_at = NOW()
		WHERE stack_id = $1 AND NOT (name = ANY($2))",
	)
	.bind(stack)
	.bind(names)
	.execute(&mut *tx)
	.await?;
	tx.commit().await
}

/// What a deploy leaves on a service row.
pub struct ServiceState<'a> {
	pub status: ServiceStatus,
	pub message: Option<&'a str>,
	pub container_id: Option<&'a str>,
	pub health: Option<&'a str>,
	pub spec_hash: Option<&'a str>,
}

pub async fn set_service_state(
	db: impl PgExecutor<'_>,
	id: StackServiceId,
	state: &ServiceState<'_>,
) -> Result<(), sqlx::Error> {
	sqlx::query(
		"UPDATE stack_services SET status = $2, status_message = $3, container_id = $4,
			health = $5, spec_hash = COALESCE($6, spec_hash), updated_at = NOW()
		WHERE id = $1",
	)
	.bind(id)
	.bind(state.status)
	.bind(state.message)
	.bind(state.container_id)
	.bind(state.health)
	.bind(state.spec_hash)
	.execute(db)
	.await?;
	Ok(())
}

pub async fn set_service_status(
	db: impl PgExecutor<'_>,
	id: StackServiceId,
	status: ServiceStatus,
	message: Option<&str>,
	health: Option<&str>,
) -> Result<(), sqlx::Error> {
	sqlx::query(
		"UPDATE stack_services SET status = $2, status_message = $3, health = $4, updated_at = NOW()
		WHERE id = $1",
	)
	.bind(id)
	.bind(status)
	.bind(message)
	.bind(health)
	.execute(db)
	.await?;
	Ok(())
}

pub async fn set_domain(
	db: impl PgExecutor<'_>,
	id: StackServiceId,
	domain: Option<&str>,
	port: Option<i32>,
) -> Result<(), sqlx::Error> {
	sqlx::query(
		"UPDATE stack_services SET domain = $2, domain_port = $3, updated_at = NOW() WHERE id = $1",
	)
	.bind(id)
	.bind(domain)
	.bind(port)
	.execute(db)
	.await?;
	Ok(())
}

pub async fn delete_service(
	db: impl PgExecutor<'_>,
	id: StackServiceId,
) -> Result<(), sqlx::Error> {
	sqlx::query("DELETE FROM stack_services WHERE id = $1")
		.bind(id)
		.execute(db)
		.await?;
	Ok(())
}

/// Whether `host` is routed anywhere on this install: an application's
/// custom domain or another stack service's domain.
pub async fn hostname_in_use(
	db: impl PgExecutor<'_>,
	host: &str,
	except: StackServiceId,
) -> Result<bool, sqlx::Error> {
	sqlx::query_scalar(
		"SELECT EXISTS (SELECT 1 FROM custom_domains WHERE hostname = $1)
			OR EXISTS (SELECT 1 FROM stack_services WHERE domain = $1 AND id <> $2)",
	)
	.bind(host)
	.bind(except)
	.fetch_one(db)
	.await
}

/// The stack's variables, decrypted.
///
/// # Errors
/// The stored blob cannot be decrypted or parsed.
pub fn decrypt_env(secrets: &SecretBox, stack: &Stack) -> Result<BTreeMap<String, String>, String> {
	decrypt_env_blob(secrets, &stack.env)
}

/// See [`decrypt_env`].
pub fn decrypt_env_blob(
	secrets: &SecretBox,
	blob: &[u8],
) -> Result<BTreeMap<String, String>, String> {
	let json = secrets
		.decrypt(blob)
		.map_err(|e| format!("stack variables decrypt failed: {e}"))?;
	serde_json::from_str(&json).map_err(|e| format!("stack variables are not valid json: {e}"))
}

/// The files a template mounts into the stack, decrypted; none for a raw
/// stack.
///
/// # Errors
/// The stored blob cannot be decrypted or parsed.
pub fn decrypt_files(
	secrets: &SecretBox,
	stack: &Stack,
) -> Result<BTreeMap<String, String>, String> {
	if stack.files.is_empty() {
		return Ok(BTreeMap::new());
	}
	decrypt_env_blob(secrets, &stack.files)
}

/// Encrypts the stack's variables for the `env` column.
///
/// # Errors
/// Encryption fails.
pub fn encrypt_env(
	secrets: &SecretBox,
	env: &BTreeMap<String, String>,
) -> Result<Vec<u8>, ServerError> {
	let json = serde_json::to_string(env).map_err(|e| {
		tracing::error!("failed to encode stack variables: {e}");
		ServerError::Internal
	})?;
	secrets.encrypt(&json).map_err(|e| {
		tracing::error!("failed to encrypt stack variables: {e}");
		ServerError::Internal
	})
}

/// Queues `kind` for `stack`. All jobs of a stack share one lock key, so
/// they run one at a time in the order they were queued.
pub async fn enqueue(
	state: &AppState,
	kind: JobKind,
	stack: &Stack,
	target: i64,
) -> Result<crate::ids::JobId, ServerError> {
	Ok(jobs::enqueue(
		state,
		kind,
		stack.server_id,
		target,
		format!("stack:{}", stack.id),
	)
	.await?)
}
