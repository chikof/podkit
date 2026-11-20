//! Applications: a git repo built and run as a container on one of the
//! team's servers.

use crate::ids::{ApplicationId, EnvironmentId, ProjectId, RegistryId, ServerId};
use crate::servers::Server;

pub mod routes;
pub mod service;

#[cfg(test)]
mod image_tests;

pub use routes::router;

/// How an application's image gets built.
#[derive(Debug, Clone, Copy, PartialEq, Eq, sqlx::Type)]
#[sqlx(type_name = "varchar", rename_all = "lowercase")]
pub enum BuildStrategy {
	/// Build from a Dockerfile in the repo.
	Dockerfile,
	/// Auto-detected build (Nixpacks-style), no Dockerfile required.
	Nixpacks,
}

impl BuildStrategy {
	/// Returns the lowercase string form used for storage and display.
	pub fn as_str(self) -> &'static str {
		match self {
			Self::Dockerfile => "dockerfile",
			Self::Nixpacks => "nixpacks",
		}
	}

	/// Parses the storage string form back into a strategy. Unrecognized
	/// input falls back to `Dockerfile` rather than failing.
	pub fn parse(s: &str) -> Self {
		match s {
			"nixpacks" => Self::Nixpacks,
			_ => Self::Dockerfile,
		}
	}
}

/// Where an application's image comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, sqlx::Type)]
#[sqlx(type_name = "text", rename_all = "lowercase")]
pub enum SourceKind {
	/// Cloned from `repo_url` and built on the server.
	Git,
	/// Pulled from a registry as `image`.
	Image,
}

impl SourceKind {
	/// The lowercase string form used for storage and display.
	pub fn as_str(self) -> &'static str {
		match self {
			Self::Git => "git",
			Self::Image => "image",
		}
	}
}

/// What `applications.registry_auth` holds once decrypted.
#[derive(serde::Serialize, serde::Deserialize)]
pub(crate) struct StoredRegistryAuth {
	pub username: String,
	pub password: String,
}

/// Row from the `applications` table.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Application {
	pub id: ApplicationId,
	pub project_id: ProjectId,
	pub environment_id: EnvironmentId,
	pub server_id: ServerId,
	pub name: String,
	/// Unique per server, not just per project; the generated subdomain is
	/// derived from `(slug, server)`.
	pub slug: String,
	/// `Some` exactly when `source_kind` is [`SourceKind::Git`].
	pub repo_url: Option<String>,
	/// Git branch, tag, or ref to deploy.
	pub git_ref: String,
	pub source_kind: SourceKind,
	/// Registry reference; `Some` exactly when `source_kind` is [`SourceKind::Image`].
	pub image: Option<String>,
	/// age-encrypted JSON `{"username", "password"}` for a private registry. Set only by
	/// the API's inline credentials; a saved registry (`registry_id`) replaces it.
	pub registry_auth: Option<Vec<u8>>,
	/// The saved registry this image is pulled with.
	pub registry_id: Option<RegistryId>,
	/// age-encrypted SSH private key (PEM) for `ssh://`/scp-form repo URLs.
	pub deploy_key: Option<Vec<u8>>,
	/// age-encrypted access token for `https://` repo URLs.
	pub git_token: Option<Vec<u8>>,
	pub build_strategy: BuildStrategy,
	/// Path to the Dockerfile within the repo, when using [`BuildStrategy::Dockerfile`].
	pub dockerfile_path: String,
	/// Port the container listens on.
	pub container_port: i32,
	/// age-encrypted webhook trigger secret. `None` disables webhook deploys.
	pub webhook_secret: Option<Vec<u8>>,
	/// Memory limit in megabytes. `None` means unlimited.
	pub memory_limit_mb: Option<i32>,
	/// Fractional CPU cores. `None` means unlimited.
	pub cpu_limit: Option<f32>,
}

/// Fields needed to insert a new application row.
pub struct NewApplication {
	pub id: ApplicationId,
	pub project_id: ProjectId,
	/// Must belong to `project_id`; the composite foreign key enforces it.
	pub environment_id: EnvironmentId,
	pub server_id: ServerId,
	pub name: String,
	pub slug: String,
	pub repo_url: Option<String>,
	pub git_ref: String,
	pub source_kind: SourceKind,
	pub image: Option<String>,
	pub registry_auth: Option<Vec<u8>>,
	pub registry_id: Option<RegistryId>,
	pub deploy_key: Option<Vec<u8>>,
	pub git_token: Option<Vec<u8>>,
	pub build_strategy: BuildStrategy,
	pub dockerfile_path: String,
	pub container_port: i32,
	pub webhook_secret: Option<Vec<u8>>,
	pub memory_limit_mb: Option<i32>,
	pub cpu_limit: Option<f32>,
}

