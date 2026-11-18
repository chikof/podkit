//! HTTP handlers of the stacks slice. They only translate: the rules live in
//! `service`.

use std::collections::BTreeMap;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::response::sse::{KeepAlive, Sse};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use super::service::{self, CreateInput, StackDetail, UpdateInput};
use super::{ServiceStatus, Stack, StackService, StackStatus};
use crate::applications::routes::log_events;
use crate::auth::AuthUser;
use crate::environments::EnvironmentFilter;
use crate::error::AppResult;
use crate::ids::{EnvironmentId, JobId, ProjectId, ServerId, StackId, StackServiceId, TeamId};
use crate::state::AppState;

pub fn router() -> Router<AppState> {
	Router::new()
		.route("/", post(create).get(list))
		.route("/{stack_id}", get(get_one).patch(update).delete(remove))
		.route("/{stack_id}/deploy", post(deploy))
		.route("/{stack_id}/stop", post(stop))
		.route("/{stack_id}/services/{service}/logs", get(logs))
		.route(
			"/{stack_id}/services/{service}/restart",
			post(restart_service),
		)
		.route(
			"/{stack_id}/services/{service}/domain",
			put(set_domain).delete(clear_domain),
		)
}

#[derive(Deserialize)]
struct CreateStackRequest {
	name: String,
	compose_yaml: String,
	server_id: ServerId,
	/// Variables the compose file interpolates. Write-only.
	#[serde(default)]
	env: BTreeMap<String, String>,
	#[serde(default)]
	environment_id: Option<EnvironmentId>,
	/// Queue the first deploy right away.
	#[serde(default)]
	deploy: bool,
}

/// Every field is optional; omitted ones stay as they are.
#[derive(Deserialize)]
struct UpdateStackRequest {
	#[serde(default)]
	name: Option<String>,
	#[serde(default)]
	compose_yaml: Option<String>,
	/// Variables to set (added or replaced). Write-only.
	#[serde(default)]
	env: BTreeMap<String, String>,
	/// Names of variables to remove.
	#[serde(default)]
	unset_env: Vec<String>,
}

#[derive(Deserialize)]
struct RemoveParams {
	#[serde(default)]
	purge_volumes: bool,
}

#[derive(Deserialize)]
struct LogsParams {
	#[serde(default = "default_tail")]
	tail: u32,
	#[serde(default = "default_follow")]
	follow: bool,
}

fn default_tail() -> u32 {
	200
}

fn default_follow() -> bool {
	true
}

#[derive(Deserialize)]
struct DomainRequest {
	hostname: String,
	/// Container port behind the domain; optional when the service exposes one.
	#[serde(default)]
	port: Option<u16>,
}

/// Never carries variable values; `GET` adds the names and the services.
#[derive(Serialize)]
struct StackResponse {
	id: StackId,
	project_id: ProjectId,
	environment_id: EnvironmentId,
	server_id: ServerId,
	name: String,
	slug: String,
	source: String,
	status: StackStatus,
	status_message: Option<String>,
	/// Unix seconds.
	created_at: i64,
	updated_at: i64,
	#[serde(skip_serializing_if = "Option::is_none")]
	job_id: Option<JobId>,
	#[serde(skip_serializing_if = "Option::is_none")]
	detail: Option<DetailResponse>,
}

#[derive(Serialize)]
struct DetailResponse {
	services: Vec<ServiceResponse>,
	/// Names of the variables set on the stack; values are never returned.
	env_keys: Vec<String>,
	/// Variables the compose file uses that the stack does not set.
	missing_vars: Vec<String>,
	/// Keys the translation ignores or approximates.
	warnings: Vec<super::compose::Warning>,
	latest_job_id: Option<JobId>,
}

#[derive(Serialize)]
struct ServiceResponse {
	id: StackServiceId,
	name: String,
	image: String,
	status: ServiceStatus,
	status_message: Option<String>,
	/// `starting`, `healthy` or `unhealthy`; absent without a health check.
	health: Option<String>,
	container_id: Option<String>,
	domain: Option<String>,
	domain_port: Option<i32>,
}

