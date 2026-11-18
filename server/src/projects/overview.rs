//! What a project holds, read for the web UI: the projects grid (counts, logos, last
//! deploy) and an environment's resource cards (applications, databases, models).
//!
//! Every function authorizes, then scopes, like the rest of the slice's services. A kind
//! of resource the caller may not read is left out, not an error, so a read-only role sees
//! a smaller grid instead of a refusal.

use sqlx::PgPool;
use time::OffsetDateTime;

use super::Project;
use crate::applications::{self, BuildStrategy};
use crate::auth::permission::{Action, Resource, authorize};
use crate::databases::{self, DatabaseStatus, EngineKind as DbEngine};
use crate::deployments::DeploymentStatus;
use crate::environments::{self, Environment};
use crate::error::{AppResult, ServerError};
use crate::ids::{ApplicationId, EnvironmentId, ProjectId, TeamId, UserId};
use crate::models::{self, EngineKind as ModelEngine, ModelStatus};
use crate::{scope, servers};

/// The three kinds of thing an environment holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResourceKind {
	Application,
	Database,
	Model,
}

impl ResourceKind {
	#[must_use]
	pub const fn label(self) -> &'static str {
		match self {
			Self::Application => "Application",
			Self::Database => "Database",
			Self::Model => "Model",
		}
	}

	/// The value of the `type` filter and of `data-kind`.
	#[must_use]
	pub const fn slug(self) -> &'static str {
		match self {
			Self::Application => "application",
			Self::Database => "database",
			Self::Model => "model",
		}
	}

	#[must_use]
	pub fn from_slug(slug: &str) -> Option<Self> {
		[Self::Application, Self::Database, Self::Model]
			.into_iter()
			.find(|k| k.slug() == slug)
	}
}

/// A resource's state, as the enum of its own kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResourceStatus {
	/// An application nobody has deployed yet.
	NotDeployed,
	Deployment(DeploymentStatus),
	Database(DatabaseStatus),
	Model(ModelStatus),
}

/// The mark to draw for a resource: `mark` is a name the logo component knows (`github`,
/// `postgres`), or `None` for the monogram of `name`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceLogo {
	pub kind: ResourceKind,
	pub name: String,
	pub mark: Option<&'static str>,
}

/// One project of the grid.
#[derive(Debug, Clone)]
pub struct ProjectCard {
	pub project: Project,
	/// Slug of the project's first environment, where its card leads.
	pub default_environment: Option<String>,
	pub environments: i64,
	pub applications: i64,
	pub databases: i64,
	pub models: i64,
	/// One per resource, applications first.
	pub logos: Vec<ResourceLogo>,
	/// When the newest deployment of any application finished (or started).
	pub last_deploy: Option<OffsetDateTime>,
}

/// One tile of an environment.
#[derive(Debug, Clone)]
pub struct ResourceCard {
	pub kind: ResourceKind,
	pub id: i64,
	pub name: String,
	pub mark: Option<&'static str>,
	/// A short line under the name: the repository, the engine and version.
	pub detail: String,
	/// An application's generated hostname.
	pub host: Option<String>,
	pub status: ResourceStatus,
	/// The newest deployment of an application, or when a model last changed.
	pub changed_at: Option<OffsetDateTime>,
}

#[derive(Debug, Clone)]
pub struct EnvironmentView {
	pub project: Project,
	/// Every environment of the project, in display order, for the tabs.
	pub environments: Vec<Environment>,
	pub environment: Environment,
	pub resources: Vec<ResourceCard>,
}

/// Which environment of a project to show.
#[derive(Debug, Clone, Copy)]
pub enum Pick<'a> {
	/// The first by position.
	Default,
	Slug(&'a str),
	Id(EnvironmentId),
}

// --- logos ---------------------------------------------------------------------------

/// The git host's mark for an application, else Docker's for a Dockerfile build, else none.
#[must_use]
pub fn application_mark(repo_url: &str, strategy: BuildStrategy) -> Option<&'static str> {
	let url = repo_url.to_ascii_lowercase();
	[
		("github", "github"),
		("gitlab", "gitlab"),
		("codeberg", "forgejo"),
		("forgejo", "forgejo"),
		("gitea", "gitea"),
	]
	.into_iter()
	.find(|(needle, _)| url.contains(needle))
	.map(|(_, mark)| mark)
	.or(match strategy {
		BuildStrategy::Dockerfile => Some("docker"),
		BuildStrategy::Nixpacks => None,
	})
}

