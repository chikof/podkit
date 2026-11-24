//! Quadlet: what podkit runs, as the Podman Quadlet units that would run it under systemd
//! without podkit. A read-only view per resource, and an "eject" export of every unit of a
//! project, an environment or the team as a `.tar.gz`.
//!
//! The units come from `proto::quadlet`, fed the very spec each resource's deploy job sends
//! the agent (`deployments::run::current_spec`, `databases::run::full_spec`,
//! `models::run::full_spec`, `stacks::run::plan`), so they match what runs. Building a spec
//! records network memberships the way a deploy does; nothing else is written and nothing
//! reaches an agent.
//!
//! Secrets never leave through the view: the environment and a stack's mounted files go into
//! files beside the units, and only an export that asks for them and may reveal them
//! (a session or a `read:sensitive` token, plus `update` on env vars, databases, models and
//! stacks) carries them. Every export is audited (`quadlet.export`).

use std::collections::BTreeMap;
use std::io::Write as _;

use proto::DeploySpec;
use proto::quadlet::{self, File, Options, ReadmeEntry, Units};

use crate::audit::{self, Diff, Target};
use crate::auth::permission::{Action, Resource, authorize};
use crate::error::{AppResult, ServerError};
use crate::ids::{
	ApplicationId, DatabaseId, EnvironmentId, ModelId, ProjectId, ServerId, StackId, TeamId, UserId,
};
use crate::jobs::JobFailure;
use crate::projects::Project;
use crate::state::AppState;
use crate::{applications, databases, environments, model_files, models, projects, scope, stacks};

pub mod routes;
#[cfg(test)]
mod tests;

/// The kinds of resource that run containers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
	Application,
	Database,
	Model,
	Stack,
}

impl Kind {
	/// `application`, `database`, `model`, `stack`.
	pub const fn as_str(self) -> &'static str {
		match self {
			Self::Application => "application",
			Self::Database => "database",
			Self::Model => "model",
			Self::Stack => "stack",
		}
	}

	const fn resource(self) -> Resource {
		match self {
			Self::Application => Resource::Application,
			Self::Database => Resource::Database,
			Self::Model => Resource::Model,
			Self::Stack => Resource::Stack,
		}
	}
}

/// The units of one container spec: one per application, database and model, one per
/// service of a stack.
#[derive(Debug, Clone)]
pub struct Part {
	/// What the units run, e.g. `application web` or `stack shop, service db`.
	pub label: String,
	pub units: Units,
}

/// The units of one resource.
#[derive(Debug, Clone)]
pub struct View {
	pub kind: Kind,
	pub id: i64,
	pub name: String,
	/// The server the containers run on.
	pub server: String,
	pub parts: Vec<Part>,
}

impl View {
	/// The unit files, once each (two services of a stack share its network): networks and
	/// volumes first, then the containers in start order.
	pub fn files(&self) -> Vec<&File> {
		let mut out: Vec<&File> = Vec::new();
		let all = self
			.parts
			.iter()
			.flat_map(|p| p.units.networks.iter().chain(&p.units.volumes))
			.chain(self.parts.iter().flat_map(|p| &p.units.containers));
		for file in all {
			if !out.iter().any(|f| f.name == file.name) {
				out.push(file);
			}
		}
		out
	}

	/// The env files the units read, with the variable names in each: never the values.
	pub fn env_files(&self) -> Vec<(&str, &[String])> {
		self.parts
			.iter()
			.filter_map(|p| {
				p.units
					.env
					.as_ref()
					.map(|f| (f.name.as_str(), p.units.env_keys.as_slice()))
			})
			.collect()
	}

	/// The names of the files mounted into the containers (their content is never shown).
	pub fn mounted(&self) -> Vec<&str> {
		self.parts
			.iter()
			.flat_map(|p| p.units.mounted.iter().map(|f| f.name.as_str()))
			.collect()
	}
}

/// The full path a unit is installed at, for a rootless Podman.
pub fn target_path(name: &str) -> String {
	format!("{}/{name}", quadlet::ROOTLESS_DIR)
}

fn part(spec: &DeploySpec, label: String, after: &[String]) -> Part {
	let description = format!("podkit {label}");
	let units = quadlet::generate(
		spec,
		&Options {
			description: &description,
			after,
		},
	);
	Part { label, units }
}

/// A deploy-path failure as a message for the person: the same words a failed deploy shows.
fn message(e: JobFailure) -> String {
	match e {
		JobFailure::Failed(m) => m,
		JobFailure::Cancelled => "cancelled".to_owned(),
	}
}

async fn server_name(s: &AppState, team: TeamId, id: ServerId) -> AppResult<String> {
	Ok(scope::server(&s.db, team, id).await?.name)
}

// --- building the units of one resource -------------------------------------------------------

