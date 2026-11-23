//! Replica tests: the settings and their limits, what the deploy hands the agent, the refusal
//! with volumes, tenancy and permissions, the per-replica status and its aggregation, and the
//! live run with a real Podman. The agent is a fake that records what it is asked.
//!
//! `cargo test -p server --all-features -- replicas`

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::body::Bytes;
use axum::extract::Path;
use axum::http::{StatusCode, header};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use proto::{ContainerHealth, ContainerInfo, ContainerState, ContainerStats, HealthStatus};
use proto::{ResourceRef, Stats, Strategy};
use serde_json::json;
use sqlx::PgPool;

use super::status::{self, ReplicaState};
use super::{ReplicaConfig, config, service};
use crate::error::ServerError;
use crate::ids::{ApplicationId, ProjectId, ServerId, TeamId, UserId};
use crate::state::AppState;
use crate::tests::{call, insert_deployment, state, user_id, user_with_team};

// --- the settings -----------------------------------------------------------------------------

fn scaled(replicas: i32) -> ReplicaConfig {
	ReplicaConfig {
		replicas,
		..ReplicaConfig::default()
	}
}

#[test]
fn replicas_one_is_the_default_and_runs_a_single_container() {
	let c = ReplicaConfig::default();
	assert_eq!(c.replicas, 1);
	assert!(c.validate().is_ok());
	assert!(!c.is_scaled());
	assert_eq!(c.spec(true).count, 1);
}

#[test]
fn replicas_spec_carries_the_count_the_rollout_and_the_cookie() {
	let c = ReplicaConfig {
		replicas: 3,
		sticky: true,
		max_surge: 2,
		max_unavailable: 0,
		shared_volumes: false,
	};
	let spec = c.spec(true);
	assert_eq!(
		(
			spec.count,
			spec.max_surge,
			spec.max_unavailable,
			spec.sticky
		),
		(3, 2, 0, true)
	);
	// No route, no cookie: an MCP server is never routed.
	assert!(!c.spec(false).sticky);
}

#[test]
fn replicas_limits_name_the_field() {
	let bad = |c: ReplicaConfig| match c.validate() {
		Err(ServerError::Validation(m)) => m,
		other => panic!("expected a validation error, got {other:?}"),
	};
	assert!(bad(scaled(0)).contains("Replicas must be between 1 and 10"));
	assert!(bad(scaled(11)).contains("Replicas must be between 1 and 10"));
	assert!(
		bad(ReplicaConfig {
			max_surge: 11,
			..scaled(2)
		})
		.contains("surge")
	);
	assert!(
		bad(ReplicaConfig {
			max_surge: 0,
			max_unavailable: 0,
			..scaled(2)
		})
		.contains("room to move")
	);
	assert!(
		ReplicaConfig {
			max_surge: 0,
			max_unavailable: 1,
			..scaled(10)
		}
		.validate()
		.is_ok()
	);
}

#[test]
fn replicas_volumes_are_refused_unless_marked_shared() {
	let refused = scaled(2).check_volumes(1);
	assert!(
		matches!(&refused, Err(ServerError::Validation(m)) if m.contains("volumes")),
		"{refused:?}"
	);
	// One replica never shares anything; no volumes is stateless; vouching lifts it.
	assert!(scaled(1).check_volumes(3).is_ok());
	assert!(scaled(5).check_volumes(0).is_ok());
	let shared = ReplicaConfig {
		shared_volumes: true,
		..scaled(5)
	};
	assert!(shared.check_volumes(3).is_ok());
}

// --- per-replica status -------------------------------------------------------------------------

fn report(
	name: &str,
	state: ContainerState,
	health: Option<HealthStatus>,
	gave_up: bool,
	restarts: u32,
) -> ContainerHealth {
	ContainerHealth {
		id: format!("id-{name}"),
		name: name.to_owned(),
		state,
		exit_code: None,
		health,
		gave_up,
		restarts,
	}
}

fn stats(name: &str, cpu: Option<f64>, mem: u64) -> ContainerStats {
	ContainerStats {
		name: name.to_owned(),
		resource: ResourceRef {
			kind: "application".into(),
			id: 1,
			service: None,
		},
		cpu_percent: cpu,
		mem_used: mem,
	}
}