/// What an update does to the saved registry an application pulls with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegistryChoice {
	/// Leave it as it is.
	Keep,
	/// Pull anonymously (or with the inline login given alongside).
	Clear,
	/// Pull with this saved registry; the inline login is dropped.
	Use(RegistryId),
}

/// Editable fields. `deploy_key`/`git_token`/`registry_auth` of `None`
/// leave the stored value untouched. The source kind cannot change.
pub struct ApplicationUpdate {
	pub name: String,
	pub slug: String,
	pub repo_url: Option<String>,
	pub git_ref: String,
	pub image: Option<String>,
	pub registry_auth: Option<Vec<u8>>,
	pub registry: RegistryChoice,
	pub deploy_key: Option<Vec<u8>>,
	pub git_token: Option<Vec<u8>>,
	pub build_strategy: BuildStrategy,
	pub dockerfile_path: String,
	pub container_port: i32,
	pub memory_limit_mb: Option<i32>,
	pub cpu_limit: Option<f32>,
}

/// Unique constraint on `(server_id, slug)`.
pub const SLUG_CONSTRAINT: &str = "applications_server_id_slug_key";

pub async fn insert(
	db: impl sqlx::PgExecutor<'_>,
	new: NewApplication,
) -> Result<Application, sqlx::Error> {
	sqlx::query_as!(
		Application,
		r#"
			INSERT INTO applications
				(id, project_id, environment_id, server_id, name, slug, repo_url, git_ref, deploy_key, git_token,
				 build_strategy, dockerfile_path, container_port, webhook_secret, memory_limit_mb, cpu_limit,
				 source_kind, image, registry_auth, registry_id)
			VALUES ($1, $2, $19, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $20)
			RETURNING
				id AS "id: ApplicationId", project_id AS "project_id: ProjectId", environment_id AS "environment_id: EnvironmentId",
				server_id AS "server_id: ServerId", name, slug, repo_url, git_ref,
				source_kind AS "source_kind: SourceKind", image, registry_auth, registry_id AS "registry_id: RegistryId",
				deploy_key, git_token, build_strategy AS "build_strategy: BuildStrategy",
				dockerfile_path, container_port, webhook_secret, memory_limit_mb, cpu_limit
		"#,
		new.id as ApplicationId,
		new.project_id as ProjectId,
		new.server_id as ServerId,
		new.name,
		new.slug,
		new.repo_url,
		new.git_ref,
		new.deploy_key,
		new.git_token,
		new.build_strategy as BuildStrategy,
		new.dockerfile_path,
		new.container_port,
		new.webhook_secret,
		new.memory_limit_mb,
		new.cpu_limit,
		new.source_kind as SourceKind,
		new.image,
		new.registry_auth,
		new.environment_id as EnvironmentId,
		new.registry_id as Option<RegistryId>,
	)
	.fetch_one(db)
	.await
}

/// The caller must already have resolved `id` through `scope::application`.
pub async fn update(
	db: impl sqlx::PgExecutor<'_>,
	id: ApplicationId,
	u: ApplicationUpdate,
) -> Result<Application, sqlx::Error> {
	sqlx::query_as!(
		Application,
		r#"
			UPDATE applications
			SET name = $2, slug = $3, repo_url = $4, git_ref = $5,
				deploy_key = COALESCE($6, deploy_key), git_token = COALESCE($7, git_token),
				build_strategy = $8, dockerfile_path = $9, container_port = $10,
				memory_limit_mb = $11, cpu_limit = $12, image = $13,
				registry_auth = CASE WHEN $16::BIGINT IS NOT NULL THEN NULL ELSE COALESCE($14, registry_auth) END,
				registry_id = CASE WHEN $15 THEN $16 ELSE registry_id END, updated_at = NOW()
			WHERE id = $1
			RETURNING
				id AS "id: ApplicationId", project_id AS "project_id: ProjectId", environment_id AS "environment_id: EnvironmentId",
				server_id AS "server_id: ServerId", name, slug, repo_url, git_ref,
				source_kind AS "source_kind: SourceKind", image, registry_auth, registry_id AS "registry_id: RegistryId",
				deploy_key, git_token, build_strategy AS "build_strategy: BuildStrategy",
				dockerfile_path, container_port, webhook_secret, memory_limit_mb, cpu_limit
		"#,
		id as ApplicationId,
		u.name,
		u.slug,
		u.repo_url,
		u.git_ref,
		u.deploy_key,
		u.git_token,
		u.build_strategy as BuildStrategy,
		u.dockerfile_path,
		u.container_port,
		u.memory_limit_mb,
		u.cpu_limit,
		u.image,
		u.registry_auth,
		!matches!(u.registry, RegistryChoice::Keep),
		match u.registry {
			RegistryChoice::Use(id) => Some(id.0),
			_ => None,
		},
	)
	.fetch_one(db)
	.await
}

