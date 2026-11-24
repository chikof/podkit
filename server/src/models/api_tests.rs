//! API and job tests against a fake agent on the local server's socket.
//! Nothing here touches Podman or the network, except the `#[ignore]`d live test.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::body::Bytes;
use axum::extract::Path;
use axum::http::{StatusCode, header};
use axum::response::IntoResponse;
use axum::routing::{delete, get, post};
use proto::{Cdi, Gpu, Hardware, Memory};
use serde_json::{Value, json};
use sqlx::PgPool;
use sqlx::types::Json;
use tokio_util::sync::CancellationToken;

use crate::jobs::{self, JobFailure, JobKind};
use crate::state::AppState;
use crate::tests::{call, state, user_with_team};

const GB: i64 = 1_000_000_000;

fn hardware(ram_mb: u64, gpus: &[(&str, u64)], cdi: &[&str]) -> Hardware {
	Hardware {
		memory: Memory {
			total_mb: ram_mb,
			available_mb: ram_mb,
		},
		gpus: gpus
			.iter()
			.map(|(vendor, vram_mb)| Gpu {
				vendor: (*vendor).to_owned(),
				name: format!("{vendor} gpu"),
				vram_mb: *vram_mb,
				vram_used_mb: None,
			})
			.collect(),
		cdi: Cdi {
			available: !cdi.is_empty(),
			devices: cdi.iter().map(|k| (*k).to_owned()).collect(),
		},
		..Hardware::default()
	}
}

/// A workstation: 30 GB of RAM, one NVIDIA card with 12 GB and its CDI spec.
fn workstation() -> Hardware {
	hardware(30_000, &[("nvidia", 12_288)], &["nvidia.com/gpu"])
}

struct Setup {
	app: Router,
	state: AppState,
	db: PgPool,
	token: String,
	team: i64,
	project: i64,
	server: i64,
}

impl Setup {
	async fn new(db: PgPool, hardware: Hardware) -> Self {
		Self::with_timeout(db, hardware, Duration::from_secs(10)).await
	}

	async fn with_timeout(db: PgPool, hardware: Hardware, ready_timeout: Duration) -> Self {
		let mut state = state(db.clone());
		std::sync::Arc::make_mut(&mut state.config).model_ready_timeout = Some(ready_timeout);
		let app = crate::routes::routes(state.clone()).await.unwrap();
		let (token, team) = user_with_team(&app, "alice").await;
		let (_, servers) = call(
			&app,
			"GET",
			&format!("/api/v1/teams/{team}/servers"),
			Some(&token),
			None,
		)
		.await;
		let server = servers[0]["id"].as_i64().unwrap();
		let (status, project) = call(
			&app,
			"POST",
			&format!("/api/v1/teams/{team}/projects"),
			Some(&token),
			Some(json!({ "name": "ai", "slug": "ai" })),
		)
		.await;
		assert!(status.is_success(), "{project}");
		let setup = Self {
			app,
			state,
			db,
			token,
			team,
			project: project["id"].as_i64().unwrap(),
			server,
		};
		setup.set_hardware(Some(hardware)).await;
		setup
	}

	async fn set_hardware(&self, hardware: Option<Hardware>) {
		sqlx::query("UPDATE servers SET hardware = $2 WHERE id = $1")
			.bind(self.server)
			.bind(hardware.map(Json))
			.execute(&self.db)
			.await
			.unwrap();
	}

	fn path(&self, rest: &str) -> String {
		format!(
			"/api/v1/teams/{}/projects/{}/models{rest}",
			self.team, self.project
		)
	}

	async fn call(&self, method: &str, rest: &str, body: Option<Value>) -> (StatusCode, Value) {
		call(&self.app, method, &self.path(rest), Some(&self.token), body).await
	}

	/// Inserts a model file on the local server.
	async fn add_file(&self, selection: Value, total_bytes: i64, status: &str) -> i64 {
		let id = crypto::generate_id();
		sqlx::query(
			"INSERT INTO model_files
				(id, team_id, server_id, repo, revision, selection, total_bytes, downloaded_bytes,
				 status, volume_name)
			VALUES ($1, $2, $3, 'acme/open', 'main', $4, $5, $5, $6, $7)",
		)
		.bind(id)
		.bind(self.team)
		.bind(self.server)
		.bind(selection)
		.bind(total_bytes)
		.bind(status)
		.bind(format!("model-files-{id}"))
		.execute(&self.db)
		.await
		.unwrap();
		id
	}

	/// The 2.5 GB `Qwen3-4B-Q4_K_M` file, downloaded.
	async fn gguf(&self) -> i64 {
		self.add_file(
			json!({ "include": [], "files": ["q.gguf"] }),
			2_497_280_256,
			"ready",
		)
		.await
	}

	async fn create(&self, file: i64, engine: &str) -> (StatusCode, Value) {
		self.create_with(json!({ "model_files_id": file, "engine": engine }))
			.await
	}

	async fn create_with(&self, mut body: Value) -> (StatusCode, Value) {
		body["name"] = body.get("name").cloned().unwrap_or(json!("Qwen"));
		self.call("POST", "", Some(body)).await
	}

	async fn count(&self) -> i64 {
		sqlx::query_scalar("SELECT COUNT(*) FROM models")
			.fetch_one(&self.db)
			.await
			.unwrap()
	}

	async fn get(&self, id: i64) -> Value {
		let (status, body) = self.call("GET", &format!("/{id}"), None).await;
		assert_eq!(status, StatusCode::OK, "{body}");
		body
	}
}