#[test]
fn replicas_each_container_has_its_own_state() {
	use ContainerState as C;
	let state = |c, h, gave_up| status::state_of(&report("x", c, h, gave_up, 0));
	assert_eq!(
		state(C::Running, Some(HealthStatus::Healthy), false),
		ReplicaState::Healthy
	);
	assert_eq!(state(C::Running, None, false), ReplicaState::Running);
	assert_eq!(
		state(C::Running, Some(HealthStatus::Starting), false),
		ReplicaState::Starting
	);
	assert_eq!(
		state(C::Running, Some(HealthStatus::Unhealthy), false),
		ReplicaState::Unhealthy
	);
	// Dead and still being restarted, or dead for good.
	assert_eq!(state(C::Exited, None, false), ReplicaState::Restarting);
	assert_eq!(state(C::Exited, None, true), ReplicaState::Crashed);
	assert_eq!(state(C::Paused, None, false), ReplicaState::Stopped);
	// Only a running replica without a failing check takes traffic.
	let up: Vec<_> = [
		ReplicaState::Healthy,
		ReplicaState::Running,
		ReplicaState::Starting,
		ReplicaState::Unhealthy,
		ReplicaState::Restarting,
		ReplicaState::Crashed,
		ReplicaState::Stopped,
	]
	.into_iter()
	.filter(|s| s.is_up())
	.collect();
	assert_eq!(up, [ReplicaState::Healthy, ReplicaState::Running]);
}

#[test]
fn replicas_names_belong_to_one_deployment() {
	assert_eq!(status::number_of("podkit-app-4-9", 4, 9), Some(1));
	assert_eq!(status::number_of("podkit-app-4-9-r3", 4, 9), Some(3));
	assert_eq!(status::number_of("podkit-app-4-9-r10", 4, 9), Some(10));
	// Another deployment of the same app, an app whose id starts the same, a stack service.
	for other in [
		"podkit-app-4-8",
		"podkit-app-4-90",
		"podkit-app-4-90-r2",
		"podkit-app-44-9",
		"podkit-app-4-9-rx",
		"podkit-app-4-9-r",
		"podkit-app-4-9-web",
		"podkit-stack-4-9",
	] {
		assert_eq!(status::number_of(other, 4, 9), None, "{other}");
	}
}

#[test]
fn replicas_status_rows_join_health_cpu_and_memory_in_order() {
	let reports = [
		report(
			"podkit-app-4-9-r3",
			ContainerState::Running,
			Some(HealthStatus::Healthy),
			false,
			0,
		),
		report(
			"podkit-app-4-9",
			ContainerState::Running,
			Some(HealthStatus::Healthy),
			false,
			2,
		),
		report("podkit-app-4-9-r2", ContainerState::Exited, None, false, 4),
		report("podkit-app-4-8", ContainerState::Running, None, false, 0),
	];
	let readings = [
		stats("podkit-app-4-9", Some(3.5), 40_000_000),
		stats("/podkit-app-4-9-r3", None, 10_000_000),
		stats("podkit-app-4-9-r2", Some(9.0), 99),
		stats("podkit-app-4-8", Some(1.0), 1),
	];
	let rows = status::rows(4, 9, &reports, &readings);
	let numbers: Vec<_> = rows.iter().map(|r| r.number).collect();
	assert_eq!(
		numbers,
		[1, 2, 3],
		"the old deployment's container is left out"
	);
	assert_eq!(rows[0].restarts, 2);
	assert_eq!((rows[0].cpu, rows[0].mem), (Some(3.5), Some(40_000_000)));
	// A replica that is down has no live figures, even if the agent had a stale reading.
	assert_eq!(rows[1].state, ReplicaState::Restarting);
	assert_eq!((rows[1].cpu, rows[1].mem), (None, None));
	// The agent has no CPU yet for a fresh one; the memory is still its own.
	assert_eq!((rows[2].cpu, rows[2].mem), (None, Some(10_000_000)));
}

#[test]
fn replicas_status_aggregates_into_one_verdict() {
	let row = |number, state| status::ReplicaRow {
		number,
		name: format!("c{number}"),
		state,
		restarts: 1,
		cpu: None,
		mem: None,
	};
	let all = [
		row(1, ReplicaState::Healthy),
		row(2, ReplicaState::Running),
		row(3, ReplicaState::Healthy),
	];
	let verdict = status::rollup(3, &all);
	assert!(verdict.complete());
	assert_eq!(verdict.label(), "3 of 3 up");
	assert_eq!(verdict.restarts, 3);

	let hurt = [
		row(1, ReplicaState::Healthy),
		row(2, ReplicaState::Crashed),
		row(3, ReplicaState::Starting),
	];
	let verdict = status::rollup(3, &hurt);
	assert!(!verdict.complete());
	assert_eq!((verdict.up, verdict.found), (1, 3));
	assert_eq!(verdict.label(), "1 of 3 up");

	// A replica the agent does not list counts against the verdict like one that is down.
	let missing = status::rollup(3, &all[..2]);
	assert!(!missing.complete());
	assert_eq!(missing.label(), "2 of 3 up");
	assert_eq!(status::rollup(2, &[]).label(), "0 of 2 up");
}

