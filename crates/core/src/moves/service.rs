//! Moves and drains, as the API and the pages ask for them: what moving an application
//! would do on each server ([`plan`]), starting a move ([`start`]), what draining a server
//! would do ([`drain_plan`]), starting a drain ([`start_drain`]) and ending it
//! ([`stop_draining`]).
//!
//! Permissions: looking needs `read application` (a move) or `read server`, and `read server`
//! for the servers on both ends. Moving needs `update application` and `read server`; a move
//! that copies volumes makes backups of them, so it needs `create backup` too. Draining needs
//! `update server`.

use sqlx::PgPool;
use time::OffsetDateTime;

use super::decide::{self, End, Facts, Verdict};
use super::{Drain, Item, ItemKind, Move, NewMove, ONE_ACTIVE_DRAIN, ONE_ACTIVE_MOVE, Outcome};
use crate::applications::{self, Application};
use crate::audit::{self, Diff, Target};
use crate::auth::permission::{Action, Resource, authorize};
use crate::db;
use crate::error::{AppResult, ServerError};
use crate::ids::{ApplicationId, BackupTargetId, ProjectId, ServerId, TeamId, UserId};
use crate::jobs::{self, JobKind};
use crate::networks::{self, ResourceKind};
use crate::servers::{self, Server, ServerStatus};
use crate::state::AppState;
use crate::{deployments, domains, scope, volumes};

/// Moves shown in an application's history.
pub const HISTORY: i64 = 10;

fn end(server: &Server, draining: &[ServerId]) -> End {
	End {
		name: server.name.clone(),
		active: server.status == ServerStatus::Active,
		draining: draining.contains(&server.id),
	}
}

async fn exists(db: &PgPool, sql: &'static str, id: i64) -> Result<bool, sqlx::Error> {
	sqlx::query_scalar(sql).bind(id).fetch_one(db).await
}

/// The resources of the application's environment that stay on its server, as `kind name`.
async fn peers(db: &PgPool, app: &Application) -> Result<Vec<String>, sqlx::Error> {
	sqlx::query_scalar(
		"SELECT kind || ' ' || name FROM (
			SELECT 'application' AS kind, name FROM applications
				WHERE environment_id = $1 AND server_id = $2 AND id <> $3
			UNION ALL SELECT 'database', name FROM databases
				WHERE environment_id = $1 AND server_id = $2
			UNION ALL SELECT 'model', name FROM models
				WHERE environment_id = $1 AND server_id = $2
		) p ORDER BY kind, name",
	)
	.bind(app.environment_id)
	.bind(app.server_id)
	.bind(app.id)
	.fetch_all(db)
	.await
}