#[must_use]
pub const fn database_mark(engine: DbEngine) -> &'static str {
	match engine {
		DbEngine::Postgres => "postgres",
		DbEngine::Mysql => "mysql",
		DbEngine::Mongodb => "mongodb",
		DbEngine::Redis => "redis",
	}
}

/// Ollama has its own mark; every other engine serves weights from Hugging Face.
#[must_use]
pub const fn model_mark(engine: ModelEngine) -> &'static str {
	match engine {
		ModelEngine::Ollama => "ollama",
		ModelEngine::LlamaCpu
		| ModelEngine::LlamaCuda
		| ModelEngine::LlamaVulkan
		| ModelEngine::Vllm
		| ModelEngine::Colibri
		| ModelEngine::Custom => "huggingface",
	}
}

/// `github.com/acme/api` for any clone URL: no scheme, no credentials, no `.git`.
#[must_use]
pub fn repo_label(url: &str) -> String {
	let url = url.trim();
	let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
	// `git@host:org/repo` and `https://user:token@host/org/repo` both lose the userinfo.
	let rest = rest.rsplit_once('@').map_or(rest, |(_, after)| after);
	let rest = rest.replacen(':', "/", 1);
	rest.trim_end_matches('/')
		.trim_end_matches(".git")
		.to_owned()
}

fn db_label(engine: DbEngine) -> &'static str {
	databases::service::engine_options()
		.into_iter()
		.find(|o| o.kind == engine)
		.map_or("Database", |o| o.label)
}

// --- the projects grid ---------------------------------------------------------------

struct ProjectRow {
	project: Project,
	default_environment: Option<String>,
	environments: i64,
	applications: i64,
	databases: i64,
	models: i64,
	last_deploy: Option<OffsetDateTime>,
}

async fn project_rows(db: &PgPool, team: TeamId) -> Result<Vec<ProjectRow>, sqlx::Error> {
	let rows = sqlx::query!(
		r#"
			SELECT p.id AS "id: ProjectId", p.team_id AS "team_id: TeamId", p.name, p.slug,
				(SELECT e.slug FROM environments e WHERE e.project_id = p.id
					ORDER BY e.position, e.id LIMIT 1) AS default_environment,
				(SELECT COUNT(*) FROM environments e WHERE e.project_id = p.id) AS "environments!",
				(SELECT COUNT(*) FROM applications a WHERE a.project_id = p.id) AS "applications!",
				(SELECT COUNT(*) FROM databases d WHERE d.project_id = p.id) AS "databases!",
				(SELECT COUNT(*) FROM models m WHERE m.project_id = p.id) AS "models!",
				(SELECT MAX(COALESCE(d.finished_at, d.started_at))
					FROM deployments d JOIN applications a ON a.id = d.application_id
					WHERE a.project_id = p.id) AS last_deploy
			FROM projects p
			WHERE p.team_id = $1
			ORDER BY p.name, p.id
		"#,
		team as TeamId
	)
	.fetch_all(db)
	.await?;
	Ok(rows
		.into_iter()
		.map(|r| ProjectRow {
			project: Project {
				id: r.id,
				team_id: r.team_id,
				name: r.name,
				slug: r.slug,
			},
			default_environment: r.default_environment,
			environments: r.environments,
			applications: r.applications,
			databases: r.databases,
			models: r.models,
			last_deploy: r.last_deploy,
		})
		.collect())
}

