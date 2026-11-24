//! The JSON API of Quadlet: the units of one resource, and the export of a project, an
//! environment or the team as a `.tar.gz`.

use axum::extract::{Path, Query, State};
use axum::http::header;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use super::{Archive, Request, Scope, View};
use crate::auth::AuthUser;
use crate::error::AppResult;
use crate::ids::{ApplicationId, DatabaseId, EnvironmentId, ModelId, ProjectId, StackId, TeamId};
use crate::state::AppState;

/// Under `/teams/{team_id}/projects/{project_id}`.
pub fn router() -> Router<AppState> {
	Router::new()
		.route("/applications/{application_id}/quadlet", get(application))
		.route("/databases/{database_id}/quadlet", get(database))
		.route("/models/{model_id}/quadlet", get(model))
		.route("/stacks/{stack_id}/quadlet", get(stack))
		.route("/quadlet/export", post(export_project))
}

/// Under `/teams/{team_id}/quadlet`.
pub fn team_router() -> Router<AppState> {
	Router::new().route("/export", post(export_team))
}

#[derive(Serialize)]
struct UnitBody {
	name: String,
	/// Where it goes for a rootless Podman.
	path: String,
	content: String,
}

#[derive(Serialize)]
struct EnvFileBody {
	name: String,
	/// The variable names; the values only come in an export with secrets.
	keys: Vec<String>,
}

#[derive(Serialize)]
struct ViewBody {
	kind: &'static str,
	id: i64,
	name: String,
	server: String,
	directory: &'static str,
	units: Vec<UnitBody>,
	env_files: Vec<EnvFileBody>,
	mounted_files: Vec<String>,
	/// The systemd services to start, in order.
	services: Vec<String>,
}

impl From<&View> for ViewBody {
	fn from(v: &View) -> Self {
		Self {
			kind: v.kind.as_str(),
			id: v.id,
			name: v.name.clone(),
			server: v.server.clone(),
			directory: proto::quadlet::ROOTLESS_DIR,
			units: v
				.files()
				.into_iter()
				.map(|f| UnitBody {
					name: f.name.clone(),
					path: super::target_path(&f.name),
					content: f.content.clone(),
				})
				.collect(),
			env_files: v
				.env_files()
				.into_iter()
				.map(|(name, keys)| EnvFileBody {
					name: name.to_owned(),
					keys: keys.to_vec(),
				})
				.collect(),
			mounted_files: v.mounted().into_iter().map(str::to_owned).collect(),
			services: v
				.parts
				.iter()
				.flat_map(|p| p.units.services.iter().cloned())
				.collect(),
		}
	}
}

async fn application(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, project, id)): Path<(TeamId, ProjectId, ApplicationId)>,
) -> AppResult<Json<ViewBody>> {
	let view = super::application(&s, user.id, team, project, id).await?;
	Ok(Json(ViewBody::from(&view)))
}

async fn database(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, project, id)): Path<(TeamId, ProjectId, DatabaseId)>,
) -> AppResult<Json<ViewBody>> {
	let view = super::database(&s, user.id, team, project, id).await?;
	Ok(Json(ViewBody::from(&view)))
}

async fn model(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, project, id)): Path<(TeamId, ProjectId, ModelId)>,
) -> AppResult<Json<ViewBody>> {
	let view = super::model(&s, user.id, team, project, id).await?;
	Ok(Json(ViewBody::from(&view)))
}

async fn stack(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, project, id)): Path<(TeamId, ProjectId, StackId)>,
) -> AppResult<Json<ViewBody>> {
	let view = super::stack(&s, user.id, team, project, id).await?;
	Ok(Json(ViewBody::from(&view)))
}

#[derive(Deserialize, Default)]
struct ExportQuery {
	/// Only this environment of the project.
	#[serde(default)]
	environment_id: Option<EnvironmentId>,
	/// Include the env files and mounted files, secrets and all.
	#[serde(default)]
	secrets: bool,
}

/// The archive as a download.
pub(crate) fn download(archive: Archive) -> Response {
	(
		[
			(header::CONTENT_TYPE, "application/gzip".to_owned()),
			(
				header::CONTENT_DISPOSITION,
				format!("attachment; filename=\"{}\"", archive.filename),
			),
			(header::CACHE_CONTROL, "no-store".to_owned()),
		],
		archive.bytes,
	)
		.into_response()
}

async fn export_project(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, project)): Path<(TeamId, ProjectId)>,
	Query(query): Query<ExportQuery>,
) -> AppResult<Response> {
	let archive = super::export(
		&s,
		user.id,
		team,
		Request {
			scope: Scope::Project {
				project,
				environment: query.environment_id,
			},
			secrets: query.secrets,
			can_reveal: user.can_reveal(),
		},
	)
	.await?;
	Ok(download(archive))
}

async fn export_team(
	State(s): State<AppState>,
	user: AuthUser,
	Path(team): Path<TeamId>,
	Query(query): Query<ExportQuery>,
) -> AppResult<Response> {
	let archive = super::export(
		&s,
		user.id,
		team,
		Request {
			scope: Scope::Team,
			secrets: query.secrets,
			can_reveal: user.can_reveal(),
		},
	)
	.await?;
	Ok(download(archive))
}
