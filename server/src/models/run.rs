//! Model jobs: start (deploy the engine container, then wait until it has
//! loaded the model), stop and remove. Every `Err` is mapped onto the model
//! row by the worker (`jobs::fail_target`), so success paths only move the row
//! forward.

use std::time::{Duration, Instant};

use axum::http::Method;
use futures_util::StreamExt;
use proto::{
	DeploySpec, Event, GpuDevices, GpuRequest, GpuVendor, Ingress, ProbeRequest, ProbeResponse,
	Source, Strategy, Stream, VolumeMount,
};
use tokio_util::sync::CancellationToken;
use tracing::warn;

use super::engines::{self, Format, LaunchInput, MODELS_MOUNT};
use super::{Model, ModelStatus};
use crate::agents::AgentClient;
use crate::error::ServerError;
use crate::ids::ModelId;
use crate::jobs::{Job, JobFailure, JobKind};
use crate::logs::JobLog;
use crate::servers::{self, Server};
use crate::state::AppState;
use crate::{model_files, models};

const LOST_CONNECTION: &str = "lost connection to server agent";
/// Lines of the engine's output kept in the error of a model that never loaded.
const ERROR_TAIL_LINES: u32 = 20;
const MAX_ERROR_CHARS: usize = 4000;
/// Seconds one readiness request may take.
const PROBE_TIMEOUT_S: u32 = 5;

fn failed(message: impl Into<String>) -> JobFailure {
	JobFailure::Failed(message.into())
}

fn db_failure(e: &sqlx::Error) -> JobFailure {
	tracing::error!("model job database error: {e}");
	failed("database error")
}

/// Keeps the agent's own client-facing messages and hides the rest.
fn agent_failure(e: ServerError) -> JobFailure {
	match e {
		ServerError::Conflict(m) | ServerError::Validation(m) => failed(m),
		e => {
			warn!("agent request failed: {e}");
			failed("server agent unavailable")
		}
	}
}

/// Runs one Model* job.
///
/// # Errors
/// [`JobFailure::Cancelled`] when `cancel` fires, otherwise
/// [`JobFailure::Failed`] with a message safe to show the model's owner.
pub async fn run(
	state: &AppState,
	job: &Job,
	cancel: &CancellationToken,
	log: &mut JobLog,
) -> Result<(), JobFailure> {
	let model = models::find_by_id(&state.db, ModelId(job.target_id))
		.await
		.map_err(|e| db_failure(&e))?
		.ok_or_else(|| failed("model not found"))?;
	if model.status == ModelStatus::Removing && job.kind != JobKind::ModelRemove {
		return Err(failed("model is being removed"));
	}

	let server = servers::find(&state.db, model.server_id)
		.await
		.map_err(|e| db_failure(&e))?
		.ok_or_else(|| failed("target server not found"))?;
	let client = state.agents.client(state, &server).await.map_err(|e| {
		warn!(server = %server.id, "cannot reach server agent: {e}");
		match e {
			ServerError::ServerNotReady => failed(e.to_string()),
			_ => failed("server agent unavailable"),
		}
	})?;

	match job.kind {
		JobKind::ModelStart => start(state, job, cancel, log, &model, &server, &client).await,
		JobKind::ModelStop => stop(state, log, &model, &client).await,
		JobKind::ModelRemove => remove(state, log, &model, &client).await,
		_ => Err(failed("not a model job")),
	}
}

async fn start(
	state: &AppState,
	job: &Job,
	cancel: &CancellationToken,
	log: &mut JobLog,
	model: &Model,
	server: &Server,
	client: &AgentClient,
) -> Result<(), JobFailure> {
	let file = model_files::find_by_id(&state.db, model.model_files_id)
		.await
		.map_err(|e| db_failure(&e))?
		.ok_or_else(|| failed("model files not found"))?;
	// Leaves `running` so a status sync does not flag the container this deploy replaces.
	models::set_status(&state.db, model.id, ModelStatus::Starting, None)
		.await
		.map_err(|e| db_failure(&e))?;

	let engine = engines::spec(model.engine);
	let vendor = model.gpu.then(|| {
		engine
			.gpu_vendor
			.or_else(|| servers::gpu_vendors(server).first().copied())
			.unwrap_or(GpuVendor::Nvidia)
	});
	let spec = build_spec(
		model,
		&file.volume_name,
		vendor,
		Ingress {
			http_port: state.config.ingress_port,
			https_port: state.config.https_port,
			acme_email: state.config.acme_email.clone(),
		},
		job.id.0,
	);

	let container_id = deploy(state, cancel, log, model, client, &spec).await?;

	let timeout = state.config.model_ready_timeout.unwrap_or_else(|| {
		Duration::from_secs(engine.ready_timeout_s(u64::try_from(file.total_bytes).unwrap_or(0)))
	});
	wait_ready(state, cancel, log, model, &container_id, client, timeout).await
}

