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
use crate::audit::{self, Diff, Target};
use crate::auth::permission::{Action, Resource, authorize};
use crate::databases::{self, DatabaseStatus, EngineKind as DbEngine};
use crate::deployments::DeploymentStatus;
use crate::environments::{self, Environment};
use crate::error::{AppResult, ServerError};
use crate::health_gate::State as GateState;
use crate::ids::{ApplicationId, DeploymentId, EnvironmentId, ProjectId, TeamId, UserId};
use crate::models::{self, EngineKind as ModelEngine, ModelStatus};
use crate::stacks::{self, StackStatus};
use crate::{scope, servers};

/// The three kinds of thing an environment holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ResourceKind {
	Application,
	Database,
	Model,
	Stack,
}

impl ResourceKind {
	#[must_use]
	pub const fn label(self) -> &'static str {
		match self {
			Self::Application => "Application",
			Self::Database => "Database",
			Self::Model => "Model",
			Self::Stack => "Stack",
		}
	}

	/// The value of the `type` filter and of `data-kind`.
	#[must_use]
	pub const fn slug(self) -> &'static str {
		match self {
			Self::Application => "application",
			Self::Database => "database",
			Self::Model => "model",
			Self::Stack => "stack",
		}
	}

	#[must_use]
	pub fn from_slug(slug: &str) -> Option<Self> {
		[Self::Application, Self::Database, Self::Model, Self::Stack]
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
	Stack(StackStatus),
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
	/// A template stack's own logo (a URL); it wins over `mark`.
	pub logo_url: Option<String>,
	/// A short line under the name: the repository, the engine and version.
	pub detail: String,
	/// An application's generated hostname.
	pub host: Option<String>,
	pub status: ResourceStatus,
	/// The newest deployment of an application, or when a model last changed.
	pub changed_at: Option<OffsetDateTime>,
	/// The newest CPU and memory sample of a running resource; `None` when it has none
	/// recent (stopped, not yet sampled).
	pub usage: Option<Usage>,
	/// A deployment on its way while an earlier one sets `status`: an application that runs
	/// while its next release builds. `None` when nothing is in flight, and for a first
	/// deployment, whose own phase is already the status.
	pub deployment: Option<InFlight>,
}

/// A deployment that is queued, building or deploying.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InFlight {
	pub id: DeploymentId,
	pub status: DeploymentStatus,
	/// The commit it builds, once known.
	pub commit: Option<String>,
}

impl InFlight {
	/// `9f3c2ab`: the commit as people quote it.
	#[must_use]
	pub fn short_commit(&self) -> Option<&str> {
		self.commit.as_deref().map(short_sha)
	}
}

/// The first seven characters of a commit sha.
#[must_use]
pub fn short_sha(sha: &str) -> &str {
	sha.get(..7).unwrap_or(sha)
}

/// Queued, building or deploying: a deployment that can still change by itself.
#[must_use]
pub const fn deployment_in_flight(status: DeploymentStatus) -> bool {
	matches!(
		status,
		DeploymentStatus::Queued | DeploymentStatus::Building | DeploymentStatus::Deploying
	)
}

/// One resource's latest sample from the metrics sampler.
#[derive(Debug, Clone, Copy)]
pub struct Usage {
	/// Share of the whole host's CPU, 0 to 100.
	pub cpu: Option<f64>,
	/// Resident memory, bytes.
	pub mem: i64,
}

/// A sample older than this is not "now": the resource stopped or the sampler is behind.
const USAGE_FRESH_MINUTES: i32 = 5;