// --- the database -------------------------------------------------------------------------------

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

	async fn application(&self, slug: &str) -> i64 {
		self.application_of(slug, "docker.io/traefik/whoami:v1.10", 8080)
			.await
			.0
	}

	/// An image application; its id and URL.
	async fn application_of(&self, slug: &str, image: &str, port: i32) -> (i64, String) {
		let (s, body) = call(
			&self.app,
			"POST",
			&format!(
				"/api/v1/teams/{}/projects/{}/applications",
				self.team, self.project
			),
			Some(&self.token),
			Some(json!({
				"name": slug, "slug": slug, "server_id": self.server, "container_port": port,
				"image": image,
			})),
		)
		.await;
		assert_eq!(s, StatusCode::OK, "{body}");
		(
			body["id"].as_i64().unwrap(),
			body["url"].as_str().unwrap_or_default().to_owned(),
		)
	}

	async fn alice(&self) -> UserId {
		UserId(user_id(&self.db, "alice").await)
	}

	async fn save(&self, app: i64, c: &ReplicaConfig) -> Result<(), ServerError> {
		service::save(
			&self.db,
			self.alice().await,
			TeamId(self.team),
			ProjectId(self.project),
			ApplicationId(app),
			c,
		)
		.await
	}

	async fn spec(&self, app: i64) -> Result<proto::DeploySpec, String> {
		let application = crate::applications::find_by_id(&self.db, ApplicationId(app))
			.await
			.unwrap()
			.unwrap();
		let deployment = crate::deployments::queue(&self.state, &application, None, None)
			.await
			.unwrap();
		let server = crate::servers::find(&self.db, ServerId(self.server))
			.await
			.unwrap()
			.unwrap();
		crate::deployments::run::build_spec(&self.state, &application, &deployment, &server)
			.await
			.map_err(|e| format!("{e:?}"))
	}
}

#[sqlx::test]
async fn replicas_deploy_spec_is_generated_from_the_setting(db: PgPool) {
	let s = Setup::new(db).await;
	let id = s.application("web1").await;

	// Nothing saved, or the defaults saved on an application that never had a setting: the
	// agent is told nothing, and the containers keep the labels they have.
	assert_eq!(s.spec(id).await.unwrap().replicas, None);
	s.save(id, &ReplicaConfig::default()).await.unwrap();
	assert_eq!(s.spec(id).await.unwrap().replicas, None);
	assert!(config(&s.db, ApplicationId(id)).await.unwrap().is_none());

	let wanted = ReplicaConfig {
		replicas: 3,
		sticky: true,
		max_surge: 1,
		max_unavailable: 0,
		shared_volumes: false,
	};
	s.save(id, &wanted).await.unwrap();
	let spec = s.spec(id).await.unwrap();
	let replicas = spec.replicas.expect("three replicas");
	assert_eq!(
		(replicas.count, replicas.max_surge, replicas.max_unavailable),
		(3, 1, 0)
	);
	assert!(replicas.sticky);
	assert_eq!(spec.strategy, Strategy::Rolling);
	// One service for all of them: the same Traefik name on every copy, whatever the count.
	assert_eq!(spec.service, format!("app-{id}"));
	// The wire form is what the agent reads.
	let wire = serde_json::to_value(&spec).unwrap();
	assert_eq!(wire["replicas"]["count"], 3);
	assert_eq!(wire["replicas"]["sticky"], true);

	// Scaled back down it still says so: one replica with the same labels, so the route does
	// not drop while the deploy swaps the containers.
	s.save(id, &ReplicaConfig::default()).await.unwrap();
	let back = s
		.spec(id)
		.await
		.unwrap()
		.replicas
		.expect("a setting is kept");
	assert_eq!((back.count, back.sticky), (1, false));
}

#[sqlx::test]
async fn replicas_refused_with_volumes_unless_shared(db: PgPool) {
	let s = Setup::new(db).await;
	let id = s.application("web2").await;
	crate::volumes::insert(&s.db, ApplicationId(id), "/data")
		.await
		.unwrap();

	let refused = s.save(id, &scaled(2)).await;
	assert!(
		matches!(&refused, Err(ServerError::Validation(m)) if m.contains("volumes")),
		"{refused:?}"
	);
	assert!(config(&s.db, ApplicationId(id)).await.unwrap().is_none());
	// One replica is fine with a volume, and keeps the recreate it always had.
	s.save(id, &scaled(1)).await.unwrap();
	assert_eq!(s.spec(id).await.unwrap().strategy, Strategy::Recreate);

	// The owner vouches for the volume: the replicas roll like any others.
	let shared = ReplicaConfig {
		shared_volumes: true,
		..scaled(2)
	};
	s.save(id, &shared).await.unwrap();
	let spec = s.spec(id).await.unwrap();
	assert_eq!(spec.strategy, Strategy::Rolling);
	assert_eq!(spec.volumes.len(), 1);
	assert_eq!(spec.replicas.unwrap().count, 2);
}

