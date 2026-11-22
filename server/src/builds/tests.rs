//! Builds: choosing the build server, what a build is told, moving the image, the build places
//! of a server, and tenancy. `cargo test -p server --all-features -- build_cache`

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Json;
use axum::body::Body;
use axum::extract::{Query, State};
use axum::http::{StatusCode, header};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use serde_json::json;
use sqlx::PgPool;
use tokio_util::sync::CancellationToken;

use super::delivery::{DeliveryError, relay};
use super::service::{self, AppInput, PruneTo, ServerInput, TeamInput};
use super::{
	AppSettings, DEFAULT_CACHE_MAX_MB, DEFAULT_MAX_CONCURRENT, ServerSettings, TeamSettings,
	choose_server, plan, slots,
};
use crate::agents::AgentClient;
use crate::error::ServerError;
use crate::ids::{ApplicationId, ProjectId, ServerId, TeamId, UserId};
use crate::state::AppState;
use crate::tests::{call, project_id, server_id, state, user_id, user_with_team};

const GIB: i64 = 1024 * 1024 * 1024;

// --- choosing the server and the plan -----------------------------------------------------------

#[test]
fn build_cache_an_app_builds_where_it_runs_unless_told_otherwise() {
	let runs_on = ServerId(1);
	let picked = choose_server(runs_on, &AppSettings::default(), &TeamSettings::default());
	assert_eq!(picked, runs_on);
}

#[test]
fn build_cache_the_team_server_beats_the_server_it_runs_on() {
	let team = TeamSettings {
		build_server_id: Some(ServerId(2)),
		..TeamSettings::default()
	};
	assert_eq!(
		choose_server(ServerId(1), &AppSettings::default(), &team),
		ServerId(2)
	);
}

#[test]
fn build_cache_the_app_choice_beats_the_team() {
	let team = TeamSettings {
		build_server_id: Some(ServerId(2)),
		..TeamSettings::default()
	};
	let app = AppSettings {
		build_server_id: Some(ServerId(3)),
		cache_enabled: true,
	};
	assert_eq!(choose_server(ServerId(1), &app, &team), ServerId(3));
}

#[test]
fn build_cache_a_build_on_the_own_server_is_not_remote() {
	let p = plan(
		ServerId(1),
		&AppSettings::default(),
		&TeamSettings::default(),
		ServerSettings::default(),
	);
	assert!(!p.remote);
	assert_eq!(p.server, ServerId(1));
	assert_eq!(
		p.max_concurrent,
		usize::try_from(DEFAULT_MAX_CONCURRENT).unwrap()
	);
	let options = p.options();
	assert!(options.cache);
	assert_eq!(options.cpu_cores, None);
	assert_eq!(options.memory_bytes, None);
	assert_eq!(
		options.cache_max_bytes,
		Some(u64::try_from(DEFAULT_CACHE_MAX_MB).unwrap() * 1024 * 1024)
	);
}

#[test]
fn build_cache_limits_are_passed_to_the_build() {
	let team = TeamSettings {
		build_server_id: Some(ServerId(2)),
		cpu_limit: Some(1.5),
		memory_limit_mb: Some(2048),
	};
	let app = AppSettings {
		build_server_id: None,
		cache_enabled: false,
	};
	let builder = ServerSettings {
		max_concurrent: 3,
		cache_max_mb: 500,
	};
	let p = plan(ServerId(1), &app, &team, builder);
	assert!(p.remote);
	assert_eq!(p.server, ServerId(2));
	assert_eq!(p.max_concurrent, 3);
	let options = p.options();
	assert!(!options.cache, "the app turned the cache off");
	assert_eq!(options.cpu_cores, Some(1.5));
	assert_eq!(options.memory_bytes, Some(2 * GIB));
	assert_eq!(options.cache_max_bytes, Some(500 * 1024 * 1024));
}

// --- settings in the database -------------------------------------------------------------------

