//! Infrastructure as code use cases, shared by the JSON API and the web UI: export a
//! project to a file, plan a file against a project, apply it.
//!
//! Every read and every change is authorized like the same action anywhere else in podkit: the
//! apply calls the same services the pages call. Before the first change runs, the permission
//! of every change in the plan is checked, so a caller who may not do all of it changes
//! nothing.

use std::collections::BTreeSet;

use super::apply::{self, Cx, Options, Report};
use super::doc::{self, Doc, EnvironmentDoc, ProjectDoc};
use super::objects::{Kind, Secrets, flatten};
use super::plan::{Action, Plan, diff, validate_doc};
use super::snapshot::{self, Lookup};
use crate::auth::permission::{Action as Do, Resource, authorize};
use crate::error::{AppResult, ServerError};
use crate::ids::{ProjectId, TeamId, UserId};
use crate::projects::Project;
use crate::scope;
use crate::state::AppState;

/// A file and what goes with it.
#[derive(Debug, Clone, Default)]
pub struct Input {
	pub text: String,
	pub secrets: Secrets,
	pub prune: bool,
	/// Apply only: deploy what was created or changed.
	pub deploy: bool,
}

/// An apply that ran: the plan it followed and what happened.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Applied {
	pub plan: Plan,
	pub report: Report,
}

fn bad(e: impl std::fmt::Display) -> ServerError {
	ServerError::Validation(e.to_string())
}

/// Authorizes reading everything a file describes.
async fn authorize_reads(state: &AppState, user: UserId, team: TeamId) -> AppResult<()> {
	for resource in [
		Resource::Project,
		Resource::Application,
		Resource::Database,
		Resource::Stack,
		Resource::Model,
		Resource::EnvVar,
		Resource::Domain,
		Resource::Volume,
		Resource::Server,
	] {
		authorize(&state.db, user, team, Do::Read, resource).await?;
	}
	Ok(())
}

/// The one project of a project-level file.
fn single(doc: Doc) -> AppResult<ProjectDoc> {
	let mut projects = doc.projects.into_values();
	match (projects.next(), projects.next()) {
		(Some(project), None) => Ok(project),
		(None, _) => Err(bad("the file describes no project")),
		_ => Err(bad(
			"this file describes several projects; import it at the team level instead",
		)),
	}
}

/// Writes `project` as a `podkit.toml`.
///
/// # Errors
/// `Forbidden` without read access to what the file holds; `ProjectNotFound` for another
/// team's project.
pub async fn export(
	state: &AppState,
	user: UserId,
	team: TeamId,
	project: ProjectId,
) -> AppResult<String> {
	authorize_reads(state, user, team).await?;
	let project = scope::project(&state.db, team, project).await?;
	let lookup = Lookup::load(state, user, team).await?;
	let snapshot = snapshot::read(state, &project, &lookup).await?;
	let mut doc = Doc::new();
	doc.projects.insert(project.slug.clone(), snapshot.doc);
	doc::render(&doc).map_err(bad)
}

struct Prepared {
	project: Project,
	desired: ProjectDoc,
	lookup: Lookup,
	snapshot: snapshot::Snapshot,
	plan: Plan,
}

async fn prepare(
	state: &AppState,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	input: &Input,
) -> AppResult<Prepared> {
	authorize_reads(state, user, team).await?;
	let project = scope::project(&state.db, team, project).await?;
	let desired = single(doc::parse(&input.text).map_err(bad)?)?;
	let lookup = Lookup::load(state, user, team).await?;
	let snapshot = snapshot::read(state, &project, &lookup).await?;
	let plan = compare(
		&snapshot,
		&desired,
		&lookup,
		input,
		Some(project.slug.clone()),
	);
	Ok(Prepared {
		project,
		desired,
		lookup,
		snapshot,
		plan,
	})
}

fn compare(
	current: &snapshot::Snapshot,
	desired: &ProjectDoc,
	lookup: &Lookup,
	input: &Input,
	slug: Option<String>,
) -> Plan {
	compare_docs(&current.doc, &current.secrets, desired, lookup, input, slug)
}

fn compare_docs(
	current: &ProjectDoc,
	current_secrets: &Secrets,
	desired: &ProjectDoc,
	lookup: &Lookup,
	input: &Input,
	slug: Option<String>,
) -> Plan {
	let issues = validate_doc(desired, &lookup.refs());
	let now = flatten(current, current_secrets);
	let want = flatten(desired, &input.secrets);
	diff(&now, &want, input.prune).into_plan(slug, input.prune, issues)
}

