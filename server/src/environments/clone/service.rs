//! Environment clone use cases shared by the JSON API and the web UI: authorize, then
//! scope, then validate, then write.
//!
//! A clone asks for more than one permission: it makes an environment (`create project`)
//! and a resource of each kind it copies (`create application`, `create database`, ...),
//! and with the options on, `create backup` for the data and the deploy permissions.

use sqlx::PgPool;
use sqlx::types::Json;

use super::plan::{self, Plan, Snapshot};
use super::{
	CloneStatus, EnvironmentClone, Options, Step, copy, find_by_id, find_scoped, run, save_report,
};
use crate::auth::permission::{Action, Resource, authorize};
use crate::environments::Environment;
use crate::error::{AppResult, ServerError};
use crate::ids::{CloneId, EnvironmentId, ProjectId, TeamId, UserId};
use crate::jobs::{self, JobKind};
use crate::scope;
use crate::state::AppState;

/// What a clone is asked for.
#[derive(Debug, Clone)]
pub struct Input {
	pub name: String,
	pub slug: String,
	pub options: Options,
}

/// The environment's slug and name checked as for any new environment.
fn validate(input: &Input) -> AppResult<String> {
	crate::environments::service::validate_fields(&input.name, &input.slug)
}

/// Whether a database error is a unique violation of any constraint.
fn is_unique(e: &ServerError) -> bool {
	matches!(e, ServerError::Sqlx(e)
		if e.as_database_error().is_some_and(|d| d.code().as_deref() == Some("23505")))
}

async fn taken(db: &PgPool, sql: &'static str, a: i64, b: &str) -> Result<bool, sqlx::Error> {
	sqlx::query_scalar(sql).bind(a).bind(b).fetch_one(db).await
}

async fn taken2(
	db: &PgPool,
	sql: &'static str,
	a: i64,
	b: &str,
	c: &str,
) -> Result<bool, sqlx::Error> {
	sqlx::query_scalar(sql)
		.bind(a)
		.bind(b)
		.bind(c)
		.fetch_one(db)
		.await
}

/// The names and slugs of the plan that are taken already.
async fn conflicts(
	db: &PgPool,
	project: ProjectId,
	snap: &Snapshot,
	plan: &Plan,
) -> Result<Vec<String>, sqlx::Error> {
	let mut out = Vec::new();
	if taken(
		db,
		"SELECT EXISTS (SELECT 1 FROM environments WHERE project_id = $1 AND slug = $2)",
		project.0,
		&plan.environment_slug,
	)
	.await?
	{
		out.push(format!(
			"An environment with the slug \"{}\" already exists in this project.",
			plan.environment_slug
		));
	}
	// The plan lists applications, databases, stacks and models in the snapshot's order.
	let mut items = plan.items.iter();
	for a in &snap.apps {
		let Some(item) = items.next() else { break };
		let slug = item.new_slug.as_deref().unwrap_or_default();
		if taken(
			db,
			"SELECT EXISTS (SELECT 1 FROM applications WHERE server_id = $1 AND slug = $2)",
			a.app.server_id.0,
			slug,
		)
		.await?
		{
			out.push(format!(
				"The application slug \"{slug}\" is taken on that server."
			));
		}
	}
	for _ in &snap.databases {
		let Some(item) = items.next() else { break };
		if taken(
			db,
			"SELECT EXISTS (SELECT 1 FROM databases WHERE project_id = $1 AND name = $2)",
			project.0,
			&item.new_name,
		)
		.await?
		{
			out.push(format!(
				"A database named \"{}\" already exists.",
				item.new_name
			));
		}
	}
	for _ in &snap.stacks {
		let Some(item) = items.next() else { break };
		if taken2(
			db,
			"SELECT EXISTS (SELECT 1 FROM stacks WHERE project_id = $1 AND (name = $2 OR slug = $3))",
			project.0,
			&item.new_name,
			item.new_slug.as_deref().unwrap_or_default(),
		)
		.await?
		{
			out.push(format!(
				"A stack named \"{}\" already exists.",
				item.new_name
			));
		}
	}
	for _ in &snap.models {
		let Some(item) = items.next() else { break };
		if taken2(
			db,
			"SELECT EXISTS (SELECT 1 FROM models WHERE (project_id = $1 AND name = $2) OR slug = $3)",
			project.0,
			&item.new_name,
			item.new_slug.as_deref().unwrap_or_default(),
		)
		.await?
		{
			out.push(format!(
				"A model named \"{}\" already exists.",
				item.new_name
			));
		}
	}
	Ok(out)
}

/// What cloning `source` as `input` would make, without making it; needs `read project`.
/// The plan's `conflicts` lists what is taken already.
///
/// # Errors
/// The source is not in the team's project (`EnvironmentNotFound`), or the name or slug
/// is not valid.
pub async fn preview(
	s: &AppState,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	source: EnvironmentId,
	input: &Input,
) -> AppResult<Plan> {
	authorize(&s.db, user, team, Action::Read, Resource::Project).await?;
	let source = scope::environment(&s.db, team, project, source).await?;
	let name = validate(input)?;
	let snap = plan::load(s, team, source).await?;
	let mut plan = Plan::build(&snap, &name, &input.slug, &input.options);
	plan.conflicts = conflicts(&s.db, project, &snap, &plan).await?;
	Ok(plan)
}

