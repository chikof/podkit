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
	RestoreRequest, Stream, UploadPartsRequest, UploadRequest, VolumeDumpRequest,
	VolumeRestoreRequest,
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
		.route("/v1/stats", get(stats))
		.route("/v1/events", get(events))
		.route("/v1/db-stats", post(db_stats))
		.route("/v1/logs/search", post(logs_search))
		.route("/v1/logs/config", put(logs_config))
		.route("/v1/logs/status", get(logs_status))
		.route("/v1/deploy", post(deploy_route))
		.route("/v1/containers", get(list_containers))
		.route("/v1/containers/{id}/start", post(start_container))
		.route("/v1/containers/{id}/stop", post(stop_container))
		.route("/v1/containers/{id}/logs", get(container_logs))
		.route("/v1/containers/{id}/health", get(container_health))
		.route("/v1/networks", get(list_networks).post(create_network))
		.route(
			"/v1/networks/{name}",
			get(get_network).delete(delete_network),
		)
		.route("/v1/networks/{name}/connect", post(connect_network))
		.route("/v1/networks/{name}/disconnect", post(disconnect_network))
		.route("/v1/services/{service}", delete(delete_service))
		.route("/v1/volumes/{name}", delete(delete_volume))
		.route("/v1/backups/dump", post(backup_dump))
		.route("/v1/backups/upload", post(backup_upload))
		.route("/v1/backups/restore", post(backup_restore))
		.route("/v1/cron/run", post(cron_run))
		.route("/v1/volume-backups/dump", post(volume_dump))
		.route("/v1/volume-backups/upload", post(volume_upload))
		.route("/v1/volume-backups/restore", post(volume_restore))
		.route("/v1/model-downloads", post(model_download_route))
		.route("/v1/probes", post(probe_route))
		.route("/v1/ingress/ensure", post(ensure_ingress))
		.route("/v1/registry-login", post(registry_login_route))
		.route("/v1/scan", post(scan_route))
		.route("/v1/scan/resolve", post(scan_resolve_route))
		.route("/v1/gateway/config", put(gateway_config))
		.route("/v1/gateway/usage/drain", post(gateway_drain))
		.route("/v1/gateway/chat", post(gateway_chat))
		.route("/v1/gateway/mcp/tools", post(gateway_mcp_tools))
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

async fn stats(State(ctx): State<Arc<Ctx>>) -> Json<proto::Stats> {
	let podman = ctx.podman();
	let graph_root = podman.graph_root().await.ok().flatten();
	Json(crate::stats::collect(graph_root.as_deref(), &ctx.data_dir, &podman).await)
}

#[derive(Deserialize)]
struct EventsQuery {
	/// Unix seconds, inclusive.
	since: i64,
	/// Unix seconds; a window longer than a day is cut to the last day.
	until: i64,
}

/// Longest window one question covers; the control plane asks every few seconds.
const MAX_EVENT_WINDOW_SECS: i64 = 86_400;

/// Restarts, OOM kills and crashes of podkit containers in a window.
async fn events(
	State(ctx): State<Arc<Ctx>>,
	Query(q): Query<EventsQuery>,
) -> Result<Json<Vec<proto::ContainerEvent>>, ApiError> {
	let since = q.since.max(q.until - MAX_EVENT_WINDOW_SECS);
	crate::events::between(&ctx.podman(), since, q.until)
		.await
		.map(Json)
		.map_err(|e| ApiError::podman("could not read container events", &e))
}

/// Connections, size and activity of database containers. A database that does not answer is
/// left out of the reply.
async fn db_stats(
	State(ctx): State<Arc<Ctx>>,
	Json(req): Json<proto::DbStatsRequest>,
) -> Json<Vec<proto::DbStat>> {
	Json(crate::dbstats::collect(&ctx.podman(), &req.targets).await)
}