#[sqlx::test]
async fn replicas_a_volume_added_later_stops_the_deploy_before_anything_starts(db: PgPool) {
	let s = Setup::new(db).await;
	let id = s.application("web3").await;
	s.save(id, &scaled(2)).await.unwrap();
	crate::volumes::insert(&s.db, ApplicationId(id), "/data")
		.await
		.unwrap();
	let refused = s.spec(id).await.unwrap_err();
	assert!(refused.contains("volumes"), "{refused}");
}

#[sqlx::test]
async fn replicas_settings_need_update_permission_and_stay_inside_the_team(db: PgPool) {
	let s = Setup::new(db).await;
	let id = s.application("web4").await;
	let alice = s.alice().await;

	// Another team's member cannot read or change them: the application is not there.
	let (_, bob_team) = user_with_team(&s.app, "bob").await;
	let bob = UserId(user_id(&s.db, "bob").await);
	let theirs = service::save(
		&s.db,
		bob,
		TeamId(bob_team),
		ProjectId(s.project),
		ApplicationId(id),
		&scaled(4),
	)
	.await;
	assert!(
		matches!(theirs, Err(ServerError::ApplicationNotFound)),
		"{theirs:?}"
	);
	let read = service::settings(
		&s.db,
		bob,
		TeamId(bob_team),
		ProjectId(s.project),
		ApplicationId(id),
	)
	.await;
	assert!(matches!(read, Err(ServerError::ApplicationNotFound)));
	assert!(config(&s.db, ApplicationId(id)).await.unwrap().is_none());

	// The owner can, and reads it back.
	s.save(id, &scaled(4)).await.unwrap();
	let back = service::settings(
		&s.db,
		alice,
		TeamId(s.team),
		ProjectId(s.project),
		ApplicationId(id),
	)
	.await
	.unwrap();
	assert_eq!(back.config.replicas, 4);

	// A member whose role cannot update applications is refused, whatever they send.
	user_with_team(&s.app, "carol").await;
	let carol = UserId(user_id(&s.db, "carol").await);
	let (st, _) = call(
		&s.app,
		"POST",
		&format!("/api/v1/teams/{}/members", s.team),
		Some(&s.token),
		Some(json!({ "email": "carol@example.com", "role_id": 2 })),
	)
	.await;
	assert!(st.is_success());
	let role = crate::tests::member_role(&s.db, s.team, "carol").await;
	crate::tests::revoke_permission(&s.db, role, "update", "application").await;
	let refused = service::save(
		&s.db,
		carol,
		TeamId(s.team),
		ProjectId(s.project),
		ApplicationId(id),
		&scaled(9),
	)
	.await;
	assert!(
		matches!(refused, Err(ServerError::Forbidden)),
		"{refused:?}"
	);
	assert_eq!(
		config(&s.db, ApplicationId(id))
			.await
			.unwrap()
			.unwrap()
			.replicas,
		4
	);
}

#[sqlx::test]
async fn replicas_a_change_is_audited_and_a_no_op_is_not(db: PgPool) {
	let s = Setup::new(db).await;
	let id = s.application("web5").await;
	let count = || async {
		sqlx::query_scalar::<_, i64>(
			"SELECT COUNT(*) FROM audit_events WHERE action = 'app.replicas' AND target_id = $1",
		)
		.bind(id)
		.fetch_one(&s.db)
		.await
		.unwrap()
	};
	s.save(id, &scaled(3)).await.unwrap();
	assert_eq!(count().await, 1);
	s.save(id, &scaled(3)).await.unwrap();
	assert_eq!(
		count().await,
		1,
		"saving the same thing again is not a change"
	);
	s.save(id, &scaled(2)).await.unwrap();
	assert_eq!(count().await, 2);
}

// --- what the agent says ------------------------------------------------------------------------

