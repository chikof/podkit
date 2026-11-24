//! Moving an application to another server of its team, and draining a server (migration
//! 0084).
//!
//! - **A move** ([`Move`]) is one row and one `application_move` job ([`run::run_move`]).
//!   How it goes is decided by [`decide`]: a stateless application is redeployed on the
//!   target with the image it runs now (copied over the way a remote build's image is,
//!   `builds::delivery`), and only once it is up does the old container go, so the old one
//!   serves until then; an application with volumes is stopped, its volumes are archived and
//!   restored on the target through a backup destination, and it is started there; when the
//!   old server does not answer, the target builds or pulls it again. Things tied to one
//!   server (a Tailscale sidecar, model and MCP links through that server's gateway, user
//!   networks, previews) are refused with the way out.
//! - **The switch** is `applications.server_id` (and the server of its scheduled tasks),
//!   written once the data is on the target and before it starts there, so the deploy that
//!   follows is a normal deploy of the application. When that deploy fails, the switch is
//!   undone and what was stopped is started again. While a move runs, its application is
//!   left out of crash detection (the old container is on another server than the row says)
//!   and other deploys of it are refused.
//! - **A drain** ([`Drain`]) marks the server draining (`servers.draining_since`), so no new
//!   resource is put on it, and runs one `server_drain` job that moves each movable
//!   application to the chosen server, one move at a time, writing what became of every
//!   thing on the server into its report. Databases, stacks and models are listed and kept,
//!   with the reason.
//!
//! Podman's checkpoint and restore (CRIU) is not used: a restored container keeps the old
//! server's routing labels (the generated address has the old IP in it) and addresses, so
//! the method is always one of the three above. `docs/api.md` says what each one costs.

pub mod decide;
mod routes;
pub mod run;
pub mod service;
#[cfg(test)]
mod tests;

use serde::{Deserialize, Serialize};
use sqlx::PgExecutor;
use time::OffsetDateTime;

pub use decide::Method;
pub use routes::{app_router, history_router, server_router, undrain_router};

use crate::error::{AppResult, ServerError};
use crate::ids::{
	ApplicationId, BackupTargetId, DeploymentId, DrainId, MoveId, ServerId, TeamId, UserId,
};
use crate::jobs::JobFailure;
use crate::servers::Server;
use crate::state::AppState;

/// Where a move is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "varchar", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum MoveStatus {
	Queued,
	Running,
	Succeeded,
	Failed,
	Cancelled,
}

impl MoveStatus {
	#[must_use]
	pub const fn as_str(self) -> &'static str {
		match self {
			Self::Queued => "queued",
			Self::Running => "running",
			Self::Succeeded => "succeeded",
			Self::Failed => "failed",
			Self::Cancelled => "cancelled",
		}
	}

	#[must_use]
	pub const fn in_flight(self) -> bool {
		matches!(self, Self::Queued | Self::Running)
	}
}

/// A row of `application_moves`.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Move {
	pub id: MoveId,
	pub application_id: ApplicationId,
	pub from_server_id: Option<ServerId>,
	pub to_server_id: Option<ServerId>,
	pub from_server_name: String,
	pub to_server_name: String,
	pub method: Method,
	pub status: MoveStatus,
	/// What it does now, or how it ended.
	pub step: String,
	pub error: Option<String>,
	/// The deployment it started on the target.
	pub deployment_id: Option<DeploymentId>,
	pub backup_target_id: Option<BackupTargetId>,
	pub drain_id: Option<DrainId>,
	pub requested_by: Option<UserId>,
	/// Set once the application points at the target.
	pub switched_at: Option<OffsetDateTime>,
	pub created_at: OffsetDateTime,
	pub started_at: Option<OffsetDateTime>,
	pub finished_at: Option<OffsetDateTime>,
}

const MOVE_COLUMNS: &str = "id, application_id, from_server_id, to_server_id, from_server_name, \
	to_server_name, method, status, step, error, deployment_id, backup_target_id, drain_id, \
	requested_by, switched_at, created_at, started_at, finished_at";

/// Where a drain is. `Partial`: some things moved, some stayed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "varchar", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum DrainStatus {
	Queued,
	Running,
	Succeeded,
	Partial,
	Failed,
	Cancelled,
}

