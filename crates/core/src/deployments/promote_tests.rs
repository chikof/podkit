//! Promotion between environments, through the API and the Deploy job: the source's running
//! image goes to the matching application of another environment, with that application's
//! own variables, and nothing is built. The job runs against a fake agent; the move between
//! two servers needs two agents and is covered only up to its refusal.
//!
//! `cargo test -p podkit-core -- promote`

use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Bytes;
use axum::http::{StatusCode, header};
use axum::response::IntoResponse;
use axum::routing::post;
use serde_json::{Value, json};
use sqlx::PgPool;
use tokio_util::sync::CancellationToken;

use crate::deployments::{self, Deployment};
use crate::ids::DeploymentId;
use crate::jobs::{self, JobFailure, JobKind};
use crate::state::AppState;
use crate::tests::{call, count, state, user_with_team};

const IMAGE: &str = "podkit-app-1:abc1234";
const SHA: &str = "abc1234def5678abc1234def5678abc1234def56";

struct Setup {
	app: Router,
	state: AppState,
	db: PgPool,
	token: String,
	team: i64,
	project: i64,
	server: i64,
	production: i64,
	staging: i64,
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
		let project = project["id"].as_i64().unwrap();
		let (_, servers) = call(
			&app,
			"GET",
			&format!("/api/v1/teams/{team}/servers"),
			Some(&token),
			None,
		)
		.await;
		let envs = format!("/api/v1/teams/{team}/projects/{project}/environments");
		let (_, list) = call(&app, "GET", &envs, Some(&token), None).await;
		let production = list[0]["id"].as_i64().unwrap();
		let (s, staging) = call(
			&app,
			"POST",
			&envs,
			Some(&token),
			Some(json!({ "name": "Staging", "slug": "staging" })),
		)
		.await;
		assert!(s.is_success(), "create staging: {s} {staging}");
		Self {
			app,
			state,
			db,
			token,
			team,
			project,
			server: servers[0]["id"].as_i64().unwrap(),
			production,
			staging: staging["id"].as_i64().unwrap(),
		}
	}

	fn apps(&self) -> String {
		format!(
			"/api/v1/teams/{}/projects/{}/applications",
			self.team, self.project
		)
	}

	async fn call(&self, method: &str, path: &str, body: Option<Value>) -> (StatusCode, Value) {
		call(&self.app, method, path, Some(&self.token), body).await
	}

	/// A git application in `environment` on the team's server.
	async fn application(&self, slug: &str, environment: i64) -> i64 {
		let (s, body) = self
			.call(
				"POST",
				&self.apps(),
				Some(json!({
					"name": slug, "slug": slug, "server_id": self.server,
					"repo_url": "https://github.com/traefik/whoami", "container_port": 80,
					"environment_id": environment,
				})),
			)
			.await;
		assert!(s.is_success(), "create {slug}: {s} {body}");
		body["id"].as_i64().unwrap()
	}

	/// A deployment of `app` that is live now, running `image`.
	async fn running(&self, id: i64, app: i64, image: &str) {
		sqlx::query(
			"INSERT INTO deployments (id, application_id, status, commit_sha, image_tag, container_id)
			VALUES ($1, $2, 'running', $3, $4, 'c0')",
		)
		.bind(id)
		.bind(app)
		.bind(SHA)
		.bind(image)
		.execute(&self.db)
		.await
		.unwrap();
	}

	async fn promote(&self, app: i64, body: Value) -> (StatusCode, Value) {
		self.call(
			"POST",
			&format!("{}/{app}/promote", self.apps()),
			Some(body),
		)
		.await
	}

	/// Runs the Deploy job of `id` the way the worker does.
	async fn run_job(&self, id: i64) -> (Result<(), JobFailure>, Deployment) {
		let job = jobs::latest_for(&self.db, JobKind::Deploy, id)
			.await
			.unwrap()
			.expect("a deploy job is queued");
		let mut log = self.state.logs.open(job.id).await.unwrap();
		let result =
			deployments::run::run(&self.state, &job, &CancellationToken::new(), &mut log).await;
		log.finish().await;
		let after = deployments::find_by_id(&self.db, DeploymentId(id))
			.await
			.unwrap()
			.unwrap();
		(result, after)
	}
}