impl From<&Stack> for StackResponse {
	fn from(stack: &Stack) -> Self {
		Self {
			id: stack.id,
			project_id: stack.project_id,
			environment_id: stack.environment_id,
			server_id: stack.server_id,
			name: stack.name.clone(),
			slug: stack.slug.clone(),
			source: stack.source.clone(),
			status: stack.status,
			status_message: stack.status_message.clone(),
			created_at: stack.created_at.unix_timestamp(),
			updated_at: stack.updated_at.unix_timestamp(),
			job_id: None,
			detail: None,
		}
	}
}

impl From<&StackService> for ServiceResponse {
	fn from(s: &StackService) -> Self {
		Self {
			id: s.id,
			name: s.name.clone(),
			image: s.image.clone(),
			status: s.status,
			status_message: s.status_message.clone(),
			health: s.health.clone(),
			container_id: s.container_id.clone(),
			domain: s.domain.clone(),
			domain_port: s.domain_port,
		}
	}
}

impl From<StackDetail> for StackResponse {
	fn from(d: StackDetail) -> Self {
		let mut response = Self::from(&d.stack);
		response.detail = Some(DetailResponse {
			services: d.services.iter().map(ServiceResponse::from).collect(),
			env_keys: d.env_keys,
			missing_vars: d.missing_vars,
			warnings: d.warnings,
			latest_job_id: d.job.map(|(id, _)| id),
		});
		response
	}
}

fn accepted(stack: &Stack, job: Option<JobId>) -> (StatusCode, Json<StackResponse>) {
	let mut response = StackResponse::from(stack);
	response.job_id = job;
	(StatusCode::ACCEPTED, Json(response))
}

/// Creates a stack from a compose file. A file that uses anything outside the
/// supported subset is refused with 422 and the list of offending keys. With
/// `"deploy": true` the first deploy is queued (202); otherwise 201.
/// Caller must hold `create stack` on the team.
async fn create(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, project)): Path<(TeamId, ProjectId)>,
	Json(body): Json<CreateStackRequest>,
) -> AppResult<(StatusCode, Json<StackResponse>)> {
	let input = CreateInput {
		name: body.name,
		compose_yaml: body.compose_yaml,
		env: body.env,
		server_id: body.server_id,
		environment_id: body.environment_id,
		source: None,
		deploy: body.deploy,
	};
	let (stack, job) = service::create(&s, user.id, team, project, input).await?;
	let status = if job.is_some() {
		StatusCode::ACCEPTED
	} else {
		StatusCode::CREATED
	};
	let mut response = StackResponse::from(&stack);
	response.job_id = job;
	Ok((status, Json(response)))
}

/// Lists the stacks of an environment (the first by default). Caller must
/// hold `read stack`.
async fn list(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, project)): Path<(TeamId, ProjectId)>,
	Query(filter): Query<EnvironmentFilter>,
) -> AppResult<Json<Vec<StackResponse>>> {
	let rows = service::list(&s.db, user.id, team, project, filter.environment_id).await?;
	Ok(Json(rows.iter().map(StackResponse::from).collect()))
}

/// One stack with its services (state refreshed from the agent when it
/// answers) and the names of its variables, never their values. Caller must
/// hold `read stack`.
async fn get_one(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, project, id)): Path<(TeamId, ProjectId, StackId)>,
) -> AppResult<Json<StackResponse>> {
	let detail = service::detail(&s, user.id, team, project, id).await?;
	Ok(Json(detail.into()))
}

