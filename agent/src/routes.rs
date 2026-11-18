//! The agent's HTTP API. Errors are non-2xx with an [`ErrorBody`]; streams are
//! `application/x-ndjson` of [`Event`], ending with one terminal event.

use std::convert::Infallible;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post, put};
use axum::{Json, Router};
use bollard::container::LogOutput;
use bollard::models::ContainerSummaryStateEnum;
use bytes::Bytes;
use futures_util::{StreamExt, stream};
use proto::{
	ContainerInfo, ContainerState, DeploySpec, DumpRequest, ErrorBody, Event, Health,
	RestoreRequest, Stream, UploadRequest,
};
use serde::Deserialize;
use tokio::sync::mpsc;
use tracing::warn;

use crate::podman::{Podman, PodmanError, summary_name};
use crate::{Ctx, backup, deploy};

/// Events buffered between a job and the response body.
const STREAM_CAPACITY: usize = 256;
const DEFAULT_PREFIX: &str = "podkit-";

/// Builds the router for all `/v1` routes.
pub fn router(ctx: Arc<Ctx>) -> Router {
	Router::new()
		.route("/v1/health", get(health))
		.route("/v1/hardware", get(hardware))
		.route("/v1/deploy", post(deploy_route))
		.route("/v1/containers", get(list_containers))
		.route("/v1/containers/{id}/start", post(start_container))
		.route("/v1/containers/{id}/stop", post(stop_container))
		.route("/v1/containers/{id}/logs", get(container_logs))
		.route("/v1/services/{service}", delete(delete_service))
		.route("/v1/volumes/{name}", delete(delete_volume))
		.route("/v1/backups/dump", post(backup_dump))
		.route("/v1/backups/upload", post(backup_upload))
		.route("/v1/backups/restore", post(backup_restore))
		.route("/v1/model-downloads", post(model_download_route))
		.route("/v1/probes", post(probe_route))
		.route("/v1/gateway/config", put(gateway_config))
		.route("/v1/gateway/usage/drain", post(gateway_drain))
		.route("/v1/gateway/chat", post(gateway_chat))
		.with_state(ctx)
}

struct ApiError {
	status: StatusCode,
	message: String,
}

impl ApiError {
	fn new(status: StatusCode, message: impl Into<String>) -> Self {
		Self {
			status,
			message: message.into(),
		}
	}

	fn podman(context: &str, error: &PodmanError) -> Self {
		let status = match error.status() {
			Some(404) => StatusCode::NOT_FOUND,
			Some(409) => StatusCode::CONFLICT,
			_ => StatusCode::INTERNAL_SERVER_ERROR,
		};
		Self::new(status, format!("{context}: {error}"))
	}
}

impl IntoResponse for ApiError {
	fn into_response(self) -> Response {
		(
			self.status,
			Json(ErrorBody {
				error: self.message,
			}),
		)
			.into_response()
	}
}

/// Container, service and volume names end up in Podman API paths.
fn check_ref(s: &str) -> Result<(), ApiError> {
	let ok = !s.is_empty()
		&& s.len() <= 255
		&& s.bytes()
			.all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'));
	if ok {
		Ok(())
	} else {
		Err(ApiError::new(
			StatusCode::BAD_REQUEST,
			format!("invalid name {s:?}"),
		))
	}
}

/// Runs `work` with an event sender and streams what it sends as NDJSON.
/// Dropping the response body (client disconnect) cancels the work.
fn ndjson<F, Fut>(work: F) -> Response
where
	F: FnOnce(mpsc::Sender<Event>) -> Fut,
	Fut: Future<Output = ()> + Send + 'static,
{
	let (tx, rx) = mpsc::channel::<Event>(STREAM_CAPACITY);
	let job = work(tx.clone());
	tokio::spawn(async move {
		tokio::select! {
			() = tx.closed() => {}
			() = job => {}
		}
	});

	let body = stream::unfold(rx, |mut rx| async move {
		let event = rx.recv().await?;
		let mut line = serde_json::to_vec(&event).unwrap_or_default();
		line.push(b'\n');
		Some((Ok::<_, Infallible>(Bytes::from(line)), rx))
	});
	(
		[(header::CONTENT_TYPE, "application/x-ndjson")],
		Body::from_stream(body),
	)
		.into_response()
}

async fn health(State(ctx): State<Arc<Ctx>>) -> Result<Json<Health>, ApiError> {
	let podman_version = ctx
		.podman()
		.version()
		.await
		.map_err(|e| ApiError::podman("podman unreachable", &e))?;
	Ok(Json(Health {
		version: env!("CARGO_PKG_VERSION").to_string(),
		podman_version,
	}))
}