#[crate::tests::db_test]
async fn build_cache_settings_default_then_round_trip(db: PgPool) {
	let s = state(db.clone());
	let app = crate::routes::routes(s.clone()).await.unwrap();
	let (_, team) = user_with_team(&app, "ada").await;
	let team = TeamId(team);
	let server = ServerId(server_id(&db, team.0).await);

	assert_eq!(
		super::team_settings(&db, team).await.unwrap(),
		TeamSettings::default()
	);
	assert_eq!(
		super::server_settings(&db, server).await.unwrap(),
		ServerSettings::default()
	);

	let saved = TeamSettings {
		build_server_id: Some(server),
		cpu_limit: Some(2.0),
		memory_limit_mb: Some(1024),
	};
	super::save_team_settings(&db, team, &saved).await.unwrap();
	assert_eq!(super::team_settings(&db, team).await.unwrap(), saved);

	let builder = ServerSettings {
		max_concurrent: 4,
		cache_max_mb: 0,
	};
	super::save_server_settings(&db, server, &builder)
		.await
		.unwrap();
	assert_eq!(super::server_settings(&db, server).await.unwrap(), builder);
}

#[test]
fn build_cache_form_numbers_are_checked() {
	let ok = TeamInput {
		build_server: String::new(),
		cpu: "1.5".into(),
		memory_mb: "512".into(),
	};
	let parsed = ok.parse().unwrap();
	assert_eq!(parsed.cpu_limit, Some(1.5));
	assert_eq!(parsed.memory_limit_mb, Some(512));
	assert_eq!(
		TeamInput::default().parse().unwrap(),
		TeamSettings::default()
	);

	for (cpu, memory) in [
		("0", ""),
		("-1", ""),
		("lots", ""),
		("", "100"),
		("", "x"),
		("1000", ""),
	] {
		let bad = TeamInput {
			build_server: String::new(),
			cpu: cpu.into(),
			memory_mb: memory.into(),
		};
		assert!(
			matches!(bad.parse(), Err(ServerError::Validation(_))),
			"cpu {cpu:?} memory {memory:?}"
		);
	}

	let server = |max: &str, cache: &str| ServerInput {
		max_concurrent: max.into(),
		cache_max_mb: cache.into(),
	};
	assert!(
		server("2", "0").parse().is_ok(),
		"0 keeps nothing and is allowed"
	);
	for (max, cache) in [
		("0", "10"),
		("17", "10"),
		("", "10"),
		("2", "-1"),
		("2", ""),
	] {
		assert!(server(max, cache).parse().is_err(), "{max:?} {cache:?}");
	}
}

// --- build places -------------------------------------------------------------------------------

#[tokio::test]
async fn build_cache_a_server_runs_no_more_builds_than_its_limit() {
	let server = ServerId(9_001);
	let first = slots::try_take(server, 2).expect("first");
	let second = slots::try_take(server, 2).expect("second");
	assert!(slots::try_take(server, 2).is_none(), "the third waits");
	assert_eq!(slots::running(server), 2);
	drop(first);
	assert_eq!(slots::running(server), 1);
	let third = slots::try_take(server, 2).expect("a place was freed");
	drop((second, third));
	assert_eq!(slots::running(server), 0);
}

#[tokio::test]
async fn build_cache_a_waiting_build_gets_the_place_when_one_frees() {
	let server = ServerId(9_002);
	let held = slots::try_take(server, 1).unwrap();
	let cancel = CancellationToken::new();
	let waiter = tokio::spawn({
		let cancel = cancel.clone();
		async move { slots::acquire(server, 1, &cancel).await.is_some() }
	});
	tokio::time::sleep(Duration::from_millis(50)).await;
	assert!(
		!waiter.is_finished(),
		"it must wait while the place is taken"
	);
	drop(held);
	assert!(
		tokio::time::timeout(Duration::from_secs(5), waiter)
			.await
			.expect("the waiter should have woken")
			.unwrap()
	);
}

#[tokio::test]
async fn build_cache_a_cancelled_wait_gives_up() {
	let server = ServerId(9_003);
	let _held = slots::try_take(server, 1).unwrap();
	let cancel = CancellationToken::new();
	let waiter = tokio::spawn({
		let cancel = cancel.clone();
		async move { slots::acquire(server, 1, &cancel).await.is_none() }
	});
	tokio::time::sleep(Duration::from_millis(30)).await;
	cancel.cancel();
	assert!(
		tokio::time::timeout(Duration::from_secs(5), waiter)
			.await
			.unwrap()
			.unwrap()
	);
	assert_eq!(slots::running(server), 1, "a give-up takes no place");
}

