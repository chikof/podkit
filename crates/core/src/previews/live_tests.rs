//! The live previews test: a real Gitea in a throwaway Podman, a real pull request, a real
//! build, a real Postgres per preview and the real job dispatcher. Ignored by default; it
//! pulls images and binds local ports.
//!
//! `PODMAN=/run/current-system/sw/bin/podman cargo test -p podkit-core --all-features -- --ignored previews_live`
//!
//! It runs its own private `podman system service` (its own store under `/tmp/pk-k3`, removed
//! when the test ends however it ends), so it never touches the containers or images of a
//! running podkit.
//!
//! Gitea's REST API for branches, pull requests and comments is documented at
//! <https://docs.gitea.com/api/1.24/> (`POST /repos/{owner}/{repo}/branches`,
//! `POST /repos/{owner}/{repo}/pulls`, `PATCH /repos/{owner}/{repo}/pulls/{index}`,
//! `GET /repos/{owner}/{repo}/issues/{index}/comments`).

use std::sync::Arc;
use std::time::Duration;

use axum::http::StatusCode;
use base64::Engine;
use serde_json::json;
use sqlx::PgPool;

use crate::databases::{self, DatabaseStatus, EngineKind, service::CreateInput};
use crate::git_providers::live_tests::{
	GITEA_IMAGE, GITEA_PORT, Gitea, HTTPS_PORT, INGRESS_PORT, Private, text, wait_for,
};
use crate::ids::{ApplicationId, ServerId};
use crate::previews::service::{self, SettingsInput};
use crate::previews::{self, State};
use crate::tests::{call, state, user_id, user_with_team};
use crate::{deployments, env_vars};

/// A page server with nothing but busybox's `nc`: one answer per connection.
const DOCKERFILE: &str = r#"FROM docker.io/library/alpine:3.21
CMD ["sh", "-c", "while true; do printf 'HTTP/1.1 200 OK\\r\\nContent-Length: 15\\r\\nConnection: close\\r\\n\\r\\npreview-answers' | nc -l -p 80; done"]
"#;

/// Names of the containers in the private Podman, one per line.
fn containers(private: &Private) -> Vec<String> {
	text(&private.podman(&["ps", "-a", "--format", "{{.Names}}"]))
		.lines()
		.map(str::to_owned)
		.collect()
}