impl DrainStatus {
	#[must_use]
	pub const fn as_str(self) -> &'static str {
		match self {
			Self::Queued => "queued",
			Self::Running => "running",
			Self::Succeeded => "succeeded",
			Self::Partial => "partial",
			Self::Failed => "failed",
			Self::Cancelled => "cancelled",
		}
	}

	#[must_use]
	pub const fn in_flight(self) -> bool {
		matches!(self, Self::Queued | Self::Running)
	}
}

/// What sits on a server.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemKind {
	Application,
	Database,
	Stack,
	Model,
	ModelFile,
	CronTask,
}

impl ItemKind {
	#[must_use]
	pub const fn label(self) -> &'static str {
		match self {
			Self::Application => "application",
			Self::Database => "database",
			Self::Stack => "stack",
			Self::Model => "model",
			Self::ModelFile => "model file",
			Self::CronTask => "scheduled task",
		}
	}
}

/// What became of one thing in a drain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
	/// Will be moved; not reached yet.
	Pending,
	/// Its move is running.
	Moving,
	Moved,
	Failed,
	/// Stays on the server; `detail` says why.
	Kept,
	/// The drain stopped before reaching it.
	Skipped,
}

/// One thing a drain found on the server, and what became of it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Item {
	pub kind: ItemKind,
	pub id: i64,
	pub name: String,
	/// The project it belongs to, when it belongs to one.
	#[serde(default)]
	pub project: Option<String>,
	pub outcome: Outcome,
	#[serde(default)]
	pub detail: Option<String>,
	#[serde(default)]
	pub method: Option<Method>,
	#[serde(default)]
	pub move_id: Option<MoveId>,
}

/// A row of `server_drains`.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Drain {
	pub id: DrainId,
	pub team_id: TeamId,
	pub server_id: ServerId,
	pub target_server_id: Option<ServerId>,
	pub target_server_name: String,
	pub backup_target_id: Option<BackupTargetId>,
	pub status: DrainStatus,
	pub report: sqlx::types::Json<Vec<Item>>,
	pub requested_by: Option<UserId>,
	pub created_at: OffsetDateTime,
	pub started_at: Option<OffsetDateTime>,
	pub finished_at: Option<OffsetDateTime>,
}

const DRAIN_COLUMNS: &str = "id, team_id, server_id, target_server_id, target_server_name, \
	backup_target_id, status, report, requested_by, created_at, started_at, finished_at";

// --- draining -------------------------------------------------------------------------------

/// Since when `server` is draining; `None` when it is not.
pub async fn draining_since(
	db: impl PgExecutor<'_>,
	server: ServerId,
) -> Result<Option<OffsetDateTime>, sqlx::Error> {
	Ok(sqlx::query_scalar::<_, Option<OffsetDateTime>>(
		"SELECT draining_since FROM servers WHERE id = $1",
	)
	.bind(server)
	.fetch_optional(db)
	.await?
	.flatten())
}

/// The team's servers that are draining.
pub async fn draining_servers(
	db: impl PgExecutor<'_>,
	team: TeamId,
) -> Result<Vec<ServerId>, sqlx::Error> {
	sqlx::query_scalar(
		"SELECT id FROM servers WHERE team_id = $1 AND draining_since IS NOT NULL ORDER BY id",
	)
	.bind(team)
	.fetch_all(db)
	.await
}

/// Marks the server draining (keeping the first time it was marked) or takes the mark off.
pub async fn set_draining(
	db: impl PgExecutor<'_>,
	server: ServerId,
	on: bool,
) -> Result<(), sqlx::Error> {
	sqlx::query(
		"UPDATE servers SET draining_since = CASE WHEN $2 THEN COALESCE(draining_since, NOW()) END
		WHERE id = $1",
	)
	.bind(server)
	.bind(on)
	.execute(db)
	.await?;
	Ok(())
}