async fn deploy(
	state: &AppState,
	cancel: &CancellationToken,
	log: &mut JobLog,
	model: &Model,
	client: &AgentClient,
	spec: &DeploySpec,
) -> Result<String, JobFailure> {
	let mut events = client
		.stream(Method::POST, "/v1/deploy", Some(spec))
		.await
		.map_err(|e| {
			warn!(
				model = model.id.0,
				"failed to start the engine deploy stream: {e}"
			);
			agent_failure(e)
		})?;
	loop {
		let event = tokio::select! {
			() = cancel.cancelled() => return Err(JobFailure::Cancelled),
			event = events.next() => event,
		};
		match event {
			Some(Ok(Event::Log { stream, line })) => log.push(stream, &line).await,
			Some(Ok(Event::Deployed { container_id, .. })) => {
				sqlx::query(
					"UPDATE models SET container_id = $2, updated_at = NOW() WHERE id = $1",
				)
				.bind(model.id)
				.bind(&container_id)
				.execute(&state.db)
				.await
				.map_err(|e| db_failure(&e))?;
				return Ok(container_id);
			}
			Some(Ok(Event::Failed { message })) => return Err(JobFailure::Failed(message)),
			Some(Ok(_)) => {}
			Some(Err(e)) => {
				warn!(model = model.id.0, "engine deploy stream failed: {e}");
				return Err(failed(LOST_CONNECTION));
			}
			None => return Err(failed(LOST_CONNECTION)),
		}
	}
}

/// The format a model path points at, for the engines that care.
fn format_of(model_path: &str) -> Format {
	engines::format_of_path(model_path).unwrap_or(Format::Safetensors)
}

/// The agent's deploy request for `model`. The container has an internal alias
/// and nothing else: no routing, no published ports, so nothing reaches it
/// from outside its server's `podkit` network.
fn build_spec(
	model: &Model,
	files_volume: &str,
	vendor: Option<GpuVendor>,
	ingress: Ingress,
	instance_id: i64,
) -> DeploySpec {
	let engine = engines::spec(model.engine);
	let launch = engine.launch(&LaunchInput {
		model_path: &model.model_path,
		format: format_of(&model.model_path),
		served_name: &model.served_name,
		context_size: u32::try_from(model.context_size).unwrap_or(0),
		gpu: model.gpu,
		extra_args: &model.extra_args.0,
	});
	let mut volumes = vec![VolumeMount {
		name: files_volume.to_owned(),
		path: MODELS_MOUNT.to_owned(),
		read_only: true,
	}];
	if let Some(dir) = engine.state_dir {
		volumes.push(VolumeMount {
			name: model.state_volume(),
			path: dir.to_owned(),
			read_only: false,
		});
	}
	DeploySpec {
		service: model.service(),
		instance_id,
		source: Source::Image {
			reference: model.image.clone(),
			pull: engine.pull,
		},
		env: launch.env,
		command: Some(launch.command),
		container_port: None,
		routing: None,
		network_aliases: vec![model.alias()],
		volumes,
		published_ports: Vec::new(),
		memory_bytes: model.memory_limit_mb.map(|mb| i64::from(mb) * 1024 * 1024),
		cpu_cores: None,
		strategy: Strategy::Recreate,
		ingress,
		gpus: vendor.map(|vendor| GpuRequest {
			vendor,
			devices: GpuDevices::All,
		}),
		entrypoint: launch.entrypoint,
		shm_bytes: engine.shm_bytes,
		stop_timeout_s: Some(engine.stop_grace_s),
		networks: Vec::new(),
		options: proto::ContainerOptions::default(),
	}
}