/// What applying `input.text` to the project would change. Nothing changes.
///
/// # Errors
/// `Validation` for a file that cannot be read; `Forbidden`; `ProjectNotFound`.
pub async fn plan(
	state: &AppState,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	input: &Input,
) -> AppResult<Plan> {
	Ok(prepare(state, user, team, project, input).await?.plan)
}

/// The permission each change of a plan needs, once each.
fn needed(plan: &Plan, deploy: bool) -> Vec<(Do, Resource)> {
	let mut list: Vec<(Do, Resource)> = Vec::new();
	let mut add = |action: Do, resource: Resource| {
		if !list
			.iter()
			.any(|(a, r)| a.as_str() == action.as_str() && r.as_str() == resource.as_str())
		{
			list.push((action, resource));
		}
	};
	for change in &plan.changes {
		let action = match change.action {
			Action::Create => Do::Create,
			Action::Update => Do::Update,
			Action::Delete => Do::Delete,
		};
		match change.kind {
			Kind::Environment => add(action, Resource::Project),
			Kind::Network => add(Do::Update, Resource::Server),
			Kind::Database => add(action, Resource::Database),
			Kind::Model => add(action, Resource::Model),
			Kind::Stack => add(action, Resource::Stack),
			Kind::App => {
				add(action, Resource::Application);
				for f in &change.fields {
					let collection = f.field.split_once('.').map(|(p, _)| p);
					let (create, resource) = match collection {
						Some("env") => (true, Resource::EnvVar),
						Some("domain") => (true, Resource::Domain),
						Some("volume") => (true, Resource::Volume),
						Some("link") => {
							add(Do::Update, Resource::Application);
							add(Do::Read, Resource::Model);
							continue;
						}
						_ => continue,
					};
					if create {
						add(
							if f.to.is_some() {
								Do::Create
							} else {
								Do::Delete
							},
							resource,
						);
					}
				}
			}
		}
		if deploy && change.will_redeploy {
			match change.kind {
				Kind::App => add(Do::Create, Resource::Deployment),
				Kind::Stack => add(Do::Update, Resource::Stack),
				_ => {}
			}
		}
	}
	list
}

async fn authorize_plan(
	state: &AppState,
	user: UserId,
	team: TeamId,
	plan: &Plan,
	deploy: bool,
) -> AppResult<()> {
	for (action, resource) in needed(plan, deploy) {
		authorize(&state.db, user, team, action, resource).await?;
	}
	Ok(())
}

/// The outcome of an apply that did not run because the plan has issues.
#[derive(Debug)]
pub enum Outcome {
	/// The plan had issues; nothing changed.
	Refused(Box<Plan>),
	Done(Box<Applied>),
}

/// Applies `input.text` to the project.
///
/// # Errors
/// As [`plan`]; `Forbidden` when any change in the plan needs a permission the caller lacks,
/// before any of it runs.
pub async fn apply(
	state: &AppState,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	input: &Input,
) -> AppResult<Outcome> {
	let prepared = prepare(state, user, team, project, input).await?;
	if prepared.plan.has_issues() {
		return Ok(Outcome::Refused(Box::new(prepared.plan)));
	}
	authorize_plan(state, user, team, &prepared.plan, input.deploy).await?;
	let mut cx = Cx {
		state,
		user,
		team,
		project: prepared.project.id,
		doc: &prepared.desired,
		secrets: &input.secrets,
		lookup: &prepared.lookup,
		ids: prepared.snapshot.ids.clone(),
	};
	let report = apply::run(
		&mut cx,
		&prepared.plan,
		Options {
			deploy: input.deploy,
		},
	)
	.await;
	Ok(Outcome::Done(Box::new(Applied {
		plan: prepared.plan,
		report,
	})))
}

// ----- team-level import

/// The plan of one project of an import.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ProjectPlan {
	pub slug: String,
	pub name: String,
	pub plan: Plan,
}

/// What the default environment of a new project looks like.
fn fresh_project(name: &str) -> ProjectDoc {
	let mut environments = std::collections::BTreeMap::new();
	environments.insert(
		"production".to_owned(),
		EnvironmentDoc {
			name: "production".to_owned(),
			apps: std::collections::BTreeMap::new(),
			databases: std::collections::BTreeMap::new(),
			stacks: std::collections::BTreeMap::new(),
			models: std::collections::BTreeMap::new(),
		},
	);
	ProjectDoc {
		name: name.to_owned(),
		environments,
		networks: Vec::new(),
	}
}

