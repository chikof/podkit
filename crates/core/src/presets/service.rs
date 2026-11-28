//! Applying a preset: the person's answers are written into the file ([`super::bind`]) and
//! the result goes through the ordinary plan and apply of a project file ([`crate::iac`]).

use serde::{Deserialize, Serialize};

use super::{Bindings, Needs};
use crate::error::{AppResult, ServerError};
use crate::iac::doc::{self, Doc};
use crate::iac::plan::{Issue, Plan};
use crate::iac::service::{self as iac, ImportOutcome, Input, Outcome};
use crate::iac::snapshot::Lookup;
use crate::ids::{ProjectId, TeamId, UserId};
use crate::scope;
use crate::state::AppState;

fn bad(e: impl std::fmt::Display) -> ServerError {
	ServerError::Validation(e.to_string())
}

/// Where a preset goes.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Target {
	/// A project made for it.
	New {
		/// Its slug; the preset's own when unset.
		#[serde(default)]
		slug: Option<String>,
		/// Its name; the preset's own when unset.
		#[serde(default)]
		name: Option<String>,
	},
	/// A project that exists: the preset's objects are added to it, and nothing it has is
	/// removed.
	Project(ProjectId),
}

/// A preset with the answers to what it asks.
#[derive(Debug, Clone, Deserialize)]
pub struct Request {
	/// The preset: a project file with one project.
	pub preset: String,
	/// The person's choices and secrets.
	#[serde(default)]
	pub answers: Bindings,
	/// Where it goes.
	pub target: Target,
	/// Apply only: deploy what was made.
	#[serde(default)]
	pub deploy: bool,
}

/// What a preset asks, with what the team has to answer it.
#[derive(Debug, Serialize)]
pub struct Asked {
	/// The slug and name the preset gives its project.
	pub slug: String,
	pub name: String,
	/// What it asks. Kept under its own key: it has a `servers` list and so does `available`.
	pub needs: Needs,
	/// What the team has to answer with.
	pub available: Available,
}

/// The team's servers, SSH keys and registries, by name.
#[derive(Debug, Serialize)]
pub struct Available {
	pub servers: Vec<String>,
	pub ssh_keys: Vec<String>,
	pub registries: Vec<String>,
}

/// What `preset` asks of whoever applies it in `team`.
///
/// # Errors
/// `Validation` for a file that is not a preset; `Forbidden` without read access to the
/// team's servers, keys and registries.
pub async fn asked(state: &AppState, user: UserId, team: TeamId, preset: &str) -> AppResult<Asked> {
	let doc = doc::parse(preset).map_err(bad)?;
	let needs = super::needs(&doc).map_err(bad)?;
	let (slug, project) = doc
		.projects
		.iter()
		.next()
		.ok_or_else(|| bad("no project"))?;
	let lookup = Lookup::load(state, user, team).await?;
	Ok(Asked {
		slug: slug.clone(),
		name: project.name.clone(),
		needs,
		available: Available {
			servers: lookup.servers.into_keys().collect(),
			ssh_keys: lookup.ssh_keys.into_keys().collect(),
			registries: lookup.registries.into_keys().collect(),
		},
	})
}

/// A preset ready to plan: the project file for its target, and what to check first.
struct Bound {
	input: Input,
	/// The project it is added to; none for one made for it.
	into: Option<ProjectId>,
	/// Each application it holds: where in the file, its server and its slug.
	apps: Vec<(String, String, String)>,
}