/// Everything [`decide::decide`] looks at, for moving `app` from `from` to `to`.
///
/// # Errors
/// The database fails.
pub async fn facts(
	db: &PgPool,
	app: &Application,
	from: Option<&Server>,
	to: Option<&Server>,
	backup_target: bool,
	draining: &[ServerId],
) -> AppResult<Facts> {
	let id = app.id.0;
	let running = deployments::latest_running(db, app.id).await?.is_some();
	let has_image = deployments::latest_with_image(db, app.id).await?.is_some();
	let user_networks = networks::memberships(db, ResourceKind::Application, id)
		.await?
		.into_iter()
		.filter(|(n, _)| n.is_user() && n.server_id == app.server_id)
		.map(|(n, _)| n.name)
		.collect();
	let slug_taken_on_target = match to {
		Some(to) => {
			sqlx::query_scalar(
				"SELECT EXISTS (SELECT 1 FROM applications WHERE server_id = $1 AND slug = $2 AND id <> $3)",
			)
			.bind(to.id)
			.bind(&app.slug)
			.bind(app.id)
			.fetch_one(db)
			.await?
		}
		None => false,
	};
	Ok(Facts {
		app_name: app.name.clone(),
		slug: app.slug.clone(),
		from: from.map(|s| end(s, draining)),
		from_id: app.server_id.0,
		to: to.map(|s| end(s, draining)),
		to_id: to.map(|s| s.id.0),
		slug_taken_on_target,
		running,
		has_image,
		volumes: volumes::list(db, app.id).await?.len(),
		backup_target,
		moving: super::moving(db, app.id).await?,
		deploying: exists(
			db,
			"SELECT EXISTS (SELECT 1 FROM deployments
			WHERE application_id = $1 AND status IN ('queued', 'building', 'deploying'))",
			id,
		)
		.await?,
		user_networks,
		model_links: exists(
			db,
			"SELECT EXISTS (SELECT 1 FROM application_models WHERE application_id = $1)",
			id,
		)
		.await?,
		mcp: exists(
			db,
			"SELECT EXISTS (SELECT 1 FROM mcp_servers WHERE application_id = $1)
				OR EXISTS (SELECT 1 FROM application_mcp_links WHERE application_id = $1)",
			id,
		)
		.await?,
		tailscale: exists(
			db,
			"SELECT EXISTS (SELECT 1 FROM tailscale_exposures WHERE application_id = $1)",
			id,
		)
		.await?,
		preview: exists(
			db,
			"SELECT EXISTS (SELECT 1 FROM previews WHERE preview_application_id = $1)",
			id,
		)
		.await?,
		domains: domains::list(db, app.id)
			.await?
			.into_iter()
			.map(|d| d.hostname)
			.collect(),
		hostname_from: from.map(|s| applications::public_hostname(&app.slug, s)),
		hostname_to: to.map(|s| applications::public_hostname(&app.slug, s)),
		peers: peers(db, app).await?,
	})
}

/// A backup destination of the team, as a select shows it.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Destination {
	pub id: BackupTargetId,
	pub name: String,
}

/// The team's backup destinations, for someone who may read backups; nobody else sees them.
async fn destinations(db: &PgPool, user: UserId, team: TeamId) -> AppResult<Vec<Destination>> {
	match authorize(db, user, team, Action::Read, Resource::Backup).await {
		Ok(()) => {}
		Err(ServerError::Forbidden) => return Ok(Vec::new()),
		Err(e) => return Err(e),
	}
	Ok(sqlx::query_as::<_, Destination>(
		"SELECT id, name FROM backup_targets WHERE team_id = $1 ORDER BY name, id",
	)
	.bind(team)
	.fetch_all(db)
	.await?)
}

/// One server an application could go to, and what moving there would do.
#[derive(Debug, Clone)]
pub struct Candidate {
	pub server: Server,
	/// The server is being drained.
	pub draining: bool,
	pub verdict: Verdict,
}

/// What moving an application would do, on every other server of the team.
#[derive(Debug, Clone)]
pub struct Plan {
	pub application: Application,
	/// Where it runs now; `None` if that server cannot be read.
	pub server: Option<Server>,
	pub volumes: usize,
	pub candidates: Vec<Candidate>,
	/// Backup destinations to copy volume data through (when the caller may read them).
	pub destinations: Vec<Destination>,
	/// Its moves, newest first.
	pub history: Vec<Move>,
}

impl Plan {
	/// The move that is queued or running, if any.
	#[must_use]
	pub fn active(&self) -> Option<&Move> {
		self.history.iter().find(|m| m.status.in_flight())
	}
}

/// What moving `app` would do on each other server of the team. `backup_target` says
/// whether one would be picked: without it an application with volumes reads as refused.
///
/// # Errors
/// 404 outside the team's project; 403 without `read application` or `read server`.
pub async fn plan(
	db: &PgPool,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	app: ApplicationId,
	backup_target: bool,
) -> AppResult<Plan> {
	authorize(db, user, team, Action::Read, Resource::Application).await?;
	authorize(db, user, team, Action::Read, Resource::Server).await?;
	let application = scope::application(db, team, project, app).await?;
	let mut all = servers::list_by_team(db, team).await?;
	all.sort_by(|a, b| a.name.cmp(&b.name).then(a.id.0.cmp(&b.id.0)));
	let draining = super::draining_servers(db, team).await?;
	let server = all.iter().find(|s| s.id == application.server_id).cloned();
	let mut candidates = Vec::with_capacity(all.len().saturating_sub(1));
	for to in all.iter().filter(|s| s.id != application.server_id) {
		let f = facts(
			db,
			&application,
			server.as_ref(),
			Some(to),
			backup_target,
			&draining,
		)
		.await?;
		candidates.push(Candidate {
			server: to.clone(),
			draining: draining.contains(&to.id),
			verdict: decide::decide(&f),
		});
	}
	Ok(Plan {
		volumes: volumes::list(db, application.id).await?.len(),
		destinations: destinations(db, user, team).await?,
		history: super::list_moves(db, application.id, HISTORY).await?,
		application,
		server,
		candidates,
	})
}

