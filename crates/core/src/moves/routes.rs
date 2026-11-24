//! The API of moves and drains:
//!
//! - `GET  …/applications/{id}/move`: what moving it would do on each other server.
//! - `POST …/applications/{id}/move`: move it (`202`, the queued move).
//! - `GET  …/applications/{id}/moves`: its moves, newest first.
//! - `GET    /teams/{team}/servers/{id}/drain`: what draining the server would do, and its last drain.
//! - `POST   /teams/{team}/servers/{id}/drain`: drain it (`202`, the queued drain).
//! - `POST   /teams/{team}/servers/{id}/undrain`: stop draining it.

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use super::decide::{Method, Verdict};
use super::service::{self, Candidate, Destination, DrainPlan, Found, Plan};
use super::{Drain, DrainStatus, Item, Move, MoveStatus};
use crate::auth::AuthUser;
use crate::error::AppResult;
use crate::ids::{
	ApplicationId, BackupTargetId, DeploymentId, DrainId, MoveId, ProjectId, ServerId, TeamId,
};
use crate::servers::{Server, ServerStatus};
use crate::state::AppState;

/// Mounted at `…/applications/{application_id}/move`.
pub fn app_router() -> Router<AppState> {
	Router::new().route("/", get(plan).post(start))
}

/// Mounted at `…/applications/{application_id}/moves`.
pub fn history_router() -> Router<AppState> {
	Router::new().route("/", get(history))
}

/// Mounted at `/teams/{team_id}/servers/{server_id}/drain`.
pub fn server_router() -> Router<AppState> {
	Router::new().route("/", get(drain_plan).post(drain))
}

/// Mounted at `/teams/{team_id}/servers/{server_id}/undrain`: a `POST`, so an API token needs
/// the `write` scope for it, as for draining, not `delete`.
pub fn undrain_router() -> Router<AppState> {
	Router::new().route("/", post(undrain))
}

fn rfc3339(t: OffsetDateTime) -> String {
	t.format(&time::format_description::well_known::Rfc3339)
		.unwrap_or_default()
}

#[derive(Serialize)]
pub struct ServerRef {
	pub id: ServerId,
	pub name: String,
	pub status: ServerStatus,
	/// Being drained: takes nothing new.
	pub draining: bool,
}

impl ServerRef {
	fn new(s: &Server, draining: bool) -> Self {
		Self {
			id: s.id,
			name: s.name.clone(),
			status: s.status,
			draining,
		}
	}
}

#[derive(Serialize)]
pub struct VerdictResponse {
	pub movable: bool,
	pub method: Method,
	pub method_label: &'static str,
	/// What the application's users notice.
	pub downtime: String,
	/// A deployment is started there (something runs now).
	pub starts: bool,
	pub refusals: Vec<String>,
	pub warnings: Vec<String>,
}

impl From<&Verdict> for VerdictResponse {
	fn from(v: &Verdict) -> Self {
		Self {
			movable: v.movable(),
			method: v.method,
			method_label: v.method.label(),
			downtime: v.downtime.clone(),
			starts: v.starts,
			refusals: v.refusals.clone(),
			warnings: v.warnings.clone(),
		}
	}
}

#[derive(Serialize)]
pub struct CandidateResponse {
	pub server: ServerRef,
	#[serde(flatten)]
	pub verdict: VerdictResponse,
}

impl From<&Candidate> for CandidateResponse {
	fn from(c: &Candidate) -> Self {
		Self {
			server: ServerRef::new(&c.server, c.draining),
			verdict: (&c.verdict).into(),
		}
	}
}

#[derive(Serialize)]
pub struct DestinationResponse {
	pub id: BackupTargetId,
	pub name: String,
}

impl From<&Destination> for DestinationResponse {
	fn from(d: &Destination) -> Self {
		Self {
			id: d.id,
			name: d.name.clone(),
		}
	}
}