/// Searches the stored container logs; see [`crate::logstore`].
async fn logs_search(
	State(ctx): State<Arc<Ctx>>,
	Json(req): Json<proto::LogSearchRequest>,
) -> Result<Json<proto::LogSearchResult>, ApiError> {
	crate::logstore::for_dir(&ctx.data_dir)
		.search(req)
		.await
		.map(Json)
		.map_err(|e| ApiError::new(StatusCode::BAD_REQUEST, e.to_string()))
}

/// How long the logs are kept and where they are forwarded.
async fn logs_config(
	State(ctx): State<Arc<Ctx>>,
	Json(config): Json<proto::LogConfig>,
) -> Result<StatusCode, ApiError> {
	crate::logstore::for_dir(&ctx.data_dir)
		.set_config(config)
		.map(|()| StatusCode::NO_CONTENT)
		.map_err(|e| ApiError::new(StatusCode::BAD_REQUEST, e.to_string()))
}

async fn logs_status(State(ctx): State<Arc<Ctx>>) -> Json<proto::LogStatus> {
	Json(crate::logstore::for_dir(&ctx.data_dir).status())
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

/// Health and state of one container, for stacks that wait on a dependency's
/// health check.
async fn container_health(
	State(ctx): State<Arc<Ctx>>,
	Path(id): Path<String>,
) -> Result<Json<proto::ContainerHealth>, ApiError> {
	check_ref(&id)?;
	let info = ctx
		.podman()
		.inspect(&id)
		.await
		.map_err(|e| ApiError::podman("inspect failed", &e))?
		.ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, format!("container {id} not found")))?;
	Ok(Json(health_response(&ctx, &id, &info).await))
}

async fn health_response(
	ctx: &Ctx,
	id: &str,
	info: &bollard::models::ContainerInspectResponse,
) -> proto::ContainerHealth {
	use bollard::models::ContainerStateStatusEnum as Status;
	let name = info
		.name
		.as_deref()
		.map_or_else(|| id.to_string(), |n| n.trim_start_matches('/').to_string());
	let state = info.state.as_ref();
	let container_state = match state.and_then(|s| s.status) {
		Some(Status::CREATED) => ContainerState::Created,
		Some(Status::RUNNING) => ContainerState::Running,
		Some(Status::PAUSED) => ContainerState::Paused,
		Some(Status::RESTARTING) => ContainerState::Restarting,
		Some(Status::EXITED) => ContainerState::Exited,
		Some(Status::DEAD) => ContainerState::Dead,
		_ => ContainerState::Unknown,
	};
	proto::ContainerHealth {
		id: info.id.clone().unwrap_or_else(|| id.to_string()),
		state: container_state,
		exit_code: matches!(
			container_state,
			ContainerState::Exited | ContainerState::Dead
		)
		.then(|| state.and_then(|s| s.exit_code))
		.flatten(),
		health: crate::podman::health_of(info),
		gave_up: ctx.supervisor.is_gave_up(&name).await,
		name,
	}
}

/// Removes a network podkit created. Podman refuses (409) while containers
/// are on it; an imported network is never removed by the agent.
async fn delete_network(
	State(ctx): State<Arc<Ctx>>,
	Path(name): Path<String>,
) -> Result<StatusCode, ApiError> {
	check_ref(&name)?;
	if !crate::networks::is_managed(&name) {
		return Err(ApiError::new(
			StatusCode::BAD_REQUEST,
			format!("{name:?} is not a podkit network"),
		));
	}
	ctx.podman()
		.remove_network(&name)
		.await
		.map_err(|e| ApiError::podman(&format!("removing network {name} failed"), &e))?;
	Ok(StatusCode::NO_CONTENT)
}

async fn list_networks(
	State(ctx): State<Arc<Ctx>>,
) -> Result<Json<Vec<proto::NetworkInfo>>, ApiError> {
	ctx.podman()
		.list_networks()
		.await
		.map(Json)
		.map_err(|e| ApiError::podman("listing networks failed", &e))
}