/// What the fake agent saw and what it answers.
#[derive(Default)]
struct FakeAgent {
	deploys: Mutex<Vec<Value>>,
	probes: Mutex<Vec<Value>>,
	stopped: Mutex<Vec<String>>,
	services: Mutex<Vec<String>>,
	volumes: Mutex<Vec<String>>,
	/// Answers to probes, in order; the last one repeats.
	probe_answers: Mutex<VecDeque<Value>>,
	log_lines: Mutex<Vec<String>>,
}

impl FakeAgent {
	fn answer_probes(&self, answers: &[Value]) {
		*self.probe_answers.lock().unwrap() = answers.iter().cloned().collect();
	}
}

fn ndjson(events: &[Value]) -> axum::response::Response {
	let body = events.iter().fold(String::new(), |mut out, e| {
		out.push_str(&e.to_string());
		out.push('\n');
		out
	});
	([(header::CONTENT_TYPE, "application/x-ndjson")], body).into_response()
}

fn loaded() -> Value {
	json!({ "running": true, "status": 200, "detail": "ok" })
}

fn loading() -> Value {
	json!({ "running": true, "status": 503, "detail": "Loading model" })
}

fn fake_agent(state: &AppState) -> Arc<FakeAgent> {
	let agent = Arc::new(FakeAgent::default());
	agent.answer_probes(&[loaded()]);
	let (deploy, probe, logs, stop, service, volume) = (
		agent.clone(),
		agent.clone(),
		agent.clone(),
		agent.clone(),
		agent.clone(),
		agent.clone(),
	);
	let app = Router::new()
		.route(
			"/v1/deploy",
			post(move |body: Bytes| async move {
				deploy
					.deploys
					.lock()
					.unwrap()
					.push(serde_json::from_slice(&body).unwrap());
				ndjson(&[
					json!({ "type": "log", "stream": "system", "line": "pulling the engine" }),
					json!({ "type": "deployed", "container_id": "c1", "image": "x",
						"commit_sha": null }),
				])
			}),
		)
		.route(
			"/v1/probes",
			post(move |body: Bytes| async move {
				probe
					.probes
					.lock()
					.unwrap()
					.push(serde_json::from_slice(&body).unwrap());
				let mut answers = probe.probe_answers.lock().unwrap();
				let answer = if answers.len() > 1 {
					answers.pop_front().unwrap()
				} else {
					answers.front().cloned().unwrap()
				};
				axum::Json(answer)
			}),
		)
		.route(
			"/v1/containers/{id}/logs",
			get(move |Path(_id): Path<String>| async move {
				let mut events: Vec<Value> = logs
					.log_lines
					.lock()
					.unwrap()
					.iter()
					.map(|l| json!({ "type": "log", "stream": "stderr", "line": l }))
					.collect();
				events.push(json!({ "type": "done" }));
				ndjson(&events)
			}),
		)
		.route(
			"/v1/containers/{id}/stop",
			post(move |Path(id): Path<String>| async move {
				stop.stopped.lock().unwrap().push(id);
				StatusCode::NO_CONTENT
			}),
		)
		.route(
			"/v1/services/{service}",
			delete(move |Path(name): Path<String>| async move {
				service.services.lock().unwrap().push(name);
				StatusCode::NO_CONTENT
			}),
		)
		.route(
			"/v1/volumes/{name}",
			delete(move |Path(name): Path<String>| async move {
				volume.volumes.lock().unwrap().push(name);
				StatusCode::NO_CONTENT
			}),
		);
	let listener = tokio::net::UnixListener::bind(&state.config.local_agent_socket).unwrap();
	tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
	agent
}

/// Runs the newest job of `kind` for the model the way the worker does: a
/// failure is written onto the row.
async fn run_job(s: &Setup, kind: JobKind, id: i64) -> (Result<(), JobFailure>, String) {
	let job = jobs::latest_for(&s.db, kind, id)
		.await
		.unwrap()
		.expect("a job is queued");
	let mut log = s.state.logs.open(job.id).await.unwrap();
	let result = super::run::run(&s.state, &job, &CancellationToken::new(), &mut log).await;
	if let Err(failure) = &result {
		jobs::fail_target(&s.state, job.kind, job.target_id, failure).await;
	}
	log.finish().await;
	let text = std::fs::read_to_string(s.state.logs.path(job.id)).unwrap();
	(result, text)
}

fn failure_message(result: Result<(), JobFailure>) -> String {
	match result {
		Err(JobFailure::Failed(message)) => message,
		other => panic!("expected a failure, got {other:?}"),
	}
}

#[sqlx::test]
async fn create_queues_a_start_and_names_the_internal_url(db: PgPool) {
	let s = Setup::new(db, workstation()).await;
	let file = s.gguf().await;
	let (status, model) = s
		.create_with(json!({
			"name": "Qwen3 4B", "model_files_id": file, "engine": "llama_cpu",
			"context_size": 4096,
		}))
		.await;
	assert_eq!(status, StatusCode::ACCEPTED, "{model}");
	let id = model["id"].as_i64().unwrap();
	assert_eq!(model["status"], "starting");
	assert_eq!(model["slug"], "qwen3-4b");
	assert_eq!(model["served_name"], "qwen3-4b");
	assert_eq!(model["model_path"], "q.gguf");
	assert_eq!(model["context_size"], 4096);
	assert_eq!(model["gpu"], false);
	assert_eq!(model["port"], 8080);
	assert_eq!(model["internal_url"], "http://model-qwen3-4b:8080/v1");
	assert!(
		model["image"]
			.as_str()
			.unwrap()
			.contains("llama.cpp:server@")
	);
	assert_eq!(model["server_id"], s.server);
	let job = jobs::latest_for(&s.db, JobKind::ModelStart, id)
		.await
		.unwrap()
		.expect("a start job is queued");
	assert_eq!(job.lock_key, format!("model:{id}"));

	let (_, listed) = s.call("GET", "", None).await;
	assert_eq!(listed.as_array().unwrap().len(), 1);

	// The same name in the same project is a conflict; a new name gets its own internal name.
	let (status, body) = s.create(file, "llama_cpu").await;
	assert_eq!(status, StatusCode::ACCEPTED, "{body}");
	let (status, body) = s.create(file, "llama_cpu").await;
	assert_eq!(status, StatusCode::CONFLICT, "{body}");
	assert_eq!(s.count().await, 2);
}