#[derive(Serialize)]
pub struct MoveResponse {
	pub id: MoveId,
	pub application_id: ApplicationId,
	pub from_server_id: Option<ServerId>,
	pub from_server: String,
	pub to_server_id: Option<ServerId>,
	pub to_server: String,
	pub method: Method,
	pub status: MoveStatus,
	pub step: String,
	pub error: Option<String>,
	pub deployment_id: Option<DeploymentId>,
	pub drain_id: Option<DrainId>,
	pub created_at: String,
	pub started_at: Option<String>,
	pub finished_at: Option<String>,
}

impl From<&Move> for MoveResponse {
	fn from(m: &Move) -> Self {
		Self {
			id: m.id,
			application_id: m.application_id,
			from_server_id: m.from_server_id,
			from_server: m.from_server_name.clone(),
			to_server_id: m.to_server_id,
			to_server: m.to_server_name.clone(),
			method: m.method,
			status: m.status,
			step: m.step.clone(),
			error: m.error.clone(),
			deployment_id: m.deployment_id,
			drain_id: m.drain_id,
			created_at: rfc3339(m.created_at),
			started_at: m.started_at.map(rfc3339),
			finished_at: m.finished_at.map(rfc3339),
		}
	}
}

#[derive(Serialize)]
pub struct MovePlanResponse {
	pub application_id: ApplicationId,
	/// Where it runs now.
	pub server: Option<ServerRef>,
	pub volumes: usize,
	pub candidates: Vec<CandidateResponse>,
	/// Destinations volume data can go through; empty without `read backup`.
	pub backup_targets: Vec<DestinationResponse>,
	/// The move that is queued or running.
	pub active: Option<MoveResponse>,
}

impl From<&Plan> for MovePlanResponse {
	fn from(p: &Plan) -> Self {
		Self {
			application_id: p.application.id,
			server: p.server.as_ref().map(|s| ServerRef::new(s, false)),
			volumes: p.volumes,
			candidates: p.candidates.iter().map(CandidateResponse::from).collect(),
			backup_targets: p
				.destinations
				.iter()
				.map(DestinationResponse::from)
				.collect(),
			active: p.active().map(MoveResponse::from),
		}
	}
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct PlanQuery {
	/// Judge volume moves as if this destination were picked.
	backup_target_id: Option<BackupTargetId>,
}

/// What moving the application would do on each other server of the team.
async fn plan(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, project, app)): Path<(TeamId, ProjectId, ApplicationId)>,
	Query(q): Query<PlanQuery>,
) -> AppResult<Json<MovePlanResponse>> {
	let plan = service::plan(
		&s.db,
		user.id,
		team,
		project,
		app,
		q.backup_target_id.is_some(),
	)
	.await?;
	Ok(Json(MovePlanResponse::from(&plan)))
}

#[derive(Deserialize)]
struct MoveRequest {
	server_id: ServerId,
	/// Needed when the application has volumes: their data is copied through it.
	#[serde(default)]
	backup_target_id: Option<BackupTargetId>,
}

/// Moves the application to `server_id`. Answers with the queued move; poll
/// `GET …/moves` (or the application) to follow it.
async fn start(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, project, app)): Path<(TeamId, ProjectId, ApplicationId)>,
	Json(body): Json<MoveRequest>,
) -> AppResult<(StatusCode, Json<MoveResponse>)> {
	let mv = service::start(
		&s,
		user.id,
		team,
		project,
		app,
		body.server_id,
		body.backup_target_id,
	)
	.await?;
	Ok((StatusCode::ACCEPTED, Json(MoveResponse::from(&mv))))
}

/// The application's moves, newest first (the last ten).
async fn history(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, project, app)): Path<(TeamId, ProjectId, ApplicationId)>,
) -> AppResult<Json<Vec<MoveResponse>>> {
	let moves = service::history(&s.db, user.id, team, project, app).await?;
	Ok(Json(moves.iter().map(MoveResponse::from).collect()))
}

// --- drains -----------------------------------------------------------------------------------

#[derive(Serialize)]
pub struct FoundResponse {
	#[serde(flatten)]
	pub item: Item,
	/// Applications only: what moving it would do.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub verdict: Option<VerdictResponse>,
}