/// Fills in each resource's latest sample, in one query for the whole environment.
async fn attach_usage(db: &PgPool, resources: &mut [ResourceCard]) -> AppResult<()> {
	if resources.is_empty() {
		return Ok(());
	}
	let kinds: Vec<String> = resources.iter().map(|r| r.kind.slug().to_owned()).collect();
	let ids: Vec<i64> = resources.iter().map(|r| r.id).collect();
	let rows = sqlx::query!(
		r#"
			SELECT DISTINCT ON (m.kind, m.resource_id)
				m.kind AS "kind!", m.resource_id AS "resource_id!", m.cpu, m.mem AS "mem!"
			FROM resource_metrics m
			JOIN UNNEST($1::text[], $2::bigint[]) AS want(kind, id)
				ON want.kind = m.kind AND want.id = m.resource_id
			WHERE NOT m.rollup AND m.at > now() - make_interval(mins => $3)
			ORDER BY m.kind, m.resource_id, m.at DESC
		"#,
		&kinds,
		&ids,
		USAGE_FRESH_MINUTES,
	)
	.fetch_all(db)
	.await?;
	for r in resources {
		r.usage = rows
			.iter()
			.find(|row| row.kind == r.kind.slug() && row.resource_id == r.id)
			.map(|row| Usage {
				cpu: row.cpu,
				mem: row.mem,
			});
	}
	Ok(())
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
pub fn application_mark(repo_url: Option<&str>, strategy: BuildStrategy) -> Option<&'static str> {
	// An image app has no repository: Docker's mark is its logo.
	let Some(repo_url) = repo_url else {
		return Some("docker");
	};
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
					mark: application_mark(a.repo_url.as_deref(), a.build_strategy),
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
	let renamed = super::update_name(db, project.id, &name).await?;
	audit::log(
		db,
		user,
		team,
		"project.rename",
		Target::new("project", project.id)
			.named(&project.slug)
			.in_project(project.id),
		Diff::new().change("name", project.name.clone(), renamed.name.clone()),
	)
	.await;
	Ok(renamed)
}

// --- an environment ------------------------------------------------------------------

struct Latest {
	application: ApplicationId,
	id: DeploymentId,
	status: DeploymentStatus,
	commit: Option<String>,
	at: Option<OffsetDateTime>,
}

/// For each application of the environment, its newest settled deployment (what is live, or
/// what last failed) and its newest one still in flight; either may be missing.
async fn latest_deployments(
	db: &PgPool,
	environment: EnvironmentId,
) -> Result<Vec<Latest>, sqlx::Error> {
	let rows = sqlx::query!(
		r#"
			SELECT DISTINCT ON (d.application_id, d.status IN ('queued', 'building', 'deploying'))
				d.id AS "id: DeploymentId",
				d.application_id AS "application_id: ApplicationId",
				d.status AS "status!: DeploymentStatus",
				d.commit_sha,
				COALESCE(d.finished_at, d.started_at) AS at
			FROM deployments d JOIN applications a ON a.id = d.application_id
			WHERE a.environment_id = $1
			ORDER BY d.application_id, d.status IN ('queued', 'building', 'deploying'), d.id DESC
		"#,
		environment as EnvironmentId
	)
	.fetch_all(db)
	.await?;
	Ok(rows
		.into_iter()
		.map(|r| Latest {
			application: r.application_id,
			id: r.id,
			status: r.status,
			commit: r.commit_sha,
			at: r.at,
		})
		.collect())
}

/// What an application's card says, from its newest settled and newest in-flight
/// deployments: the settled one is the status and the other rides along; with nothing settled
/// yet, the first deployment's own phase is the status.
fn application_state(
	settled: Option<&Latest>,
	flying: Option<&Latest>,
) -> (ResourceStatus, Option<InFlight>, Option<OffsetDateTime>) {
	match (settled, flying) {
		(Some(live), flying) => (
			ResourceStatus::Deployment(live.status),
			flying.map(|f| InFlight {
				id: f.id,
				status: f.status,
				commit: f.commit.clone(),
			}),
			live.at,
		),
		(None, Some(first)) => (ResourceStatus::Deployment(first.status), None, first.at),
		(None, None) => (ResourceStatus::NotDeployed, None, None),
	}
}