/// Every project of the team with its counts, one logo per resource and its last deploy;
/// needs `read project`. Counts and logos of a kind the caller may not read are left out.
pub async fn cards(db: &PgPool, user: UserId, team: TeamId) -> AppResult<Vec<ProjectCard>> {
	authorize(db, user, team, Action::Read, Resource::Project).await?;
	let can_apps = can(db, user, team, Resource::Application).await;
	let can_dbs = can(db, user, team, Resource::Database).await;
	let can_models = can(db, user, team, Resource::Model).await;

	let mut out: Vec<ProjectCard> = project_rows(db, team)
		.await?
		.into_iter()
		.map(|r| ProjectCard {
			project: r.project,
			default_environment: r.default_environment,
			environments: r.environments,
			applications: if can_apps { r.applications } else { 0 },
			databases: if can_dbs { r.databases } else { 0 },
			models: if can_models { r.models } else { 0 },
			logos: Vec::new(),
			last_deploy: if can_apps { r.last_deploy } else { None },
		})
		.collect();

	let mut push = |project: ProjectId, logo: ResourceLogo| {
		if let Some(card) = out.iter_mut().find(|c| c.project.id == project) {
			card.logos.push(logo);
		}
	};
	if can_apps {
		let apps = sqlx::query!(
			r#"
				SELECT a.project_id AS "project_id: ProjectId", a.name, a.repo_url,
					a.build_strategy AS "build_strategy: BuildStrategy"
				FROM applications a JOIN projects p ON p.id = a.project_id
				WHERE p.team_id = $1
				ORDER BY a.id
			"#,
			team as TeamId
		)
		.fetch_all(db)
		.await?;
		for a in apps {
			push(
				a.project_id,
				ResourceLogo {
					kind: ResourceKind::Application,
					mark: application_mark(&a.repo_url, a.build_strategy),
					name: a.name,
				},
			);
		}
	}
	if can_dbs {
		let dbs = sqlx::query!(
			r#"
				SELECT d.project_id AS "project_id: ProjectId", d.name,
					d.engine AS "engine: DbEngine"
				FROM databases d JOIN projects p ON p.id = d.project_id
				WHERE p.team_id = $1
				ORDER BY d.id
			"#,
			team as TeamId
		)
		.fetch_all(db)
		.await?;
		for d in dbs {
			push(
				d.project_id,
				ResourceLogo {
					kind: ResourceKind::Database,
					mark: Some(database_mark(d.engine)),
					name: d.name,
				},
			);
		}
	}
	if can_models {
		for m in models::list_by_team(db, team).await? {
			push(
				m.project_id,
				ResourceLogo {
					kind: ResourceKind::Model,
					mark: Some(model_mark(m.engine)),
					name: m.name,
				},
			);
		}
	}
	Ok(out)
}

async fn can(db: &PgPool, user: UserId, team: TeamId, resource: Resource) -> bool {
	authorize(db, user, team, Action::Read, resource)
		.await
		.is_ok()
}

// --- renaming ------------------------------------------------------------------------

/// Changes a project's name (its slug stays, so links keep working); needs `update
/// project`. A project of another team is `ProjectNotFound`.
pub async fn rename(
	db: &PgPool,
	user: UserId,
	team: TeamId,
	id: ProjectId,
	name: &str,
) -> AppResult<Project> {
	authorize(db, user, team, Action::Update, Resource::Project).await?;
	let project = scope::project(db, team, id).await?;
	let name = super::service::validate_name(name)?;
	Ok(super::update_name(db, project.id, &name).await?)
}

// --- an environment ------------------------------------------------------------------

struct Latest {
	application: ApplicationId,
	status: DeploymentStatus,
	at: Option<OffsetDateTime>,
}

/// The newest deployment of each application of the environment.
async fn latest_deployments(
	db: &PgPool,
	environment: EnvironmentId,
) -> Result<Vec<Latest>, sqlx::Error> {
	let rows = sqlx::query!(
		r#"
			SELECT DISTINCT ON (d.application_id)
				d.application_id AS "application_id: ApplicationId",
				d.status AS "status!: DeploymentStatus",
				COALESCE(d.finished_at, d.started_at) AS at
			FROM deployments d JOIN applications a ON a.id = d.application_id
			WHERE a.environment_id = $1
			ORDER BY d.application_id, d.id DESC
		"#,
		environment as EnvironmentId
	)
	.fetch_all(db)
	.await?;
	Ok(rows
		.into_iter()
		.map(|r| Latest {
			application: r.application_id,
			status: r.status,
			at: r.at,
		})
		.collect())
}