/// Inserts a move and queues its job: the one way every move starts, from a person or a
/// drain. The job runs on the target's queue under its own lock, `move:<app>`: the work it
/// waits for (a deploy, a volume backup) takes the application's lock, `app:<app>`.
///
/// # Errors
/// [`ServerError::Conflict`] when a move of the application is already queued or running; the
/// database fails.
pub async fn queue(s: &AppState, new: &NewMove<'_>) -> AppResult<Move> {
	let mv = super::insert_move(&s.db, new).await.map_err(|e| {
		if db::is_unique_violation(&e, ONE_ACTIVE_MOVE) {
			ServerError::Conflict("This application is already being moved.".to_owned())
		} else {
			e.into()
		}
	})?;
	if let Err(e) = jobs::enqueue(
		s,
		JobKind::ApplicationMove,
		new.to.id,
		mv.id.0,
		format!("move:{}", new.application),
	)
	.await
	{
		let _ = super::finish_move(
			&s.db,
			mv.id,
			super::MoveStatus::Failed,
			"could not be queued",
			Some("failed to queue the move"),
		)
		.await;
		return Err(e.into());
	}
	Ok(mv)
}

/// The refusals of a verdict as one error: a busy application is a conflict, the rest is a
/// request that cannot be done.
fn refused(verdict: &Verdict, f: &Facts) -> ServerError {
	let text = verdict.refusals.join(" ");
	if f.moving || f.deploying {
		ServerError::Conflict(text)
	} else {
		ServerError::Validation(text)
	}
}

/// Moves `app` to the server `to`. `backup_target` is needed when it has volumes: their data
/// goes through it.
///
/// # Errors
/// 404 for an application, server or backup destination outside the team; 403 without
/// `update application`, `read server` (or `create backup` for volumes); 400 with the reasons
/// when it cannot move there; 409 when it is being moved or deployed already.
pub async fn start(
	s: &AppState,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	app: ApplicationId,
	to: ServerId,
	backup_target: Option<BackupTargetId>,
) -> AppResult<Move> {
	authorize(&s.db, user, team, Action::Update, Resource::Application).await?;
	authorize(&s.db, user, team, Action::Read, Resource::Server).await?;
	let application = scope::application(&s.db, team, project, app).await?;
	let to = scope::server(&s.db, team, to).await?;
	let from = scope::server(&s.db, team, application.server_id).await.ok();
	let volumes = volumes::list(&s.db, application.id).await?.len();
	// A destination is only used for volumes; one given without volumes is left out.
	let backup_target = match backup_target.filter(|_| volumes > 0) {
		Some(id) => {
			authorize(&s.db, user, team, Action::Create, Resource::Backup).await?;
			Some(scope::backup_target(&s.db, team, id).await?.id)
		}
		None => None,
	};
	let draining = super::draining_servers(&s.db, team).await?;
	let f = facts(
		&s.db,
		&application,
		from.as_ref(),
		Some(&to),
		backup_target.is_some(),
		&draining,
	)
	.await?;
	let verdict = decide::decide(&f);
	if !verdict.movable() {
		return Err(refused(&verdict, &f));
	}
	let Some(from) = from else {
		return Err(ServerError::ServerNotFound);
	};
	let mv = queue(
		s,
		&NewMove {
			application: application.id,
			from: &from,
			to: &to,
			method: verdict.method,
			backup_target,
			drain: None,
			requested_by: Some(user),
		},
	)
	.await?;
	audit::log(
		&s.db,
		user,
		team,
		"app.move",
		Target::new("application", application.id)
			.named(&application.slug)
			.in_project(application.project_id),
		Diff::new()
			.set("move_id", mv.id.0)
			.set("from_server", from.name.as_str())
			.set("to_server", to.name.as_str())
			.set("method", verdict.method.as_str())
			.set("backup_target_id", backup_target.map(|b| b.0)),
	)
	.await;
	Ok(mv)
}