#[tokio::test]
async fn build_cache_the_places_of_two_servers_are_separate() {
	let (a, b) = (ServerId(9_004), ServerId(9_005));
	let _held = slots::try_take(a, 1).unwrap();
	assert!(slots::try_take(a, 1).is_none());
	assert!(slots::try_take(b, 1).is_some());
}

// --- moving the image (stub agents) -------------------------------------------------------------

/// What a stub agent answers and what it saw.
#[derive(Default)]
struct Stub {
	/// The id the export announces; `None` makes the route fail.
	image_id: Option<String>,
	/// The id the import reports; `None` makes the route fail.
	loaded_id: Option<String>,
	archive: Vec<u8>,
	received: Mutex<Vec<u8>>,
	asked: Mutex<Vec<String>>,
}

#[derive(serde::Deserialize)]
struct Reference {
	reference: String,
}

/// An agent on its own socket that exports and imports from `stub`.
fn stub_agent(stub: &Arc<Stub>) -> AgentClient {
	let socket = tempfile::tempdir().unwrap().keep().join("agent.sock");
	let app = axum::Router::new()
		.route(
			"/v1/images/export",
			get(
				|State(s): State<Arc<Stub>>, Query(q): Query<Reference>| async move {
					s.asked.lock().unwrap().push(q.reference);
					match &s.image_id {
						Some(id) => (
							[
								(
									header::CONTENT_TYPE.as_str(),
									"application/x-tar".to_owned(),
								),
								(proto::IMAGE_ID_HEADER, id.clone()),
							],
							Body::from(s.archive.clone()),
						)
							.into_response(),
						None => (StatusCode::NOT_FOUND, "no such image").into_response(),
					}
				},
			),
		)
		.route(
			"/v1/images/import",
			post(
				|State(s): State<Arc<Stub>>, Query(q): Query<Reference>, body: Body| async move {
					s.asked.lock().unwrap().push(q.reference);
					let bytes = axum::body::to_bytes(body, usize::MAX).await.unwrap();
					s.received.lock().unwrap().extend_from_slice(&bytes);
					match &s.loaded_id {
						Some(id) => Json(json!({ "image_id": id })).into_response(),
						None => (StatusCode::UNPROCESSABLE_ENTITY, "bad archive").into_response(),
					}
				},
			),
		)
		.with_state(Arc::clone(stub));
	let listener = tokio::net::UnixListener::bind(&socket).unwrap();
	tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
	AgentClient::at(socket)
}

fn archive() -> Vec<u8> {
	(0..=255u8).cycle().take(300_000).collect()
}

#[tokio::test]
async fn build_cache_the_image_reaches_the_target_intact() {
	let from = Arc::new(Stub {
		image_id: Some("sha256:abc".into()),
		archive: archive(),
		..Stub::default()
	});
	let to = Arc::new(Stub {
		loaded_id: Some("sha256:abc".into()),
		..Stub::default()
	});
	let sent = relay(
		&stub_agent(&from),
		&stub_agent(&to),
		"podkit-app-1:0123abcd",
	)
	.await
	.unwrap();
	assert_eq!(sent, "sha256:abc");
	assert_eq!(
		*to.received.lock().unwrap(),
		archive(),
		"every byte arrived"
	);
	assert_eq!(*from.asked.lock().unwrap(), ["podkit-app-1:0123abcd"]);
	assert_eq!(*to.asked.lock().unwrap(), ["podkit-app-1:0123abcd"]);
}

#[tokio::test]
async fn build_cache_an_image_that_arrives_changed_is_refused() {
	let from = Arc::new(Stub {
		image_id: Some("sha256:abc".into()),
		archive: archive(),
		..Stub::default()
	});
	let to = Arc::new(Stub {
		loaded_id: Some("sha256:different".into()),
		..Stub::default()
	});
	let err = relay(&stub_agent(&from), &stub_agent(&to), "podkit-app-1:x")
		.await
		.unwrap_err();
	assert_eq!(
		err,
		DeliveryError::Mismatch {
			sent: "sha256:abc".into(),
			got: "sha256:different".into()
		}
	);
}

#[tokio::test]
async fn build_cache_a_build_server_without_the_image_fails_the_send() {
	let from = Arc::new(Stub::default());
	let to = Arc::new(Stub {
		loaded_id: Some("sha256:abc".into()),
		..Stub::default()
	});
	let err = relay(&stub_agent(&from), &stub_agent(&to), "podkit-app-1:x")
		.await
		.unwrap_err();
	assert!(matches!(err, DeliveryError::Send(_)), "{err:?}");
	assert!(
		to.received.lock().unwrap().is_empty(),
		"nothing was sent on"
	);
}