#[sqlx::test]
#[ignore = "pulls gitea, postgres, traefik and alpine into a private Podman store; binds 23000, 28080 and 28081"]
async fn previews_live_a_pull_request_gets_a_fresh_postgres_and_closing_it_removes_everything(
	db: PgPool,
) {
	let _ = rustls::crypto::ring::default_provider().install_default();
	let private = Private::start();
	wait_for("the private podman service", 30, || async {
		private.socket().exists() && private.podman(&["info"]).status.success()
	})
	.await;

	// podkit, listening where the Gitea container can reach it.
	let listener = tokio::net::TcpListener::bind("0.0.0.0:0").await.unwrap();
	let podkit_port = listener.local_addr().unwrap().port();
	let mut st = state(db.clone());
	{
		let config = Arc::make_mut(&mut st.config);
		config.ingress_port = INGRESS_PORT;
		config.https_port = HTTPS_PORT;
		config.public_url = Some(format!("http://host.containers.internal:{podkit_port}"));
	}
	let app = crate::routes::routes(st.clone()).await.unwrap();
	tokio::spawn({
		let app = app.clone();
		async move { axum::serve(listener, app).await.unwrap() }
	});

	// The agent, in this process, on the private Podman, and the real dispatcher.
	let podman = agent::podman::Podman::connect(private.socket().to_str().unwrap())
		.expect("connect to the private podman");
	let data_dir = tempfile::tempdir().unwrap().keep();
	for sub in ["work", "backups", "traefik-acme", "gateway"] {
		std::fs::create_dir_all(data_dir.join(sub)).unwrap();
	}
	let agent_dir = data_dir.clone();
	let ctx = Arc::new(agent::Ctx {
		docker: podman.docker().clone(),
		supervisor: agent::supervisor::Supervisor::start(podman.clone(), &data_dir).await,
		data_dir,
		podman_socket: private.socket().display().to_string(),
		http: reqwest::Client::new(),
	});
	let agent_listener = tokio::net::UnixListener::bind(&st.config.local_agent_socket).unwrap();
	tokio::spawn(async move {
		axum::serve(agent_listener, agent::routes::router(ctx))
			.await
			.unwrap();
	});
	tokio::spawn(crate::jobs::worker::run_dispatcher(
		st.clone(),
		st.shutdown.clone(),
	));

	// Gitea, with an admin and a token.
	let started = private.podman(&[
		"run",
		"-d",
		"--name",
		"gitea",
		"-p",
		&format!("127.0.0.1:{GITEA_PORT}:3000"),
		"-e",
		"GITEA__security__INSTALL_LOCK=true",
		"-e",
		"GITEA__database__DB_TYPE=sqlite3",
		"-e",
		&format!("GITEA__server__ROOT_URL=http://127.0.0.1:{GITEA_PORT}/"),
		"-e",
		"GITEA__server__DISABLE_SSH=true",
		"-e",
		"GITEA__service__DISABLE_REGISTRATION=true",
		"-e",
		"GITEA__webhook__ALLOWED_HOST_LIST=*",
		GITEA_IMAGE,
	]);
	assert!(started.status.success(), "{}", text(&started));
	let http = reqwest::Client::new();
	wait_for("gitea", 180, || async {
		http.get(format!("http://127.0.0.1:{GITEA_PORT}/api/healthz"))
			.send()
			.await
			.is_ok_and(|r| r.status().is_success())
	})
	.await;
	let created = private.podman(&[
		"exec",
		"gitea",
		"gitea",
		"admin",
		"user",
		"create",
		"--username",
		"ops",
		"--password",
		"correct-horse-battery",
		"--email",
		"ops@example.com",
		"--admin",
		"--must-change-password=false",
	]);
	assert!(created.status.success(), "{}", text(&created));
	let minted = private.podman(&[
		"exec",
		"gitea",
		"gitea",
		"admin",
		"user",
		"generate-access-token",
		"--username",
		"ops",
		"--token-name",
		"podkit",
		"--scopes",
		"all",
		"--raw",
	]);
	assert!(minted.status.success(), "{}", text(&minted));
	let token = String::from_utf8_lossy(&minted.stdout)
		.split_whitespace()
		.last()
		.unwrap()
		.to_owned();
	let gitea = Gitea {
		http: http.clone(),
		token: token.clone(),
	};
	let (s, repo) = gitea
		.send(
			reqwest::Method::POST,
			"/user/repos",
			Some(
				json!({ "name": "demo", "private": true, "auto_init": true, "default_branch": "main" }),
			),
		)
		.await;
	assert_eq!(s, StatusCode::CREATED, "{repo}");
	let file = |branch: &str, name: &str, content: &str| {
		json!({
			"content": base64::engine::general_purpose::STANDARD.encode(content),
			"message": format!("add {name}"), "branch": branch,
		})
	};
	let (s, body) = gitea
		.send(
			reqwest::Method::POST,
			"/repos/ops/demo/contents/Dockerfile",
			Some(file("main", "Dockerfile", DOCKERFILE)),
		)
		.await;
	assert_eq!(s, StatusCode::CREATED, "{body}");

	// podkit: the provider, a project, the application, a production Postgres.
	let (api_token, team) = user_with_team(&app, "alice").await;
	let alice = crate::ids::UserId(user_id(&db, "alice").await);
	let t = Some(api_token.as_str());
	let (s, provider) = call(
		&app,
		"POST",
		&format!("/api/v1/teams/{team}/git-providers"),
		t,
		Some(json!({
			"kind": "gitea", "name": "live", "base_url": format!("http://127.0.0.1:{GITEA_PORT}"), "token": token,
		})),
	)
	.await;
	assert_eq!(s, StatusCode::CREATED, "{provider}");
	let (_, project) = call(
		&app,
		"POST",
		&format!("/api/v1/teams/{team}/projects"),
		t,
		Some(json!({ "name": "web", "slug": "web" })),
	)
	.await;
	let (_, servers) = call(
		&app,
		"GET",
		&format!("/api/v1/teams/{team}/servers"),
		t,
		None,
	)
	.await;
	let (s, application) = call(
		&app,
		"POST",
		&format!(
			"/api/v1/teams/{team}/projects/{}/applications",
			project["id"]
		),
		t,
		Some(json!({
			"name": "demo", "slug": "demo", "server_id": servers[0]["id"],
			"git_provider_id": provider["id"], "git_repo": "ops/demo", "git_ref": "main",
			"container_port": 80,
		})),
	)
	.await;
	assert_eq!(s, StatusCode::OK, "{application}");
	let app_id = ApplicationId(application["id"].as_i64().unwrap());
	let team_id = crate::ids::TeamId(team);
	let project_id = crate::ids::ProjectId(project["id"].as_i64().unwrap());
	let stored = crate::applications::find_by_id(&db, app_id)
		.await
		.unwrap()
		.unwrap();

	let outcome = std::panic::AssertUnwindSafe(async {
		let prod = databases::service::create(
			&st,
			alice,
			team_id,
			project_id,
			CreateInput {
				name: "orders".to_owned(),
				engine: EngineKind::Postgres,
				version: None,
				server_id: ServerId(servers[0]["id"].as_i64().unwrap()),
				public_port: None,
				memory_limit_mb: None,
				cpu_limit: None,
				environment_id: Some(stored.environment_id),
			},
		)
		.await
		.unwrap();
		wait_for("the production database", 300, || async {
			databases::find_by_id(&db, prod.id)
				.await
				.unwrap()
				.is_some_and(|d| d.status == DatabaseStatus::Running)
		})
		.await;
		let prod_url = previews::build::connection(&st, &prod).unwrap().url;
		env_vars::service::upsert(
			&st,
			alice,
			team_id,
			project_id,
			app_id,
			"DATABASE_URL",
			&prod_url,
		)
		.await
		.unwrap();
		let var = env_vars::list(&db, app_id).await.unwrap().remove(0);
		service::set_variable(&st, alice, team_id, project_id, app_id, var.id, true)
			.await
			.unwrap();
		service::save(
			&st,
			alice,
			team_id,
			project_id,
			app_id,
			&SettingsInput {
				enabled: true,
				// The "migration": it proves the command runs in the preview, with the
				// preview's own connection URL.
				command: "echo \"$DATABASE_URL\" > /tmp/migrated".to_owned(),
				..SettingsInput::default()
			},
		)
		.await
		.unwrap();
		let (_, hooks) = gitea
			.send(reqwest::Method::GET, "/repos/ops/demo/hooks", None)
			.await;
		let events = hooks[0]["events"].to_string();
		assert!(
			events.contains("pull_request"),
			"the webhook asks for pull request events: {events}"
		);

		// A pull request from a branch.
		let (s, body) = gitea
			.send(
				reqwest::Method::POST,
				"/repos/ops/demo/branches",
				Some(json!({ "new_branch_name": "feature", "old_branch_name": "main" })),
			)
			.await;
		assert_eq!(s, StatusCode::CREATED, "{body}");
		let (s, body) = gitea
			.send(
				reqwest::Method::POST,
				"/repos/ops/demo/contents/feature.txt",
				Some(file("feature", "feature.txt", "feature\n")),
			)
			.await;
		assert_eq!(s, StatusCode::CREATED, "{body}");
		let (s, pr) = gitea
			.send(
				reqwest::Method::POST,
				"/repos/ops/demo/pulls",
				Some(json!({ "head": "feature", "base": "main", "title": "A feature" })),
			)
			.await;
		assert_eq!(s, StatusCode::CREATED, "{pr}");
		let number = i32::try_from(pr["number"].as_i64().unwrap()).unwrap();

		// The preview comes up.
		wait_for("the preview to be ready", 900, || async {
			previews::find_for_pr(&db, app_id, number)
				.await
				.unwrap()
				.is_some_and(|p| matches!(p.state(), State::Ready | State::Failed))
		})
		.await;
		let preview = previews::find_for_pr(&db, app_id, number)
			.await
			.unwrap()
			.unwrap();
		assert_eq!(preview.state(), State::Ready, "{}", preview.detail);
		assert!(
			preview.url.contains(&format!("pr-{number}-demo")),
			"{}",
			preview.url
		);
		let preview_app_id = preview.preview_application_id.unwrap();
		let preview_env = preview.environment_id.unwrap();

		// Its own, empty Postgres, running; the command ran in the preview with that URL.
		let dbs = databases::list_by_environment(&db, preview_env)
			.await
			.unwrap();
		assert_eq!(dbs.len(), 1, "one fresh database");
		assert_ne!(dbs[0].id, prod.id);
		assert_eq!(dbs[0].status, DatabaseStatus::Running);
		let preview_url = previews::build::connection(&st, &dbs[0]).unwrap().url;
		assert_ne!(preview_url, prod_url);
		let running = deployments::latest_running(&db, preview_app_id)
			.await
			.unwrap()
			.unwrap();
		let container = running.container_id.unwrap();
		let migrated = private.podman(&["exec", &container, "cat", "/tmp/migrated"]);
		let migrated = text(&migrated);
		assert_eq!(
			migrated.trim(),
			preview_url,
			"the command saw the preview's URL"
		);
		assert!(
			!migrated.contains(&format!("podkit-db-{}", prod.id)),
			"{migrated}"
		);
		// Nothing of production's data: the preview database has no tables.
		let rows = private.podman(&[
			"exec",
			dbs[0].container_id.as_deref().unwrap(),
			"psql",
			"-U",
			"podkit",
			"-d",
			"podkit",
			"-tA",
			"-c",
			"SELECT count(*) FROM information_schema.tables WHERE table_schema = 'public'",
		]);
		assert_eq!(text(&rows).trim(), "0", "{}", text(&rows));

		// The URL answers, through the ingress, by its host name.
		let host = preview
			.url
			.trim_start_matches("http://")
			.split([':', '/'])
			.next()
			.unwrap()
			.to_owned();
		wait_for("the preview URL to answer", 120, || async {
			http.get(format!("http://127.0.0.1:{INGRESS_PORT}/"))
				.header("host", &host)
				.send()
				.await
				.is_ok_and(|r| r.status().is_success())
		})
		.await;
		let body = http
			.get(format!("http://127.0.0.1:{INGRESS_PORT}/"))
			.header("host", &host)
			.send()
			.await
			.unwrap()
			.text()
			.await
			.unwrap();
		assert!(body.contains("preview-answers"), "{body}");

		// The pull request carries one comment with the link.
		let (_, comments) = gitea
			.send(
				reqwest::Method::GET,
				&format!("/repos/ops/demo/issues/{number}/comments"),
				None,
			)
			.await;
		let ours = comments
			.as_array()
			.unwrap()
			.iter()
			.filter(|c| c["body"].as_str().is_some_and(|b| b.contains(&preview.url)))
			.count();
		assert_eq!(ours, 1, "{comments}");

		// Close the pull request: everything goes.
		let before = containers(&private);
		assert!(
			before
				.iter()
				.any(|n| n.contains(&format!("podkit-db-{}", dbs[0].id))),
			"{before:?}"
		);
		let (s, body) = gitea
			.send(
				reqwest::Method::PATCH,
				&format!("/repos/ops/demo/pulls/{number}"),
				Some(json!({ "state": "closed" })),
			)
			.await;
		assert!(s.is_success(), "{s}: {body}");
		wait_for("the preview to be removed", 300, || async {
			previews::find_for_pr(&db, app_id, number)
				.await
				.unwrap()
				.is_none()
		})
		.await;
		assert!(
			crate::applications::find_by_id(&db, preview_app_id)
				.await
				.unwrap()
				.is_none(),
			"the preview application is gone"
		);
		assert!(
			databases::list_by_environment(&db, preview_env)
				.await
				.unwrap()
				.is_empty(),
			"the preview database is gone"
		);
		let environments: i64 =
			sqlx::query_scalar("SELECT COUNT(*) FROM environments WHERE id = $1")
				.bind(preview_env)
				.fetch_one(&db)
				.await
				.unwrap();
		assert_eq!(environments, 0, "the preview environment is gone");
		let after = containers(&private);
		assert!(
			!after
				.iter()
				.any(|n| n.contains(&format!("podkit-db-{}", dbs[0].id)) || *n == container),
			"{after:?}"
		);
		assert!(
			after
				.iter()
				.any(|n| n.contains(&format!("podkit-db-{}", prod.id))),
			"production's database is untouched: {after:?}"
		);
		tokio::time::sleep(Duration::from_secs(1)).await;
	});
	let result = futures_util::FutureExt::catch_unwind(outcome).await;
	drop(private);
	let _ = std::fs::remove_dir_all(&agent_dir);
	if let Err(panic) = result {
		std::panic::resume_unwind(panic);
	}
}

