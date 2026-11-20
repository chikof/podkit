//! Health gate tests: the state machine of a deployment's health, the settings, what the
//! deploy hands the agent and does with what the agent says, and the watch that rolls a bad
//! release back. The agent is a fake that records what it is asked.
//!
//! `cargo test -p server --all-features -- health_gate`

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::body::Bytes;
use axum::http::{StatusCode, header};
use axum::response::IntoResponse;
use axum::routing::post;
use proto::{ContainerHealth, ContainerState, HealthCheck, HealthStatus, Strategy};
use serde_json::{Value, json};
use sqlx::PgPool;
use tokio_util::sync::CancellationToken;

use super::machine::{Effect, Machine, Signal};
use super::watch;
use super::{HealthConfig, Kind, State, config, of_deployment, service};
use crate::deployments::{self, Deployment, DeploymentStatus};
use crate::ids::{ApplicationId, DeploymentId, ProjectId, ServerId, TeamId, UserId};
use crate::jobs::{self, JobFailure, JobKind};
use crate::state::AppState;
use crate::tests::{call, insert_deployment, state, user_id, user_with_team};

const SECOND: Duration = Duration::from_secs(1);
const MINUTE: Duration = Duration::from_secs(60);

// --- the state machine ------------------------------------------------------------------------

fn machine() -> Machine {
	Machine::new(3, 5 * MINUTE, false, true)
}

fn healthy() -> Machine {
	let mut m = machine();
	m.on(Signal::Started, Duration::ZERO);
	m.on(Signal::Deployed, Duration::ZERO);
	assert_eq!(m.state, Some(State::Healthy));
	m
}

#[test]
fn health_gate_machine_success_waits_then_takes_over() {
	let mut m = machine();
	assert_eq!(m.state, None);
	assert_eq!(m.on(Signal::Started, Duration::ZERO), Effect::None);
	assert_eq!(m.state, Some(State::Waiting));
	assert_eq!(m.on(Signal::Deployed, Duration::ZERO), Effect::None);
	assert_eq!(m.state, Some(State::Healthy));
	// A passing check keeps it healthy and never rolls anything back.
	assert_eq!(m.on(Signal::CheckPassed, MINUTE), Effect::None);
	assert_eq!(m.state, Some(State::Healthy));
}

#[test]
fn health_gate_machine_timeout_keeps_the_old_container() {
	let mut m = machine();
	m.on(Signal::Started, Duration::ZERO);
	// The gate gave up: the deploy failed and the old container was never touched, so there
	// is nothing to roll back and nothing the watch should do.
	assert_eq!(m.on(Signal::DeployFailed, Duration::ZERO), Effect::None);
	assert_eq!(m.state, Some(State::Unhealthy));
	for signal in [Signal::CheckFailed, Signal::Crashed, Signal::Deployed] {
		assert_eq!(m.on(signal, SECOND), Effect::None, "{signal:?}");
		assert_eq!(m.state, Some(State::Unhealthy), "{signal:?}");
	}
	// A deploy that failed before it reached the health check says nothing about health.
	let mut early = machine();
	early.on(Signal::DeployFailed, Duration::ZERO);
	assert_eq!(early.state, None);
}

#[test]
fn health_gate_machine_crash_after_the_switch_rolls_back() {
	let mut m = healthy();
	let Effect::Rollback(reason) = m.on(Signal::Crashed, 40 * SECOND) else {
		panic!("a crash inside the window rolls back");
	};
	assert!(reason.contains("crashed"), "{reason}");
	assert!(reason.contains("40s"), "{reason}");
	assert_eq!(m.state, Some(State::RolledBack));
	// Once rolled back it is settled: no second rollback.
	assert_eq!(m.on(Signal::Crashed, 50 * SECOND), Effect::None);
}

#[test]
fn health_gate_machine_failed_checks_roll_back_after_the_threshold_in_a_row() {
	let mut m = healthy();
	assert_eq!(m.on(Signal::CheckFailed, 30 * SECOND), Effect::None);
	assert_eq!(m.on(Signal::CheckFailed, 60 * SECOND), Effect::None);
	assert_eq!(m.failures, 2);
	// A pass in between starts the count again.
	assert_eq!(m.on(Signal::CheckPassed, 90 * SECOND), Effect::None);
	assert_eq!(m.failures, 0);
	assert_eq!(m.on(Signal::CheckFailed, 120 * SECOND), Effect::None);
	assert_eq!(m.on(Signal::CheckFailed, 150 * SECOND), Effect::None);
	let Effect::Rollback(reason) = m.on(Signal::CheckFailed, 180 * SECOND) else {
		panic!("the third failure in a row rolls back");
	};
	assert!(reason.contains("health check"), "{reason}");
	assert_eq!(m.state, Some(State::RolledBack));
}

