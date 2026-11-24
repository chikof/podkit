use axum::extract::{Path, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use super::plan::Plan;
use super::service::{self, Input};
use super::{CloneStatus, EnvironmentClone, Options, Secrets, Step};
use crate::auth::AuthUser;
use crate::error::AppResult;
use crate::ids::{BackupTargetId, CloneId, EnvironmentId, JobId, ProjectId, TeamId};
use crate::state::AppState;

/// Mounted next to the environment routes, under `/projects/{p}/environments`.
pub fn router() -> Router<AppState> {
	Router::new()
		.route("/{environment_id}/clone", post(start))
		.route("/{environment_id}/clone/preview", post(preview))
		.route("/clones/{clone_id}", get(show))
}

#[derive(Deserialize)]
struct CloneRequest {
	name: String,
	slug: String,
	#[serde(default)]
	secrets: Secrets,
	#[serde(default)]
	data: bool,
	#[serde(default)]
	backup_target_id: Option<BackupTargetId>,
	#[serde(default)]
	deploy: bool,
}

impl From<CloneRequest> for Input {
	fn from(r: CloneRequest) -> Self {
		Self {
			name: r.name,
			slug: r.slug,
			options: Options {
				secrets: r.secrets,
				data: r.data,
				backup_target_id: r.backup_target_id,
				deploy: r.deploy,
			},
		}
	}
}

#[derive(Serialize)]
struct CloneResponse {
	id: CloneId,
	status: CloneStatus,
	source_name: String,
	/// `None` once the new environment was deleted.
	environment_id: Option<EnvironmentId>,
	name: String,
	slug: String,
	options: Options,
	report: Vec<Step>,
	job_id: Option<JobId>,
	created_at: String,
	finished_at: Option<String>,
}

impl From<EnvironmentClone> for CloneResponse {
	fn from(c: EnvironmentClone) -> Self {
		Self {
			id: c.id,
			status: c.status,
			source_name: c.source_name,
			environment_id: c.environment_id,
			name: c.name,
			slug: c.slug,
			options: c.options.0,
			report: c.report.0,
			job_id: c.job_id,
			created_at: c.created_at.to_string(),
			finished_at: c.finished_at.map(|t| t.to_string()),
		}
	}
}

/// Clones the environment into a new one of the same project; caller must hold `create
/// project` and `create` on every kind of resource the environment has. The
/// configuration is copied at once; the response's report is filled in by a job.
async fn start(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, project, source)): Path<(TeamId, ProjectId, EnvironmentId)>,
	Json(body): Json<CloneRequest>,
) -> AppResult<Json<CloneResponse>> {
	let clone = service::clone(&s, user.id, team, project, source, body.into()).await?;
	Ok(Json(clone.into()))
}

/// What the clone would make, and what is taken already; caller must hold `read project`.
async fn preview(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, project, source)): Path<(TeamId, ProjectId, EnvironmentId)>,
	Json(body): Json<CloneRequest>,
) -> AppResult<Json<Plan>> {
	let plan = service::preview(&s, user.id, team, project, source, &body.into()).await?;
	Ok(Json(plan))
}

/// A clone and its report; caller must hold `read project`.
async fn show(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, project, id)): Path<(TeamId, ProjectId, CloneId)>,
) -> AppResult<Json<CloneResponse>> {
	let clone = service::get(&s.db, user.id, team, project, id).await?;
	Ok(Json(clone.into()))
}