/// The preset as a project file for its target, with its secrets.
async fn bound(state: &AppState, team: TeamId, request: &Request) -> AppResult<Bound> {
	let parsed = doc::parse(&request.preset).map_err(bad)?;
	let own = parsed.projects.keys().next().cloned().unwrap_or_default();
	// The project it goes to is known first: its applications are named after it.
	let (slug, name, into) = match &request.target {
		Target::New { slug, name } => (slug.clone().unwrap_or(own), name.clone(), None),
		Target::Project(id) => {
			let existing = scope::project(&state.db, team, *id).await?;
			// The file would rename the project it is added to.
			(existing.slug, Some(existing.name), Some(*id))
		}
	};
	let (bound, secrets) = super::bind(parsed, &request.answers, &slug, || {
		crypto::generate_token(16)
	})
	.map_err(|missing| bad(missing.join("; ")))?;
	let mut project = bound
		.projects
		.into_values()
		.next()
		.ok_or_else(|| bad("no project"))?;
	if let Some(name) = name.as_deref().map(str::trim).filter(|n| !n.is_empty()) {
		name.clone_into(&mut project.name);
	}
	let apps = project
		.environments
		.iter()
		.flat_map(|(env, e)| {
			e.apps.iter().map(move |(name, app)| {
				(
					format!("environments.{env}.apps.{name}.slug"),
					app.server.clone(),
					app.slug.clone(),
				)
			})
		})
		.collect();
	let mut doc = Doc::new();
	doc.projects.insert(slug, project);
	let input = Input {
		text: doc::render(&doc).map_err(bad)?,
		secrets,
		// Adding to a project removes nothing of it. A new one is made whole by the import.
		prune: false,
		deploy: request.deploy,
	};
	Ok(Bound { input, into, apps })
}

/// An application's slug is unique on its server across the install, and the plan of a
/// project file does not look: the apply would stop there, after it had made the project
/// and whatever came before. Said here instead, as issues, before anything is made.
///
/// An application of the project the preset is added to is not in the way: by the same
/// name it is the one being updated.
async fn taken_slugs(
	state: &AppState,
	user: UserId,
	team: TeamId,
	bound: &Bound,
) -> AppResult<Vec<Issue>> {
	let lookup = Lookup::load(state, user, team).await?;
	let mut issues = Vec::new();
	for (path, server, slug) in &bound.apps {
		// A server the team does not have is the plan's to report.
		let Some(server_id) = lookup.servers.get(server) else {
			continue;
		};
		let taken: Option<(String,)> = sqlx::query_as(
			"SELECT p.name FROM applications a JOIN projects p ON p.id = a.project_id \
			 WHERE a.server_id = $1 AND a.slug = $2 AND ($3::BIGINT IS NULL OR a.project_id <> $3)",
		)
		.bind(*server_id)
		.bind(slug)
		.bind(bound.into)
		.fetch_optional(&state.db)
		.await?;
		if taken.is_some() {
			issues.push(Issue::new(
				path.clone(),
				format!(
					"an application \"{slug}\" already runs on the server \"{server}\"; a slug is unique on a server, so give this one another"
				),
			));
		}
	}
	Ok(issues)
}

/// What applying the preset would do.
///
/// # Errors
/// `Validation` naming every unanswered need; otherwise as the plan of a project file.
pub async fn plan(
	state: &AppState,
	user: UserId,
	team: TeamId,
	request: &Request,
) -> AppResult<Plan> {
	let bound = bound(state, team, request).await?;
	let mut plan = match bound.into {
		Some(project) => iac::plan(state, user, team, project, &bound.input).await?,
		None => one(iac::import_plan(state, user, team, &bound.input).await?)?.plan,
	};
	plan.issues
		.extend(taken_slugs(state, user, team, &bound).await?);
	Ok(plan)
}

fn one<T>(mut list: Vec<T>) -> AppResult<T> {
	match (list.pop(), list.is_empty()) {
		(Some(only), true) => Ok(only),
		_ => Err(bad("a preset holds exactly one project")),
	}
}

/// What applying a preset came to.
#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Applied {
	/// The plan had issues; nothing changed.
	Refused(Box<Plan>),
	/// A change failed part-way. What ran before it remains, in the project named: creating
	/// a database already asks its server to provision it, so nothing is taken back here.
	Failed {
		project_id: ProjectId,
		applied: Box<iac::Applied>,
	},
	/// It ran: the project it went to, and the plan and report of the apply.
	Done {
		project_id: ProjectId,
		applied: Box<iac::Applied>,
	},
}