#[test]
fn health_gate_machine_never_rolls_back_outside_the_window_or_without_a_target() {
	// Past the window the release is the baseline: failures are the app's, not the deploy's.
	let mut late = healthy();
	assert_eq!(late.on(Signal::Crashed, 6 * MINUTE), Effect::None);
	assert_eq!(late.state, Some(State::Healthy));
	// A window of zero turns the feature off.
	let mut off = Machine::new(3, Duration::ZERO, false, true);
	off.on(Signal::Started, Duration::ZERO);
	off.on(Signal::Deployed, Duration::ZERO);
	assert_eq!(off.on(Signal::Crashed, SECOND), Effect::None);
	// Nothing to go back to.
	let mut first = Machine::new(3, 5 * MINUTE, false, false);
	first.on(Signal::Started, Duration::ZERO);
	first.on(Signal::Deployed, Duration::ZERO);
	assert_eq!(first.on(Signal::Crashed, SECOND), Effect::None);
	assert_eq!(first.state, Some(State::Healthy));
	// A rollback is never rolled back: two bad releases would flip forever.
	let mut again = Machine::new(3, 5 * MINUTE, true, true);
	again.on(Signal::Started, Duration::ZERO);
	again.on(Signal::Deployed, Duration::ZERO);
	assert_eq!(again.on(Signal::Crashed, SECOND), Effect::None);
}

#[test]
fn health_gate_watch_reads_a_container_report() {
	let report = |state, health, gave_up| ContainerHealth {
		id: "c".into(),
		name: "podkit-app-1-2".into(),
		state,
		exit_code: None,
		health,
		gave_up,
	};
	let passing = report(ContainerState::Running, Some(HealthStatus::Healthy), false);
	assert_eq!(watch::signal(&passing), Signal::CheckPassed);
	assert_eq!(
		watch::signal(&report(ContainerState::Running, None, false)),
		Signal::CheckPassed
	);
	assert_eq!(
		watch::signal(&report(
			ContainerState::Running,
			Some(HealthStatus::Starting),
			false
		)),
		Signal::CheckPassed
	);
	assert_eq!(
		watch::signal(&report(
			ContainerState::Running,
			Some(HealthStatus::Unhealthy),
			false
		)),
		Signal::CheckFailed
	);
	// Dying and being restarted is a failed check; the supervisor giving up is a crash.
	assert_eq!(
		watch::signal(&report(ContainerState::Exited, None, false)),
		Signal::CheckFailed
	);
	assert_eq!(
		watch::signal(&report(ContainerState::Exited, None, true)),
		Signal::Crashed
	);
}

// --- the settings -----------------------------------------------------------------------------

#[test]
fn health_gate_config_becomes_the_gate_the_agent_runs() {
	let c = HealthConfig {
		path: "/up".into(),
		expect_status: 204,
		interval_s: 2,
		timeout_s: 1,
		retries: 4,
		start_period_s: 7,
		gate_timeout_s: 45,
		drain_s: 9,
		..HealthConfig::default()
	};
	let gate = c.to_gate(8080);
	assert_eq!(
		gate.check,
		HealthCheck::Http {
			path: "/up".into(),
			status: 204,
			port: 8080
		}
	);
	assert_eq!(
		(
			gate.interval_ms,
			gate.timeout_ms,
			gate.retries,
			gate.start_period_ms
		),
		(2000, 1000, 4, 7000)
	);
	assert_eq!((gate.gate_timeout_s, gate.drain_s), (45, 9));
	let cmd = HealthConfig {
		kind: Kind::Command,
		command: Some("pg_isready -q".into()),
		..HealthConfig::default()
	};
	assert_eq!(
		cmd.to_gate(5432).check,
		HealthCheck::Command {
			command: "pg_isready -q".into()
		}
	);
}

