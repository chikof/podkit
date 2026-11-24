//! Backups of one volume over the JSON API, under
//! `/api/v1/teams/{team}/projects/{project}/applications/{application}/volumes/{volume}/backups`
//! and `/api/v1/teams/{team}/projects/{project}/stacks/{stack}/volumes/{name}/backups`.
//! The use cases are in [`super::service`].

use axum::extract::{FromRequestParts, Query, RawPathParams, State};
use axum::http::StatusCode;
use axum::http::request::Parts;
use axum::routing::{delete, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use super::service::{self, RestoreInput, SettingsInput};
use super::subject::Vol;
use super::{Consistency, Policy, Restore, RestoreMode, RestoreState, VolumeBackup};
use crate::auth::AuthUser;
use crate::backups::BackupStatus;
use crate::error::{AppResult, ServerError};
use crate::ids::{
	ApplicationId, BackupTargetId, ProjectId, StackId, TeamId, UserId, VolumeBackupId, VolumeId,
	VolumeRestoreId,
};
use crate::state::AppState;

/// Routes under an application volume's `/backups`.
pub fn app_router() -> Router<AppState> {
	router()
}

/// Routes under a stack volume's `/backups`.
pub fn stack_router() -> Router<AppState> {
	router()
}

fn router() -> Router<AppState> {
	Router::new()
		.route("/", post(create).get(list))
		.route("/settings", axum::routing::get(settings).put(save_settings))
		.route("/{backup_id}", delete(remove))
		.route("/{backup_id}/restore", post(restore))
}

/// The volume a request path names, whichever kind of owner it is under. The UI and
/// the API share it: it reads the named path parameters, so the same handler serves
/// `…/applications/{application_id}/volumes/{volume_id}/…` and
/// `…/stacks/{stack_id}/volumes/{volume_key}/…`.
#[derive(Debug, Clone)]
pub struct VolRoute {
	pub team: TeamId,
	pub project: ProjectId,
	pub vol: Vol,
	pub backup: Option<VolumeBackupId>,
}

impl<S: Send + Sync> FromRequestParts<S> for VolRoute {
	type Rejection = ServerError;

	async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
		let raw = RawPathParams::from_request_parts(parts, state)
			.await
			.map_err(|_| ServerError::NotFound)?;
		let get = |names: &[&str]| {
			raw.iter()
				.find(|(k, _)| names.contains(k))
				.map(|(_, v)| v.to_owned())
		};
		let id = |names: &[&str]| -> Result<i64, ServerError> {
			get(names)
				.and_then(|v| v.parse().ok())
				.ok_or(ServerError::NotFound)
		};
		let vol = if let Some(stack) = get(&["stack_id"]) {
			Vol::Stack {
				stack: StackId(stack.parse().map_err(|_| ServerError::NotFound)?),
				key: get(&["volume_key"]).ok_or(ServerError::NotFound)?,
			}
		} else {
			Vol::App {
				app: ApplicationId(id(&["application_id", "app_id"])?),
				volume: VolumeId(id(&["volume_id"])?),
			}
		};
		Ok(Self {
			team: TeamId(id(&["team_id"])?),
			project: ProjectId(id(&["project_id"])?),
			vol,
			backup: get(&["backup_id"])
				.map(|v| {
					v.parse()
						.map(VolumeBackupId)
						.map_err(|_| ServerError::NotFound)
				})
				.transpose()?,
		})
	}
}

impl VolRoute {
	/// The backup the path names; a route without one is a bug.
	///
	/// # Errors
	/// Not found.
	pub fn backup(&self) -> AppResult<VolumeBackupId> {
		self.backup.ok_or(ServerError::BackupNotFound)
	}
}

#[derive(Serialize)]
struct BackupResponse {
	id: VolumeBackupId,
	target_id: Option<BackupTargetId>,
	status: BackupStatus,
	consistency: Consistency,
	object_key: Option<String>,
	size_bytes: Option<i64>,
	raw_bytes: Option<i64>,
	entries: Option<i64>,
	sha256: Option<String>,
	warning: Option<String>,
	error: Option<String>,
	triggered_by: Option<UserId>,
	created_at: String,
	finished_at: Option<String>,
}

impl From<&VolumeBackup> for BackupResponse {
	fn from(b: &VolumeBackup) -> Self {
		Self {
			id: b.id,
			target_id: b.target_id,
			status: b.status,
			consistency: b.consistency,
			object_key: b.object_key.clone(),
			size_bytes: b.size_bytes,
			raw_bytes: b.raw_bytes,
			entries: b.entries,
			sha256: b.sha256.clone(),
			warning: b.warning.clone(),
			error: b.error.clone(),
			triggered_by: b.triggered_by,
			created_at: b.created_at.to_string(),
			finished_at: b.finished_at.map(|t| t.to_string()),
		}
	}
}

#[derive(Serialize)]
struct SettingsResponse {
	target_id: Option<BackupTargetId>,
	schedule: Option<String>,
	next_run_at: Option<String>,
	keep: i32,
	consistency: Consistency,
}