/// Never fails: parts the host cannot report are empty.
async fn hardware(State(ctx): State<Arc<Ctx>>) -> Json<proto::Hardware> {
	let graph_root = ctx.podman().graph_root().await.ok().flatten();
	Json(crate::hardware::collect(graph_root.as_deref(), &ctx.data_dir).await)
}

async fn deploy_route(State(ctx): State<Arc<Ctx>>, Json(spec): Json<DeploySpec>) -> Response {
	ndjson(move |tx| deploy::run(ctx, spec, tx))
}

#[derive(Deserialize)]
struct ListQuery {
	prefix: Option<String>,
}

fn container_state(state: Option<ContainerSummaryStateEnum>) -> ContainerState {
	match state {
		Some(ContainerSummaryStateEnum::CREATED) => ContainerState::Created,
		Some(ContainerSummaryStateEnum::RUNNING) => ContainerState::Running,
		Some(ContainerSummaryStateEnum::PAUSED) => ContainerState::Paused,
		Some(ContainerSummaryStateEnum::RESTARTING) => ContainerState::Restarting,
		Some(ContainerSummaryStateEnum::EXITED) => ContainerState::Exited,
		Some(ContainerSummaryStateEnum::DEAD) => ContainerState::Dead,
		_ => ContainerState::Unknown,
	}
}

async fn list_containers(
	State(ctx): State<Arc<Ctx>>,
	Query(query): Query<ListQuery>,
) -> Result<Json<Vec<ContainerInfo>>, ApiError> {
	let prefix = query.prefix.unwrap_or_else(|| DEFAULT_PREFIX.to_string());
	let podman = ctx.podman();
	let summaries = podman
		.list(&prefix)
		.await
		.map_err(|e| ApiError::podman("listing containers failed", &e))?;

	let mut containers = Vec::with_capacity(summaries.len());
	for summary in &summaries {
		let (Some(id), Some(name)) = (&summary.id, summary_name(summary)) else {
			continue;
		};
		let state = container_state(summary.state);
		let exit_code = if matches!(state, ContainerState::Exited | ContainerState::Dead) {
			podman
				.inspect(id)
				.await
				.ok()
				.flatten()
				.and_then(|info| info.state)
				.and_then(|s| s.exit_code)
		} else {
			None
		};
		containers.push(ContainerInfo {
			id: id.clone(),
			name: name.to_string(),
			state,
			exit_code,
			gave_up: ctx.supervisor.is_gave_up(name).await,
		});
	}
	Ok(Json(containers))
}

/// The container's name for a name-or-id; `None` when it does not exist.
async fn resolve_name(podman: &Podman, id: &str) -> Result<Option<String>, ApiError> {
	let info = podman
		.inspect(id)
		.await
		.map_err(|e| ApiError::podman("inspect failed", &e))?;
	Ok(info.map(|i| {
		i.name
			.map_or_else(|| id.to_string(), |n| n.trim_start_matches('/').to_string())
	}))
}

async fn start_container(
	State(ctx): State<Arc<Ctx>>,
	Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
	check_ref(&id)?;
	let podman = ctx.podman();
	let name = resolve_name(&podman, &id)
		.await?
		.ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, format!("container {id} not found")))?;
	ctx.supervisor.add(&name).await;
	podman
		.start(&name)
		.await
		.map_err(|e| ApiError::podman("container start failed", &e))?;
	Ok(StatusCode::NO_CONTENT)
}