#[sqlx::test]
async fn the_internal_name_is_unique_across_projects(db: PgPool) {
	let s = Setup::new(db, workstation()).await;
	let file = s.gguf().await;
	let (status, second) = call(
		&s.app,
		"POST",
		&format!("/api/v1/teams/{}/projects", s.team),
		Some(&s.token),
		Some(json!({ "name": "other", "slug": "other" })),
	)
	.await;
	assert!(status.is_success());
	let other = second["id"].as_i64().unwrap();

	let (_, first) = s.create(file, "llama_cpu").await;
	let (status, twin) = call(
		&s.app,
		"POST",
		&format!("/api/v1/teams/{}/projects/{other}/models", s.team),
		Some(&s.token),
		Some(json!({ "name": "Qwen", "model_files_id": file, "engine": "llama_cpu" })),
	)
	.await;
	assert_eq!(status, StatusCode::ACCEPTED, "{twin}");
	assert_eq!(first["slug"], "qwen");
	let slug = twin["slug"].as_str().unwrap();
	assert!(slug.starts_with("qwen-") && slug != "qwen", "{slug}");
	assert_eq!(twin["internal_url"], format!("http://model-{slug}:8080/v1"));
}

#[sqlx::test]
async fn a_gpu_engine_on_a_server_without_cdi_is_refused(db: PgPool) {
	// The card is there, the toolkit's CDI spec is not.
	let s = Setup::new(db, hardware(30_000, &[("nvidia", 12_288)], &[])).await;
	let file = s.gguf().await;
	let (status, body) = s.create(file, "llama_cuda").await;
	assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
	let message = body["error"].as_str().unwrap();
	assert!(
		message.contains("llama.cpp (CUDA) needs an NVIDIA GPU with a CDI spec"),
		"{message}"
	);
	assert!(message.contains("no CDI spec"), "{message}");
	assert!(message.contains("nvidia-container-toolkit"), "{message}");

	// An optional-GPU engine asked for a GPU the server does not have.
	s.set_hardware(Some(hardware(30_000, &[], &[]))).await;
	let (status, body) = s
		.create_with(json!({ "model_files_id": file, "engine": "ollama", "gpu": true }))
		.await;
	assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
	assert!(
		body["error"]
			.as_str()
			.unwrap()
			.contains("has no NVIDIA GPU"),
		"{body}"
	);

	// CPU-only engines refuse a GPU, GPU-only engines refuse to run without one.
	let (status, body) = s
		.create_with(json!({ "model_files_id": file, "engine": "llama_cpu", "gpu": true }))
		.await;
	assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
	assert!(
		body["error"].as_str().unwrap().contains("CPU only"),
		"{body}"
	);
	let (status, body) = s
		.create_with(json!({ "model_files_id": file, "engine": "llama_cuda", "gpu": false }))
		.await;
	assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
	assert_eq!(s.count().await, 0);
}

#[sqlx::test]
async fn gguf_on_vllm_is_refused_with_both_formats_named(db: PgPool) {
	let s = Setup::new(db, workstation()).await;
	let file = s.gguf().await;
	let (status, body) = s.create(file, "vllm").await;
	assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
	assert_eq!(
		body["error"],
		"vLLM serves safetensors, but these files are gguf"
	);

	// Safetensors on llama.cpp is refused the same way round.
	let st = s
		.add_file(
			json!({ "include": ["*.safetensors", "*.json"], "files": [] }),
			4_063_515_592,
			"ready",
		)
		.await;
	let (status, body) = s.create(st, "llama_cpu").await;
	assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
	assert!(
		body["error"].as_str().unwrap().contains("serves gguf"),
		"{body}"
	);
	let (status, body) = s.create(st, "vllm").await;
	assert_eq!(status, StatusCode::ACCEPTED, "{body}");
	assert_eq!(body["model_path"], "");
	assert_eq!(body["gpu"], true);
	assert_eq!(s.count().await, 1);
}