/// A page server whose answer is `answer`, which must be exactly 15 bytes.
fn answering(answer: &str) -> String {
	assert_eq!(answer.len(), 15, "the Content-Length below is 15");
	DOCKERFILE.replace("preview-answers", answer)
}

/// The preview's page, by its host name through the ingress; `None` while it does not answer
/// (the page server takes one connection at a time, so a try or two may find it busy).
async fn page(http: &reqwest::Client, url: &str) -> Option<String> {
	for _ in 0..10 {
		if let Some(text) = page_once(http, url).await {
			return Some(text);
		}
		tokio::time::sleep(Duration::from_millis(500)).await;
	}
	None
}

async fn page_once(http: &reqwest::Client, url: &str) -> Option<String> {
	let host = url
		.trim_start_matches("http://")
		.split([':', '/'])
		.next()
		.unwrap();
	let res = http
		.get(format!("http://127.0.0.1:{INGRESS_PORT}/"))
		.header("host", host)
		.send()
		.await
		.ok()?;
	res.status().is_success().then_some(())?;
	res.text().await.ok()
}

/// A fork's pull request in a real Gitea: a maintainer approves it with `/preview`, the author
/// pushes a second commit, and the running preview goes on serving the approved commit until
/// the maintainer approves again.
///
/// `PODMAN=/run/current-system/sw/bin/podman cargo test -p podkit-core --all-features -- --ignored previews_live_fork`
#[sqlx::test]
#[ignore = "pulls gitea and alpine into a private Podman store; binds 23000, 28080 and 28081"]
#[allow(clippy::too_many_lines, reason = "one scenario, read top to bottom")]
async fn previews_live_fork_push_after_approval_serves_the_approved_commit_until_reapproved(
	db: PgPool,
) {
	let _ = rustls::crypto::ring::default_provider().install_default();
	let private = Private::start();
	wait_for("the private podman service", 30, || async {
		private.socket().exists() && private.podman(&["info"]).status.success()
	})
	.await;

	let listener = tokio::net::TcpListener::bind("0.0.0.0:0").await.unwrap();
	let podkit_port = listener.local_addr().unwrap().port();
	let mut st = state(db.clone());
	{
		let config = Arc::make_mut(&mut st.config);
		config.ingress_port = INGRESS_PORT;
		config.https_port = HTTPS_PORT;
		config.public_url = Some(format!("http://host.containers.internal:{podkit_port}"));
	}
	let app = crate::routes::routes(st.clone()).await.unwrap();
	tokio::spawn({
		let app = app.clone();
		async move { axum::serve(listener, app).await.unwrap() }
	});
	let podman = agent::podman::Podman::connect(private.socket().to_str().unwrap())
		.expect("connect to the private podman");
	let data_dir = tempfile::tempdir().unwrap().keep();
	for sub in ["work", "backups", "traefik-acme", "gateway"] {
		std::fs::create_dir_all(data_dir.join(sub)).unwrap();
	}
	let agent_dir = data_dir.clone();
	let ctx = Arc::new(agent::Ctx {
		docker: podman.docker().clone(),
		supervisor: agent::supervisor::Supervisor::start(podman.clone(), &data_dir).await,
		data_dir,
		podman_socket: private.socket().display().to_string(),
		http: reqwest::Client::new(),
	});
	let agent_listener = tokio::net::UnixListener::bind(&st.config.local_agent_socket).unwrap();
	tokio::spawn(async move {
		axum::serve(agent_listener, agent::routes::router(ctx))
			.await
			.unwrap();
	});
	tokio::spawn(crate::jobs::worker::run_dispatcher(
		st.clone(),
		st.shutdown.clone(),
	));

	// Gitea with a maintainer (`ops`, admin) and a contributor (`bob`).
	let started = private.podman(&[
		"run",
		"-d",
		"--name",
		"gitea",
		"-p",
		&format!("127.0.0.1:{GITEA_PORT}:3000"),
		"-e",
		"GITEA__security__INSTALL_LOCK=true",
		"-e",
		"GITEA__database__DB_TYPE=sqlite3",
		"-e",
		&format!("GITEA__server__ROOT_URL=http://127.0.0.1:{GITEA_PORT}/"),
		"-e",
		"GITEA__server__DISABLE_SSH=true",
		"-e",
		"GITEA__service__DISABLE_REGISTRATION=true",
		"-e",
		"GITEA__webhook__ALLOWED_HOST_LIST=*",
		GITEA_IMAGE,
	]);
	assert!(started.status.success(), "{}", text(&started));
	let http = reqwest::Client::new();
	wait_for("gitea", 180, || async {
		http.get(format!("http://127.0.0.1:{GITEA_PORT}/api/healthz"))
			.send()
			.await
			.is_ok_and(|r| r.status().is_success())
	})
	.await;
	let account = |name: &str, admin: bool| {
		let mut args = vec![
			"exec",
			"gitea",
			"gitea",
			"admin",
			"user",
			"create",
			"--username",
			name,
			"--password",
			"correct-horse-battery",
			"--email",
		];
		let email = format!("{name}@example.com");
		args.push(&email);
		if admin {
			args.push("--admin");
		}
		args.push("--must-change-password=false");
		let created = private.podman(&args);
		assert!(created.status.success(), "{}", text(&created));
		let minted = private.podman(&[
			"exec",
			"gitea",
			"gitea",
			"admin",
			"user",
			"generate-access-token",
			"--username",
			name,
			"--token-name",
			"podkit",
			"--scopes",
			"all",
			"--raw",
		]);
		assert!(minted.status.success(), "{}", text(&minted));
		String::from_utf8_lossy(&minted.stdout)
			.split_whitespace()
			.last()
			.unwrap()
			.to_owned()
	};
	let ops_token = account("ops", true);
	let bob_token = account("bob", false);
	let ops = Gitea {
		http: http.clone(),
		token: ops_token.clone(),
	};
	let bob = Gitea {
		http: http.clone(),
		token: bob_token,
	};
	// Public, so that the contributor may fork it.
	let (s, repo) = ops
		.send(
			reqwest::Method::POST,
			"/user/repos",
			Some(
				json!({ "name": "demo", "private": false, "auto_init": true, "default_branch": "main" }),
			),
		)
		.await;
	assert_eq!(s, StatusCode::CREATED, "{repo}");
	let put = |gitea: &Gitea, owner: &'static str, branch: &'static str, answer: &'static str| {
		let (gitea_http, token) = (gitea.http.clone(), gitea.token.clone());
		async move {
			let gitea = Gitea {
				http: gitea_http,
				token,
			};
			let path = format!("/repos/{owner}/demo/contents/Dockerfile");
			let (s, existing) = gitea
				.send(reqwest::Method::GET, &format!("{path}?ref={branch}"), None)
				.await;
			let mut body = json!({
				"content": base64::engine::general_purpose::STANDARD.encode(answering(answer)),
				"message": format!("serve {answer}"), "branch": branch,
			});
			let method = if s == StatusCode::OK {
				body["sha"] = existing["sha"].clone();
				reqwest::Method::PUT
			} else {
				reqwest::Method::POST
			};
			let (s, res) = gitea.send(method, &path, Some(body)).await;
			assert!(s.is_success(), "{s}: {res}");
			res["commit"]["sha"].as_str().unwrap().to_owned()
		}
	};
	put(&ops, "ops", "main", "main-answers!!!").await;

	let (api_token, team) = user_with_team(&app, "alice").await;
	let alice = crate::ids::UserId(user_id(&db, "alice").await);
	let t = Some(api_token.as_str());
	let (s, provider) = call(
		&app,
		"POST",
		&format!("/api/v1/teams/{team}/git-providers"),
		t,
		Some(json!({
			"kind": "gitea", "name": "live", "base_url": format!("http://127.0.0.1:{GITEA_PORT}"),
			"token": ops_token,
		})),
	)
	.await;
	assert_eq!(s, StatusCode::CREATED, "{provider}");
	let (_, project) = call(
		&app,
		"POST",
		&format!("/api/v1/teams/{team}/projects"),
		t,
		Some(json!({ "name": "web", "slug": "web" })),
	)
	.await;
	let (_, servers) = call(
		&app,
		"GET",
		&format!("/api/v1/teams/{team}/servers"),
		t,
		None,
	)
	.await;
	let (s, application) = call(
		&app,
		"POST",
		&format!(
			"/api/v1/teams/{team}/projects/{}/applications",
			project["id"]
		),
		t,
		Some(json!({
			"name": "demo", "slug": "demo", "server_id": servers[0][
				"id"], "git_provider_id": provider["id"], "git_repo": "ops/demo",
			"git_ref": "main", "container_port": 80,
		})),
	)
	.await;
	assert_eq!(s, StatusCode::OK, "{application}");
	let app_id = ApplicationId(application["id"].as_i64().unwrap());
	let team_id = crate::ids::TeamId(team);
	let project_id = crate::ids::ProjectId(project["id"].as_i64().unwrap());

	let outcome = std::panic::AssertUnwindSafe(async {
		service::save(
			&st,
			alice,
			team_id,
			project_id,
			app_id,
			&SettingsInput {
				enabled: true,
				..SettingsInput::default()
			},
		)
		.await
		.unwrap();

		// bob forks, and opens a pull request from his branch.
		let (s, fork) = bob
			.send(
				reqwest::Method::POST,
				"/repos/ops/demo/forks",
				Some(json!({})),
			)
			.await;
		assert!(s.is_success(), "{s}: {fork}");
		wait_for("the fork", 60, || async {
			bob.send(reqwest::Method::GET, "/repos/bob/demo", None)
				.await
				.0
				== StatusCode::OK
		})
		.await;
		let (s, body) = bob
			.send(
				reqwest::Method::POST,
				"/repos/bob/demo/branches",
				Some(json!({ "new_branch_name": "feature", "old_branch_name": "main" })),
			)
			.await;
		assert_eq!(s, StatusCode::CREATED, "{body}");
		let approved = put(&bob, "bob", "feature", "approved-answer").await;
		let (s, pr) = bob
			.send(
				reqwest::Method::POST,
				"/repos/ops/demo/pulls",
				Some(json!({ "head": "bob:feature", "base": "main", "title": "From a fork" })),
			)
			.await;
		assert_eq!(s, StatusCode::CREATED, "{pr}");
		let number = i32::try_from(pr["number"].as_i64().unwrap()).unwrap();
		let find = || async { previews::find_for_pr(&db, app_id, number).await.unwrap() };

		// Nothing is built before a maintainer says so.
		wait_for("the pull request to be seen", 60, || async {
			find().await.is_some()
		})
		.await;
		let p = find().await.unwrap();
		assert_eq!(p.state(), State::AwaitingApproval, "{}", p.detail);
		assert!(p.from_fork && p.environment_id.is_none());

		// The maintainer approves it.
		let comment = |text: &'static str| {
			let ops = &ops;
			async move {
				let (s, body) = ops
					.send(
						reqwest::Method::POST,
						&format!("/repos/ops/demo/issues/{number}/comments"),
						Some(json!({ "body": text })),
					)
					.await;
				assert_eq!(s, StatusCode::CREATED, "{body}");
			}
		};
		comment("/preview").await;
		wait_for("the preview to be ready", 900, || async {
			find()
				.await
				.is_some_and(|p| matches!(p.state(), State::Ready | State::Failed))
		})
		.await;
		let p = find().await.unwrap();
		assert_eq!(p.state(), State::Ready, "{}", p.detail);
		assert_eq!(p.approved_sha, approved, "the approval recorded the commit");
		let url = p.url.clone();
		wait_for("the preview to answer", 120, || async {
			page(&http, &url).await.is_some()
		})
		.await;
		assert_eq!(page(&http, &url).await.unwrap(), "approved-answer");
		let preview_app = p.preview_application_id.unwrap();
		let deploys = || async {
			sqlx::query_scalar::<_, i64>(
				"SELECT COUNT(*) FROM deployments WHERE application_id = $1",
			)
			.bind(preview_app)
			.fetch_one(&db)
			.await
			.unwrap()
		};
		let deployed = deploys().await;

		// The author pushes a second commit.
		let second = put(&bob, "bob", "feature", "second-version!").await;
		assert_ne!(second, approved);
		wait_for("the push to revoke the approval", 120, || async {
			find().await.is_some_and(|p| p.head_sha == second)
		})
		.await;
		let p = find().await.unwrap();
		assert!(!p.approved, "the push revoked it");
		assert_eq!(p.state(), State::AwaitingApproval);
		assert_eq!(
			p.approved_sha, approved,
			"the approved commit is the one that runs"
		);
		let (_, comments) = ops
			.send(
				reqwest::Method::GET,
				&format!("/repos/ops/demo/issues/{number}/comments"),
				None,
			)
			.await;
		assert!(
			comments.to_string().contains("New commits need a new"),
			"{comments}"
		);

		// Time to build the new commit, were anything going to: the preview still serves the
		// approved one, and nothing was deployed.
		tokio::time::sleep(Duration::from_secs(20)).await;
		assert_eq!(page(&http, &url).await.as_deref(), Some("approved-answer"));
		assert_eq!(
			deploys().await,
			deployed,
			"no build for an unapproved commit"
		);

		// The maintainer approves the new commit: now it is built and served.
		comment("/preview").await;
		wait_for("the second commit to be served", 900, || async {
			page(&http, &url).await.as_deref() == Some("second-version!")
		})
		.await;
		let p = find().await.unwrap();
		assert!(p.approved);
		assert_eq!(p.approved_sha, second);
		// The page changes a moment before the job says it is done; closing in that moment
		// races the job's last write (a separate matter from this test).
		wait_for("the job to finish", 120, || async {
			find().await.is_some_and(|p| p.state() == State::Ready)
		})
		.await;
		let (s, body) = ops
			.send(
				reqwest::Method::PATCH,
				&format!("/repos/ops/demo/pulls/{number}"),
				Some(json!({ "state": "closed" })),
			)
			.await;
		assert!(s.is_success(), "{s}: {body}");
		wait_for("the preview to be removed", 300, || async {
			find().await.is_none()
		})
		.await;
	});
	let result = futures_util::FutureExt::catch_unwind(outcome).await;
	drop(private);
	let _ = std::fs::remove_dir_all(&agent_dir);
	if let Err(panic) = result {
		std::panic::resume_unwind(panic);
	}
}