/// A fake agent on the local server's socket. `containers` is what it lists and reports on;
/// it records the stop calls.
fn fake_agent(
	state: &AppState,
	containers: Vec<ContainerHealth>,
	readings: Vec<ContainerStats>,
) -> Arc<Mutex<Vec<String>>> {
	let stopped = Arc::new(Mutex::new(Vec::new()));
	let recorder = stopped.clone();
	let listed = containers.clone();
	let reports = containers;
	let router = Router::new()
		.route(
			"/v1/containers",
			get(move || {
				let all: Vec<ContainerInfo> = listed
					.iter()
					.map(|c| ContainerInfo {
						id: c.id.clone(),
						name: c.name.clone(),
						state: c.state,
						exit_code: None,
						gave_up: c.gave_up,
					})
					.collect();
				async move { axum::Json(all) }
			}),
		)
		.route(
			"/v1/containers/{id}/health",
			get(move |Path(id): Path<String>| {
				let found = reports.iter().find(|c| c.id == id).cloned();
				async move {
					match found {
						Some(report) => axum::Json(report).into_response(),
						None => StatusCode::NOT_FOUND.into_response(),
					}
				}
			}),
		)
		.route(
			"/v1/containers/{id}/stop",
			post(move |Path(id): Path<String>| {
				recorder.lock().unwrap().push(id);
				async { StatusCode::NO_CONTENT }
			}),
		)
		.route(
			"/v1/stats",
			get(move || {
				let figures = Stats {
					containers: readings.clone(),
					..Stats::default()
				};
				async move { axum::Json(figures) }
			}),
		)
		.route(
			"/v1/deploy",
			post(move |_: Bytes| async move {
				let body = format!(
					"{}\n{}\n",
					json!({ "type": "phase", "phase": "starting" }),
					json!({ "type": "deployed", "container_id": "c-new",
						"image": "docker.io/traefik/whoami:v1.10", "commit_sha": null }),
				);
				([(header::CONTENT_TYPE, "application/x-ndjson")], body).into_response()
			}),
		);
	let listener = tokio::net::UnixListener::bind(&state.config.local_agent_socket).unwrap();
	tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
	stopped
}

#[sqlx::test]
async fn replicas_status_lists_every_replica_of_the_running_deployment(db: PgPool) {
	let s = Setup::new(db).await;
	let id = s.application("web6").await;
	insert_deployment(
		&s.db,
		id,
		700,
		"running",
		Some("img"),
		Some(&format!("id-podkit-app-{id}-700")),
	)
	.await;
	let name = |suffix: &str| format!("podkit-app-{id}-700{suffix}");
	let healthy = |n: &str, restarts| {
		report(
			n,
			ContainerState::Running,
			Some(HealthStatus::Healthy),
			false,
			restarts,
		)
	};
	fake_agent(
		&s.state,
		vec![
			healthy(&name(""), 0),
			healthy(&name("-r2"), 3),
			report(&name("-r3"), ContainerState::Exited, None, true, 5),
			// A container of an older deployment of the same application.
			healthy(&format!("podkit-app-{id}-699"), 0),
		],
		vec![
			stats(&name(""), Some(2.0), 30_000_000),
			stats(&name("-r2"), Some(4.0), 31_000_000),
		],
	);

	// One replica has no list: the header and Monitoring show nothing extra.
	let alice = s.alice().await;
	let status = || {
		service::status(
			&s.state,
			alice,
			TeamId(s.team),
			ProjectId(s.project),
			ApplicationId(id),
		)
	};
	assert!(status().await.unwrap().is_none());

	s.save(id, &scaled(3)).await.unwrap();
	let live = status()
		.await
		.unwrap()
		.expect("a scaled application has a list");
	let states: Vec<_> = live.rows.iter().map(|r| (r.number, r.state)).collect();
	assert_eq!(
		states,
		[
			(1, ReplicaState::Healthy),
			(2, ReplicaState::Healthy),
			(3, ReplicaState::Crashed)
		]
	);
	assert_eq!(live.rows[1].restarts, 3);
	assert_eq!(live.rows[0].cpu, Some(2.0));
	assert_eq!(live.rollup.label(), "2 of 3 up");
	assert!(!live.rollup.complete());
}

#[sqlx::test]
async fn replicas_stop_takes_every_replica_down(db: PgPool) {
	let s = Setup::new(db).await;
	let id = s.application("web7").await;
	s.save(id, &scaled(3)).await.unwrap();
	insert_deployment(
		&s.db,
		id,
		710,
		"running",
		Some("img"),
		Some(&format!("id-podkit-app-{id}-710")),
	)
	.await;
	let name = |suffix: &str| format!("podkit-app-{id}-710{suffix}");
	let up = |n: &str| report(n, ContainerState::Running, None, false, 0);
	let stopped = fake_agent(
		&s.state,
		vec![up(&name("")), up(&name("-r2")), up(&name("-r3"))],
		Vec::new(),
	);
	crate::deployments::service::stop(
		&s.state,
		s.alice().await,
		TeamId(s.team),
		ProjectId(s.project),
		ApplicationId(id),
	)
	.await
	.unwrap();
	let mut stopped = stopped.lock().unwrap().clone();
	stopped.sort();
	assert_eq!(
		stopped,
		[
			format!("id-{}", name("")),
			format!("id-{}", name("-r2")),
			format!("id-{}", name("-r3")),
		]
	);
}