#[sqlx::test]
async fn a_model_that_cannot_fit_is_refused_with_numbers(db: PgPool) {
	let s = Setup::new(
		db,
		hardware(14_000, &[("nvidia", 12_288)], &["nvidia.com/gpu"]),
	)
	.await;
	let big = s
		.add_file(
			json!({ "include": [], "files": ["big.gguf"] }),
			20 * GB,
			"ready",
		)
		.await;
	let (status, body) = s.create(big, "llama_cpu").await;
	assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
	let message = body["error"].as_str().unwrap();
	// 20 GB is 19074 MiB of weights, 1024 MiB of context, 512 + 15 % of the weights as overhead.
	assert!(
		message.contains(
			"needs about 23471 MB (19074 MB of weights, 1024 MB for a 8192-token context and \
			3373 MB of runtime overhead)"
		),
		"{message}"
	);
	assert!(message.contains("has 14000 MB of RAM"), "{message}");

	// On the GPU the card's 12 GB is what counts.
	let (status, body) = s.create(big, "llama_cuda").await;
	assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
	assert!(
		body["error"]
			.as_str()
			.unwrap()
			.contains("has 12288 MB of GPU memory"),
		"{body}"
	);

	// The 4B model fits both.
	let small = s.gguf().await;
	let (status, body) = s.create(small, "llama_cuda").await;
	assert_eq!(status, StatusCode::ACCEPTED, "{body}");
	assert_eq!(body["gpu"], true);

	// No inventory yet: the request waits instead of guessing.
	s.set_hardware(None).await;
	let (status, body) = s
		.create_with(json!({ "name": "Other", "model_files_id": small, "engine": "llama_cpu" }))
		.await;
	assert_eq!(status, StatusCode::CONFLICT, "{body}");
}

#[sqlx::test]
async fn bad_requests_are_refused_before_a_row_exists(db: PgPool) {
	let s = Setup::new(db, workstation()).await;
	let file = s.gguf().await;
	let pending = s
		.add_file(
			json!({ "include": [], "files": ["q.gguf"] }),
			GB,
			"downloading",
		)
		.await;
	let whole_repo = s
		.add_file(json!({ "include": [], "files": [] }), GB, "ready")
		.await;

	let cases = [
		(
			json!({ "model_files_id": pending, "engine": "llama_cpu" }),
			StatusCode::CONFLICT,
			"wait until they are ready",
		),
		(
			json!({ "model_files_id": whole_repo, "engine": "llama_cpu" }),
			StatusCode::BAD_REQUEST,
			"whole repo",
		),
		(
			json!({ "model_files_id": file, "engine": "llama_cpu", "extra_args": ["--port", "1"] }),
			StatusCode::BAD_REQUEST,
			"set by podkit",
		),
		(
			json!({ "model_files_id": file, "engine": "ollama", "extra_args": ["--x"] }),
			StatusCode::BAD_REQUEST,
			"no extra arguments",
		),
		(
			json!({ "model_files_id": file, "engine": "llama_cpu", "model_path": "../x.gguf" }),
			StatusCode::BAD_REQUEST,
			"invalid model_path",
		),
		(
			json!({ "model_files_id": file, "engine": "llama_cpu", "context_size": 10 }),
			StatusCode::BAD_REQUEST,
			"context_size",
		),
		(
			json!({ "model_files_id": file, "engine": "llama_cpu", "served_name": "a b" }),
			StatusCode::BAD_REQUEST,
			"served_name",
		),
		(
			json!({ "model_files_id": file, "engine": "llama_cpu", "image": "x:1" }),
			StatusCode::BAD_REQUEST,
			"only for the custom",
		),
		(
			json!({ "model_files_id": file, "engine": "custom" }),
			StatusCode::BAD_REQUEST,
			"needs an image",
		),
		(
			json!({ "model_files_id": file, "engine": "custom", "image": "bad image" }),
			StatusCode::BAD_REQUEST,
			"image must be",
		),
		(
			json!({ "model_files_id": file, "engine": "llama_cpu", "name": "  " }),
			StatusCode::BAD_REQUEST,
			"name must be",
		),
	];
	for (body, status, needle) in cases {
		let (got, response) = s.create_with(body.clone()).await;
		assert_eq!(got, status, "{body}: {response}");
		assert!(
			response["error"].as_str().unwrap().contains(needle),
			"{body}: {response}"
		);
	}
	assert_eq!(s.count().await, 0);

	// A custom engine takes any image that serves /v1.
	let (status, body) = s
		.create_with(json!({
			"model_files_id": file, "engine": "custom", "image": "example.com/serve:1",
			"port": 9000, "extra_args": ["serve", "--fast"],
		}))
		.await;
	assert_eq!(status, StatusCode::ACCEPTED, "{body}");
	assert_eq!(body["image"], "example.com/serve:1");
	assert_eq!(body["port"], 9000);
	assert_eq!(body["internal_url"], "http://model-qwen:9000/v1");
}