/// Refuses `server` as the home of a new resource while it is draining.
///
/// # Errors
/// [`ServerError::Conflict`] naming the server; the database fails.
pub async fn ensure_open(db: impl PgExecutor<'_>, server: &Server) -> AppResult<()> {
	if draining_since(db, server.id).await?.is_some() {
		return Err(ServerError::Conflict(format!(
			"{} is being drained and takes no new resources. Pick another server.",
			server.name
		)));
	}
	Ok(())
}

/// `servers` without the draining ones, for a create form's server select.
pub async fn open_servers(db: impl PgExecutor<'_>, servers: Vec<Server>) -> AppResult<Vec<Server>> {
	let Some(team) = servers.first().map(|s| s.team_id) else {
		return Ok(servers);
	};
	let draining = draining_servers(db, team).await?;
	Ok(servers
		.into_iter()
		.filter(|s| !draining.contains(&s.id))
		.collect())
}

/// Whether a move of the application is queued or running.
pub async fn moving(db: impl PgExecutor<'_>, app: ApplicationId) -> Result<bool, sqlx::Error> {
	sqlx::query_scalar(
		"SELECT EXISTS (SELECT 1 FROM application_moves
		WHERE application_id = $1 AND status IN ('queued', 'running'))",
	)
	.bind(app)
	.fetch_one(db)
	.await
}

/// Refuses a deploy (or restart, rollback, promotion) of an application that is being moved:
/// the move runs its own deploy on the target.
///
/// # Errors
/// [`ServerError::Conflict`]; the database fails.
pub async fn ensure_not_moving(
	db: impl PgExecutor<'_>,
	app: &crate::applications::Application,
) -> AppResult<()> {
	if moving(db, app.id).await? {
		return Err(ServerError::Conflict(format!(
			"{} is being moved to another server. Deploy it once the move is done.",
			app.name
		)));
	}
	Ok(())
}

// --- moves ------------------------------------------------------------------------------------

/// What a new move row holds.
pub struct NewMove<'a> {
	pub application: ApplicationId,
	pub from: &'a Server,
	pub to: &'a Server,
	pub method: Method,
	pub backup_target: Option<BackupTargetId>,
	pub drain: Option<DrainId>,
	pub requested_by: Option<UserId>,
}

/// Unique index that allows one queued or running move per application.
pub const ONE_ACTIVE_MOVE: &str = "application_moves_one_active";

pub async fn insert_move(db: impl PgExecutor<'_>, new: &NewMove<'_>) -> Result<Move, sqlx::Error> {
	sqlx::query_as::<_, Move>(sqlx::AssertSqlSafe(format!(
		"INSERT INTO application_moves (id, application_id, from_server_id, to_server_id,
			from_server_name, to_server_name, method, backup_target_id, drain_id, requested_by, step)
		VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, 'waiting for its turn')
		RETURNING {MOVE_COLUMNS}"
	)))
	.bind(MoveId::generate())
	.bind(new.application)
	.bind(new.from.id)
	.bind(new.to.id)
	.bind(&new.from.name)
	.bind(&new.to.name)
	.bind(new.method)
	.bind(new.backup_target)
	.bind(new.drain)
	.bind(new.requested_by)
	.fetch_one(db)
	.await
}

pub async fn find_move(db: impl PgExecutor<'_>, id: MoveId) -> Result<Option<Move>, sqlx::Error> {
	sqlx::query_as::<_, Move>(sqlx::AssertSqlSafe(format!(
		"SELECT {MOVE_COLUMNS} FROM application_moves WHERE id = $1"
	)))
	.bind(id)
	.fetch_optional(db)
	.await
}

/// The application's moves, newest first.
pub async fn list_moves(
	db: impl PgExecutor<'_>,
	app: ApplicationId,
	limit: i64,
) -> Result<Vec<Move>, sqlx::Error> {
	sqlx::query_as::<_, Move>(sqlx::AssertSqlSafe(format!(
		"SELECT {MOVE_COLUMNS} FROM application_moves WHERE application_id = $1
		ORDER BY id DESC LIMIT $2"
	)))
	.bind(app)
	.bind(limit)
	.fetch_all(db)
	.await
}

