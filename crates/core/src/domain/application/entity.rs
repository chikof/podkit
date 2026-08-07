use time::OffsetDateTime;

use crate::domain::shared::ids::{ApplicationId, ProjectId, ServerId};

/// How an application's image gets built.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuildStrategy {
	/// Build from a Dockerfile in the repo.
	Dockerfile,
	/// Auto-detected build (Nixpacks-style), no Dockerfile required.
	Nixpacks,
}

impl BuildStrategy {
	/// Returns the lowercase string form used for storage and display.
	#[must_use]
	pub fn as_str(self) -> &'static str {
		match self {
			Self::Dockerfile => "dockerfile",
			Self::Nixpacks => "nixpacks",
		}
	}

	/// Parses the storage string form back into a strategy. Unrecognized
	/// input falls back to `Dockerfile` rather than failing.
	#[must_use]
	pub fn parse(s: &str) -> Self {
		match s {
			"nixpacks" => Self::Nixpacks,
			_ => Self::Dockerfile,
		}
	}
}

/// A deployable unit: one git repo, built and run as a container on one of
/// the owning project's team's servers.
#[derive(Debug, Clone)]
pub struct Application {
	/// Unique id of this application.
	pub id: ApplicationId,
	/// The project this application belongs to.
	pub project_id: ProjectId,
	/// The server it's deployed on.
	pub server_id: ServerId,
	/// Human-readable name.
	pub name: String,
	/// Unique per server, not just per project; the generated subdomain is
	/// derived from `(slug, server)`.
	pub slug: String,
	/// Git repository URL to clone from.
	pub repo_url: String,
	/// Git branch, tag, or ref to deploy.
	pub git_ref: String,
	/// age-encrypted deploy key/PAT. `None` for public repos.
	pub deploy_key: Option<Vec<u8>>,
	/// How the image gets built.
	pub build_strategy: BuildStrategy,
	/// Path to the Dockerfile within the repo, when using [`BuildStrategy::Dockerfile`].
	pub dockerfile_path: String,
	/// Port the container listens on.
	pub container_port: i32,
	/// age-encrypted webhook trigger secret. `None` disables webhook
	/// deploys for this app.
	pub webhook_secret: Option<Vec<u8>>,
	/// Memory limit in megabytes. `None` means unlimited.
	pub memory_limit_mb: Option<i32>,
	/// Fractional CPU cores. `None` = unlimited.
	pub cpu_limit: Option<f64>,
	/// When the application was created.
	pub created_at: OffsetDateTime,
	/// When the application was last updated.
	pub updated_at: OffsetDateTime,
}

impl Application {
	/// Creates a new application, stamping both timestamps to now.
	#[must_use]
	#[allow(clippy::too_many_arguments)]
	pub fn new(
		id: ApplicationId,
		project_id: ProjectId,
		server_id: ServerId,
		name: String,
		slug: String,
		repo_url: String,
		git_ref: String,
		deploy_key: Option<Vec<u8>>,
		build_strategy: BuildStrategy,
		dockerfile_path: String,
		container_port: i32,
		webhook_secret: Option<Vec<u8>>,
		memory_limit_mb: Option<i32>,
		cpu_limit: Option<f64>,
	) -> Self {
		let now = OffsetDateTime::now_utc();
		Self {
			id,
			project_id,
			server_id,
			name,
			slug,
			repo_url,
			git_ref,
			deploy_key,
			build_strategy,
			dockerfile_path,
			container_port,
			webhook_secret,
			memory_limit_mb,
			cpu_limit,
			created_at: now,
			updated_at: now,
		}
	}
}