#[test]
fn health_gate_config_validation_names_the_field() {
	assert!(HealthConfig::default().validate().is_ok());
	let bad = |edit: fn(&mut HealthConfig)| {
		let mut c = HealthConfig::default();
		edit(&mut c);
		c.validate().unwrap_err().to_string()
	};
	assert!(bad(|c| c.path = "up".into()).contains("path"));
	assert!(bad(|c| c.path = "/a b".into()).contains("path"));
	assert!(bad(|c| c.expect_status = 700).contains("status"));
	assert!(bad(|c| c.interval_s = 0).contains("interval"));
	assert!(bad(|c| c.retries = 99).contains("retries"));
	assert!(bad(|c| c.gate_timeout_s = 1).contains("deploy wait"));
	assert!(bad(|c| c.rollback_window_min = 500).contains("rollback window"));
	assert!(
		bad(|c| {
			c.kind = Kind::Command;
			c.command = None;
		})
		.contains("command")
	);
}

// --- the database -----------------------------------------------------------------------------

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
	async fn new(db: PgPool) -> Self {
		Self::with_state(db.clone(), state(db)).await
	}

	async fn with_state(db: PgPool, state: AppState) -> Self {
		let app = crate::routes::routes(state.clone()).await.unwrap();
		let (token, team) = user_with_team(&app, "alice").await;
		let (s, project) = call(
			&app,
			"POST",
			&format!("/api/v1/teams/{team}/projects"),
			Some(&token),
			Some(json!({ "name": "web", "slug": "web" })),
		)
		.await;
		assert!(s.is_success(), "create project: {s}");
		let (_, servers) = call(
			&app,
			"GET",
			&format!("/api/v1/teams/{team}/servers"),
			Some(&token),
			None,
		)
		.await;
		Self {
			app,
			state,
			db,
			token,
			team,
			project: project["id"].as_i64().unwrap(),
			server: servers[0]["id"].as_i64().unwrap(),
		}
	}

	/// An image application; its id.
	async fn application(&self, slug: &str) -> i64 {
		let (s, body) = call(
			&self.app,
			"POST",
			&format!(
				"/api/v1/teams/{}/projects/{}/applications",
				self.team, self.project
			),
			Some(&self.token),
			Some(json!({
				"name": slug, "slug": slug, "server_id": self.server, "container_port": 8080,
				"image": "docker.io/traefik/whoami:v1.10",
			})),
		)
		.await;
		assert_eq!(s, StatusCode::OK, "{body}");
		body["id"].as_i64().unwrap()
	}

	async fn save_config(&self, app: i64, config: Option<&HealthConfig>) {
		let user = UserId(user_id(&self.db, "alice").await);
		service::save(
			&self.db,
			user,
			TeamId(self.team),
			ProjectId(self.project),
			ApplicationId(app),
			config,
		)
		.await
		.unwrap();
	}

	async fn stored(&self, id: i64) -> crate::applications::Application {
		crate::applications::find_by_id(&self.db, ApplicationId(id))
			.await
			.unwrap()
			.unwrap()
	}

	async fn server(&self) -> crate::servers::Server {
		crate::servers::find(&self.db, ServerId(self.server))
			.await
			.unwrap()
			.unwrap()
	}

	async fn health_of(&self, id: i64) -> Option<(State, Option<String>, Option<i64>)> {
		of_deployment(&self.db, DeploymentId(id))
			.await
			.unwrap()
			.map(|h| (h.state, h.detail, h.rollback_of))
	}

	/// Queues a deploy and runs its job the way the worker does, failure handling included.
	async fn deploy(&self, app: i64) -> (Deployment, Result<(), JobFailure>) {
		let queued = deployments::queue(&self.state, &self.stored(app).await, None, None)
			.await
			.unwrap();
		let job = jobs::latest_for(&self.db, JobKind::Deploy, queued.id.0)
			.await
			.unwrap()
			.expect("a deploy job is queued");
		let mut log = self.state.logs.open(job.id).await.unwrap();
		let result =
			deployments::run::run(&self.state, &job, &CancellationToken::new(), &mut log).await;
		log.finish().await;
		if let Err(failure) = &result {
			jobs::fail_target(&self.state, job.kind, job.target_id, failure).await;
		}
		let after = deployments::find_by_id(&self.db, queued.id)
			.await
			.unwrap()
			.unwrap();
		(after, result)
	}
}