async fn get_network(
	State(ctx): State<Arc<Ctx>>,
	Path(name): Path<String>,
) -> Result<Json<proto::NetworkInfo>, ApiError> {
	check_ref(&name)?;
	ctx.podman()
		.inspect_network(&name)
		.await
		.map_err(|e| ApiError::podman("inspecting the network failed", &e))?
		.map(Json)
		.ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, format!("network {name} not found")))
}

async fn create_network(
	State(ctx): State<Arc<Ctx>>,
	Json(req): Json<proto::NetworkCreate>,
) -> Result<(StatusCode, Json<proto::NetworkInfo>), ApiError> {
	if !crate::networks::is_managed(&req.name) {
		return Err(ApiError::new(
			StatusCode::BAD_REQUEST,
			format!("invalid network name {:?}", req.name),
		));
	}
	let podman = ctx.podman();
	podman
		.create_network(&req.name, req.internal)
		.await
		.map_err(|e| ApiError::podman("creating the network failed", &e))?;
	let info = podman
		.inspect_network(&req.name)
		.await
		.map_err(|e| ApiError::podman("inspecting the network failed", &e))?
		.ok_or_else(|| {
			ApiError::new(
				StatusCode::INTERNAL_SERVER_ERROR,
				"the network did not appear",
			)
		})?;
	Ok((StatusCode::CREATED, Json(info)))
}

/// A podkit container: only those may be moved between networks.
fn check_member(req: &proto::NetworkMember) -> Result<(), ApiError> {
	check_ref(&req.container)?;
	if !req.container.starts_with(DEFAULT_PREFIX) {
		return Err(ApiError::new(
			StatusCode::BAD_REQUEST,
			format!("{:?} is not a podkit container", req.container),
		));
	}
	Ok(())
}

async fn connect_network(
	State(ctx): State<Arc<Ctx>>,
	Path(name): Path<String>,
	Json(req): Json<proto::NetworkMember>,
) -> Result<StatusCode, ApiError> {
	check_ref(&name)?;
	check_member(&req)?;
	if !crate::networks::valid_member_network(&name)
		|| req.aliases.iter().any(|a| !deploy_valid_alias(a))
	{
		return Err(ApiError::new(
			StatusCode::BAD_REQUEST,
			format!("cannot connect to network {name:?}"),
		));
	}
	ctx.podman()
		.connect_network(&name, &req.container, &req.aliases)
		.await
		.map_err(|e| ApiError::podman(&format!("joining network {name} failed"), &e))?;
	deploy::sync_ingress_networks(&ctx.podman()).await;
	// Traefik does not notice a container changing networks.
	if let Err(e) = crate::ingress::refresh(&ctx.podman()).await {
		tracing::warn!(error = %e, "refreshing Traefik's routes failed");
	}
	Ok(StatusCode::NO_CONTENT)
}

async fn disconnect_network(
	State(ctx): State<Arc<Ctx>>,
	Path(name): Path<String>,
	Json(req): Json<proto::NetworkMember>,
) -> Result<StatusCode, ApiError> {
	check_ref(&name)?;
	check_member(&req)?;
	// The system network is left too, by `migrate`; the host's own never.
	if !crate::networks::valid_member_network(&name) && name != proto::SYSTEM_NETWORK {
		return Err(ApiError::new(
			StatusCode::BAD_REQUEST,
			format!("cannot leave network {name:?}"),
		));
	}
	ctx.podman()
		.disconnect_network(&name, &req.container)
		.await
		.map_err(|e| ApiError::podman(&format!("leaving network {name} failed"), &e))?;
	deploy::sync_ingress_networks(&ctx.podman()).await;
	// Traefik does not notice a container changing networks.
	if let Err(e) = crate::ingress::refresh(&ctx.podman()).await {
		tracing::warn!(error = %e, "refreshing Traefik's routes failed");
	}
	Ok(StatusCode::NO_CONTENT)
}