/// Says what the move is doing now; marks it running the first time.
pub async fn set_step(db: impl PgExecutor<'_>, id: MoveId, step: &str) -> Result<(), sqlx::Error> {
	sqlx::query(
		"UPDATE application_moves SET step = left($2, 300),
			status = CASE WHEN status = 'queued' THEN 'running' ELSE status END,
			started_at = COALESCE(started_at, NOW())
		WHERE id = $1",
	)
	.bind(id)
	.bind(step)
	.execute(db)
	.await?;
	Ok(())
}

/// Ends a move that is still in flight. A finished one is never rewritten.
pub async fn finish_move(
	db: impl PgExecutor<'_>,
	id: MoveId,
	status: MoveStatus,
	step: &str,
	error: Option<&str>,
) -> Result<(), sqlx::Error> {
	sqlx::query(
		"UPDATE application_moves SET status = $2, step = left($3, 300), error = $4,
			finished_at = NOW()
		WHERE id = $1 AND status IN ('queued', 'running')",
	)
	.bind(id)
	.bind(status)
	.bind(step)
	.bind(error)
	.execute(db)
	.await?;
	Ok(())
}

/// Points the application (and its scheduled tasks) at `to`, if it still runs on `from`.
/// Returns whether it did. Records the switch on the move in the same transaction.
///
/// # Errors
/// A unique violation on `(server_id, slug)` when the target got an application of the same
/// slug since the move was planned; other database errors.
pub async fn switch(
	db: &sqlx::PgPool,
	mv: MoveId,
	app: ApplicationId,
	from: ServerId,
	to: ServerId,
) -> Result<bool, sqlx::Error> {
	let mut tx = db.begin().await?;
	let moved = sqlx::query(
		"UPDATE applications SET server_id = $3, updated_at = NOW() WHERE id = $1 AND server_id = $2",
	)
	.bind(app)
	.bind(from)
	.bind(to)
	.execute(&mut *tx)
	.await?
	.rows_affected()
		== 1;
	if moved {
		sqlx::query(
			"UPDATE cron_tasks SET server_id = $2, updated_at = NOW() WHERE application_id = $1",
		)
		.bind(app)
		.bind(to)
		.execute(&mut *tx)
		.await?;
		sqlx::query(
			"UPDATE application_moves SET switched_at = NOW(), step = 'pointed at the new server'
			WHERE id = $1",
		)
		.bind(mv)
		.execute(&mut *tx)
		.await?;
	}
	tx.commit().await?;
	Ok(moved)
}

/// Undoes [`switch`]: the application (and its tasks) point at `from` again.
pub async fn switch_back(
	db: &sqlx::PgPool,
	mv: MoveId,
	app: ApplicationId,
	from: ServerId,
	to: ServerId,
) -> Result<bool, sqlx::Error> {
	let mut tx = db.begin().await?;
	let back = sqlx::query(
		"UPDATE applications SET server_id = $2, updated_at = NOW() WHERE id = $1 AND server_id = $3",
	)
	.bind(app)
	.bind(from)
	.bind(to)
	.execute(&mut *tx)
	.await?
	.rows_affected()
		== 1;
	if back {
		sqlx::query(
			"UPDATE cron_tasks SET server_id = $2, updated_at = NOW() WHERE application_id = $1",
		)
		.bind(app)
		.bind(from)
		.execute(&mut *tx)
		.await?;
		sqlx::query("UPDATE application_moves SET switched_at = NULL WHERE id = $1")
			.bind(mv)
			.execute(&mut *tx)
			.await?;
	}
	tx.commit().await?;
	Ok(back)
}

/// Reflects a failed or cancelled move job onto its row. A move cut off after its switch
/// (podkit restarted) is put back on the old server first, which still has its volumes and,
/// unless it was stopped for the copy, its container.
pub async fn fail_move(state: &AppState, id: i64, failure: &JobFailure) -> Result<(), sqlx::Error> {
	let Some(mv) = find_move(&state.db, MoveId(id)).await? else {
		return Ok(());
	};
	if !mv.status.in_flight() {
		return Ok(());
	}
	let (status, message) = match failure {
		JobFailure::Cancelled => (MoveStatus::Cancelled, "cancelled".to_owned()),
		JobFailure::Failed(m) => (MoveStatus::Failed, m.clone()),
	};
	let mut step = format!("it stays on {}", mv.from_server_name);
	if let (Some(_), Some(from), Some(to)) = (mv.switched_at, mv.from_server_id, mv.to_server_id)
		&& switch_back(&state.db, mv.id, mv.application_id, from, to).await?
	{
		step = format!(
			"put back on {}; start it again if it was stopped",
			mv.from_server_name
		);
	}
	finish_move(&state.db, mv.id, status, &step, Some(&message)).await
}