/// The units of an application, or why there are none.
async fn of_application(
	s: &AppState,
	team: TeamId,
	app: &applications::Application,
) -> AppResult<Result<View, String>> {
	let server = server_name(s, team, app.server_id).await?;
	let spec = match crate::deployments::run::current_spec(s, app).await {
		Ok(spec) => spec,
		Err(e) => return Ok(Err(message(e))),
	};
	Ok(Ok(View {
		kind: Kind::Application,
		id: app.id.0,
		name: app.name.clone(),
		server,
		parts: vec![part(&spec, format!("application {}", app.name), &[])],
	}))
}

async fn of_database(
	s: &AppState,
	team: TeamId,
	database: &databases::Database,
) -> AppResult<Result<View, String>> {
	let server = server_name(s, team, database.server_id).await?;
	let spec = match databases::run::full_spec(s, 0, database).await {
		Ok(spec) => spec,
		Err(e) => return Ok(Err(message(e))),
	};
	Ok(Ok(View {
		kind: Kind::Database,
		id: database.id.0,
		name: database.name.clone(),
		server,
		parts: vec![part(&spec, format!("database {}", database.name), &[])],
	}))
}

async fn of_model(
	s: &AppState,
	team: TeamId,
	model: &models::Model,
) -> AppResult<Result<View, String>> {
	let server = scope::server(&s.db, team, model.server_id).await?;
	let Some(file) = model_files::find_by_id(&s.db, model.model_files_id).await? else {
		return Ok(Err("the model's files are gone".to_owned()));
	};
	let spec = match models::run::full_spec(s, model, &server, &file, 0).await {
		Ok(spec) => spec,
		Err(e) => return Ok(Err(message(e))),
	};
	Ok(Ok(View {
		kind: Kind::Model,
		id: model.id.0,
		name: model.name.clone(),
		server: server.name,
		parts: vec![part(&spec, format!("model {}", model.name), &[])],
	}))
}

async fn of_stack(
	s: &AppState,
	team: TeamId,
	stack: &stacks::Stack,
) -> AppResult<Result<View, String>> {
	let server = server_name(s, team, stack.server_id).await?;
	let services = stacks::list_services(&s.db, stack.id).await?;
	let plan = match stacks::run::plan(s, stack, &services).await {
		Ok(plan) => plan,
		Err(e) => return Ok(Err(message(e))),
	};
	let parts = plan
		.services
		.iter()
		.map(|planned| {
			let after: Vec<String> = planned
				.depends_on
				.iter()
				.map(|(name, _)| stacks::translate::service_name(stack.id.0, name))
				.collect();
			part(
				&planned.spec,
				format!("stack {}, service {}", stack.name, planned.name),
				&after,
			)
		})
		.collect();
	Ok(Ok(View {
		kind: Kind::Stack,
		id: stack.id.0,
		name: stack.name.clone(),
		server,
		parts,
	}))
}

/// A resource that cannot be turned into units: the reason, as a conflict.
fn unavailable(r: Result<View, String>) -> AppResult<View> {
	r.map_err(|m| ServerError::Conflict(format!("no Quadlet units: {m}")))
}

/// The view needs `read` on the resource and on env vars (the unit names the variables).
async fn authorize_view(s: &AppState, user: UserId, team: TeamId, kind: Kind) -> AppResult<()> {
	authorize(&s.db, user, team, Action::Read, kind.resource()).await?;
	authorize(&s.db, user, team, Action::Read, Resource::EnvVar).await
}

// --- the view ---------------------------------------------------------------------------------

/// The units of one application; needs `read application` and `read env_var`.
///
/// # Errors
/// `Forbidden`, `ApplicationNotFound` for another team's or project's application, and
/// `Conflict` when its spec cannot be built (the message a deploy would fail with).
pub async fn application(
	s: &AppState,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	id: ApplicationId,
) -> AppResult<View> {
	authorize_view(s, user, team, Kind::Application).await?;
	let app = scope::application(&s.db, team, project, id).await?;
	unavailable(of_application(s, team, &app).await?)
}

/// The units of one database; needs `read database` and `read env_var`.
///
/// # Errors
/// As [`application`].
pub async fn database(
	s: &AppState,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	id: DatabaseId,
) -> AppResult<View> {
	authorize_view(s, user, team, Kind::Database).await?;
	let database = scope::database(&s.db, team, project, id).await?;
	unavailable(of_database(s, team, &database).await?)
}

/// The units of one model; needs `read model` and `read env_var`.
///
/// # Errors
/// As [`application`].
pub async fn model(
	s: &AppState,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	id: ModelId,
) -> AppResult<View> {
	authorize_view(s, user, team, Kind::Model).await?;
	let model = scope::model(&s.db, team, project, id).await?;
	unavailable(of_model(s, team, &model).await?)
}