/// A fake agent on the local server's socket that records every deploy spec and answers with
/// the image it was asked to run.
fn fake_agent(state: &AppState) -> Arc<Mutex<Vec<Value>>> {
	let seen = Arc::new(Mutex::new(Vec::new()));
	let recorder = seen.clone();
	let app = Router::new().route(
		"/v1/deploy",
		post(move |body: Bytes| async move {
			let spec: Value = serde_json::from_slice(&body).unwrap();
			let image = spec["source"]["reference"].clone();
			recorder.lock().unwrap().push(spec);
			let events = [
				json!({ "type": "deployed", "container_id": "c1", "image": image,
				"commit_sha": null }),
			];
			let body = events.iter().fold(String::new(), |mut out, e| {
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
async fn promote_runs_the_staging_image_in_production_with_production_variables(db: PgPool) {
	let state = state(db.clone());
	let seen = fake_agent(&state);
	let s = Setup::with_state(db, state).await;
	let prod = s.application("api", s.production).await;
	let staging = s.application("api-staging", s.staging).await;
	s.running(9203, staging, IMAGE).await;
	for (app, value) in [(prod, "production"), (staging, "staging")] {
		let (st, _) = s
			.call(
				"POST",
				&format!("{}/{app}/env-vars", s.apps()),
				Some(json!({ "key": "MODE", "value": value })),
			)
			.await;
		assert!(st.is_success());
	}

	// The plan names the source and the matching application.
	let (st, plan) = s
		.call("GET", &format!("{}/{staging}/promote", s.apps()), None)
		.await;
	assert_eq!(st, StatusCode::OK, "{plan}");
	assert_eq!(plan["source"]["deployment_id"], 9203);
	assert_eq!(plan["source"]["image"], IMAGE);
	assert_eq!(plan["source"]["commit_sha"], SHA);
	let dest = &plan["destinations"][0];
	assert_eq!(dest["environment"]["id"], s.production);
	assert_eq!(dest["application"]["id"], prod);
	assert_eq!(dest["refusal"], Value::Null);
	assert_eq!(dest["moves_image"], false);

	let (st, made) = s
		.promote(
			staging,
			json!({ "environment_id": s.production, "deployment_id": 9203 }),
		)
		.await;
	assert_eq!(st, StatusCode::ACCEPTED, "{made}");
	assert_eq!(made["application_id"], prod);
	assert_eq!(made["image_tag"], IMAGE);
	assert_eq!(made["commit_sha"], SHA);
	assert_eq!(made["promoted_from"], 9203);
	assert_eq!(made["promoted_from_environment"], "Staging");

	// Nothing is built: the agent is asked to run the staging image as it is, with the
	// variables of production.
	let id = made["id"].as_i64().unwrap();
	let (result, after) = s.run_job(id).await;
	assert!(result.is_ok(), "{result:?}");
	assert_eq!(after.status, deployments::DeploymentStatus::Running);
	assert_eq!(after.image_tag.as_deref(), Some(IMAGE));
	let sent = seen.lock().unwrap()[0].clone();
	assert_eq!(sent["service"], format!("app-{prod}"));
	assert_eq!(sent["source"]["type"], "image");
	assert_eq!(sent["source"]["reference"], IMAGE);
	assert_eq!(sent["source"]["pull"], false);
	assert_eq!(sent["env"], json!([["MODE", "production"]]), "{sent}");

	// The history says where it came from; the staging release is untouched.
	let (_, listed) = s
		.call("GET", &format!("{}/{prod}/deployments", s.apps()), None)
		.await;
	assert_eq!(listed[0]["promoted_from"], 9203);
	let source = deployments::find_by_id(&s.db, DeploymentId(9203))
		.await
		.unwrap()
		.unwrap();
	assert_eq!(source.status, deployments::DeploymentStatus::Running);

	// One audit event, on the target, naming the source.
	let (action, diff): (String, Value) = sqlx::query_as(
		"SELECT action, diff FROM audit_events WHERE target_id = $1 ORDER BY created_at DESC LIMIT 1",
	)
	.bind(prod)
	.fetch_one(&s.db)
	.await
	.unwrap();
	assert_eq!(action, "app.promote");
	assert_eq!(diff["from_deployment"], 9203);
	assert_eq!(diff["from_environment"], "Staging");
	assert_eq!(diff["image"], IMAGE);
}

#[sqlx::test]
async fn promote_refuses_what_it_cannot_do_and_changes_nothing(db: PgPool) {
	let s = Setup::new(db).await;
	s.application("api", s.production).await;
	let staging = s.application("api-staging", s.staging).await;
	let lonely = s.application("worker-staging", s.staging).await;
	let before = count(&s.db, "deployments").await;

	// Nothing runs in staging yet: no image.
	let (st, body) = s
		.promote(staging, json!({ "environment_id": s.production }))
		.await;
	assert_eq!(st, StatusCode::BAD_REQUEST, "{body}");
	assert!(
		body["error"].as_str().unwrap().contains("not running"),
		"{body}"
	);

	s.running(9300, staging, IMAGE).await;
	s.running(9301, lonely, IMAGE).await;
	let before = before + 2;

	// Its own environment.
	let (st, body) = s
		.promote(staging, json!({ "environment_id": s.staging }))
		.await;
	assert_eq!(st, StatusCode::BAD_REQUEST, "{body}");

	// No match in production says what it looked for.
	let (st, body) = s
		.promote(lonely, json!({ "environment_id": s.production }))
		.await;
	assert_eq!(st, StatusCode::BAD_REQUEST, "{body}");
	let error = body["error"].as_str().unwrap();
	assert!(error.contains("`worker-staging` or `worker`"), "{error}");

	// The release the dialog showed is no longer the one running.
	let (st, body) = s
		.promote(
			staging,
			json!({ "environment_id": s.production, "deployment_id": 1 }),
		)
		.await;
	assert_eq!(st, StatusCode::CONFLICT, "{body}");

	// An environment of another project is not found.
	let (_, other) = s
		.call(
			"POST",
			&format!("/api/v1/teams/{}/projects", s.team),
			Some(json!({ "name": "other", "slug": "other" })),
		)
		.await;
	let (_, envs) = s
		.call(
			"GET",
			&format!(
				"/api/v1/teams/{}/projects/{}/environments",
				s.team, other["id"]
			),
			None,
		)
		.await;
	let (st, _) = s
		.promote(staging, json!({ "environment_id": envs[0]["id"] }))
		.await;
	assert_eq!(st, StatusCode::NOT_FOUND);

	assert_eq!(
		count(&s.db, "deployments").await,
		before,
		"a refusal queued a deploy"
	);
}

#[sqlx::test]
async fn promote_needs_what_a_deploy_needs(db: PgPool) {
	let s = Setup::new(db).await;
	s.application("api", s.production).await;
	let staging = s.application("api-staging", s.staging).await;
	s.running(9400, staging, IMAGE).await;

	// carol is a viewer (role 2) of alice's team: she may read, not deploy.
	let (carol, _) = user_with_team(&s.app, "carol").await;
	let id = crate::tests::user_id(&s.db, "carol").await;
	sqlx::query("INSERT INTO team_members (id, team_id, user_id, role_id) VALUES ($1, $2, $3, 2)")
		.bind(crypto::generate_id())
		.bind(s.team)
		.bind(id)
		.execute(&s.db)
		.await
		.unwrap();
	let path = format!("{}/{staging}/promote", s.apps());
	let (st, _) = call(&s.app, "GET", &path, Some(&carol), None).await;
	assert_eq!(st, StatusCode::OK, "reading the plan is reading");
	let (st, _) = call(
		&s.app,
		"POST",
		&path,
		Some(&carol),
		Some(json!({ "environment_id": s.production })),
	)
	.await;
	assert_eq!(st, StatusCode::FORBIDDEN);

	// A team she is not in is a 404, whatever she asks for.
	let (dave, _) = user_with_team(&s.app, "dave").await;
	let (st, _) = call(&s.app, "GET", &path, Some(&dave), None).await;
	assert_eq!(st, StatusCode::NOT_FOUND);

	// A token needs the deploy scope.
	let (_, made) = s
		.call(
			"POST",
			&format!("/api/v1/teams/{}/api-tokens", s.team),
			Some(json!({ "name": "ro", "scopes": ["read", "write"] })),
		)
		.await;
	let token = made["secret"].as_str().unwrap();
	let (st, _) = call(
		&s.app,
		"POST",
		&path,
		Some(token),
		Some(json!({ "environment_id": s.production })),
	)
	.await;
	assert_eq!(st, StatusCode::FORBIDDEN);
}

#[sqlx::test]
async fn promote_from_another_server_refuses_clearly_when_that_server_is_down(db: PgPool) {
	let s = Setup::new(db).await;
	let prod = s.application("api", s.production).await;
	// The staging copy runs on a second server whose agent is not running.
	let other = crypto::generate_id();
	sqlx::query(
		"INSERT INTO servers (id, team_id, name, hostname, podman_socket_path, is_local, status)
		VALUES ($1, $2, 'node-02', 'node-02', '/run/podman.sock', TRUE, 'unreachable')",
	)
	.bind(other)
	.bind(s.team)
	.execute(&s.db)
	.await
	.unwrap();
	let staging = crypto::generate_id();
	sqlx::query(
		"INSERT INTO applications (id, project_id, environment_id, server_id, name, slug, repo_url, container_port)
		VALUES ($1, $2, $3, $4, 'api', 'api', 'https://example.com/r.git', 80)",
	)
	.bind(staging)
	.bind(s.project)
	.bind(s.staging)
	.bind(other)
	.execute(&s.db)
	.await
	.unwrap();
	s.running(9500, staging, IMAGE).await;

	let (_, plan) = s
		.call("GET", &format!("{}/{staging}/promote", s.apps()), None)
		.await;
	assert_eq!(plan["destinations"][0]["application"]["id"], prod);
	assert_eq!(plan["destinations"][0]["moves_image"], true);

	let (st, made) = s
		.promote(staging, json!({ "environment_id": s.production }))
		.await;
	assert_eq!(st, StatusCode::ACCEPTED, "{made}");
	let (result, after) = s.run_job(made["id"].as_i64().unwrap()).await;
	let Err(JobFailure::Failed(message)) = result else {
		panic!("the move cannot work: {result:?}");
	};
	assert!(
		message.contains("node-02") && message.contains("not ready"),
		"{message}"
	);
	// The worker marks the row failed; the job itself leaves it building.
	assert_ne!(after.status, deployments::DeploymentStatus::Running);
}