/// Lookup by id alone, with no team check. Only for callers without a team
/// context (webhooks, the deploy job); handlers use `scope::application`.
pub async fn find_by_id(
	db: impl sqlx::PgExecutor<'_>,
	id: ApplicationId,
) -> Result<Option<Application>, sqlx::Error> {
	sqlx::query_as!(
		Application,
		r#"
			SELECT
				id AS "id: ApplicationId", project_id AS "project_id: ProjectId", environment_id AS "environment_id: EnvironmentId",
				server_id AS "server_id: ServerId", name, slug, repo_url, git_ref,
				source_kind AS "source_kind: SourceKind", image, registry_auth, registry_id AS "registry_id: RegistryId",
				deploy_key, git_token, build_strategy AS "build_strategy: BuildStrategy",
				dockerfile_path, container_port, webhook_secret, memory_limit_mb, cpu_limit
			FROM applications WHERE id = $1
		"#,
		id as ApplicationId,
	)
	.fetch_optional(db)
	.await
}

pub async fn list_by_environment(
	db: impl sqlx::PgExecutor<'_>,
	environment: EnvironmentId,
) -> Result<Vec<Application>, sqlx::Error> {
	sqlx::query_as!(
		Application,
		r#"
			SELECT
				id AS "id: ApplicationId", project_id AS "project_id: ProjectId", environment_id AS "environment_id: EnvironmentId",
				server_id AS "server_id: ServerId", name, slug, repo_url, git_ref,
				source_kind AS "source_kind: SourceKind", image, registry_auth, registry_id AS "registry_id: RegistryId",
				deploy_key, git_token, build_strategy AS "build_strategy: BuildStrategy",
				dockerfile_path, container_port, webhook_secret, memory_limit_mb, cpu_limit
			FROM applications WHERE environment_id = $1 ORDER BY id
		"#,
		environment as EnvironmentId,
	)
	.fetch_all(db)
	.await
}

pub async fn delete(db: impl sqlx::PgExecutor<'_>, id: ApplicationId) -> Result<(), sqlx::Error> {
	sqlx::query!(
		"DELETE FROM applications WHERE id = $1",
		id as ApplicationId
	)
	.execute(db)
	.await?;
	Ok(())
}

/// Stores a new (already encrypted) webhook secret.
pub async fn set_webhook_secret(
	db: impl sqlx::PgExecutor<'_>,
	id: ApplicationId,
	sealed: &[u8],
) -> Result<(), sqlx::Error> {
	sqlx::query!(
		"UPDATE applications SET webhook_secret = $2 WHERE id = $1",
		id as ApplicationId,
		sealed
	)
	.execute(db)
	.await?;
	Ok(())
}

/// Derives the zero-config public hostname for an app on `server`, via
/// sslip.io wildcard DNS. We don't own a wildcard domain ourselves in v1,
/// so this is the workaround.
///
/// For a local server this resolves to `127.0.0.1`, genuinely reachable
/// and testable on the same machine, not just a placeholder. For a remote
/// server, `server.hostname` is assumed to already be a bare IPv4 address.
/// Resolving DNS-name servers to an IP is a known gap for a later version.
pub fn public_hostname(slug: &str, server: &Server) -> String {
	sslip_hostname(slug, server.is_local, &server.hostname)
}

/// The server's address as sslip.io writes it: `203-0-113-7`.
pub fn dashed_ip(is_local: bool, hostname: &str) -> String {
	if is_local {
		"127-0-0-1".to_string()
	} else {
		hostname.replace('.', "-")
	}
}

fn sslip_hostname(slug: &str, is_local: bool, hostname: &str) -> String {
	format!("{slug}.{}.sslip.io", dashed_ip(is_local, hostname))
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn local_server_hostname_uses_loopback() {
		assert_eq!(
			sslip_hostname("my-app", true, "localhost"),
			"my-app.127-0-0-1.sslip.io"
		);
	}

	#[test]
	fn remote_server_hostname_dashes_the_ip() {
		assert_eq!(
			sslip_hostname("my-app", false, "203.0.113.7"),
			"my-app.203-0-113-7.sslip.io"
		);
	}

	#[test]
	fn build_strategy_parse_falls_back_to_dockerfile() {
		assert_eq!(BuildStrategy::parse("nixpacks"), BuildStrategy::Nixpacks);
		assert_eq!(BuildStrategy::parse("bogus"), BuildStrategy::Dockerfile);
	}
}