// --- live ---------------------------------------------------------------------------------------

/// Three replicas of `traefik/whoami` on the real agent, a real Podman and Traefik.
///
/// - 30 requests are answered by at least two different hostnames (round robin);
/// - one replica is killed, and every request polled through the supervisor's restart is 200
///   (Traefik skips the dead container, the others serve);
/// - a rolling redeploy keeps every request at 200, and ends with three new replicas.
///
/// Needs the network (traefik/whoami) and a `podman.sock`; binds ports 24065 and 24066.
/// `cargo test -p server --all-features -- --ignored replicas_live`
#[sqlx::test]
#[ignore = "pulls traefik/whoami from docker.io with a real podman.sock, binds ports 24065 and 24066"]
async fn replicas_live_three_whoami_share_requests_survive_a_kill_and_a_rolling_redeploy(
	db: PgPool,
) {
	use std::collections::HashSet;
	use std::sync::atomic::{AtomicU32, Ordering};

	use tokio_util::sync::CancellationToken;

	use crate::deployments::{self, DeploymentStatus};
	use crate::health_gate::HealthConfig;
	use crate::jobs::{self, JobKind};

	let mut state = state(db.clone());
	{
		let config = Arc::make_mut(&mut state.config);
		config.ingress_port = 24065;
		config.https_port = 24066;
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
	let had_whoami = podman
		.inspect_image("docker.io/traefik/whoami")
		.await
		.ok()
		.flatten()
		.is_some();
	let data_dir = tempfile::tempdir().unwrap().keep();
	let ctx = Arc::new(agent::Ctx {
		docker: podman.docker().clone(),
		supervisor: agent::supervisor::Supervisor::start(podman.clone(), &data_dir).await,
		data_dir,
		podman_socket: agent::podman::local_socket_path(),
		http: reqwest::Client::new(),
	});
	let listener = tokio::net::UnixListener::bind(&state.config.local_agent_socket).unwrap();
	tokio::spawn({
		let ctx = ctx.clone();
		async move {
			axum::serve(listener, agent::routes::router(ctx))
				.await
				.unwrap();
		}
	});

	let s = Setup::with_state(db, state).await;
	let (id, host) = s
		.application_of("rep-live", "docker.io/traefik/whoami", 80)
		.await;
	crate::health_gate::save(
		&s.db,
		ApplicationId(id),
		&HealthConfig {
			path: "/".into(),
			interval_s: 1,
			timeout_s: 2,
			retries: 2,
			start_period_s: 0,
			gate_timeout_s: 30,
			drain_s: 2,
			..HealthConfig::default()
		},
	)
	.await
	.unwrap();
	s.save(id, &scaled(3)).await.unwrap();

	let deploy = || async {
		let app = crate::applications::find_by_id(&s.db, ApplicationId(id))
			.await
			.unwrap()
			.unwrap();
		let queued = deployments::queue(&s.state, &app, None, None)
			.await
			.unwrap();
		let job = jobs::latest_for(&s.db, JobKind::Deploy, queued.id.0)
			.await
			.unwrap()
			.expect("a deploy job");
		let mut log = s.state.logs.open(job.id).await.unwrap();
		let result =
			deployments::run::run(&s.state, &job, &CancellationToken::new(), &mut log).await;
		log.finish().await;
		if let Err(failure) = &result {
			jobs::fail_target(&s.state, job.kind, job.target_id, failure).await;
		}
		let after = deployments::find_by_id(&s.db, queued.id)
			.await
			.unwrap()
			.unwrap();
		(after, result)
	};
	let service_name = format!("app-{id}");
	let names = || async {
		podman
			.service_containers(&service_name)
			.await
			.unwrap_or_default()
			.iter()
			.filter_map(|c| agent::podman::summary_name(c).map(str::to_owned))
			.collect::<Vec<_>>()
	};

	let outcome = std::panic::AssertUnwindSafe(async {
		let (first, result) = deploy().await;
		result.unwrap_or_else(|e| panic!("the first deploy failed: {e:?}"));
		assert_eq!(first.status, DeploymentStatus::Running);
		assert_eq!(names().await.len(), 3, "three replicas run");

		let client = reqwest::Client::new();
		let hostname_of = |client: reqwest::Client, host: String| async move {
			let response = match client
				.get("http://127.0.0.1:24065/")
				.header("Host", host)
				.timeout(Duration::from_secs(2))
				.send()
				.await
			{
				Ok(response) => response,
				Err(e) => {
					eprintln!("request failed: {e:?}");
					return None;
				}
			};
			if response.status() != StatusCode::OK {
				eprintln!("request answered {}", response.status());
				return None;
			}
			let body = response.text().await.ok()?;
			body.lines()
				.find_map(|l| l.strip_prefix("Hostname: "))
				.map(str::to_owned)
		};
		let mut up = false;
		for _ in 0..40 {
			if hostname_of(client.clone(), host.clone()).await.is_some() {
				up = true;
				break;
			}
			tokio::time::sleep(Duration::from_millis(500)).await;
		}
		assert!(up, "{host} never answered 200 through ingress");

		// Traefik's docker provider picks the containers up as its events come in; give it
		// time to know all three.
		let mut known = HashSet::new();
		for _ in 0..60 {
			if let Some(hostname) = hostname_of(client.clone(), host.clone()).await {
				known.insert(hostname);
			}
			if known.len() == 3 {
				break;
			}
			tokio::time::sleep(Duration::from_millis(250)).await;
		}
		assert_eq!(known.len(), 3, "Traefik only knows {known:?} of 3 replicas");
		// 1. Round robin: 30 requests reach at least two replicas.
		let mut seen = HashSet::new();
		for _ in 0..30 {
			seen.insert(
				hostname_of(client.clone(), host.clone())
					.await
					.expect("every request is answered"),
			);
		}
		assert!(seen.len() >= 2, "only {seen:?} answered 30 requests");

		// From here on every request must be answered 200.
		let poll = |stop: CancellationToken| {
			let (total, bad) = (Arc::new(AtomicU32::new(0)), Arc::new(AtomicU32::new(0)));
			let task = tokio::spawn({
				let (total, bad) = (total.clone(), bad.clone());
				let (client, host) = (client.clone(), host.clone());
				async move {
					while !stop.is_cancelled() {
						total.fetch_add(1, Ordering::Relaxed);
						if hostname_of(client.clone(), host.clone()).await.is_none() {
							bad.fetch_add(1, Ordering::Relaxed);
						}
						tokio::time::sleep(Duration::from_millis(40)).await;
					}
				}
			});
			(total, bad, task)
		};

		// 2. Kill one replica: Traefik stops sending it requests, the others answer, and the
		// supervisor brings it back.
		let victim = names().await.into_iter().min().unwrap();
		let stop = CancellationToken::new();
		let (total, bad, task) = poll(stop.clone());
		tokio::time::sleep(Duration::from_millis(400)).await;
		podman
			.docker()
			.kill_container(&victim, None)
			.await
			.expect("kill a replica");
		let mut back = false;
		for _ in 0..60 {
			tokio::time::sleep(Duration::from_millis(500)).await;
			let running = podman
				.inspect(&victim)
				.await
				.ok()
				.flatten()
				.and_then(|i| i.state)
				.and_then(|s| s.running)
				== Some(true);
			if running {
				back = true;
				break;
			}
		}
		tokio::time::sleep(Duration::from_secs(4)).await;
		stop.cancel();
		task.await.unwrap();
		assert!(back, "the supervisor did not restart {victim}");
		let (total, bad) = (total.load(Ordering::Relaxed), bad.load(Ordering::Relaxed));
		assert!(total > 40, "the poller only made {total} requests");
		assert_eq!(
			bad, 0,
			"{bad} of {total} requests failed while a replica was killed"
		);

		// 3. A rolling redeploy keeps every request at 200 and ends with three new replicas.
		let stop = CancellationToken::new();
		let (total, bad, task) = poll(stop.clone());
		let (second, result) = deploy().await;
		stop.cancel();
		task.await.unwrap();
		result.unwrap_or_else(|e| panic!("the rolling redeploy failed: {e:?}"));
		assert_eq!(second.status, DeploymentStatus::Running);
		let (total, bad) = (total.load(Ordering::Relaxed), bad.load(Ordering::Relaxed));
		assert!(total > 100, "the poller only made {total} requests");
		assert_eq!(
			bad, 0,
			"{bad} of {total} requests failed during the rolling redeploy"
		);
		let after = names().await;
		assert_eq!(after.len(), 3, "{after:?}");
		assert!(
			after
				.iter()
				.all(|n| n.starts_with(&format!("podkit-app-{id}-{}", second.id.0))),
			"only the new replicas are left: {after:?}"
		);

		// 4. Scaling back to one replica is a rolling update too: the new container has
		// fewer labels than the old ones, and the route must not drop meanwhile.
		s.save(id, &scaled(1)).await.unwrap();
		let stop = CancellationToken::new();
		let (total, bad, task) = poll(stop.clone());
		let (third, result) = deploy().await;
		stop.cancel();
		task.await.unwrap();
		result.unwrap_or_else(|e| panic!("scaling down failed: {e:?}"));
		assert_eq!(third.status, DeploymentStatus::Running);
		let (total, bad) = (total.load(Ordering::Relaxed), bad.load(Ordering::Relaxed));
		assert_eq!(
			bad, 0,
			"{bad} of {total} requests failed while scaling down"
		);
		assert_eq!(names().await.len(), 1);
	});
	let result = futures_util::FutureExt::catch_unwind(outcome).await;

	// Leave nothing behind, whatever happened.
	for name in names().await {
		let _ = podman.remove(&name).await;
	}
	let _ = podman.remove(agent::ingress::TRAEFIK_CONTAINER_NAME).await;
	if !had_whoami {
		let _ = podman.remove_image("docker.io/traefik/whoami").await;
	}
	if let Err(panic) = result {
		std::panic::resume_unwind(panic);
	}
}

// --- what the agent does with the spec ---------------------------------------------------------
//
// The agent's own tests cover these too; they are repeated here so the proof command of this
// slice (`-p server -- replicas`) shows the rolling order, the health gating and the labels.

#[test]
fn replicas_rolling_order_replaces_one_at_a_time_oldest_first() {
	use agent::replicas::{name, plan};
	let steps = plan(3, 3, 1, 0);
	assert_eq!(steps.len(), 3);
	for (i, step) in steps.iter().enumerate() {
		assert_eq!(step.start, [i], "round {i} starts one replica");
		assert_eq!((step.retire_before, step.retire_after), (0, 1));
	}
	// The new replicas are named apart from each other and from the old deployment's.
	let new: Vec<_> = (0..3).map(|i| name("app-1", 12, i)).collect();
	assert_eq!(
		new,
		[
			"podkit-app-1-12",
			"podkit-app-1-12-r2",
			"podkit-app-1-12-r3"
		]
	);
	// Old containers are told by (deployment, replica) number, oldest first.
	let mut old = ["podkit-app-1-11-r2", "podkit-app-1-10", "podkit-app-1-11"];
	old.sort_by_key(|n| {
		n.strip_prefix("podkit-app-1-")
			.and_then(agent::replicas::order_key)
	});
	assert_eq!(
		old,
		["podkit-app-1-10", "podkit-app-1-11", "podkit-app-1-11-r2"]
	);
}

#[test]
fn replicas_rolling_is_gated_on_health_before_an_old_replica_goes() {
	use agent::replicas::plan;
	// With a surge, no round takes an old replica away before its own start: the agent
	// waits for the round's new containers to be healthy, and only then retires
	// `retire_after`. So the live count never drops under what was running.
	for (old, count, surge) in [(3, 3, 1), (4, 4, 2), (2, 5, 1), (5, 2, 1)] {
		let (mut live_old, mut live_new) = (old, 0);
		for step in plan(old, count, surge, 0) {
			assert_eq!(step.retire_before, 0, "a surge never retires first");
			live_new += step.start.len();
			live_old -= step.retire_after;
			assert!(live_old + live_new >= old.min(count), "{old}->{count}");
		}
		assert_eq!((live_old, live_new), (0, count));
	}
	// Without a surge the unavailable limit is what may go first, never more.
	for step in plan(6, 6, 0, 2) {
		assert!(step.retire_before <= 2 && step.retire_after == 0);
	}
}

#[test]
fn replicas_sticky_labels_set_a_cookie_on_the_shared_service() {
	let labels = agent::ingress::sticky_labels("app-9");
	let get = |k: &str| {
		labels
			.iter()
			.find(|(key, _)| key == k)
			.map(|(_, v)| v.as_str())
	};
	let base = "traefik.http.services.app-9.loadbalancer.sticky.cookie";
	assert_eq!(get(&format!("{base}.name")), Some("podkit-app-9"));
	assert_eq!(get(&format!("{base}.httponly")), Some("true"));
	assert_eq!(get(&format!("{base}.samesite")), Some("lax"));
	// Never `secure`: podkit works over plain HTTP, where the browser would not send it back.
	assert!(!labels.iter().any(|(k, _)| k.ends_with(".secure")));
}