/// The application's moves, newest first; needs `read application`.
///
/// # Errors
/// 404 outside the team's project; 403.
pub async fn history(
	db: &PgPool,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	app: ApplicationId,
) -> AppResult<Vec<Move>> {
	authorize(db, user, team, Action::Read, Resource::Application).await?;
	let application = scope::application(db, team, project, app).await?;
	Ok(super::list_moves(db, application.id, HISTORY).await?)
}

// --- drains -----------------------------------------------------------------------------------

/// One thing on a server, with what a drain would do to it.
#[derive(Debug, Clone)]
pub struct Found {
	pub item: Item,
	/// Applications only.
	pub verdict: Option<Verdict>,
}

/// Rows of `(id, name, project name)`.
async fn named(
	db: &PgPool,
	sql: &'static str,
	server: ServerId,
) -> Result<Vec<(i64, String, Option<String>)>, sqlx::Error> {
	sqlx::query_as(sql).bind(server).fetch_all(db).await
}

fn kept(kind: ItemKind, id: i64, name: String, project: Option<String>, why: &str) -> Found {
	Found {
		item: Item {
			kind,
			id,
			name,
			project,
			outcome: Outcome::Kept,
			detail: Some(why.to_owned()),
			method: None,
			move_id: None,
		},
		verdict: None,
	}
}

const DATABASE_KEPT: &str = "Databases are not moved: a copy needs a dump taken while nothing writes to it. Back it up, create it on the target, restore the backup there, then point the applications at it.";
const STACK_KEPT: &str = "Stacks are not moved: create the stack on the target from its compose file, then remove it here.";
const MODEL_KEPT: &str = "Models are not moved: their weights are a download on this server. Download the file on the target and make the model there.";
const MODEL_FILE_KEPT: &str =
	"Model files are not moved: download it again on the target, then remove it here.";