impl From<&Policy> for SettingsResponse {
	fn from(p: &Policy) -> Self {
		Self {
			target_id: p.target_id,
			schedule: p.schedule.clone(),
			next_run_at: p.next_run_at.map(|t| t.to_string()),
			keep: p.keep,
			consistency: p.consistency,
		}
	}
}

#[derive(Serialize)]
struct RestoreResponse {
	id: VolumeRestoreId,
	backup_id: VolumeBackupId,
	status: RestoreState,
	mode: RestoreMode,
	dest_volume: String,
}

impl From<&Restore> for RestoreResponse {
	fn from(r: &Restore) -> Self {
		Self {
			id: r.id,
			backup_id: r.backup_id,
			status: r.status,
			mode: r.mode,
			dest_volume: r.dest_volume.clone(),
		}
	}
}

#[derive(Serialize)]
struct DeleteResponse {
	id: VolumeBackupId,
}

#[derive(Deserialize)]
struct ListParams {
	limit: Option<i64>,
	before: Option<VolumeBackupId>,
}

#[derive(Deserialize)]
struct SettingsBody {
	target_id: Option<BackupTargetId>,
	schedule: Option<String>,
	keep: Option<i32>,
	consistency: Option<Consistency>,
}

#[derive(Deserialize)]
struct RestoreBody {
	mode: RestoreMode,
	new_volume: Option<String>,
	confirm: String,
}

/// Queues a backup of the volume to its configured destination and returns
/// immediately. Poll the list for the outcome. Caller must hold `create backup`.
async fn create(
	State(s): State<AppState>,
	user: AuthUser,
	route: VolRoute,
) -> AppResult<(StatusCode, Json<BackupResponse>)> {
	let backup = service::create(&s, user.id, route.team, route.project, &route.vol).await?;
	Ok((StatusCode::ACCEPTED, Json(BackupResponse::from(&backup))))
}

/// The volume's backups, newest first, paged with `?limit=` (1..=200, default 20) and
/// `?before=<backup id>`. Caller must hold `read backup`.
async fn list(
	State(s): State<AppState>,
	user: AuthUser,
	route: VolRoute,
	Query(params): Query<ListParams>,
) -> AppResult<Json<Vec<BackupResponse>>> {
	let rows = service::list(
		&s,
		user.id,
		route.team,
		route.project,
		&route.vol,
		params.before,
		params.limit,
	)
	.await?;
	Ok(Json(rows.iter().map(BackupResponse::from).collect()))
}

/// The volume's destination, schedule, retention and consistency. Caller must hold
/// `read backup`.
async fn settings(
	State(s): State<AppState>,
	user: AuthUser,
	route: VolRoute,
) -> AppResult<Json<SettingsResponse>> {
	let view = service::overview(&s, user.id, route.team, route.project, &route.vol).await?;
	Ok(Json(match &view.policy {
		Some(policy) => SettingsResponse::from(policy),
		None => SettingsResponse {
			target_id: None,
			schedule: None,
			next_run_at: None,
			keep: super::DEFAULT_KEEP,
			consistency: Consistency::Live,
		},
	}))
}

/// Replaces the settings. Caller must hold `update backup`; the destination must be
/// the team's own.
async fn save_settings(
	State(s): State<AppState>,
	user: AuthUser,
	route: VolRoute,
	Json(body): Json<SettingsBody>,
) -> AppResult<Json<SettingsResponse>> {
	let policy = service::save_settings(
		&s,
		user.id,
		route.team,
		route.project,
		&route.vol,
		SettingsInput {
			target_id: body.target_id,
			schedule: body.schedule,
			keep: body.keep,
			consistency: body.consistency,
		},
	)
	.await?;
	Ok(Json(SettingsResponse::from(&policy)))
}

/// Queues a restore of a succeeded backup. The owner is stopped while it runs.
/// Caller must hold `update volume`; `confirm` must be the volume's mount path or
/// compose name.
async fn restore(
	State(s): State<AppState>,
	user: AuthUser,
	route: VolRoute,
	Json(body): Json<RestoreBody>,
) -> AppResult<(StatusCode, Json<RestoreResponse>)> {
	let restore = service::restore(
		&s,
		user.id,
		route.team,
		route.project,
		&route.vol,
		route.backup()?,
		RestoreInput {
			mode: body.mode,
			new_volume: body.new_volume,
			confirm: body.confirm,
		},
	)
	.await?;
	Ok((StatusCode::ACCEPTED, Json(RestoreResponse::from(&restore))))
}

/// Deletes the backup's object from its destination, then the row. Caller must hold
/// `delete backup`.
async fn remove(
	State(s): State<AppState>,
	user: AuthUser,
	route: VolRoute,
) -> AppResult<Json<DeleteResponse>> {
	let id = service::remove(
		&s,
		user.id,
		route.team,
		route.project,
		&route.vol,
		route.backup()?,
	)
	.await?;
	Ok(Json(DeleteResponse { id }))
}