#[tokio::test]
async fn build_cache_a_target_that_cannot_load_fails_the_load() {
	let from = Arc::new(Stub {
		image_id: Some("sha256:abc".into()),
		archive: archive(),
		..Stub::default()
	});
	let to = Arc::new(Stub::default());
	let err = relay(&stub_agent(&from), &stub_agent(&to), "podkit-app-1:x")
		.await
		.unwrap_err();
	assert!(matches!(err, DeliveryError::Load(_)), "{err:?}");
	assert!(
		err.to_string()
			.starts_with("the server could not load the image")
	);
}

// --- tenancy ------------------------------------------------------------------------------------

struct World {
	state: AppState,
	db: PgPool,
	alice: UserId,
	alice_team: TeamId,
	alice_server: ServerId,
	alice_project: ProjectId,
	alice_app: ApplicationId,
	bob: UserId,
	bob_team: TeamId,
	bob_server: ServerId,
}

impl World {
	async fn new(db: PgPool) -> Self {
		let state = state(db.clone());
		let app = crate::routes::routes(state.clone()).await.unwrap();
		let (token, alice_team) = user_with_team(&app, "alice").await;
		let (_, bob_team) = user_with_team(&app, "bob").await;
		let (status, project) = call(
			&app,
			"POST",
			&format!("/api/v1/teams/{alice_team}/projects"),
			Some(&token),
			Some(json!({ "name": "web", "slug": "web" })),
		)
		.await;
		assert!(status.is_success(), "{project}");
		let alice_project = project_id(&db, "web").await;
		let alice_server = server_id_of(&db, alice_team).await;
		crate::tests::seed_application_on(&db, alice_project, alice_server).await;
		Self {
			alice: UserId(user_id(&db, "alice").await),
			alice_team: TeamId(alice_team),
			alice_server: ServerId(alice_server),
			alice_project: ProjectId(alice_project),
			alice_app: ApplicationId(5002),
			bob: UserId(user_id(&db, "bob").await),
			bob_team: TeamId(bob_team),
			bob_server: ServerId(server_id_of(&db, bob_team).await),
			state,
			db,
		}
	}
}

async fn server_id_of(db: &PgPool, team: i64) -> i64 {
	server_id(db, team).await
}

fn team_input(server: ServerId) -> TeamInput {
	TeamInput {
		build_server: server.0.to_string(),
		cpu: "2".into(),
		memory_mb: "1024".into(),
	}
}

#[crate::tests::db_test]
async fn build_cache_a_team_can_pick_its_own_server_to_build(db: PgPool) {
	let w = World::new(db).await;
	let saved = service::save_team(&w.state, w.alice, w.alice_team, &team_input(w.alice_server))
		.await
		.unwrap();
	assert_eq!(saved.build_server_id, Some(w.alice_server));
	let view = service::team_view(&w.state, w.alice, w.alice_team)
		.await
		.unwrap();
	assert_eq!(view.settings.build_server_id, Some(w.alice_server));
	assert_eq!(view.servers.len(), 1);
	assert_eq!(view.servers[0].server.id, w.alice_server);
}

#[crate::tests::db_test]
async fn build_cache_another_teams_server_cannot_be_the_build_server(db: PgPool) {
	let w = World::new(db).await;
	let err = service::save_team(&w.state, w.alice, w.alice_team, &team_input(w.bob_server))
		.await
		.unwrap_err();
	assert!(matches!(err, ServerError::ServerNotFound), "{err:?}");
	assert_eq!(
		super::team_settings(&w.db, w.alice_team).await.unwrap(),
		TeamSettings::default(),
		"nothing was stored"
	);
}