/// The stacks of an environment as resource cards: a template stack carries the template's own
/// logo, any other the Docker mark; the link goes to the first service's hostname.
async fn stack_cards(db: &PgPool, environment: EnvironmentId) -> AppResult<Vec<ResourceCard>> {
	let mut cards = Vec::new();
	for st in stacks::list_by_environment(db, environment).await? {
		let services = stacks::list_services(db, st.id).await?;
		let host = services.iter().find_map(|svc| svc.domain.clone());
		let origin =
			crate::templates::service::origin(db, crate::templates::service::Kind::Stack, st.id.0)
				.await?;
		let logo_url = origin.map(|o| crate::logos::template_url(&o.template));
		cards.push(ResourceCard {
			kind: ResourceKind::Stack,
			id: st.id.0,
			mark: Some("docker"),
			logo_url,
			detail: match services.len() {
				1 => "1 service".to_owned(),
				n => format!("{n} services"),
			},
			host,
			status: ResourceStatus::Stack(st.status),
			changed_at: Some(st.updated_at),
			usage: None,
			deployment: None,
			name: st.name,
		});
	}
	Ok(cards)
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
			let mine = |flying: bool| {
				latest
					.iter()
					.find(|l| l.application == app.id && deployment_in_flight(l.status) == flying)
			};
			let (status, deployment, changed_at) = application_state(mine(false), mine(true));
			// A git application shows its host's mark and repository; an image application
			// the registry's logo for its image (else Docker's) and the image itself.
			let (mark, logo_url, detail) = match (&app.repo_url, &app.image) {
				(Some(repo), _) => (
					application_mark(Some(repo), app.build_strategy),
					None,
					format!("{} · {}", repo_label(repo), app.git_ref),
				),
				(None, Some(image)) => (
					Some("docker"),
					crate::logos::for_image(image),
					image.clone(),
				),
				(None, None) => (None, None, String::new()),
			};
			resources.push(ResourceCard {
				kind: ResourceKind::Application,
				id: app.id.0,
				mark,
				logo_url,
				detail,
				host: Some(applications::public_hostname(&app.slug, server)),
				status,
				changed_at,
				usage: None,
				deployment,
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
				logo_url: None,
				detail: format!("{} {}", db_label(d.engine), d.version),
				host: None,
				status: ResourceStatus::Database(d.status),
				changed_at: None,
				usage: None,
				deployment: None,
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
				logo_url: None,
				detail: models::engines::spec(m.engine).label.to_owned(),
				host: None,
				status: ResourceStatus::Model(m.status),
				changed_at: Some(m.updated_at),
				usage: None,
				deployment: None,
				name: m.name,
			});
		}
	}
	if can(db, user, team, Resource::Stack).await {
		resources.extend(stack_cards(db, environment.id).await?);
	}
	attach_usage(db, &mut resources).await?;

	Ok(EnvironmentView {
		project,
		environments: all,
		environment,
		resources,
	})
}

// --- deployments of an environment ----------------------------------------------------

/// The deployments of an environment's applications over the last seven days.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DeployWeek {
	pub total: i64,
	/// Failed or crashed, not counting the ones the health gate rolled back.
	pub failed: i64,
	/// Went live, failed its health watch, and the previous image was put back.
	pub rolled_back: i64,
}

/// Reading deployments, with the applications they belong to: `read application` and
/// `read deployment`.
async fn allow_deployments(db: &PgPool, user: UserId, team: TeamId) -> AppResult<()> {
	authorize(db, user, team, Action::Read, Resource::Application).await?;
	authorize(db, user, team, Action::Read, Resource::Deployment).await
}

/// How many deployments the environment's applications had in the last seven days, and how
/// many of them failed or were rolled back; needs `read application` and `read deployment`.
/// An environment of another team counts nothing.
pub async fn deploy_week(
	db: &PgPool,
	user: UserId,
	team: TeamId,
	environment: EnvironmentId,
) -> AppResult<DeployWeek> {
	allow_deployments(db, user, team).await?;
	let row = sqlx::query!(
		r#"
			SELECT COUNT(*) AS "total!",
				COUNT(*) FILTER (WHERE d.status IN ('failed', 'crashed')
					AND h.state IS DISTINCT FROM 'rolled_back') AS "failed!",
				COUNT(*) FILTER (WHERE h.state = 'rolled_back') AS "rolled_back!"
			FROM deployments d
			JOIN applications a ON a.id = d.application_id
			JOIN projects p ON p.id = a.project_id
			LEFT JOIN deployment_health h ON h.deployment_id = d.id
			WHERE a.environment_id = $1 AND p.team_id = $2
				AND d.created_at > now() - INTERVAL '7 days'
		"#,
		environment as EnvironmentId,
		team as TeamId,
	)
	.fetch_one(db)
	.await?;
	Ok(DeployWeek {
		total: row.total,
		failed: row.failed,
		rolled_back: row.rolled_back,
	})
}

/// One deployment of an environment's application, for its activity feed.
#[derive(Debug, Clone)]
pub struct DeploymentRecord {
	pub id: DeploymentId,
	pub application: ApplicationId,
	/// The application's name.
	pub name: String,
	pub status: DeploymentStatus,
	pub commit: Option<String>,
	pub error: Option<String>,
	/// The health gate's verdict, when the application has a gate.
	pub gate: Option<GateState>,
	/// An automatic rollback started it.
	pub rollback: bool,
	pub created_at: OffsetDateTime,
	pub finished_at: Option<OffsetDateTime>,
}