/// What the fake agent answers a deploy with.
#[derive(Clone)]
enum Script {
	/// Waits for health, then takes over.
	Healthy,
	/// Waits for health and never gets it.
	NeverHealthy(&'static str),
}

/// A fake agent on the local server's socket: records every deploy spec and plays `script`.
fn fake_agent(state: &AppState, script: Arc<Mutex<Script>>) -> Arc<Mutex<Vec<Value>>> {
	let seen = Arc::new(Mutex::new(Vec::new()));
	let recorder = seen.clone();
	let app = Router::new().route(
		"/v1/deploy",
		post(move |body: Bytes| async move {
			recorder
				.lock()
				.unwrap()
				.push(serde_json::from_slice(&body).unwrap());
			let waiting = [
				json!({ "type": "phase", "phase": "starting" }),
				json!({ "type": "phase", "phase": "health_check" }),
			];
			let end = match script.lock().unwrap().clone() {
				Script::Healthy => vec![
					json!({ "type": "phase", "phase": "swapping" }),
					json!({ "type": "deployed", "container_id": "c-new", "image": "docker.io/traefik/whoami:v1.10",
						"commit_sha": null }),
				],
				Script::NeverHealthy(why) => vec![json!({ "type": "failed", "message": why })],
			};
			let body = waiting
				.iter()
				.chain(&end)
				.fold(String::new(), |mut out, e| {
					out.push_str(&e.to_string());
					out.push('\n');
					out
				});
			([(header::CONTENT_TYPE, "application/x-ndjson")], body).into_response()
		}),
	);
	let listener = tokio::net::UnixListener::bind(&state.config.local_agent_socket).unwrap();
	tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
	seen
}

#[sqlx::test]
async fn health_gate_deploy_spec_carries_the_health_config(db: PgPool) {
	let s = Setup::new(db).await;
	let id = s.application("web1").await;
	let deployment = deployments::queue(&s.state, &s.stored(id).await, None, None)
		.await
		.unwrap();
	let build = || async {
		deployments::run::build_spec(
			&s.state,
			&s.stored(id).await,
			&deployment,
			&s.server().await,
		)
		.await
		.unwrap_or_else(|_| panic!("spec"))
	};

	// No setting, no gate: the agent keeps its plain check.
	assert!(build().await.health.is_none());

	let saved = HealthConfig {
		path: "/ready".into(),
		expect_status: 200,
		gate_timeout_s: 30,
		drain_s: 2,
		..HealthConfig::default()
	};
	s.save_config(id, Some(&saved)).await;
	let spec = build().await;
	assert_eq!(spec.health, Some(saved.to_gate(8080)));
	assert_eq!(spec.strategy, Strategy::Rolling);

	// Turning it off takes it out of the spec again.
	s.save_config(id, None).await;
	assert!(build().await.health.is_none());
	assert!(config(&s.db, ApplicationId(id)).await.unwrap().is_none());
}

#[sqlx::test]
async fn health_gate_recreate_keeps_the_volume_and_still_gates(db: PgPool) {
	let s = Setup::new(db).await;
	let id = s.application("web2").await;
	crate::volumes::insert(&s.db, ApplicationId(id), "/data")
		.await
		.unwrap();
	s.save_config(id, Some(&HealthConfig::default())).await;
	let deployment = deployments::queue(&s.state, &s.stored(id).await, None, None)
		.await
		.unwrap();
	let spec = deployments::run::build_spec(
		&s.state,
		&s.stored(id).await,
		&deployment,
		&s.server().await,
	)
	.await
	.unwrap_or_else(|_| panic!("spec"));
	// A volume can only be mounted by one container: the old one goes first, the volume stays
	// mounted at the same path, and the new container still waits for its check.
	assert_eq!(spec.strategy, Strategy::Recreate);
	assert_eq!(spec.volumes.len(), 1);
	assert_eq!(spec.volumes[0].path, "/data");
	assert!(spec.health.is_some());
}

#[sqlx::test]
async fn health_gate_deploy_that_becomes_healthy_is_recorded_as_healthy(db: PgPool) {
	let s = Setup::new(db).await;
	let script = Arc::new(Mutex::new(Script::Healthy));
	let seen = fake_agent(&s.state, script);
	let id = s.application("web3").await;
	s.save_config(id, Some(&HealthConfig::default())).await;

	let (d, result) = s.deploy(id).await;
	result.unwrap_or_else(|e| panic!("{e:?}"));
	assert_eq!(d.status, DeploymentStatus::Running);
	assert_eq!(s.health_of(d.id.0).await.unwrap().0, State::Healthy);
	assert_eq!(seen.lock().unwrap()[0]["health"]["check"]["type"], "http");
	assert_eq!(seen.lock().unwrap()[0]["health"]["check"]["port"], 8080);
}

#[sqlx::test]
async fn health_gate_deploy_that_never_becomes_healthy_fails_and_says_why(db: PgPool) {
	let s = Setup::new(db).await;
	let why = "the new container did not become healthy: the container is exited (exit code 1)";
	let script = Arc::new(Mutex::new(Script::NeverHealthy(why)));
	fake_agent(&s.state, script);
	let id = s.application("web4").await;
	s.save_config(id, Some(&HealthConfig::default())).await;

	let (d, result) = s.deploy(id).await;
	assert!(matches!(result, Err(JobFailure::Failed(ref m)) if m == why));
	assert_eq!(d.status, DeploymentStatus::Failed);
	assert_eq!(d.error_message.as_deref(), Some(why));
	let (state, detail, rollback_of) = s.health_of(d.id.0).await.unwrap();
	assert_eq!(state, State::Unhealthy);
	assert_eq!(detail.as_deref(), Some(why));
	assert_eq!(rollback_of, None);
}

#[sqlx::test]
async fn health_gate_deploy_without_a_gate_leaves_no_health_row(db: PgPool) {
	let s = Setup::new(db).await;
	fake_agent(&s.state, Arc::new(Mutex::new(Script::Healthy)));
	let id = s.application("web5").await;
	let (d, result) = s.deploy(id).await;
	result.unwrap_or_else(|e| panic!("{e:?}"));
	assert!(s.health_of(d.id.0).await.is_none());
}

/// A live release `bad` and the earlier one `good` it replaced, inside a gate with a window.
async fn released(s: &Setup, slug: &str) -> (i64, i64, i64) {
	let id = s.application(slug).await;
	s.save_config(id, Some(&HealthConfig::default())).await;
	let (good, bad) = (id * 10 + 1, id * 10 + 2);
	insert_deployment(
		&s.db,
		id,
		good,
		"stopped",
		Some("img@sha256:good"),
		Some("c-good"),
	)
	.await;
	insert_deployment(
		&s.db,
		id,
		bad,
		"running",
		Some("img@sha256:bad"),
		Some("c-bad"),
	)
	.await;
	sqlx::query("UPDATE deployments SET created_at = NOW() - INTERVAL '1 hour' WHERE id = $1")
		.bind(good)
		.execute(&s.db)
		.await
		.unwrap();
	crate::health_gate::set_state(&s.db, DeploymentId(bad), State::Healthy, None)
		.await
		.unwrap();
	(id, good, bad)
}

#[sqlx::test]
async fn health_gate_crash_inside_the_window_redeploys_the_previous_image(db: PgPool) {
	let s = Setup::new(db).await;
	let (id, good, bad) = released(&s, "web6").await;

	let watched = watch::candidates(&s.db, ServerId(s.server)).await.unwrap();
	assert_eq!(watched.len(), 1);
	watch::apply(&s.state, &watched[0], Signal::Crashed)
		.await
		.unwrap();

	// The bad release is marked, and a new deployment runs the good image.
	let (state, detail, _) = s.health_of(bad).await.unwrap();
	assert_eq!(state, State::RolledBack);
	assert!(detail.unwrap().contains("crashed"));
	let newest = deployments::list(&s.db, ApplicationId(id), None, 10)
		.await
		.unwrap();
	let rollback = newest
		.iter()
		.find(|d| d.id.0 != good && d.id.0 != bad)
		.unwrap();
	assert_eq!(rollback.status, DeploymentStatus::Queued);
	assert_eq!(rollback.image_tag.as_deref(), Some("img@sha256:good"));
	assert_eq!(s.health_of(rollback.id.0).await.unwrap().2, Some(bad));
	// It is queued like any deploy: a job exists for the worker.
	assert!(
		jobs::latest_for(&s.db, JobKind::Deploy, rollback.id.0)
			.await
			.unwrap()
			.is_some()
	);

	// The rolled-back release is no longer watched, so it is not rolled back twice.
	assert!(
		watch::candidates(&s.db, ServerId(s.server))
			.await
			.unwrap()
			.is_empty()
	);
}

#[sqlx::test]
async fn health_gate_failed_checks_count_across_looks_and_roll_back_at_the_threshold(db: PgPool) {
	let s = Setup::new(db).await;
	let (_, _, bad) = released(&s, "web7").await;
	for look in 1..=2 {
		let watched = watch::candidates(&s.db, ServerId(s.server)).await.unwrap();
		watch::apply(&s.state, &watched[0], Signal::CheckFailed)
			.await
			.unwrap();
		let failures: i32 =
			sqlx::query_scalar("SELECT failures FROM deployment_health WHERE deployment_id = $1")
				.bind(bad)
				.fetch_one(&s.db)
				.await
				.unwrap();
		assert_eq!(failures, look);
		assert_eq!(s.health_of(bad).await.unwrap().0, State::Healthy);
	}
	// One pass resets the count.
	let watched = watch::candidates(&s.db, ServerId(s.server)).await.unwrap();
	watch::apply(&s.state, &watched[0], Signal::CheckPassed)
		.await
		.unwrap();
	for _ in 0..3 {
		let watched = watch::candidates(&s.db, ServerId(s.server)).await.unwrap();
		assert_eq!(s.health_of(bad).await.unwrap().0, State::Healthy);
		watch::apply(&s.state, &watched[0], Signal::CheckFailed)
			.await
			.unwrap();
	}
	assert_eq!(s.health_of(bad).await.unwrap().0, State::RolledBack);
}

#[sqlx::test]
async fn health_gate_nothing_to_roll_back_to_leaves_the_release_alone(db: PgPool) {
	let s = Setup::new(db).await;
	let id = s.application("web8").await;
	s.save_config(id, Some(&HealthConfig::default())).await;
	insert_deployment(
		&s.db,
		id,
		99,
		"running",
		Some("img@sha256:only"),
		Some("c-only"),
	)
	.await;
	crate::health_gate::set_state(&s.db, DeploymentId(99), State::Healthy, None)
		.await
		.unwrap();
	let watched = watch::candidates(&s.db, ServerId(s.server)).await.unwrap();
	watch::apply(&s.state, &watched[0], Signal::Crashed)
		.await
		.unwrap();
	assert_eq!(s.health_of(99).await.unwrap().0, State::Healthy);
	assert_eq!(
		deployments::list(&s.db, ApplicationId(id), None, 10)
			.await
			.unwrap()
			.len(),
		1
	);
}

#[sqlx::test]
async fn health_gate_a_release_past_its_window_is_not_watched(db: PgPool) {
	let s = Setup::new(db).await;
	let (_, _, bad) = released(&s, "web9").await;
	sqlx::query("UPDATE deployments SET finished_at = NOW() - INTERVAL '10 minutes' WHERE id = $1")
		.bind(bad)
		.execute(&s.db)
		.await
		.unwrap();
	assert!(
		watch::candidates(&s.db, ServerId(s.server))
			.await
			.unwrap()
			.is_empty()
	);
	// A window of zero minutes is the switch for the whole feature.
	sqlx::query("UPDATE deployments SET finished_at = NOW() WHERE id = $1")
		.bind(bad)
		.execute(&s.db)
		.await
		.unwrap();
	assert_eq!(
		watch::candidates(&s.db, ServerId(s.server))
			.await
			.unwrap()
			.len(),
		1
	);
	sqlx::query("UPDATE application_health SET rollback_window_min = 0")
		.execute(&s.db)
		.await
		.unwrap();
	assert!(
		watch::candidates(&s.db, ServerId(s.server))
			.await
			.unwrap()
			.is_empty()
	);
}

#[sqlx::test]
async fn health_gate_settings_need_update_permission_and_stay_inside_the_team(db: PgPool) {
	let s = Setup::new(db).await;
	let id = s.application("web10").await;
	let config = HealthConfig::default();
	let alice = UserId(user_id(&s.db, "alice").await);

	// Another team's member cannot read or change the settings: the application is not there.
	let (_, bob_team) = user_with_team(&s.app, "bob").await;
	let bob = UserId(user_id(&s.db, "bob").await);
	let theirs = service::save(
		&s.db,
		bob,
		TeamId(bob_team),
		ProjectId(s.project),
		ApplicationId(id),
		Some(&config),
	)
	.await;
	assert!(
		matches!(theirs, Err(crate::error::ServerError::ApplicationNotFound)),
		"{theirs:?}"
	);
	let read = service::get(
		&s.db,
		bob,
		TeamId(bob_team),
		ProjectId(s.project),
		ApplicationId(id),
	)
	.await;
	assert!(read.is_err());
	assert!(config_of(&s, id).await.is_none());

	// The owner can, and reads it back.
	service::save(
		&s.db,
		alice,
		TeamId(s.team),
		ProjectId(s.project),
		ApplicationId(id),
		Some(&config),
	)
	.await
	.unwrap();
	assert_eq!(config_of(&s, id).await, Some(config));
}

async fn config_of(s: &Setup, id: i64) -> Option<HealthConfig> {
	config(&s.db, ApplicationId(id)).await.unwrap()
}

// --- live ---------------------------------------------------------------------------------------

/// The real agent in this process, a real Podman and Traefik: deploy a good image, then one
/// that exits at once. The URL answers 200 the whole time (polled every 40 ms through the
/// deploy), the bad deploy fails and says why, and the good container is still the one serving.
///
/// Needs the network (traefik/whoami, alpine) and a `podman.sock`; binds ports 21951 and 21952.
/// `cargo test -p server --all-features -- --ignored zero_downtime_live`
#[sqlx::test]
#[ignore = "pulls traefik/whoami and alpine from docker.io with a real podman.sock, binds ports 21951 and 21952"]
async fn health_gate_zero_downtime_live_a_broken_release_never_takes_traffic(db: PgPool) {
	use std::sync::atomic::{AtomicU32, Ordering};

	let mut state = state(db.clone());
	{
		let config = Arc::make_mut(&mut state.config);
		config.ingress_port = 21951;
		config.https_port = 21952;
	}
	// The health check of a gated container is the agent binary, run inside it: here the agent
	// is this test, so point at the real one.
	let target = std::env::current_exe()
		.unwrap()
		.parent()
		.and_then(std::path::Path::parent)
		.unwrap()
		.to_path_buf();
	let build = std::process::Command::new("cargo")
		.args(["build", "-p", "agent", "--bin", "podkit-agent"])
		.env("SQLX_OFFLINE", "true")
		.status()
		.expect("cargo build -p agent");
	assert!(build.success());
	agent::health::set_agent_exe(target.join("podkit-agent"));

	let podman = agent::podman::Podman::connect(&agent::podman::local_socket_path())
		.expect("connect to podman.sock");
	let had = |image: &'static str| {
		let podman = podman.clone();
		async move { podman.inspect_image(image).await.ok().flatten().is_some() }
	};
	let (had_whoami, had_alpine) = (
		had("docker.io/traefik/whoami").await,
		had("docker.io/library/alpine:3").await,
	);
	let data_dir = tempfile::tempdir().unwrap().keep();
	let ctx = Arc::new(agent::Ctx {
		docker: podman.docker().clone(),
		supervisor: agent::supervisor::Supervisor::start(podman.clone(), &data_dir).await,
		data_dir,
		podman_socket: agent::podman::local_socket_path(),
		http: reqwest::Client::new(),
	});
	let listener = tokio::net::UnixListener::bind(&state.config.local_agent_socket).unwrap();
	tokio::spawn(async move {
		axum::serve(listener, agent::routes::router(ctx))
			.await
			.unwrap();
	});