/// Changes the name, compose file or variables; nothing is redeployed until
/// `/deploy`. Caller must hold `update stack`.
async fn update(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, project, id)): Path<(TeamId, ProjectId, StackId)>,
	Json(body): Json<UpdateStackRequest>,
) -> AppResult<Json<StackResponse>> {
	let input = UpdateInput {
		name: body.name,
		compose_yaml: body.compose_yaml,
		env_set: body.env,
		env_unset: body.unset_env,
	};
	service::update(&s, user.id, team, project, id, input).await?;
	let detail = service::detail(&s, user.id, team, project, id).await?;
	Ok(Json(detail.into()))
}

/// Queues a deploy: services whose translated spec changed are replaced, the
/// rest keep running. Caller must hold `update stack`.
async fn deploy(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, project, id)): Path<(TeamId, ProjectId, StackId)>,
) -> AppResult<(StatusCode, Json<StackResponse>)> {
	let (stack, job) = service::deploy(&s, user.id, team, project, id).await?;
	Ok(accepted(&stack, Some(job)))
}

/// Stops every container of the stack; the agent will not restart them.
/// Caller must hold `update stack`.
async fn stop(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, project, id)): Path<(TeamId, ProjectId, StackId)>,
) -> AppResult<(StatusCode, Json<StackResponse>)> {
	let (stack, job) = service::stop(&s, user.id, team, project, id).await?;
	Ok(accepted(&stack, Some(job)))
}

/// Marks the stack `deleting` and queues the removal of its containers and
/// network; `?purge_volumes=true` also deletes its volumes. The row
/// disappears when the job finishes. Caller must hold `delete stack`.
async fn remove(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, project, id)): Path<(TeamId, ProjectId, StackId)>,
	Query(params): Query<RemoveParams>,
) -> AppResult<(StatusCode, Json<StackResponse>)> {
	let (stack, job) =
		service::remove(&s, user.id, team, project, id, params.purge_volumes).await?;
	Ok(accepted(&stack, Some(job)))
}

/// Recreates one service whether or not it changed. Caller must hold
/// `update stack`.
async fn restart_service(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, project, id, name)): Path<(TeamId, ProjectId, StackId, String)>,
) -> AppResult<impl IntoResponse> {
	let (svc, job) = service::restart_service(&s, user.id, team, project, id, &name).await?;
	Ok((
		StatusCode::ACCEPTED,
		Json(serde_json::json!({ "service": ServiceResponse::from(&svc), "job_id": job })),
	))
}

/// Streams one service's stdout/stderr as server-sent events, exactly like
/// application runtime logs. `?tail=` is capped at 5000 lines. Caller must
/// hold `read stack`.
async fn logs(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, project, id, name)): Path<(TeamId, ProjectId, StackId, String)>,
	Query(params): Query<LogsParams>,
) -> AppResult<impl IntoResponse> {
	let events = service::service_logs(
		&s,
		user.id,
		team,
		project,
		id,
		&name,
		params.tail,
		params.follow,
	)
	.await?;
	Ok(Sse::new(log_events(events, s.shutdown.clone())).keep_alive(KeepAlive::default()))
}

/// Routes a hostname to a service; takes effect on the next deploy. Caller
/// must hold `update stack`.
async fn set_domain(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, project, id, name)): Path<(TeamId, ProjectId, StackId, String)>,
	Json(body): Json<DomainRequest>,
) -> AppResult<Json<ServiceResponse>> {
	let svc = service::set_domain(
		&s,
		user.id,
		team,
		project,
		id,
		&name,
		&body.hostname,
		body.port,
	)
	.await?;
	Ok(Json(ServiceResponse::from(&svc)))
}

/// Stops routing a hostname to a service; takes effect on the next deploy.
/// Caller must hold `update stack`.
async fn clear_domain(
	State(s): State<AppState>,
	user: AuthUser,
	Path((team, project, id, name)): Path<(TeamId, ProjectId, StackId, String)>,
) -> AppResult<Json<ServiceResponse>> {
	let svc = service::clear_domain(&s, user.id, team, project, id, &name).await?;
	Ok(Json(ServiceResponse::from(&svc)))
}