/// Applies the preset to its target.
///
/// # Errors
/// As [`plan`], and `Forbidden` when a change needs a permission the caller lacks.
pub async fn apply(
	state: &AppState,
	user: UserId,
	team: TeamId,
	request: &Request,
) -> AppResult<Applied> {
	// The plan is made here first for the issues only presets look for; the apply then makes
	// its own.
	let checked = plan(state, user, team, request).await?;
	if checked.has_issues() {
		return Ok(Applied::Refused(Box::new(checked)));
	}
	let Bound { input, into, .. } = bound(state, team, request).await?;
	Ok(match into {
		Some(project) => match iac::apply(state, user, team, project, &input).await? {
			Outcome::Refused(plan) => Applied::Refused(plan),
			Outcome::Done(applied) if !applied.report.ok() => Applied::Failed {
				project_id: project,
				applied,
			},
			Outcome::Done(applied) => Applied::Done {
				project_id: project,
				applied,
			},
		},
		None => match iac::import_apply(state, user, team, &input).await? {
			ImportOutcome::Refused(plans) => Applied::Refused(Box::new(one(plans)?.plan)),
			ImportOutcome::Done(done) => {
				let made = one(done)?;
				let (project_id, applied) = (made.project_id, Box::new(made.applied));
				if applied.report.ok() {
					Applied::Done {
						project_id,
						applied,
					}
				} else {
					Applied::Failed {
						project_id,
						applied,
					}
				}
			}
		},
	})
}

// ------------------------------------------------------------ kept presets

use super::store::{self, Origin, Preset};
use crate::auth::permission::{Action, Resource, authorize};
use crate::ids::PresetId;

/// A preset to keep.
#[derive(Debug, Clone, Deserialize)]
pub struct NewPreset {
	pub name: String,
	/// The preset's own project slug when unset.
	#[serde(default)]
	pub slug: Option<String>,
	#[serde(default)]
	pub description: String,
}

/// The largest file kept: what a compose file may be, four times over.
pub(crate) const MAX_FILE: usize = 1024 * 1024;

fn checked(new: &NewPreset, file: &str) -> AppResult<(String, String, String)> {
	let name = new.name.trim();
	if name.is_empty() || name.chars().count() > 120 {
		return Err(bad("a preset needs a name of at most 120 characters"));
	}
	let description = new.description.trim();
	if description.chars().count() > 500 {
		return Err(bad("the description is at most 500 characters"));
	}
	if file.len() > MAX_FILE {
		return Err(bad("the preset is larger than 1 MiB"));
	}
	let parsed = doc::parse(file).map_err(bad)?;
	super::needs(&parsed).map_err(bad)?;
	let own = parsed.projects.keys().next().cloned().unwrap_or_default();
	let slug = new.slug.clone().unwrap_or(own);
	if !crate::validate::slug(&slug) {
		return Err(bad(
			"a preset is named by a slug: lowercase letters, digits and hyphens",
		));
	}
	Ok((name.to_owned(), slug, description.to_owned()))
}

async fn keep(
	state: &AppState,
	user: UserId,
	team: TeamId,
	new: &NewPreset,
	file: String,
	origin: Origin,
) -> AppResult<Preset> {
	authorize(&state.db, user, team, Action::Create, Resource::Project).await?;
	let (name, slug, description) = checked(new, &file)?;
	let preset = Preset {
		id: PresetId::generate(),
		team_id: team,
		slug,
		name,
		description,
		origin,
		file,
		created_by: Some(user),
		created_at: time::OffsetDateTime::now_utc(),
	};
	match store::insert(&state.db, &preset).await {
		Ok(()) => Ok(preset),
		Err(sqlx::Error::Database(e)) if e.is_unique_violation() => Err(ServerError::Conflict(
			format!("This team already has a preset \"{}\".", preset.slug),
		)),
		Err(e) => Err(e.into()),
	}
}