/// Everything on `server`, each with what a drain to `target` would do, in the order a drain
/// takes them ([`decide::order`]).
///
/// # Errors
/// The database fails.
#[allow(clippy::too_many_lines, reason = "one query per kind of resource")]
pub async fn inventory(
	db: &PgPool,
	team: TeamId,
	server: &Server,
	target: Option<&Server>,
	backup_target: bool,
) -> AppResult<Vec<Found>> {
	let draining = super::draining_servers(db, team).await?;
	let mut found = Vec::new();
	let apps: Vec<(i64, Option<String>)> = sqlx::query_as(
		"SELECT a.id, p.name FROM applications a JOIN projects p ON p.id = a.project_id
		WHERE a.server_id = $1 ORDER BY a.name",
	)
	.bind(server.id)
	.fetch_all(db)
	.await?;
	for (id, project) in apps {
		let Some(app) = applications::find_by_id(db, ApplicationId(id)).await? else {
			continue;
		};
		let f = facts(db, &app, Some(server), target, backup_target, &draining).await?;
		let verdict = decide::decide(&f);
		let (outcome, detail) = if verdict.movable() {
			(Outcome::Pending, None)
		} else {
			(Outcome::Kept, Some(verdict.refusals.join(" ")))
		};
		found.push(Found {
			item: Item {
				kind: ItemKind::Application,
				id,
				name: app.name.clone(),
				project,
				outcome,
				detail,
				method: Some(verdict.method),
				move_id: None,
			},
			verdict: Some(verdict),
		});
	}
	decide::order(&mut found, |f| f.verdict.as_ref(), |f| f.item.name.as_str());

	for (id, name, project) in named(
		db,
		"SELECT d.id, d.name, p.name FROM databases d JOIN projects p ON p.id = d.project_id
		WHERE d.server_id = $1 ORDER BY d.name",
		server.id,
	)
	.await?
	{
		found.push(kept(ItemKind::Database, id, name, project, DATABASE_KEPT));
	}
	for (id, name, project) in named(
		db,
		"SELECT s.id, s.name, p.name FROM stacks s JOIN projects p ON p.id = s.project_id
		WHERE s.server_id = $1 ORDER BY s.name",
		server.id,
	)
	.await?
	{
		found.push(kept(ItemKind::Stack, id, name, project, STACK_KEPT));
	}
	for (id, name, project) in named(
		db,
		"SELECT m.id, m.name, p.name FROM models m JOIN projects p ON p.id = m.project_id
		WHERE m.server_id = $1 ORDER BY m.name",
		server.id,
	)
	.await?
	{
		found.push(kept(ItemKind::Model, id, name, project, MODEL_KEPT));
	}
	for (id, name, project) in named(
		db,
		"SELECT id, repo, NULL::text FROM model_files WHERE server_id = $1 ORDER BY repo",
		server.id,
	)
	.await?
	{
		found.push(kept(
			ItemKind::ModelFile,
			id,
			name,
			project,
			MODEL_FILE_KEPT,
		));
	}
	// A task of an application goes with its application; one that runs an image of its own
	// only names a server, so it is simply pointed at the target.
	for (id, name, project) in named(
		db,
		"SELECT t.id, t.name, p.name FROM cron_tasks t JOIN projects p ON p.id = t.project_id
		WHERE t.server_id = $1 AND t.application_id IS NULL ORDER BY t.name",
		server.id,
	)
	.await?
	{
		found.push(Found {
			item: Item {
				kind: ItemKind::CronTask,
				id,
				name,
				project,
				outcome: Outcome::Pending,
				detail: None,
				method: None,
				move_id: None,
			},
			verdict: None,
		});
	}
	Ok(found)
}

/// What draining a server would do, and how its last drain went.
#[derive(Debug, Clone)]
pub struct DrainPlan {
	pub server: Server,
	pub draining_since: Option<OffsetDateTime>,
	/// The other servers of the team, by name.
	pub targets: Vec<Server>,
	/// The one picked, if any.
	pub target: Option<Server>,
	pub found: Vec<Found>,
	pub destinations: Vec<Destination>,
	pub latest: Option<Drain>,
}

/// What draining `server` into `target` would do; needs `read server`. Without a target the
/// applications are judged on their own.
///
/// # Errors
/// 404 for servers outside the team; 403.
pub async fn drain_plan(
	db: &PgPool,
	user: UserId,
	team: TeamId,
	server: ServerId,
	target: Option<ServerId>,
	backup_target: bool,
) -> AppResult<DrainPlan> {
	authorize(db, user, team, Action::Read, Resource::Server).await?;
	let server = scope::server(db, team, server).await?;
	let target = match target.filter(|t| *t != server.id) {
		Some(id) => Some(scope::server(db, team, id).await?),
		None => None,
	};
	let mut targets: Vec<Server> = servers::list_by_team(db, team)
		.await?
		.into_iter()
		.filter(|s| s.id != server.id)
		.collect();
	targets.sort_by(|a, b| a.name.cmp(&b.name).then(a.id.0.cmp(&b.id.0)));
	Ok(DrainPlan {
		draining_since: super::draining_since(db, server.id).await?,
		found: inventory(db, team, &server, target.as_ref(), backup_target).await?,
		destinations: destinations(db, user, team).await?,
		latest: super::latest_drain(db, server.id).await?,
		targets,
		target,
		server,
	})
}