impl From<&Found> for FoundResponse {
	fn from(f: &Found) -> Self {
		Self {
			item: f.item.clone(),
			verdict: f.verdict.as_ref().map(VerdictResponse::from),
		}
	}
}

#[derive(Serialize)]
pub struct DrainResponse {
	pub id: DrainId,
	pub server_id: ServerId,
	pub target_server_id: Option<ServerId>,
	pub target_server: String,
	pub backup_target_id: Option<BackupTargetId>,
	pub status: DrainStatus,
	pub report: Vec<Item>,
	pub created_at: String,
	pub started_at: Option<String>,
	pub finished_at: Option<String>,
}

impl From<&Drain> for DrainResponse {
	fn from(d: &Drain) -> Self {
		Self {
			id: d.id,
			server_id: d.server_id,
			target_server_id: d.target_server_id,
			target_server: d.target_server_name.clone(),
			backup_target_id: d.backup_target_id,
			status: d.status,
			report: d.report.0.clone(),
			created_at: rfc3339(d.created_at),
			started_at: d.started_at.map(rfc3339),
			finished_at: d.finished_at.map(rfc3339),
		}
	}
}

#[derive(Serialize)]
pub struct DrainPlanResponse {
	pub server: ServerRef,
	pub draining_since: Option<String>,
	pub target: Option<ServerRef>,
	/// The other servers of the team.
	pub targets: Vec<ServerRef>,
	/// Everything on the server, in the order a drain takes it.
	pub items: Vec<FoundResponse>,
	pub backup_targets: Vec<DestinationResponse>,
	/// The newest drain of the server.
	pub latest: Option<DrainResponse>,
}

impl From<&DrainPlan> for DrainPlanResponse {
	fn from(p: &DrainPlan) -> Self {
		Self {
			server: ServerRef::new(&p.server, p.draining_since.is_some()),
			draining_since: p.draining_since.map(rfc3339),
			target: p.target.as_ref().map(|t| ServerRef::new(t, false)),
			targets: p.targets.iter().map(|t| ServerRef::new(t, false)).collect(),
			items: p.found.iter().map(FoundResponse::from).collect(),
			backup_targets: p
				.destinations
				.iter()
				.map(DestinationResponse::from)
				.collect(),
			latest: p.latest.as_ref().map(DrainResponse::from),
		}
	}
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct DrainQuery {
	target_server_id: Option<ServerId>,
	backup_target_id: Option<BackupTargetId>,
}

/// What draining the server into `target_server_id` would do, and its newest drain.
async fn drain_plan(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, server)): Path<(TeamId, ServerId)>,
	Query(q): Query<DrainQuery>,
) -> AppResult<Json<DrainPlanResponse>> {
	let plan = service::drain_plan(
		&s.db,
		user.id,
		team,
		server,
		q.target_server_id,
		q.backup_target_id.is_some(),
	)
	.await?;
	Ok(Json(DrainPlanResponse::from(&plan)))
}

#[derive(Deserialize)]
struct DrainRequest {
	target_server_id: ServerId,
	#[serde(default)]
	backup_target_id: Option<BackupTargetId>,
}

/// Marks the server draining and moves what can move to `target_server_id`, one
/// application at a time.
async fn drain(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, server)): Path<(TeamId, ServerId)>,
	Json(body): Json<DrainRequest>,
) -> AppResult<(StatusCode, Json<DrainResponse>)> {
	let d = service::start_drain(
		&s,
		user.id,
		team,
		server,
		body.target_server_id,
		body.backup_target_id,
	)
	.await?;
	Ok((StatusCode::ACCEPTED, Json(DrainResponse::from(&d))))
}

#[derive(Serialize)]
struct UndrainResponse {
	server_id: ServerId,
	draining: bool,
}

/// Stops draining: new resources may go to the server again, and a drain in progress stops
/// after the move it is on.
async fn undrain(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, server)): Path<(TeamId, ServerId)>,
) -> AppResult<Json<UndrainResponse>> {
	let server = service::stop_draining(&s, user.id, team, server).await?;
	Ok(Json(UndrainResponse {
		server_id: server.id,
		draining: false,
	}))
}