fn deploy_valid_alias(alias: &str) -> bool {
	!alias.is_empty()
		&& alias
			.bytes()
			.all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'))
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
	deploy::sync_ingress_networks(&podman).await;

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

/// Makes sure Traefik runs with the current configuration (a Traefik of an
/// older agent is replaced) and sits on the networks of the routed containers.
async fn ensure_ingress(
	State(ctx): State<Arc<Ctx>>,
	Json(ingress): Json<proto::Ingress>,
) -> Result<StatusCode, ApiError> {
	let podman = ctx.podman();
	crate::ingress::ensure_traefik(&podman, &ingress, &ctx.podman_socket, &ctx.data_dir)
		.await
		.map_err(|e| ApiError::podman("ingress setup failed", &e))?;
	ctx.supervisor
		.add(crate::ingress::TRAEFIK_CONTAINER_NAME)
		.await;
	Ok(StatusCode::NO_CONTENT)
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

async fn registry_login_route(
	State(ctx): State<Arc<Ctx>>,
	Json(req): Json<proto::RegistryLogin>,
) -> Result<Json<proto::RegistryLoginResult>, ApiError> {
	if req.server.is_empty()
		|| req.server.len() > 255
		|| !req
			.server
			.chars()
			.all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | ':' | '[' | ']'))
	{
		return Err(ApiError::new(
			StatusCode::BAD_REQUEST,
			"invalid registry host",
		));
	}
	Ok(Json(crate::registry::check(&ctx.http, &req).await))
}

async fn scan_resolve_route(
	State(ctx): State<Arc<Ctx>>,
	Json(req): Json<proto::ScanResolveRequest>,
) -> Result<Json<proto::ScanResolveResponse>, ApiError> {
	crate::scan::resolve(&ctx.podman(), &req.image)
		.await
		.map(|image_id| Json(proto::ScanResolveResponse { image_id }))
		.map_err(|e| scan_error(&e))
}

async fn scan_route(
	State(ctx): State<Arc<Ctx>>,
	Json(req): Json<proto::ScanRequest>,
) -> Result<Json<proto::ScanResult>, ApiError> {
	let config = crate::scan::ScanConfig::new(&ctx.data_dir);
	crate::scan::run(&ctx.podman(), &config, &req)
		.await
		.map(Json)
		.map_err(|e| scan_error(&e))
}

fn scan_error(e: &crate::scan::ScanError) -> ApiError {
	use crate::scan::ScanError;
	let status = match *e {
		ScanError::ImageMissing(_) => StatusCode::NOT_FOUND,
		ScanError::Invalid(_) => StatusCode::BAD_REQUEST,
		ScanError::Failed(_) => StatusCode::INTERNAL_SERVER_ERROR,
	};
	ApiError::new(status, e.to_string())
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

async fn cron_run(State(ctx): State<Arc<Ctx>>, Json(req): Json<proto::CronRunRequest>) -> Response {
	ndjson(move |tx| crate::cron::run(ctx, req, tx))
}

async fn volume_dump(State(ctx): State<Arc<Ctx>>, Json(req): Json<VolumeDumpRequest>) -> Response {
	ndjson(move |tx| crate::volume_backup::dump(ctx, req, tx))
}

async fn volume_upload(
	State(ctx): State<Arc<Ctx>>,
	Json(req): Json<UploadPartsRequest>,
) -> Response {
	ndjson(move |tx| crate::volume_backup::upload_parts(ctx, req, tx))
}

async fn volume_restore(
	State(ctx): State<Arc<Ctx>>,
	Json(req): Json<VolumeRestoreRequest>,
) -> Response {
	ndjson(move |tx| crate::volume_backup::restore(ctx, req, tx))
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

async fn gateway_mcp_tools(
	State(ctx): State<Arc<Ctx>>,
	Json(req): Json<proto::GatewayMcpToolsRequest>,
) -> Response {
	if let Err(message) = crate::gateway::mcp::validate(&req) {
		return ApiError::new(StatusCode::BAD_REQUEST, message).into_response();
	}
	let podman = ctx.podman();
	ndjson(move |tx| crate::gateway::mcp::tools(podman, req, tx))
}