/// Polls the engine's readiness check until it answers, the container exits,
/// or `timeout` passes. On the last two the last lines of the container's
/// output go into the error, and the container is stopped so a model that
/// never loaded does not keep holding memory.
async fn wait_ready(
	state: &AppState,
	cancel: &CancellationToken,
	log: &mut JobLog,
	model: &Model,
	container_id: &str,
	client: &AgentClient,
	timeout: Duration,
) -> Result<(), JobFailure> {
	let engine = engines::spec(model.engine);
	let (method, path, body) = engine.ready_request(&model.served_name);
	let request = ProbeRequest {
		container_id: container_id.to_owned(),
		host: model.alias(),
		port: u16::try_from(model.port).unwrap_or(0),
		method: method.to_owned(),
		path: path.to_owned(),
		body,
		timeout_s: PROBE_TIMEOUT_S,
	};
	log.push(
		Stream::System,
		&format!(
			"waiting for the engine to load the model ({method} {path}, up to {}s)",
			timeout.as_secs()
		),
	)
	.await;

	let started = Instant::now();
	let mut logged = String::new();
	loop {
		let last = match client
			.json::<ProbeResponse>(Method::POST, "/v1/probes", Some(&request))
			.await
		{
			Ok(r) if !r.running => {
				let tail = log_tail(client, container_id).await;
				return Err(failed(cap(&format!(
					"the engine exited while loading the model; last log lines:\n{tail}"
				))));
			}
			Ok(ProbeResponse {
				status: Some(200..=299),
				..
			}) => {
				log.push(
					Stream::System,
					&format!("model loaded after {}s", started.elapsed().as_secs()),
				)
				.await;
				return mark_running(state, model).await;
			}
			Ok(r) => match r.status {
				Some(status) => format!("HTTP {status}: {}", first_line(&r.detail)),
				None => format!("no answer: {}", first_line(&r.detail)),
			},
			Err(e) => {
				warn!(model = model.id.0, "readiness probe failed: {e}");
				"the agent could not run the check".to_owned()
			}
		};
		if last != logged {
			log.push(Stream::System, &format!("not ready yet: {last}"))
				.await;
			logged.clone_from(&last);
		}

		if started.elapsed() >= timeout {
			let tail = log_tail(client, container_id).await;
			if let Err(e) = client
				.send(Method::POST, &format!("/v1/containers/{container_id}/stop"))
				.await
			{
				warn!(
					model = model.id.0,
					"stopping the unready engine failed: {e}"
				);
			}
			return Err(failed(cap(&format!(
				"the engine did not load the model within {}s (last check: {last}); last log \
				lines:\n{tail}",
				timeout.as_secs()
			))));
		}
		tokio::select! {
			() = cancel.cancelled() => return Err(JobFailure::Cancelled),
			() = tokio::time::sleep(state.config.model_ready_interval) => {}
		}
	}
}

async fn mark_running(state: &AppState, model: &Model) -> Result<(), JobFailure> {
	// A removal or stop requested meanwhile wins.
	sqlx::query(
		"UPDATE models SET status = 'running', error = NULL, updated_at = NOW()
		WHERE id = $1 AND status = 'starting'",
	)
	.bind(model.id)
	.execute(&state.db)
	.await
	.map_err(|e| db_failure(&e))?;
	Ok(())
}

fn first_line(text: &str) -> String {
	text.lines()
		.next()
		.unwrap_or_default()
		.chars()
		.take(200)
		.collect()
}

fn cap(message: &str) -> String {
	if message.chars().count() <= MAX_ERROR_CHARS {
		return message.to_owned();
	}
	let mut out: String = message.chars().take(MAX_ERROR_CHARS).collect();
	out.push('…');
	out
}

/// The last lines the container printed, or a note when there are none.
async fn log_tail(client: &AgentClient, container_id: &str) -> String {
	let path = format!("/v1/containers/{container_id}/logs?tail={ERROR_TAIL_LINES}&follow=false");
	let mut lines = Vec::new();
	match client.stream(Method::GET, &path, None::<&()>).await {
		Ok(mut events) => {
			while let Some(event) = events.next().await {
				match event {
					Ok(Event::Log { line, .. }) => lines.push(line),
					Ok(_) => {}
					Err(e) => {
						warn!("reading the engine's last log lines failed: {e}");
						break;
					}
				}
			}
		}
		Err(e) => warn!("reading the engine's last log lines failed: {e}"),
	}
	let keep = ERROR_TAIL_LINES as usize;
	let start = lines.len().saturating_sub(keep);
	let tail = lines[start..].join("\n");
	if tail.trim().is_empty() {
		"(the engine printed nothing)".to_owned()
	} else {
		tail
	}
}

async fn stop(
	state: &AppState,
	log: &mut JobLog,
	model: &Model,
	client: &AgentClient,
) -> Result<(), JobFailure> {
	let Some(container_id) = model.container_id.as_deref() else {
		return Err(failed("model has no container; use start"));
	};
	log.push(
		Stream::System,
		&format!("stopping container {container_id}"),
	)
	.await;
	client
		.send(Method::POST, &format!("/v1/containers/{container_id}/stop"))
		.await
		.map_err(agent_failure)?;
	models::set_status(&state.db, model.id, ModelStatus::Stopped, None)
		.await
		.map_err(|e| db_failure(&e))
}