/// The environment's deployments that moved last, newest first, at most `limit`; needs `read
/// application` and `read deployment`. "Moved" is when one finished, else when it was queued.
pub async fn recent_deployments(
	db: &PgPool,
	user: UserId,
	team: TeamId,
	environment: EnvironmentId,
	limit: i64,
) -> AppResult<Vec<DeploymentRecord>> {
	allow_deployments(db, user, team).await?;
	let rows = sqlx::query!(
		r#"
			SELECT d.id AS "id: DeploymentId", d.application_id AS "application_id: ApplicationId",
				a.name, d.status AS "status!: DeploymentStatus", d.commit_sha, d.error_message,
				h.state AS "gate?: GateState", (h.rollback_of IS NOT NULL) AS "rollback!",
				d.created_at, d.finished_at
			FROM deployments d
			JOIN applications a ON a.id = d.application_id
			JOIN projects p ON p.id = a.project_id
			LEFT JOIN deployment_health h ON h.deployment_id = d.id
			WHERE a.environment_id = $1 AND p.team_id = $2
			ORDER BY COALESCE(d.finished_at, d.created_at) DESC, d.id DESC
			LIMIT $3
		"#,
		environment as EnvironmentId,
		team as TeamId,
		limit.clamp(1, 50),
	)
	.fetch_all(db)
	.await?;
	Ok(rows
		.into_iter()
		.map(|r| DeploymentRecord {
			id: r.id,
			application: r.application_id,
			name: r.name,
			status: r.status,
			commit: r.commit_sha,
			error: r.error_message,
			gate: r.gate,
			rollback: r.rollback,
			created_at: r.created_at,
			finished_at: r.finished_at,
		})
		.collect())
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn a_git_host_picks_its_mark_and_a_dockerfile_falls_back_to_docker() {
		let d = BuildStrategy::Dockerfile;
		let n = BuildStrategy::Nixpacks;
		assert_eq!(
			application_mark(Some("git@github.com:acme/api.git"), n),
			Some("github")
		);
		assert_eq!(
			application_mark(Some("https://GitLab.example.com/a/b"), n),
			Some("gitlab")
		);
		assert_eq!(
			application_mark(Some("https://codeberg.org/a/b"), d),
			Some("forgejo")
		);
		assert_eq!(
			application_mark(Some("https://git.example.com/a/b"), d),
			Some("docker")
		);
		assert_eq!(
			application_mark(Some("https://git.example.com/a/b"), n),
			None
		);
		assert_eq!(application_mark(None, n), Some("docker"));
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
	fn a_live_deployment_is_the_status_and_one_in_flight_rides_along() {
		let at = OffsetDateTime::from_unix_timestamp(1_800_000_000).unwrap();
		let latest = |id: i64, status| Latest {
			application: ApplicationId(1),
			id: DeploymentId(id),
			status,
			commit: Some("9f3c2abbbbbb".to_owned()),
			at: Some(at),
		};
		let live = latest(1, DeploymentStatus::Running);
		let next = latest(2, DeploymentStatus::Building);
		let (status, flying, when) = application_state(Some(&live), Some(&next));
		assert_eq!(
			status,
			ResourceStatus::Deployment(DeploymentStatus::Running)
		);
		let flying = flying.unwrap();
		assert_eq!(flying.status, DeploymentStatus::Building);
		assert_eq!(flying.short_commit(), Some("9f3c2ab"));
		assert_eq!(when, Some(at));
		// A first deployment's phase is the status itself, with nothing riding along.
		let (status, flying, _) = application_state(None, Some(&next));
		assert_eq!(
			status,
			ResourceStatus::Deployment(DeploymentStatus::Building)
		);
		assert_eq!(flying, None);
		let (status, flying, when) = application_state(None, None);
		assert_eq!(status, ResourceStatus::NotDeployed);
		assert_eq!((flying, when), (None, None));
		assert!(deployment_in_flight(DeploymentStatus::Queued));
		assert!(!deployment_in_flight(DeploymentStatus::Crashed));
		assert_eq!(short_sha("abc"), "abc");
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