async fn existing_slugs(state: &AppState, team: TeamId) -> AppResult<BTreeSet<String>> {
	Ok(crate::projects::list_by_team(&state.db, team)
		.await?
		.into_iter()
		.map(|p| p.project.slug)
		.collect())
}

/// Plans a file that makes new projects: each is compared with a project that has only its
/// default environment.
///
/// # Errors
/// `Forbidden` without `create project` and read access; `Validation` for an unreadable file.
pub async fn import_plan(
	state: &AppState,
	user: UserId,
	team: TeamId,
	input: &Input,
) -> AppResult<Vec<ProjectPlan>> {
	authorize_reads(state, user, team).await?;
	authorize(&state.db, user, team, Do::Create, Resource::Project).await?;
	let doc = doc::parse(&input.text).map_err(bad)?;
	if doc.projects.is_empty() {
		return Err(bad("the file describes no project"));
	}
	let lookup = Lookup::load(state, user, team).await?;
	let taken = existing_slugs(state, team).await?;
	let mut plans = Vec::new();
	for (slug, desired) in &doc.projects {
		let mut forced = input.clone();
		forced.prune = true;
		let mut plan = compare_docs(
			&fresh_project(&desired.name),
			&Secrets::default(),
			desired,
			&lookup,
			&forced,
			Some(slug.clone()),
		);
		if taken.contains(slug) {
			plan.issues.push(super::plan::Issue::new(
				format!("projects.{slug}"),
				"a project with this slug already exists; import makes new projects, so open it and apply the file there",
			));
		}
		if !crate::validate::slug(slug) {
			plan.issues.push(super::plan::Issue::new(
				format!("projects.{slug}"),
				"a project is named by a slug: lowercase letters, digits and hyphens",
			));
		}
		if desired.name.trim().is_empty() {
			plan.issues.push(super::plan::Issue::new(
				format!("projects.{slug}.name"),
				"the name is empty",
			));
		}
		plans.push(ProjectPlan {
			slug: slug.clone(),
			name: desired.name.clone(),
			plan,
		});
	}
	Ok(plans)
}

/// The result of an import: the projects made, each with its apply.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Imported {
	pub slug: String,
	pub project_id: ProjectId,
	pub applied: Applied,
}

#[derive(Debug)]
pub enum ImportOutcome {
	Refused(Vec<ProjectPlan>),
	Done(Vec<Imported>),
}

/// Makes the projects of a file and applies each to its new project, stopping at the first
/// failure.
///
/// # Errors
/// As [`import_plan`]; a failing project is reported in its own report, and the projects
/// after it are not made.
pub async fn import_apply(
	state: &AppState,
	user: UserId,
	team: TeamId,
	input: &Input,
) -> AppResult<ImportOutcome> {
	let plans = import_plan(state, user, team, input).await?;
	if plans.iter().any(|p| p.plan.has_issues()) {
		return Ok(ImportOutcome::Refused(plans));
	}
	for p in &plans {
		let mut forced = Input {
			prune: true,
			..input.clone()
		};
		forced.prune = true;
		authorize_plan(state, user, team, &p.plan, input.deploy).await?;
	}
	let doc = doc::parse(&input.text).map_err(bad)?;
	let mut done = Vec::new();
	for (slug, desired) in doc.projects {
		let made =
			crate::projects::service::create(&state.db, user, team, &desired.name, &slug).await?;
		let one = Doc {
			version: doc::VERSION,
			projects: [(slug.clone(), desired)].into(),
		};
		let text = doc::render(&one).map_err(bad)?;
		let sub = Input {
			text,
			secrets: input.secrets.clone(),
			prune: true,
			deploy: input.deploy,
		};
		match apply(state, user, team, made.id, &sub).await? {
			Outcome::Done(applied) => {
				let failed = applied.report.failed.is_some();
				done.push(Imported {
					slug,
					project_id: made.id,
					applied: *applied,
				});
				if failed {
					break;
				}
			}
			Outcome::Refused(plan) => {
				// The project exists but the file did not fit it: say so rather than leave it
				// looking like a clean import.
				done.push(Imported {
					slug,
					project_id: made.id,
					applied: Applied {
						plan: *plan,
						report: Report::default(),
					},
				});
				break;
			}
		}
	}
	Ok(ImportOutcome::Done(done))
}