/// The units of every service of one stack; needs `read stack` and `read env_var`.
///
/// # Errors
/// As [`application`].
pub async fn stack(
	s: &AppState,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	id: StackId,
) -> AppResult<View> {
	authorize_view(s, user, team, Kind::Stack).await?;
	let stack = scope::stack(&s.db, team, project, id).await?;
	unavailable(of_stack(s, team, &stack).await?)
}

// --- the export -------------------------------------------------------------------------------

/// What an export covers.
#[derive(Debug, Clone, Copy)]
pub enum Scope {
	/// One project, or one environment of it.
	Project {
		project: ProjectId,
		environment: Option<EnvironmentId>,
	},
	/// Every project of the team.
	Team,
}

/// What an export asks for.
#[derive(Debug, Clone, Copy)]
pub struct Request {
	pub scope: Scope,
	/// Put the env files and mounted files, secrets included, into the archive.
	pub secrets: bool,
	/// Whether the caller may see secrets at all: a session, or a token with
	/// `read:sensitive` (`AuthUser::can_reveal`).
	pub can_reveal: bool,
}

/// A finished export.
#[derive(Debug)]
pub struct Archive {
	/// `podkit-quadlet-<project>.tar.gz`.
	pub filename: String,
	pub bytes: Vec<u8>,
}

/// Everything an export reads.
const READS: [Resource; 6] = [
	Resource::Project,
	Resource::Application,
	Resource::Database,
	Resource::Model,
	Resource::Stack,
	Resource::EnvVar,
];

/// What exporting secrets takes beyond the reads: what revealing them takes (a database
/// password needs `update database`), for every kind that has them.
const SECRET_UPDATES: [Resource; 4] = [
	Resource::EnvVar,
	Resource::Database,
	Resource::Model,
	Resource::Stack,
];

async fn authorize_export(
	s: &AppState,
	user: UserId,
	team: TeamId,
	req: &Request,
) -> AppResult<()> {
	for resource in READS {
		authorize(&s.db, user, team, Action::Read, resource).await?;
	}
	if req.secrets {
		if !req.can_reveal {
			return Err(ServerError::MissingScope(
				crate::auth::tokens::Scope::ReadSensitive.as_str(),
			));
		}
		for resource in SECRET_UPDATES {
			authorize(&s.db, user, team, Action::Update, resource).await?;
		}
	}
	Ok(())
}

/// The units of every resource of `environments`, databases first so the README starts
/// them first; and what could not be exported, with the reason.
async fn collect(
	s: &AppState,
	team: TeamId,
	environments: &[environments::Environment],
) -> AppResult<(Vec<View>, Vec<String>)> {
	let mut views = Vec::new();
	let mut skipped = Vec::new();
	let mut keep = |label: String, r: Result<View, String>| match r {
		Ok(view) => views.push(view),
		Err(m) => skipped.push(format!("{label}: {m}")),
	};
	for env in environments {
		for database in databases::list_by_environment(&s.db, env.id).await? {
			if database.status == databases::DatabaseStatus::Deleting {
				continue;
			}
			let r = of_database(s, team, &database).await?;
			keep(format!("database {}", database.name), r);
		}
		for model in models::list_by_environment(&s.db, env.id).await? {
			if model.status == models::ModelStatus::Removing {
				continue;
			}
			let r = of_model(s, team, &model).await?;
			keep(format!("model {}", model.name), r);
		}
		for stack in stacks::list_by_environment(&s.db, env.id).await? {
			if stack.status == stacks::StackStatus::Deleting {
				continue;
			}
			let r = of_stack(s, team, &stack).await?;
			keep(format!("stack {}", stack.name), r);
		}
		for app in applications::list_by_environment(&s.db, env.id).await? {
			let r = of_application(s, team, &app).await?;
			keep(format!("application {}", app.name), r);
		}
	}
	Ok((views, skipped))
}