	let s = Setup::with_state(db, state).await;
	let (status, created) = call(
		&s.app,
		"POST",
		&format!(
			"/api/v1/teams/{}/projects/{}/applications",
			s.team, s.project
		),
		Some(&s.token),
		Some(json!({
			"name": "zd-live", "slug": "zd-live", "server_id": s.server, "container_port": 80,
			"image": "docker.io/traefik/whoami",
		})),
	)
	.await;
	assert_eq!(status, StatusCode::OK, "{created}");
	let id = created["id"].as_i64().unwrap();
	let host = created["url"].as_str().unwrap().to_owned();
	s.save_config(
		id,
		Some(&HealthConfig {
			path: "/".into(),
			interval_s: 1,
			timeout_s: 2,
			retries: 3,
			start_period_s: 0,
			gate_timeout_s: 20,
			drain_s: 1,
			..HealthConfig::default()
		}),
	)
	.await;

	let outcome = std::panic::AssertUnwindSafe(async {
		let (first, result) = s.deploy(id).await;
		result.unwrap_or_else(|e| panic!("the good image failed to deploy: {e:?}"));
		assert_eq!(first.status, DeploymentStatus::Running);
		assert_eq!(s.health_of(first.id.0).await.unwrap().0, State::Healthy);
		let first_container = first.container_id.clone().unwrap();

		let client = reqwest::Client::new();
		let get = |client: reqwest::Client, host: String| async move {
			client
				.get("http://127.0.0.1:21951/")
				.header("Host", host)
				.timeout(Duration::from_secs(2))
				.send()
				.await
				.map(|r| r.status())
		};
		let mut up = false;
		for _ in 0..40 {
			if get(client.clone(), host.clone()).await.ok() == Some(StatusCode::OK) {
				up = true;
				break;
			}
			tokio::time::sleep(Duration::from_millis(500)).await;
		}
		assert!(up, "{host} never answered 200 through ingress");

		// From here on every request must be answered 200.
		let (total, bad) = (Arc::new(AtomicU32::new(0)), Arc::new(AtomicU32::new(0)));
		let stop = CancellationToken::new();
		let poller = {
			let (total, bad, stop) = (total.clone(), bad.clone(), stop.clone());
			let (client, host) = (client.clone(), host.clone());
			tokio::spawn(async move {
				while !stop.is_cancelled() {
					total.fetch_add(1, Ordering::Relaxed);
					if get(client.clone(), host.clone()).await.ok() != Some(StatusCode::OK) {
						bad.fetch_add(1, Ordering::Relaxed);
					}
					tokio::time::sleep(Duration::from_millis(40)).await;
				}
			})
		};

		// A release that exits at once.
		let (status, body) = call(
			&s.app,
			"PATCH",
			&format!(
				"/api/v1/teams/{}/projects/{}/applications/{id}",
				s.team, s.project
			),
			Some(&s.token),
			Some(json!({
				"name": "zd-live", "slug": "zd-live", "container_port": 80,
				"image": "docker.io/library/alpine:3",
			})),
		)
		.await;
		assert_eq!(status, StatusCode::OK, "{body}");
		let (second, result) = s.deploy(id).await;
		tokio::time::sleep(Duration::from_secs(2)).await;
		stop.cancel();
		poller.await.unwrap();

		let Err(JobFailure::Failed(message)) = result else {
			panic!("a release that exits at once must not deploy: {result:?}");
		};
		assert!(message.contains("did not become healthy"), "{message}");
		assert_eq!(second.status, DeploymentStatus::Failed);
		let (state, detail, _) = s.health_of(second.id.0).await.unwrap();
		assert_eq!(state, State::Unhealthy);
		assert!(detail.is_some());
		let (total, bad) = (total.load(Ordering::Relaxed), bad.load(Ordering::Relaxed));
		assert!(total > 100, "the poller only made {total} requests");
		assert_eq!(
			bad, 0,
			"{bad} of {total} requests were not answered 200 during the deploy"
		);

		// The old container is still the one serving, and it is the only one left.
		let old = podman
			.inspect(&first_container)
			.await
			.unwrap()
			.expect("the old container");
		assert_eq!(old.state.and_then(|st| st.running), Some(true));
		let left = podman
			.service_containers(&format!("app-{id}"))
			.await
			.unwrap();
		assert_eq!(left.len(), 1, "{left:?}");
		assert_eq!(
			get(client, host).await.ok(),
			Some(StatusCode::OK),
			"the old container no longer answers"
		);
	});
	let result = futures_util::FutureExt::catch_unwind(outcome).await;

	// Leave nothing behind, whatever happened.
	for container in podman
		.service_containers(&format!("app-{id}"))
		.await
		.unwrap_or_default()
	{
		if let Some(name) = agent::podman::summary_name(&container) {
			let _ = podman.remove(name).await;
		}
	}
	let _ = podman.remove(agent::ingress::TRAEFIK_CONTAINER_NAME).await;
	if !had_whoami {
		let _ = podman.remove_image("docker.io/traefik/whoami").await;
	}
	if !had_alpine {
		let _ = podman.remove_image("docker.io/library/alpine:3").await;
	}
	if let Err(panic) = result {
		std::panic::resume_unwind(panic);
	}
}