#[sqlx::test]
async fn status_transitions_follow_the_jobs(db: PgPool) {
	let s = Setup::new(db, workstation()).await;
	let agent = fake_agent(&s.state);
	let file = s.gguf().await;
	let (_, created) = s.create(file, "llama_cpu").await;
	let id = created["id"].as_i64().unwrap();
	assert_eq!(created["status"], "starting");

	// starting -> running once the engine answers; the first probes say "loading".
	agent.answer_probes(&[loading(), loading(), loaded()]);
	let (result, log) = run_job(&s, JobKind::ModelStart, id).await;
	result.unwrap();
	assert!(log.contains("pulling the engine"), "{log}");
	assert!(
		log.contains("not ready yet: HTTP 503: Loading model"),
		"{log}"
	);
	assert!(log.contains("model loaded after"), "{log}");
	let model = s.get(id).await;
	assert_eq!(model["status"], "running");
	assert_eq!(model["container_id"], "c1");
	assert_eq!(model["error"], Value::Null);
	assert_eq!(agent.probes.lock().unwrap().len(), 3);

	// What was deployed: internal only, files read-only, the engine's own arguments.
	let deploys = agent.deploys.lock().unwrap().clone();
	assert_eq!(deploys.len(), 1);
	let spec = &deploys[0];
	assert_eq!(spec["service"], format!("model-{id}"));
	assert_eq!(spec["network_aliases"], json!(["model-qwen"]));
	assert_eq!(spec["routing"], Value::Null);
	assert_eq!(spec["container_port"], Value::Null);
	assert_eq!(spec["published_ports"], json!([]));
	assert_eq!(spec["strategy"], "recreate");
	assert_eq!(spec["stop_timeout_s"], 10);
	assert!(spec.get("gpus").is_none());
	assert_eq!(
		spec["volumes"],
		json!([{ "name": format!("model-files-{file}"), "path": "/models", "read_only": true }])
	);
	assert_eq!(spec["source"]["pull"], true);
	assert_eq!(spec["command"][0], "-m");
	assert_eq!(spec["command"][1], "/models/q.gguf");
	let probe = agent.probes.lock().unwrap()[0].clone();
	assert_eq!(probe["host"], "model-qwen");
	assert_eq!(probe["port"], 8080);
	assert_eq!(probe["path"], "/health");
	assert_eq!(probe["container_id"], "c1");

	// running -> stopped, and not startable twice.
	let (status, body) = s.call("POST", &format!("/{id}/start"), None).await;
	assert_eq!(status, StatusCode::CONFLICT, "{body}");
	let (status, body) = s.call("POST", &format!("/{id}/stop"), None).await;
	assert_eq!(status, StatusCode::ACCEPTED, "{body}");
	assert_eq!(body["status"], "running", "the row moves when the job does");
	run_job(&s, JobKind::ModelStop, id).await.0.unwrap();
	assert_eq!(s.get(id).await["status"], "stopped");
	assert_eq!(*agent.stopped.lock().unwrap(), ["c1"]);
	let (status, _) = s.call("POST", &format!("/{id}/stop"), None).await;
	assert_eq!(status, StatusCode::CONFLICT);

	// stopped -> starting -> running again, with a fresh container.
	let (status, body) = s.call("POST", &format!("/{id}/start"), None).await;
	assert_eq!(status, StatusCode::ACCEPTED, "{body}");
	assert_eq!(body["status"], "starting");
	run_job(&s, JobKind::ModelStart, id).await.0.unwrap();
	assert_eq!(s.get(id).await["status"], "running");
	assert_eq!(agent.deploys.lock().unwrap().len(), 2);

	// restart works from running too.
	let (status, body) = s.call("POST", &format!("/{id}/restart"), None).await;
	assert_eq!(status, StatusCode::ACCEPTED, "{body}");
	assert_eq!(body["status"], "starting");
	run_job(&s, JobKind::ModelStart, id).await.0.unwrap();
	assert_eq!(agent.deploys.lock().unwrap().len(), 3);

	// The files cannot be deleted from under a model.
	let (status, body) = call(
		&s.app,
		"DELETE",
		&format!("/api/v1/teams/{}/model-files/{file}", s.team),
		Some(&s.token),
		None,
	)
	.await;
	assert_eq!(status, StatusCode::CONFLICT, "{body}");
	assert!(
		body["error"]
			.as_str()
			.unwrap()
			.contains("serves these files"),
		"{body}"
	);

	// removing -> gone, with the container; the files stay.
	let (status, body) = s.call("DELETE", &format!("/{id}"), None).await;
	assert_eq!(status, StatusCode::ACCEPTED, "{body}");
	assert_eq!(body["status"], "removing");
	let (status, _) = s.call("DELETE", &format!("/{id}"), None).await;
	assert_eq!(status, StatusCode::CONFLICT);
	let (status, _) = s.call("POST", &format!("/{id}/start"), None).await;
	assert_eq!(status, StatusCode::CONFLICT);
	run_job(&s, JobKind::ModelRemove, id).await.0.unwrap();
	assert_eq!(*agent.services.lock().unwrap(), [format!("model-{id}")]);
	assert!(
		agent.volumes.lock().unwrap().is_empty(),
		"llama.cpp keeps no state"
	);
	assert_eq!(s.count().await, 0);
	let (status, _) = s.call("GET", &format!("/{id}"), None).await;
	assert_eq!(status, StatusCode::NOT_FOUND);
	let remaining: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM model_files WHERE id = $1")
		.bind(file)
		.fetch_one(&s.db)
		.await
		.unwrap();
	assert_eq!(remaining, 1);
}

#[sqlx::test]
async fn ollama_keeps_a_state_volume_that_removal_deletes(db: PgPool) {
	let s = Setup::new(db, workstation()).await;
	let agent = fake_agent(&s.state);
	let file = s.gguf().await;
	let (status, created) = s.create(file, "ollama").await;
	assert_eq!(status, StatusCode::ACCEPTED, "{created}");
	let id = created["id"].as_i64().unwrap();
	// No GPU was asked for, but the server has one, so the optional engine takes it.
	assert_eq!(created["gpu"], true);
	run_job(&s, JobKind::ModelStart, id).await.0.unwrap();

	let spec = agent.deploys.lock().unwrap()[0].clone();
	assert_eq!(
		spec["volumes"][1]["name"],
		format!("podkit-model-{id}-state")
	);
	assert_eq!(spec["volumes"][1]["path"], "/root/.ollama");
	assert_eq!(
		spec["gpus"],
		json!({ "vendor": "nvidia", "devices": "all" })
	);
	assert_eq!(spec["entrypoint"][0], "sh");
	assert_eq!(spec["command"], json!(["/models/q.gguf", "qwen"]));
	// Ollama answers on `/` before the model is loaded, so the model is asked to load.
	let probe = agent.probes.lock().unwrap()[0].clone();
	assert_eq!(probe["method"], "POST");
	assert_eq!(probe["path"], "/api/generate");
	assert_eq!(probe["port"], 11434);
	let body: Value = serde_json::from_str(probe["body"].as_str().unwrap()).unwrap();
	assert_eq!(body, json!({ "model": "qwen", "keep_alive": -1 }));

	s.call("DELETE", &format!("/{id}"), None).await;
	run_job(&s, JobKind::ModelRemove, id).await.0.unwrap();
	assert_eq!(
		*agent.volumes.lock().unwrap(),
		[format!("podkit-model-{id}-state")]
	);
	assert_eq!(s.count().await, 0);
}