/// Exports the units of a project, an environment or the team as a `.tar.gz`: one folder per
/// server with a README and its units, plus the env files when `secrets` is asked for. Needs
/// `read` on projects, applications, databases, models, stacks and env vars; secrets also
/// need a session or `read:sensitive` and `update` on env vars, databases, models and stacks.
/// Audited as `quadlet.export`.
///
/// # Errors
/// `Forbidden`/`MissingScope` as above, `ProjectNotFound`/`EnvironmentNotFound` outside the
/// team, and database failures.
pub async fn export(s: &AppState, user: UserId, team: TeamId, req: Request) -> AppResult<Archive> {
	authorize_export(s, user, team, &req).await?;

	let (projects, environment): (Vec<Project>, Option<EnvironmentId>) = match req.scope {
		Scope::Project {
			project,
			environment,
		} => {
			let project = scope::project(&s.db, team, project).await?;
			if let Some(env) = environment {
				scope::environment(&s.db, team, project.id, env).await?;
			}
			(vec![project], environment)
		}
		Scope::Team => (
			projects::list_by_team(&s.db, team)
				.await?
				.into_iter()
				.map(|p| p.project)
				.collect(),
			None,
		),
	};

	let mut views = Vec::new();
	let mut skipped = Vec::new();
	let mut env_slug = None;
	for project in &projects {
		let envs: Vec<environments::Environment> = environments::list_by_project(&s.db, project.id)
			.await?
			.into_iter()
			.filter(|e| environment.is_none_or(|id| e.id == id))
			.collect();
		if environment.is_some() {
			env_slug = envs.first().map(|e| e.slug.clone());
		}
		let (v, sk) = collect(s, team, &envs).await?;
		let prefix = |line: String| {
			if projects.len() > 1 {
				format!("{}: {line}", project.name)
			} else {
				line
			}
		};
		views.extend(v);
		skipped.extend(sk.into_iter().map(prefix));
	}

	let (title, stem) = match (req.scope, projects.first()) {
		(Scope::Project { .. }, Some(project)) => match &env_slug {
			Some(env) => (
				format!("project {}, environment {env}", project.name),
				format!("podkit-quadlet-{}-{env}", project.slug),
			),
			None => (
				format!("project {}", project.name),
				format!("podkit-quadlet-{}", project.slug),
			),
		},
		_ => (
			format!("team {team}"),
			format!("podkit-quadlet-team-{team}"),
		),
	};
	let bytes = archive(&stem, &title, &views, &skipped, req.secrets)
		.map_err(|e| ServerError::Validation(format!("failed to write the archive: {e}")))?;

	let target = match (req.scope, projects.first()) {
		(Scope::Project { .. }, Some(project)) => Target::new("project", project.id)
			.named(&project.name)
			.in_project(project.id),
		_ => Target::new("team", team),
	};
	audit::log(
		&s.db,
		user,
		team,
		"quadlet.export",
		target,
		Diff::new()
			.set("environment_id", environment.map(|e| e.0))
			.set("resources", views.len())
			.set("secrets", req.secrets),
	)
	.await;

	Ok(Archive {
		filename: format!("{stem}.tar.gz"),
		bytes,
	})
}

/// A server name as a folder name.
fn folder(name: &str) -> String {
	let out: String = name
		.chars()
		.map(|c| {
			if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
				c
			} else {
				'-'
			}
		})
		.collect();
	match out.trim_matches('.') {
		"" => "server".to_owned(),
		s => s.to_owned(),
	}
}

/// The `.tar.gz`: `<stem>/<server>/README.md` and `<stem>/<server>/units/...`.
fn archive(
	stem: &str,
	title: &str,
	views: &[View],
	skipped: &[String],
	secrets: bool,
) -> std::io::Result<Vec<u8>> {
	let mut by_server: BTreeMap<&str, Vec<&View>> = BTreeMap::new();
	for view in views {
		by_server
			.entry(view.server.as_str())
			.or_default()
			.push(view);
	}
	let now = time::OffsetDateTime::now_utc().unix_timestamp();
	let mtime = u64::try_from(now).unwrap_or(0);
	let mut builder = tar::Builder::new(Vec::new());
	let mut add = |path: &str, content: &str, mode: u32| -> std::io::Result<()> {
		let mut header = tar::Header::new_gnu();
		header.set_size(content.len() as u64);
		header.set_mode(mode);
		header.set_mtime(mtime);
		header.set_cksum();
		builder.append_data(&mut header, path, content.as_bytes())
	};
	if by_server.is_empty() {
		let text = quadlet::readme(title, &[], secrets, skipped);
		add(&format!("{stem}/README.md"), &text, 0o644)?;
	}
	for (server, views) in &by_server {
		let dir = format!("{stem}/{}", folder(server));
		let mut written: Vec<String> = Vec::new();
		let mut entries = Vec::new();
		for view in views {
			for p in &view.parts {
				entries.push(ReadmeEntry {
					label: &p.label,
					units: &p.units,
				});
			}
			for file in view.files() {
				if written.contains(&file.name) {
					continue;
				}
				add(&format!("{dir}/units/{}", file.name), &file.content, 0o644)?;
				written.push(file.name.clone());
			}
			if secrets {
				for file in view.parts.iter().flat_map(|p| p.units.secret()) {
					if written.contains(&file.name) {
						continue;
					}
					add(&format!("{dir}/units/{}", file.name), &file.content, 0o600)?;
					written.push(file.name.clone());
				}
			}
		}
		let title = format!("{title}, server {server}");
		// Every folder lists what was skipped: it may have been meant for any server.
		let text = quadlet::readme(&title, &entries, secrets, skipped);
		add(&format!("{dir}/README.md"), &text, 0o644)?;
	}
	let tar = builder.into_inner()?;
	let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
	gz.write_all(&tar)?;
	gz.finish()
}
