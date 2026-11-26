//! Applying a preset: the person's answers are written into the file ([`super::bind`]) and
//! the result goes through the ordinary plan and apply of a project file ([`crate::iac`]).

use serde::{Deserialize, Serialize};

use super::{Bindings, Needs};
use crate::error::{AppResult, ServerError};
use crate::iac::doc::{self, Doc};
use crate::iac::plan::Plan;
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

/// The preset as a project file for its target, with its secrets.
async fn bound(
	state: &AppState,
	team: TeamId,
	request: &Request,
) -> AppResult<(Input, Option<ProjectId>)> {
	let parsed = doc::parse(&request.preset).map_err(bad)?;
	let (bound, secrets) = super::bind(parsed, &request.answers, || crypto::generate_token(16))
		.map_err(|missing| bad(missing.join("; ")))?;
	let (slug, mut project) = bound
		.projects
		.into_iter()
		.next()
		.ok_or_else(|| bad("no project"))?;
	let (slug, into) = match &request.target {
		Target::New { slug: wanted, name } => {
			if let Some(name) = name.as_deref().map(str::trim).filter(|n| !n.is_empty()) {
				name.clone_into(&mut project.name);
			}
			(wanted.clone().unwrap_or(slug), None)
		}
		Target::Project(id) => {
			// The file would rename the project it is added to.
			let existing = scope::project(&state.db, team, *id).await?;
			project.name = existing.name;
			(existing.slug, Some(*id))
		}
	};
	let mut doc = Doc::new();
	doc.projects.insert(slug, project);
	let input = Input {
		text: doc::render(&doc).map_err(bad)?,
		secrets,
		// Adding to a project removes nothing of it. A new one is made whole by the import.
		prune: false,
		deploy: request.deploy,
	};
	Ok((input, into))
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
	let (input, into) = bound(state, team, request).await?;
	match into {
		Some(project) => iac::plan(state, user, team, project, &input).await,
		None => one(iac::import_plan(state, user, team, &input).await?).map(|p| p.plan),
	}
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
	let (input, into) = bound(state, team, request).await?;
	Ok(match into {
		Some(project) => match iac::apply(state, user, team, project, &input).await? {
			Outcome::Refused(plan) => Applied::Refused(plan),
			Outcome::Done(applied) => Applied::Done {
				project_id: project,
				applied,
			},
		},
		None => match iac::import_apply(state, user, team, &input).await? {
			ImportOutcome::Refused(plans) => Applied::Refused(Box::new(one(plans)?.plan)),
			ImportOutcome::Done(done) => {
				let made = one(done)?;
				Applied::Done {
					project_id: made.project_id,
					applied: Box::new(made.applied),
				}
			}
		},
	})
}