#[sqlx::test]
async fn readiness_timeout_fails_with_the_last_log_lines(db: PgPool) {
	let s = Setup::with_timeout(db, workstation(), Duration::from_millis(150)).await;
	let agent = fake_agent(&s.state);
	let file = s.gguf().await;
	let (_, created) = s.create(file, "llama_cpu").await;
	let id = created["id"].as_i64().unwrap();
	agent.answer_probes(&[loading()]);
	*agent.log_lines.lock().unwrap() = vec![
		"llama_model_load: loading tensors".to_owned(),
		"ggml_backend: out of memory".to_owned(),
	];

	let (result, _) = run_job(&s, JobKind::ModelStart, id).await;
	let message = failure_message(result);
	assert!(
		message.contains("did not load the model within 0s"),
		"{message}"
	);
	assert!(
		message.contains("last check: HTTP 503: Loading model"),
		"{message}"
	);

	let model = s.get(id).await;
	assert_eq!(model["status"], "failed");
	let error = model["error"].as_str().unwrap();
	assert!(
		error.contains("llama_model_load: loading tensors\nggml_backend: out of memory"),
		"{error}"
	);
	assert!(agent.probes.lock().unwrap().len() >= 2, "it polled");
	// It does not keep holding memory.
	assert_eq!(*agent.stopped.lock().unwrap(), ["c1"]);

	// A failed model can be started again, and the error clears.
	agent.answer_probes(&[loaded()]);
	let (status, body) = s.call("POST", &format!("/{id}/start"), None).await;
	assert_eq!(status, StatusCode::ACCEPTED, "{body}");
	assert_eq!(body["error"], Value::Null);
	run_job(&s, JobKind::ModelStart, id).await.0.unwrap();
	let model = s.get(id).await;
	assert_eq!(model["status"], "running");
	assert_eq!(model["error"], Value::Null);
}

#[sqlx::test]
async fn an_engine_that_exits_while_loading_fails_at_once(db: PgPool) {
	let s = Setup::new(db, workstation()).await;
	let agent = fake_agent(&s.state);
	let file = s.gguf().await;
	let (_, created) = s.create(file, "llama_cpu").await;
	let id = created["id"].as_i64().unwrap();
	agent.answer_probes(&[
		loading(),
		json!({ "running": false, "status": null, "detail": "container is not running" }),
	]);
	*agent.log_lines.lock().unwrap() = vec!["error: failed to load model".to_owned()];

	let (result, _) = run_job(&s, JobKind::ModelStart, id).await;
	assert!(result.is_err());
	let model = s.get(id).await;
	assert_eq!(model["status"], "failed");
	assert_eq!(
		model["error"],
		"the engine exited while loading the model; last log lines:\nerror: failed to load model"
	);
	assert_eq!(agent.probes.lock().unwrap().len(), 2);
}

#[sqlx::test]
async fn an_unreachable_agent_fails_the_job_without_leaking_details(db: PgPool) {
	let s = Setup::new(db, workstation()).await;
	let file = s.gguf().await;
	let (_, created) = s.create(file, "llama_cpu").await;
	let id = created["id"].as_i64().unwrap();
	let (result, _) = run_job(&s, JobKind::ModelStart, id).await;
	assert_eq!(failure_message(result), "server agent unavailable");
	let model = s.get(id).await;
	assert_eq!(model["status"], "failed");
	assert_eq!(model["error"], "server agent unavailable");

	// A failed removal keeps the row so it can be retried.
	let (status, _) = s.call("DELETE", &format!("/{id}"), None).await;
	assert_eq!(status, StatusCode::ACCEPTED);
	run_job(&s, JobKind::ModelRemove, id).await.0.unwrap_err();
	assert_eq!(s.get(id).await["status"], "failed");
}

#[sqlx::test]
async fn a_start_that_fails_after_a_removal_was_requested_keeps_removing(db: PgPool) {
	let s = Setup::new(db, workstation()).await;
	let file = s.gguf().await;
	let (_, created) = s.create(file, "llama_cpu").await;
	let id = created["id"].as_i64().unwrap();
	let (status, _) = s.call("DELETE", &format!("/{id}"), None).await;
	assert_eq!(status, StatusCode::ACCEPTED);
	// The queued start runs first (same lock key) and must not hide the removal.
	let (result, _) = run_job(&s, JobKind::ModelStart, id).await;
	assert_eq!(failure_message(result), "model is being removed");
	assert_eq!(s.get(id).await["status"], "removing");
}