/// Marks `server` draining and queues the job that moves what can move to `target`, one
/// application at a time; needs `update server`.
///
/// # Errors
/// 404 for servers or a destination outside the team; 403; 400 when the target is the server
/// itself, not ready or draining; 409 while a drain of the server is in flight.
pub async fn start_drain(
	s: &AppState,
	user: UserId,
	team: TeamId,
	server: ServerId,
	target: ServerId,
	backup_target: Option<BackupTargetId>,
) -> AppResult<Drain> {
	authorize(&s.db, user, team, Action::Update, Resource::Server).await?;
	let server = scope::server(&s.db, team, server).await?;
	let target = scope::server(&s.db, team, target).await?;
	if target.id == server.id {
		return Err(ServerError::Validation(
			"Pick another server to move things to.".to_owned(),
		));
	}
	if target.status != ServerStatus::Active {
		return Err(ServerError::Validation(format!(
			"{} is not ready: its agent does not answer.",
			target.name
		)));
	}
	if super::draining_since(&s.db, target.id).await?.is_some() {
		return Err(ServerError::Validation(format!(
			"{} is being drained itself. Pick another server.",
			target.name
		)));
	}
	let backup_target = match backup_target {
		Some(id) => {
			authorize(&s.db, user, team, Action::Create, Resource::Backup).await?;
			Some(scope::backup_target(&s.db, team, id).await?.id)
		}
		None => None,
	};
	let found = inventory(&s.db, team, &server, Some(&target), backup_target.is_some()).await?;
	let report: Vec<Item> = found.into_iter().map(|f| f.item).collect();
	let drain = super::insert_drain(&s.db, &server, &target, backup_target, &report, user)
		.await
		.map_err(|e| {
			if db::is_unique_violation(&e, ONE_ACTIVE_DRAIN) {
				ServerError::Conflict(format!("{} is being drained already.", server.name))
			} else {
				e.into()
			}
		})?;
	super::set_draining(&s.db, server.id, true).await?;
	if let Err(e) = jobs::enqueue(
		s,
		JobKind::ServerDrain,
		server.id,
		drain.id.0,
		format!("drain:{}", server.id),
	)
	.await
	{
		let _ = super::fail_drain(
			s,
			drain.id.0,
			&jobs::JobFailure::Failed("failed to queue the drain".to_owned()),
		)
		.await;
		return Err(e.into());
	}
	audit::log(
		&s.db,
		user,
		team,
		"server.drain",
		Target::new("server", server.id).named(&server.name),
		Diff::new()
			.set("drain_id", drain.id.0)
			.set("to_server", target.name.as_str())
			.set("backup_target_id", backup_target.map(|b| b.0))
			.set(
				"applications",
				report
					.iter()
					.filter(|i| i.kind == ItemKind::Application && i.outcome == Outcome::Pending)
					.count(),
			),
	)
	.await;
	Ok(drain)
}

/// Takes the draining mark off `server`, so new resources may go to it again, and stops a
/// drain that is still going: the move in progress finishes, nothing after it starts. Needs
/// `update server`.
///
/// # Errors
/// 404 outside the team; 403; 400 when the server is not draining.
pub async fn stop_draining(
	s: &AppState,
	user: UserId,
	team: TeamId,
	server: ServerId,
) -> AppResult<Server> {
	authorize(&s.db, user, team, Action::Update, Resource::Server).await?;
	let server = scope::server(&s.db, team, server).await?;
	if super::draining_since(&s.db, server.id).await?.is_none() {
		return Err(ServerError::Validation(format!(
			"{} is not being drained.",
			server.name
		)));
	}
	super::set_draining(&s.db, server.id, false).await?;
	if let Some(drain) = super::latest_drain(&s.db, server.id)
		.await?
		.filter(|d| d.status.in_flight())
		&& let Some(job) = jobs::latest_for(&s.db, JobKind::ServerDrain, drain.id.0).await?
	{
		jobs::worker::cancel(s, job.id).await?;
	}
	audit::log(
		&s.db,
		user,
		team,
		"server.undrain",
		Target::new("server", server.id).named(&server.name),
		Diff::new(),
	)
	.await;
	Ok(server)
}