async fn stop_container(
	State(ctx): State<Arc<Ctx>>,
	Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
	check_ref(&id)?;
	let podman = ctx.podman();
	let Some(name) = resolve_name(&podman, &id).await? else {
		ctx.supervisor.remove(&id).await;
		return Ok(StatusCode::NO_CONTENT);
	};
	// Out of the desired set first, or the supervisor would restart it.
	ctx.supervisor.remove(&name).await;
	podman
		.stop(&name)
		.await
		.map_err(|e| ApiError::podman("container stop failed", &e))?;
	Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct LogsQuery {
	tail: Option<u32>,
	follow: Option<bool>,
}

async fn container_logs(
	State(ctx): State<Arc<Ctx>>,
	Path(id): Path<String>,
	Query(query): Query<LogsQuery>,
) -> Result<Response, ApiError> {
	check_ref(&id)?;
	let podman = ctx.podman();
	if resolve_name(&podman, &id).await?.is_none() {
		return Err(ApiError::new(
			StatusCode::NOT_FOUND,
			format!("container {id} not found"),
		));
	}
	let follow = query.follow.unwrap_or(false);
	Ok(ndjson(move |tx| {
		stream_logs(podman, id, query.tail, follow, tx)
	}))
}

async fn stream_logs(
	podman: Podman,
	id: String,
	tail: Option<u32>,
	follow: bool,
	tx: mpsc::Sender<Event>,
) {
	let mut logs = std::pin::pin!(podman.logs(&id, tail, follow));
	while let Some(item) = logs.next().await {
		let output = match item {
			Ok(output) => output,
			Err(e) => {
				let _ = tx
					.send(Event::Failed {
						message: e.to_string(),
					})
					.await;
				return;
			}
		};
		let stream = match output {
			LogOutput::StdErr { .. } => Stream::Stderr,
			_ => Stream::Stdout,
		};
		for line in String::from_utf8_lossy(output.as_ref()).lines() {
			let event = Event::Log {
				stream,
				line: line.to_string(),
			};
			if tx.send(event).await.is_err() {
				return;
			}
		}
	}
	let _ = tx.send(Event::Done).await;
}

async fn delete_service(
	State(ctx): State<Arc<Ctx>>,
	Path(service): Path<String>,
) -> Result<StatusCode, ApiError> {
	check_ref(&service)?;
	let podman = ctx.podman();

	let containers = podman
		.service_containers(&service)
		.await
		.map_err(|e| ApiError::podman("listing containers failed", &e))?;
	for name in containers.iter().filter_map(summary_name) {
		ctx.supervisor.remove(name).await;
		podman
			.remove(name)
			.await
			.map_err(|e| ApiError::podman(&format!("removing {name} failed"), &e))?;
	}

	match podman.service_images(&service).await {
		Ok(images) => {
			for tag in images.iter().flat_map(|i| &i.repo_tags) {
				if tag.starts_with(&format!("podkit-{service}:"))
					&& let Err(e) = podman.remove_image(tag).await
				{
					warn!(image = %tag, error = %e, "image removal failed");
				}
			}
		}
		Err(e) => warn!(service, error = %e, "listing images failed"),
	}
	Ok(StatusCode::NO_CONTENT)
}

async fn delete_volume(
	State(ctx): State<Arc<Ctx>>,
	Path(name): Path<String>,
) -> Result<StatusCode, ApiError> {
	check_ref(&name)?;
	match ctx.podman().remove_volume(&name).await {
		Ok(()) => Ok(StatusCode::NO_CONTENT),
		Err(e) if e.is_not_found() => Ok(StatusCode::NO_CONTENT),
		Err(e) => Err(ApiError::podman(
			&format!("removing volume {name} failed"),
			&e,
		)),
	}
}

async fn probe_route(
	State(ctx): State<Arc<Ctx>>,
	Json(req): Json<proto::ProbeRequest>,
) -> Result<Json<proto::ProbeResponse>, ApiError> {
	check_ref(&req.container_id)?;
	crate::probe::validate(&req).map_err(|m| ApiError::new(StatusCode::BAD_REQUEST, m))?;
	crate::probe::run(&ctx.podman(), &req)
		.await
		.map(Json)
		.map_err(|m| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, m))
}

async fn model_download_route(
	State(ctx): State<Arc<Ctx>>,
	Json(req): Json<proto::ModelDownloadRequest>,
) -> Response {
	ndjson(move |tx| crate::model_download::run(ctx, req, tx))
}

async fn backup_dump(State(ctx): State<Arc<Ctx>>, Json(req): Json<DumpRequest>) -> Response {
	ndjson(move |tx| backup::dump(ctx, req, tx))
}

async fn backup_upload(State(ctx): State<Arc<Ctx>>, Json(req): Json<UploadRequest>) -> Response {
	ndjson(move |tx| backup::upload(ctx, req, tx))
}

async fn backup_restore(State(ctx): State<Arc<Ctx>>, Json(req): Json<RestoreRequest>) -> Response {
	ndjson(move |tx| backup::restore(ctx, req, tx))
}

async fn gateway_config(
	State(ctx): State<Arc<Ctx>>,
	Json(config): Json<proto::GatewayConfig>,
) -> Result<Json<serde_json::Value>, ApiError> {
	crate::gateway::sidecar::apply(&ctx.podman(), &ctx.supervisor, &ctx.data_dir, &config)
		.await
		.map(|()| Json(serde_json::json!({})))
		.map_err(|m| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, m))
}

async fn gateway_drain(State(ctx): State<Arc<Ctx>>) -> Json<Vec<proto::GatewayUsage>> {
	Json(crate::gateway::sidecar::drain_usage(&crate::gateway::sidecar::dir(&ctx.data_dir)).await)
}

async fn gateway_chat(
	State(ctx): State<Arc<Ctx>>,
	Json(req): Json<proto::GatewayChatRequest>,
) -> Response {
	if let Err(message) = crate::gateway::chat::validate(&req) {
		return ApiError::new(StatusCode::BAD_REQUEST, message).into_response();
	}
	let podman = ctx.podman();
	ndjson(move |tx| crate::gateway::chat::run(podman, req, tx))
}