#[sqlx::test]
async fn logs_stream_the_container_output(db: PgPool) {
	let s = Setup::new(db, workstation()).await;
	let agent = fake_agent(&s.state);
	let file = s.gguf().await;
	let (_, created) = s.create(file, "llama_cpu").await;
	let id = created["id"].as_i64().unwrap();

	// No container before the first start.
	let (status, body) = s.call("GET", &format!("/{id}/logs"), None).await;
	assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

	run_job(&s, JobKind::ModelStart, id).await.0.unwrap();
	*agent.log_lines.lock().unwrap() = vec!["main: server is listening".to_owned()];
	let response = tower::ServiceExt::oneshot(
		s.app.clone(),
		axum::http::Request::builder()
			.uri(s.path(&format!("/{id}/logs?follow=false")))
			.header(header::AUTHORIZATION, format!("Bearer {}", s.token))
			.body(axum::body::Body::empty())
			.unwrap(),
	)
	.await
	.unwrap();
	assert_eq!(response.status(), StatusCode::OK);
	let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
		.await
		.unwrap();
	let text = String::from_utf8_lossy(&bytes);
	assert!(text.contains("event: log"), "{text}");
	assert!(text.contains("main: server is listening"), "{text}");
}

#[sqlx::test]
async fn the_engine_catalog_lists_every_template(db: PgPool) {
	let s = Setup::new(db, workstation()).await;
	let (status, body) = s.call("GET", "/engines", None).await;
	assert_eq!(status, StatusCode::OK, "{body}");
	let engines: Vec<&str> = body
		.as_array()
		.unwrap()
		.iter()
		.map(|e| e["engine"].as_str().unwrap())
		.collect();
	assert_eq!(
		engines,
		[
			"llama_cpu",
			"llama_cuda",
			"llama_vulkan",
			"ollama",
			"vllm",
			"colibri",
			"custom"
		]
	);
	let vllm = &body[4];
	assert_eq!(vllm["formats"], json!(["safetensors"]));
	assert_eq!(vllm["gpu"], "required");
	assert_eq!(vllm["port"], 8000);
	assert_eq!(body[6]["image"], Value::Null);
}

#[sqlx::test]
async fn other_teams_get_not_found_everywhere(db: PgPool) {
	let s = Setup::new(db, workstation()).await;
	let file = s.gguf().await;
	let (_, created) = s.create(file, "llama_cpu").await;
	let id = created["id"].as_i64().unwrap();

	let (bob, bob_team) = user_with_team(&s.app, "bob").await;
	let (status, bob_project) = call(
		&s.app,
		"POST",
		&format!("/api/v1/teams/{bob_team}/projects"),
		Some(&bob),
		Some(json!({ "name": "mine", "slug": "mine" })),
	)
	.await;
	assert!(status.is_success());
	let bob_project = bob_project["id"].as_i64().unwrap();
	let bob_path =
		|rest: &str| format!("/api/v1/teams/{bob_team}/projects/{bob_project}/models{rest}");

	// Alice's model id inside Bob's own project is a miss, never Forbidden or a leak.
	for (method, rest) in [
		("GET", format!("/{id}")),
		("DELETE", format!("/{id}")),
		("POST", format!("/{id}/start")),
		("POST", format!("/{id}/stop")),
		("POST", format!("/{id}/restart")),
		("GET", format!("/{id}/logs")),
	] {
		let (status, _) = call(&s.app, method, &bob_path(&rest), Some(&bob), None).await;
		assert_eq!(status, StatusCode::NOT_FOUND, "{method} {rest}");
	}
	// Alice's project through Bob's team is a miss too.
	for rest in [format!("/{id}"), String::new()] {
		let (status, _) = call(
			&s.app,
			"GET",
			&format!(
				"/api/v1/teams/{bob_team}/projects/{}/models{rest}",
				s.project
			),
			Some(&bob),
			None,
		)
		.await;
		assert_eq!(status, StatusCode::NOT_FOUND, "{rest}");
	}
	// Alice's team path is a 404 for a stranger: the team is not there for him.
	let (status, _) = call(&s.app, "GET", &s.path(&format!("/{id}")), Some(&bob), None).await;
	assert_eq!(status, StatusCode::NOT_FOUND);

	// Bob cannot serve Alice's files, and sees none of her models.
	let (status, _) = call(
		&s.app,
		"POST",
		&bob_path(""),
		Some(&bob),
		Some(json!({ "name": "x", "model_files_id": file, "engine": "llama_cpu" })),
	)
	.await;
	assert_eq!(status, StatusCode::NOT_FOUND);
	let (_, listed) = call(&s.app, "GET", &bob_path(""), Some(&bob), None).await;
	assert_eq!(listed, json!([]));
	assert_eq!(s.count().await, 1);
	assert_eq!(s.get(id).await["status"], "starting");
}

#[sqlx::test]
async fn members_can_read_but_not_change(db: PgPool) {
	let s = Setup::new(db, workstation()).await;
	let file = s.gguf().await;
	let (_, created) = s.create(file, "llama_cpu").await;
	let id = created["id"].as_i64().unwrap();

	let (carol, _) = user_with_team(&s.app, "carol").await;
	let carol_id: i64 =
		sqlx::query_scalar("SELECT id FROM users WHERE email = 'carol@example.com'")
			.fetch_one(&s.db)
			.await
			.unwrap();
	sqlx::query("INSERT INTO team_members (id, team_id, user_id, role_id) VALUES ($1, $2, $3, 2)")
		.bind(crypto::generate_id())
		.bind(s.team)
		.bind(carol_id)
		.execute(&s.db)
		.await
		.unwrap();

	let as_carol = |method: &'static str, rest: String, body: Option<Value>| {
		let (app, path, carol) = (s.app.clone(), s.path(&rest), carol.clone());
		async move { call(&app, method, &path, Some(&carol), body).await.0 }
	};
	for rest in [String::new(), format!("/{id}"), "/engines".to_owned()] {
		assert_eq!(
			as_carol("GET", rest.clone(), None).await,
			StatusCode::OK,
			"{rest}"
		);
	}
	let create = Some(json!({ "name": "c", "model_files_id": file, "engine": "llama_cpu" }));
	assert_eq!(
		as_carol("POST", String::new(), create).await,
		StatusCode::FORBIDDEN
	);
	for (method, rest) in [
		("POST", format!("/{id}/start")),
		("POST", format!("/{id}/stop")),
		("POST", format!("/{id}/restart")),
		("DELETE", format!("/{id}")),
	] {
		assert_eq!(
			as_carol(method, rest.clone(), None).await,
			StatusCode::FORBIDDEN,
			"{method} {rest}"
		);
	}
	assert_eq!(s.count().await, 1);
	assert_eq!(s.get(id).await["status"], "starting");
}