// --- drains -----------------------------------------------------------------------------------

/// Unique index that allows one queued or running drain per server.
pub const ONE_ACTIVE_DRAIN: &str = "server_drains_one_active";

pub async fn insert_drain(
	db: impl PgExecutor<'_>,
	server: &Server,
	target: &Server,
	backup_target: Option<BackupTargetId>,
	report: &[Item],
	requested_by: UserId,
) -> Result<Drain, sqlx::Error> {
	sqlx::query_as::<_, Drain>(sqlx::AssertSqlSafe(format!(
		"INSERT INTO server_drains (id, team_id, server_id, target_server_id, target_server_name,
			backup_target_id, report, requested_by)
		VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
		RETURNING {DRAIN_COLUMNS}"
	)))
	.bind(DrainId::generate())
	.bind(server.team_id)
	.bind(server.id)
	.bind(target.id)
	.bind(&target.name)
	.bind(backup_target)
	.bind(sqlx::types::Json(report))
	.bind(requested_by)
	.fetch_one(db)
	.await
}

pub async fn find_drain(
	db: impl PgExecutor<'_>,
	id: DrainId,
) -> Result<Option<Drain>, sqlx::Error> {
	sqlx::query_as::<_, Drain>(sqlx::AssertSqlSafe(format!(
		"SELECT {DRAIN_COLUMNS} FROM server_drains WHERE id = $1"
	)))
	.bind(id)
	.fetch_optional(db)
	.await
}

/// The newest drain of a server.
pub async fn latest_drain(
	db: impl PgExecutor<'_>,
	server: ServerId,
) -> Result<Option<Drain>, sqlx::Error> {
	sqlx::query_as::<_, Drain>(sqlx::AssertSqlSafe(format!(
		"SELECT {DRAIN_COLUMNS} FROM server_drains WHERE server_id = $1 ORDER BY id DESC LIMIT 1"
	)))
	.bind(server)
	.fetch_optional(db)
	.await
}

/// Stores the report and the status. A drain that already ended is never rewritten.
pub async fn save_drain(
	db: impl PgExecutor<'_>,
	id: DrainId,
	status: DrainStatus,
	report: &[Item],
) -> Result<(), sqlx::Error> {
	sqlx::query(
		"UPDATE server_drains SET status = $2, report = $3,
			started_at = COALESCE(started_at, NOW()),
			finished_at = CASE WHEN $2 IN ('queued', 'running') THEN NULL ELSE NOW() END
		WHERE id = $1 AND status IN ('queued', 'running')",
	)
	.bind(id)
	.bind(status)
	.bind(sqlx::types::Json(report))
	.execute(db)
	.await?;
	Ok(())
}

/// Reflects a failed or cancelled drain job onto its row; the report keeps what it said.
pub async fn fail_drain(
	state: &AppState,
	id: i64,
	failure: &JobFailure,
) -> Result<(), sqlx::Error> {
	let status = match failure {
		JobFailure::Cancelled => DrainStatus::Cancelled,
		JobFailure::Failed(_) => DrainStatus::Failed,
	};
	sqlx::query(
		"UPDATE server_drains SET status = $2, finished_at = NOW()
		WHERE id = $1 AND status IN ('queued', 'running')",
	)
	.bind(id)
	.bind(status)
	.execute(&state.db)
	.await?;
	Ok(())
}

/// What a finished drain report adds up to: everything moved, some of it, or none.
#[must_use]
pub fn drain_status(report: &[Item]) -> DrainStatus {
	let moved = report
		.iter()
		.filter(|i| i.outcome == Outcome::Moved)
		.count();
	if moved == report.len() {
		DrainStatus::Succeeded
	} else if moved > 0 {
		DrainStatus::Partial
	} else {
		DrainStatus::Failed
	}
}