#[crate::tests::db_test]
async fn build_cache_another_teams_server_cannot_be_tuned_or_pruned(db: PgPool) {
	let w = World::new(db).await;
	let input = ServerInput {
		max_concurrent: "4".into(),
		cache_max_mb: "1".into(),
	};
	let err = service::save_server(&w.state, w.alice, w.alice_team, w.bob_server, &input)
		.await
		.unwrap_err();
	assert!(matches!(err, ServerError::ServerNotFound), "{err:?}");
	let err = service::prune_cache(
		&w.state,
		w.alice,
		w.alice_team,
		w.bob_server,
		PruneTo::Empty,
	)
	.await
	.unwrap_err();
	assert!(matches!(err, ServerError::ServerNotFound), "{err:?}");
	assert_eq!(
		super::server_settings(&w.db, w.bob_server).await.unwrap(),
		ServerSettings::default()
	);
	// The form of the whole page refuses a foreign server before it stores anything.
	let err = service::save_all(
		&w.state,
		w.alice,
		w.alice_team,
		&team_input(w.alice_server),
		&[(w.alice_server, input.clone()), (w.bob_server, input)],
	)
	.await
	.unwrap_err();
	assert!(matches!(err, ServerError::ServerNotFound), "{err:?}");
	assert_eq!(
		super::team_settings(&w.db, w.alice_team).await.unwrap(),
		TeamSettings::default(),
		"the page saved nothing"
	);
}

#[crate::tests::db_test]
async fn build_cache_another_teams_member_gets_nothing(db: PgPool) {
	let w = World::new(db).await;
	// Bob is not a member of alice's team.
	assert!(
		service::team_view(&w.state, w.bob, w.alice_team)
			.await
			.is_err()
	);
	assert!(
		service::save_team(&w.state, w.bob, w.alice_team, &TeamInput::default())
			.await
			.is_err()
	);
	assert!(
		service::app_view(&w.state, w.bob, w.alice_team, w.alice_project, w.alice_app)
			.await
			.is_err()
	);
	// And his own team cannot reach her application through his team id.
	let err = service::app_view(&w.state, w.bob, w.bob_team, w.alice_project, w.alice_app)
		.await
		.unwrap_err();
	assert!(
		matches!(
			err,
			ServerError::ProjectNotFound | ServerError::ApplicationNotFound
		),
		"{err:?}"
	);
	let input = AppInput {
		build_server: String::new(),
		cache: false,
		strategy: "nixpacks".into(),
		dockerfile_path: String::new(),
	};
	assert!(
		service::save_app(
			&w.state,
			w.bob,
			w.bob_team,
			w.alice_project,
			w.alice_app,
			&input
		)
		.await
		.is_err()
	);
	assert_eq!(
		super::app_settings(&w.db, w.alice_app).await.unwrap(),
		AppSettings::default()
	);
}

#[crate::tests::db_test]
async fn build_cache_an_app_saves_its_server_cache_and_strategy(db: PgPool) {
	let w = World::new(db).await;
	let input = AppInput {
		build_server: w.alice_server.0.to_string(),
		cache: false,
		strategy: "nixpacks".into(),
		dockerfile_path: String::new(),
	};
	service::save_app(
		&w.state,
		w.alice,
		w.alice_team,
		w.alice_project,
		w.alice_app,
		&input,
	)
	.await
	.unwrap();
	let view = service::app_view(
		&w.state,
		w.alice,
		w.alice_team,
		w.alice_project,
		w.alice_app,
	)
	.await
	.unwrap();
	assert_eq!(view.settings.build_server_id, Some(w.alice_server));
	assert!(!view.settings.cache_enabled);
	assert_eq!(view.strategy, crate::applications::BuildStrategy::Nixpacks);

	// A foreign server, an unknown strategy and a path outside the repository are refused.
	for bad in [
		AppInput {
			build_server: w.bob_server.0.to_string(),
			..input.clone()
		},
		AppInput {
			strategy: "magic".into(),
			..input.clone()
		},
		AppInput {
			dockerfile_path: "../etc/passwd".into(),
			..input.clone()
		},
	] {
		assert!(
			service::save_app(
				&w.state,
				w.alice,
				w.alice_team,
				w.alice_project,
				w.alice_app,
				&bad
			)
			.await
			.is_err(),
			"{bad:?}"
		);
	}
}

#[crate::tests::db_test]
async fn build_cache_a_saved_change_is_in_the_audit_log(db: PgPool) {
	let w = World::new(db).await;
	service::save_team(&w.state, w.alice, w.alice_team, &team_input(w.alice_server))
		.await
		.unwrap();
	let n = crate::tests::scalar(
		&w.db,
		"SELECT COUNT(*) FROM audit_events WHERE action = 'builds.settings'",
		None,
	)
	.await;
	assert_eq!(n, 1);
}