/// The proof from the card: llama.cpp on the CPU with a small GGUF, reached by
/// name from another container on the `podkit` network. The agent runs in this
/// process on the local server's socket. The GGUF is copied into a volume
/// standing in for the AI 03 download.
///
/// ```text
/// cargo test -p server --all-features -- --ignored live_llama_cpu
/// ```
///
/// Needs `~/Projects/.podkit-spikes-cache/models/Qwen3-4B-Q4_K_M.gguf` (2.5 GB,
/// from the engine spike) or `PODKIT_LIVE_GGUF`, the llama.cpp CPU image and
/// `docker.io/curlimages/curl`.
#[sqlx::test]
#[ignore = "runs llama.cpp in a real podman with a 2.5 GB GGUF"]
async fn live_llama_cpu_serves_v1_models_on_the_podkit_network(db: PgPool) {
	use std::process::Command;

	let gguf = std::env::var("PODKIT_LIVE_GGUF").unwrap_or_else(|_| {
		format!(
			"{}/Projects/.podkit-spikes-cache/models/Qwen3-4B-Q4_K_M.gguf",
			std::env::var("HOME").unwrap()
		)
	});
	assert!(std::path::Path::new(&gguf).exists(), "{gguf} is missing");
	let size = i64::try_from(std::fs::metadata(&gguf).unwrap().len()).unwrap();
	assert!(size <= 4 * GB, "the live test is for models up to 4 GB");

	let s = Setup::with_timeout(db, hardware(14_000, &[], &[]), Duration::from_mins(5)).await;
	let podman = agent::podman::Podman::connect(&agent::podman::local_socket_path())
		.expect("connect to podman.sock");
	let data_dir = tempfile::tempdir().unwrap().keep();
	let ctx = Arc::new(agent::Ctx {
		docker: podman.docker().clone(),
		supervisor: agent::supervisor::Supervisor::start(podman.clone(), &data_dir).await,
		data_dir,
		podman_socket: agent::podman::local_socket_path(),
		http: reqwest::Client::new(),
	});
	let listener = tokio::net::UnixListener::bind(&s.state.config.local_agent_socket).unwrap();
	tokio::spawn(async move {
		axum::serve(listener, agent::routes::router(ctx))
			.await
			.unwrap();
	});

	let file = s
		.add_file(json!({ "include": [], "files": ["q.gguf"] }), size, "ready")
		.await;
	let volume = format!("model-files-{file}");
	let podman_cli = |args: &[&str]| {
		let out = Command::new("podman")
			.args(args)
			.output()
			.expect("podman on PATH");
		assert!(
			out.status.success(),
			"podman {args:?}: {}",
			String::from_utf8_lossy(&out.stderr)
		);
	};
	podman_cli(&["volume", "create", &volume]);
	podman_cli(&[
		"run",
		"--rm",
		"-v",
		&format!("{volume}:/models"),
		"-v",
		&format!("{gguf}:/src/q.gguf:ro"),
		"docker.io/library/busybox",
		"cp",
		"/src/q.gguf",
		"/models/q.gguf",
	]);

	let (status, created) = s
		.create_with(json!({
			"name": "Live", "model_files_id": file, "engine": "llama_cpu",
			"context_size": 2048, "memory_limit_mb": 12_288,
		}))
		.await;
	assert_eq!(status, StatusCode::ACCEPTED, "{created}");
	let id = created["id"].as_i64().unwrap();
	let slug = created["slug"].as_str().unwrap().to_owned();

	let (result, log) = run_job(&s, JobKind::ModelStart, id).await;
	let model = s.get(id).await;
	let proof = (model["status"] == "running").then(|| {
		Command::new("sh")
			.args([
				"-c",
				&format!(
					"podman run --rm --network podkit docker.io/curlimages/curl -s \
					http://model-{slug}:8080/v1/models | jq -e '.data | length >= 1'"
				),
			])
			.output()
			.unwrap()
	});
	// Clean up before asserting, so a failure leaves nothing running.
	s.call("DELETE", &format!("/{id}"), None).await;
	run_job(&s, JobKind::ModelRemove, id).await.0.unwrap();
	podman_cli(&["volume", "rm", "-f", &volume]);

	result.unwrap_or_else(|e| panic!("start failed: {e:?}\n{log}"));
	assert_eq!(model["status"], "running", "{model}\n{log}");
	let proof = proof.unwrap();
	println!("{}", String::from_utf8_lossy(&proof.stdout));
	assert!(
		proof.status.success(),
		"the proof command failed: {}",
		String::from_utf8_lossy(&proof.stderr)
	);
	assert_eq!(s.count().await, 0);
}