/// Removes the container and, for engines that keep a store, that volume;
/// then the row. The model files stay.
async fn remove(
	state: &AppState,
	log: &mut JobLog,
	model: &Model,
	client: &AgentClient,
) -> Result<(), JobFailure> {
	log.push(Stream::System, "removing the engine container")
		.await;
	client
		.send(Method::DELETE, &format!("/v1/services/{}", model.service()))
		.await
		.map_err(agent_failure)?;
	if engines::spec(model.engine).state_dir.is_some() {
		log.push(Stream::System, "removing the engine's state volume")
			.await;
		client
			.send(
				Method::DELETE,
				&format!("/v1/volumes/{}", model.state_volume()),
			)
			.await
			.map_err(agent_failure)?;
	}
	models::delete(&state.db, model.id)
		.await
		.map_err(|e| db_failure(&e))
}

#[cfg(test)]
mod tests {
	use sqlx::types::Json;
	use time::OffsetDateTime;

	use super::*;
	use crate::ids::{EnvironmentId, ModelFileId, ProjectId, ServerId};
	use crate::models::EngineKind;

	fn model(engine: EngineKind, gpu: bool) -> Model {
		Model {
			id: ModelId(7),
			project_id: ProjectId(1),
			environment_id: EnvironmentId(1),
			server_id: ServerId(2),
			model_files_id: ModelFileId(3),
			name: "Qwen".to_owned(),
			slug: "qwen".to_owned(),
			engine,
			image: "example.com/engine:1".to_owned(),
			port: 8080,
			model_path: "q.gguf".to_owned(),
			served_name: "qwen".to_owned(),
			context_size: 4096,
			gpu,
			extra_args: Json(vec!["--threads".to_owned(), "2".to_owned()]),
			memory_limit_mb: Some(12_288),
			status: ModelStatus::Starting,
			error: None,
			container_id: None,
			created_at: OffsetDateTime::UNIX_EPOCH,
			updated_at: OffsetDateTime::UNIX_EPOCH,
		}
	}

	fn ingress() -> Ingress {
		Ingress {
			http_port: 80,
			https_port: 443,
			acme_email: None,
		}
	}

	#[test]
	fn the_container_is_internal_only() {
		for engine in [
			EngineKind::LlamaCpu,
			EngineKind::LlamaCuda,
			EngineKind::Ollama,
			EngineKind::Vllm,
			EngineKind::Colibri,
			EngineKind::Custom,
		] {
			let spec = build_spec(&model(engine, false), "model-files-3", None, ingress(), 9);
			assert!(spec.routing.is_none(), "{engine:?}");
			assert!(spec.container_port.is_none(), "{engine:?}");
			assert!(spec.published_ports.is_empty(), "{engine:?}");
			assert_eq!(spec.network_aliases, ["model-qwen"]);
			assert_eq!(spec.service, "model-7");
			assert_eq!(spec.strategy, Strategy::Recreate);
			assert_eq!(spec.memory_bytes, Some(12_288 * 1024 * 1024));
			assert!(spec.gpus.is_none());
		}
	}

	#[test]
	fn the_files_are_mounted_read_only_and_the_gpu_is_passed_on() {
		let spec = build_spec(
			&model(EngineKind::LlamaCuda, true),
			"model-files-3",
			Some(GpuVendor::Nvidia),
			ingress(),
			9,
		);
		assert_eq!(spec.volumes.len(), 1);
		assert_eq!(spec.volumes[0].name, "model-files-3");
		assert_eq!(spec.volumes[0].path, "/models");
		assert!(spec.volumes[0].read_only);
		assert_eq!(
			spec.gpus,
			Some(GpuRequest {
				vendor: GpuVendor::Nvidia,
				devices: GpuDevices::All
			})
		);
		assert_eq!(spec.stop_timeout_s, Some(10));
		let command = spec.command.unwrap().join(" ");
		assert!(command.contains("-ngl 99"), "{command}");
		assert!(command.ends_with("--threads 2"), "{command}");
	}

	#[test]
	fn ollama_gets_a_state_volume_and_an_entrypoint() {
		let spec = build_spec(&model(EngineKind::Ollama, false), "f", None, ingress(), 9);
		assert_eq!(spec.volumes.len(), 2);
		assert_eq!(spec.volumes[1].name, "podkit-model-7-state");
		assert_eq!(spec.volumes[1].path, "/root/.ollama");
		assert!(!spec.volumes[1].read_only);
		assert!(spec.entrypoint.is_some());
		let vllm = build_spec(
			&model(EngineKind::Vllm, true),
			"f",
			Some(GpuVendor::Nvidia),
			ingress(),
			9,
		);
		assert_eq!(vllm.shm_bytes, Some(2 << 30));
	}

	#[test]
	fn errors_are_capped() {
		assert_eq!(cap("short"), "short");
		let long = "x".repeat(5000);
		assert_eq!(cap(&long).chars().count(), MAX_ERROR_CHARS + 1);
		assert_eq!(first_line("a\nb"), "a");
	}
}