/// One environment of a project with its tabs and resource cards; needs `read project`.
/// A project or environment of another team (or of another project) is a not-found error,
/// and an unknown slug is `EnvironmentNotFound`.
pub async fn environment_view(
	db: &PgPool,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	pick: Pick<'_>,
) -> AppResult<EnvironmentView> {
	authorize(db, user, team, Action::Read, Resource::Project).await?;
	let project = scope::project(db, team, project).await?;
	let all = environments::list_by_project(db, project.id).await?;
	let environment = match pick {
		Pick::Default => all.first(),
		Pick::Slug(slug) => all.iter().find(|e| e.slug == slug),
		Pick::Id(id) => all.iter().find(|e| e.id == id),
	}
	.cloned()
	.ok_or(ServerError::EnvironmentNotFound)?;

	let mut resources = Vec::new();
	if can(db, user, team, Resource::Application).await {
		let servers = servers::list_by_team(db, team).await?;
		let latest = latest_deployments(db, environment.id).await?;
		for app in applications::list_by_environment(db, environment.id).await? {
			let Some(server) = servers.iter().find(|s| s.id == app.server_id) else {
				continue;
			};
			let latest = latest.iter().find(|l| l.application == app.id);
			resources.push(ResourceCard {
				kind: ResourceKind::Application,
				id: app.id.0,
				mark: application_mark(&app.repo_url, app.build_strategy),
				detail: format!("{} · {}", repo_label(&app.repo_url), app.git_ref),
				host: Some(applications::public_hostname(&app.slug, server)),
				status: latest.map_or(ResourceStatus::NotDeployed, |l| {
					ResourceStatus::Deployment(l.status)
				}),
				changed_at: latest.and_then(|l| l.at),
				name: app.name,
			});
		}
	}
	if can(db, user, team, Resource::Database).await {
		for d in databases::list_by_environment(db, environment.id).await? {
			resources.push(ResourceCard {
				kind: ResourceKind::Database,
				id: d.id.0,
				mark: Some(database_mark(d.engine)),
				detail: format!("{} {}", db_label(d.engine), d.version),
				host: None,
				status: ResourceStatus::Database(d.status),
				changed_at: None,
				name: d.name,
			});
		}
	}
	if can(db, user, team, Resource::Model).await {
		for m in models::list_by_environment(db, environment.id).await? {
			resources.push(ResourceCard {
				kind: ResourceKind::Model,
				id: m.id.0,
				mark: Some(model_mark(m.engine)),
				detail: models::engines::spec(m.engine).label.to_owned(),
				host: None,
				status: ResourceStatus::Model(m.status),
				changed_at: Some(m.updated_at),
				name: m.name,
			});
		}
	}

	Ok(EnvironmentView {
		project,
		environments: all,
		environment,
		resources,
	})
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn a_git_host_picks_its_mark_and_a_dockerfile_falls_back_to_docker() {
		let d = BuildStrategy::Dockerfile;
		let n = BuildStrategy::Nixpacks;
		assert_eq!(
			application_mark("git@github.com:acme/api.git", n),
			Some("github")
		);
		assert_eq!(
			application_mark("https://GitLab.example.com/a/b", n),
			Some("gitlab")
		);
		assert_eq!(
			application_mark("https://codeberg.org/a/b", d),
			Some("forgejo")
		);
		assert_eq!(
			application_mark("https://git.example.com/a/b", d),
			Some("docker")
		);
		assert_eq!(application_mark("https://git.example.com/a/b", n), None);
	}

	#[test]
	fn a_repo_label_drops_the_scheme_the_credentials_and_the_suffix() {
		assert_eq!(
			repo_label("git@github.com:acme/api.git"),
			"github.com/acme/api"
		);
		assert_eq!(
			repo_label("https://user:s3cret@git.example.com/acme/api.git/"),
			"git.example.com/acme/api"
		);
		assert_eq!(
			repo_label("ssh://git@host.example:2222/acme/api"),
			"host.example/2222/acme/api"
		);
	}

	#[test]
	fn the_type_filter_round_trips() {
		for kind in [
			ResourceKind::Application,
			ResourceKind::Database,
			ResourceKind::Model,
		] {
			assert_eq!(ResourceKind::from_slug(kind.slug()), Some(kind));
		}
		assert_eq!(ResourceKind::from_slug("all"), None);
	}
}