/// The team's kept presets, without their files; needs `read project`.
///
/// # Errors
/// `Forbidden`.
pub async fn list(state: &AppState, user: UserId, team: TeamId) -> AppResult<Vec<Preset>> {
	authorize(&state.db, user, team, Action::Read, Resource::Project).await?;
	Ok(store::list(&state.db, team).await?)
}

/// The team's kept presets with their files; needs `read project`.
///
/// # Errors
/// `Forbidden`; database failures.
pub async fn list_whole(state: &AppState, user: UserId, team: TeamId) -> AppResult<Vec<Preset>> {
	authorize(&state.db, user, team, Action::Read, Resource::Project).await?;
	Ok(store::list_whole(&state.db, team).await?)
}

/// One kept preset with its file; needs `read project`.
///
/// # Errors
/// `Forbidden`; `NotFound` for another team's.
pub async fn get(state: &AppState, user: UserId, team: TeamId, id: PresetId) -> AppResult<Preset> {
	authorize(&state.db, user, team, Action::Read, Resource::Project).await?;
	store::get(&state.db, team, id)
		.await?
		.ok_or(ServerError::NotFound)
}

/// Keeps a preset handed in as a file. It is marked imported: nobody here wrote it.
///
/// # Errors
/// `Forbidden` without `create project`; `Validation` for a file that is not a preset;
/// `Conflict` for a slug the team has.
pub async fn import(
	state: &AppState,
	user: UserId,
	team: TeamId,
	new: &NewPreset,
	file: String,
) -> AppResult<Preset> {
	keep(state, user, team, new, file, Origin::Imported).await
}

/// What saving `project` as a preset would keep: its file with the team's server, key and
/// registry names taken out. Secrets are placeholders in any exported file. For the person to
/// read before they save.
///
/// # Errors
/// As the export of a project.
pub async fn from_project(
	state: &AppState,
	user: UserId,
	team: TeamId,
	project: ProjectId,
) -> AppResult<String> {
	let exported = iac::export(state, user, team, project).await?;
	let stripped = super::strip(doc::parse(&exported).map_err(bad)?);
	doc::render(&stripped).map_err(bad)
}

/// Saves `project` as a preset of the team.
///
/// # Errors
/// As [`from_project`] and [`import`].
pub async fn save_project(
	state: &AppState,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	new: &NewPreset,
) -> AppResult<Preset> {
	let file = from_project(state, user, team, project).await?;
	keep(state, user, team, new, file, Origin::Saved).await
}

/// Removes a kept preset; needs `delete project`. Projects made from it are not touched.
///
/// # Errors
/// `Forbidden`; `NotFound`.
pub async fn remove(state: &AppState, user: UserId, team: TeamId, id: PresetId) -> AppResult<()> {
	authorize(&state.db, user, team, Action::Delete, Resource::Project).await?;
	if store::delete(&state.db, team, id).await? {
		Ok(())
	} else {
		Err(ServerError::NotFound)
	}
}

// ---------------------------------------------------------- the catalog's presets

use crate::templates::CatalogPreset;

/// The catalog's presets, without their files. They are the same for every team; a team
/// applies one as it applies its own.
#[must_use]
pub fn catalog() -> Vec<CatalogPreset> {
	let registry = crate::templates::load_all();
	registry
		.presets
		.iter()
		.map(|p| CatalogPreset {
			file: String::new(),
			..p.clone()
		})
		.collect()
}

/// The presets the template catalog brought, each with its file.
#[must_use]
pub fn catalog_whole() -> Vec<CatalogPreset> {
	crate::templates::load_all().presets.clone()
}

/// One preset of the catalog with its file.
///
/// # Errors
/// `NotFound` for an id the catalog does not have.
pub fn catalog_preset(id: &str) -> AppResult<CatalogPreset> {
	let registry = crate::templates::load_all();
	registry
		.presets
		.iter()
		.find(|p| p.id == id)
		.cloned()
		.ok_or(ServerError::NotFound)
}