/// The permissions a clone of `snap` with `options` needs besides `create project`.
async fn authorize_resources(
	db: &PgPool,
	user: UserId,
	team: TeamId,
	snap: &Snapshot,
	options: &Options,
) -> AppResult<()> {
	let mut needs = Vec::new();
	if !snap.apps.is_empty() {
		needs.push((Action::Create, Resource::Application));
		if options.deploy {
			needs.push((Action::Create, Resource::Deployment));
		}
	}
	if !snap.databases.is_empty() {
		needs.push((Action::Create, Resource::Database));
	}
	if !snap.stacks.is_empty() {
		needs.push((Action::Create, Resource::Stack));
		if options.deploy {
			needs.push((Action::Update, Resource::Stack));
		}
	}
	if !snap.models.is_empty() {
		needs.push((Action::Create, Resource::Model));
		if options.deploy {
			needs.push((Action::Update, Resource::Model));
		}
	}
	if options.data && snap.has_data() {
		needs.push((Action::Create, Resource::Backup));
	}
	for (action, resource) in needs {
		authorize(db, user, team, action, resource).await?;
	}
	Ok(())
}

/// Clones `source` into a new environment of the same project.
///
/// The configuration is copied in one transaction: either the whole environment is made
/// or nothing is. What follows (provisioning, data, deploying) is a job; the returned
/// clone says which, and [`get`] reports its progress.
///
/// # Errors
/// `Forbidden` without the permissions above; `EnvironmentNotFound` for a source of
/// another team or project; `Conflict` naming what is taken already; `Validation` for a
/// bad name or a data copy without a backup destination.
pub async fn clone(
	s: &AppState,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	source: EnvironmentId,
	input: Input,
) -> AppResult<EnvironmentClone> {
	authorize(&s.db, user, team, Action::Create, Resource::Project).await?;
	let source: Environment = scope::environment(&s.db, team, project, source).await?;
	let name = validate(&input)?;
	let snap = plan::load(s, team, source).await?;
	authorize_resources(&s.db, user, team, &snap, &input.options).await?;

	let mut options = input.options.clone();
	if options.data && snap.has_data() {
		let Some(target) = options.backup_target_id else {
			return Err(ServerError::Validation(
				"Choose a backup destination to copy data through.".to_owned(),
			));
		};
		scope::backup_target(&s.db, team, target).await?;
	} else {
		// Nothing to copy through a destination: do not keep one that is not used.
		options.backup_target_id = None;
		options.data = options.data && snap.has_data();
	}

	let mut plan = Plan::build(&snap, &name, &input.slug, &options);
	plan.conflicts = conflicts(&s.db, project, &snap, &plan).await?;
	if !plan.conflicts.is_empty() {
		return Err(ServerError::Conflict(plan.conflicts.join(" ")));
	}

	let id = CloneId::generate();
	let mut tx = s.db.begin().await?;
	let (environment, pairs) = copy::apply(
		s,
		&mut tx,
		user,
		team,
		project,
		&snap,
		&name,
		&input.slug,
		&options,
	)
	.await
	.map_err(|e| {
		if is_unique(&e) {
			ServerError::Conflict(
				"A name or slug the clone needs was taken while it was being made. Try again, or pick another slug."
					.to_owned(),
			)
		} else {
			e
		}
	})?;
	sqlx::query(
		"INSERT INTO environment_clones
			(id, team_id, project_id, source_environment_id, source_name, environment_id, name, slug,
			 options, pairs, created_by)
		VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)",
	)
	.bind(id)
	.bind(team)
	.bind(project)
	.bind(snap.environment.id)
	.bind(&snap.environment.name)
	.bind(environment.id)
	.bind(&name)
	.bind(&input.slug)
	.bind(Json(&options))
	.bind(Json(&pairs))
	.bind(user)
	.execute(&mut *tx)
	.await?;
	tx.commit().await?;

	finish_setup(s, id, &snap).await?;
	find_by_id(&s.db, id).await?.ok_or(ServerError::Internal)
}

/// After the configuration is committed: the stacks' service rows, the list of steps and
/// the job that runs them. A clone with nothing to run is done.
async fn finish_setup(s: &AppState, id: CloneId, snap: &Snapshot) -> AppResult<()> {
	let Some(clone) = find_by_id(&s.db, id).await? else {
		return Err(ServerError::Internal);
	};
	let result = async {
		let work = run::work_list(s, &clone).await?;
		run::sync_stack_services(s, &clone).await?;
		let steps: Vec<Step> = work.iter().map(run::Work::step).collect();
		let Some(server) = snap.first_server().filter(|_| !steps.is_empty()) else {
			save_report(&s.db, id, CloneStatus::Succeeded, &steps).await?;
			return Ok(());
		};
		save_report(&s.db, id, CloneStatus::Queued, &steps).await?;
		let job = jobs::enqueue(
			s,
			JobKind::EnvironmentClone,
			server,
			id.0,
			format!("env-clone:{id}"),
		)
		.await?;
		sqlx::query("UPDATE environment_clones SET job_id = $2 WHERE id = $1")
			.bind(id)
			.bind(job)
			.execute(&s.db)
			.await?;
		Ok::<(), ServerError>(())
	}
	.await;
	if let Err(e) = &result {
		// The environment exists and is consistent; the report says it was not carried on.
		tracing::error!(clone = id.0, "cannot queue the environment clone: {e}");
		let step = Step {
			label: "Queue the clone".to_owned(),
			status: super::StepStatus::Failed,
			detail: Some("could not be queued; the configuration was copied".to_owned()),
		};
		save_report(&s.db, id, CloneStatus::Failed, &[step]).await?;
	}
	result
}

/// One clone with its report; needs `read project`. A clone of another team or project is
/// `EnvironmentNotFound`.
///
/// # Errors
/// As above.
pub async fn get(
	db: &PgPool,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	id: CloneId,
) -> AppResult<EnvironmentClone> {
	authorize(db, user, team, Action::Read, Resource::Project).await?;
	scope::project(db, team, project).await?;
	find_scoped(db, team, project, id)
		.await?
		.ok_or(ServerError::EnvironmentNotFound)
}
