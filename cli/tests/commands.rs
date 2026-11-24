//! The commands against a stand-in podkit: finding an application by slug, following a
//! deploy's build log to its end, reading the health gate, and pushing a `.env`.

use std::fmt::Write as _;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::IntoResponse;
use axum::routing::{delete, get, post};
use cli::args::{Place, Wait};
use cli::commands::{self, Ctx, Push};
use cli::config::{Settings, Source};
use serde_json::{Value, json};

const APP: &str = "/api/v1/teams/7/projects/3/applications/21";

#[derive(Default)]
struct Seen {
	/// Method and path of every call.
	calls: Vec<String>,
	/// The bodies of bulk pushes.
	bulk: Vec<String>,
	/// How often the deployment was asked for.
	polls: usize,
}

#[derive(Clone)]
struct World {
	seen: Arc<Mutex<Seen>>,
	/// What `GET …/deployments/{id}` answers once the build log has ended.
	last: Value,
}

fn authorized(headers: &HeaderMap) -> bool {
	headers
		.get(header::AUTHORIZATION)
		.and_then(|v| v.to_str().ok())
		== Some("Bearer pk_test_x")
}

async fn stand_in(last: Value) -> (String, Arc<Mutex<Seen>>) {
	async fn teams(headers: HeaderMap) -> impl IntoResponse {
		if !authorized(&headers) {
			return (
				StatusCode::UNAUTHORIZED,
				r#"{"error":"invalid token"}"#.to_owned(),
			);
		}
		(
			StatusCode::OK,
			json!([{ "id": 7, "slug": "chikof" }]).to_string(),
		)
	}
	async fn projects() -> String {
		json!([{ "id": 3, "slug": "chiko-web" }, { "id": 4, "slug": "empty" }]).to_string()
	}
	async fn apps(Path((_, p)): Path<(i64, i64)>) -> String {
		if p == 3 {
			json!([{ "id": 21, "slug": "web", "name": "Web", "url": "http://web.example" }])
				.to_string()
		} else {
			"[]".to_owned()
		}
	}
	async fn deploy(State(w): State<World>) -> impl IntoResponse {
		w.seen.lock().unwrap().calls.push("POST deployments".into());
		(
			StatusCode::ACCEPTED,
			json!({ "id": 99, "status": "queued" }).to_string(),
		)
	}
	async fn build_logs() -> impl IntoResponse {
		let lines = [
			json!({ "seq": 1, "ts": 0, "stream": "system", "line": "cloning" }),
			json!({ "seq": 2, "ts": 0, "stream": "stdout", "line": "step 1/2" }),
		];
		let mut body = String::new();
		for l in lines {
			let _ = write!(body, "event: log\ndata: {l}\n\n");
		}
		body.push_str(":\n\nevent: end\ndata: \n\n");
		([(header::CONTENT_TYPE, "text/event-stream")], body)
	}
	async fn one(State(w): State<World>) -> String {
		w.seen.lock().unwrap().polls += 1;
		w.last.to_string()
	}
	async fn env_list() -> String {
		json!([{ "id": 1, "key": "KEEP" }, { "id": 2, "key": "OLD" }, { "id": 3, "key": "SECRET" }])
			.to_string()
	}
	async fn bulk(State(w): State<World>, body: String) -> String {
		let v: Value = serde_json::from_str(&body).unwrap();
		let text = v["text"].as_str().unwrap().to_owned();
		let saved = text.lines().count();
		w.seen.lock().unwrap().bulk.push(text);
		json!({ "saved": saved, "errors": [] }).to_string()
	}
	async fn env_delete(State(w): State<World>, Path(rest): Path<(i64, i64, i64, i64)>) -> String {
		w.seen
			.lock()
			.unwrap()
			.calls
			.push(format!("DELETE env {}", rest.3));
		json!({ "id": rest.3 }).to_string()
	}

	let seen = Arc::new(Mutex::new(Seen::default()));
	let world = World {
		seen: seen.clone(),
		last,
	};
	let app = Router::new()
		.route("/api/v1/teams", get(teams))
		.route("/api/v1/teams/{t}/projects", get(projects))
		.route("/api/v1/teams/{t}/projects/{p}/applications", get(apps))
		.route(&format!("{APP}/deployments"), post(deploy))
		.route(&format!("{APP}/deployments/99/build-logs"), get(build_logs))
		.route(&format!("{APP}/deployments/99"), get(one))
		.route(&format!("{APP}/env-vars"), get(env_list))
		.route(&format!("{APP}/env-vars/bulk"), post(bulk))
		.route(
			"/api/v1/teams/{t}/projects/{p}/applications/{a}/env-vars/{id}",
			delete(env_delete),
		)
		.with_state(world);
	let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
	let addr = listener.local_addr().unwrap();
	tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
	(format!("http://{addr}"), seen)
}

fn ctx(url: &str, token: &str) -> Ctx {
	Ctx::new(Settings {
		url: url.to_owned(),
		url_from: Source::Flag,
		token: Some(token.to_owned()),
		token_from: Source::Flag,
		team: None,
		project: None,
	})
	.unwrap()
}

const WAIT: Wait = Wait {
	on: true,
	settle: 0,
	timeout: 20,
};

#[tokio::test]
async fn deploy_wait_follows_the_log_and_succeeds_once_running() {
	let (url, seen) = stand_in(json!({ "id": 99, "status": "running", "health": "healthy" })).await;
	commands::deploy(
		&ctx(&url, "pk_test_x"),
		&Place::default(),
		"web",
		WAIT,
		false,
	)
	.await
	.unwrap();
	let seen = seen.lock().unwrap();
	assert_eq!(seen.calls, ["POST deployments"]);
	// The log ended, so the status was asked for once and not polled again.
	assert_eq!(seen.polls, 1);
}

#[tokio::test]
async fn deploy_wait_exits_3_when_the_health_gate_kept_the_old_container() {
	let (url, _) = stand_in(json!({
		"id": 99, "status": "failed", "health": "unhealthy",
		"health_detail": "GET / answered 502", "error_message": "health check failed",
	}))
	.await;
	let err = commands::deploy(
		&ctx(&url, "pk_test_x"),
		&Place::default(),
		"web",
		WAIT,
		true,
	)
	.await
	.unwrap_err();
	assert_eq!(err.code, 3);
	assert!(
		err.message.contains("never passed") && err.message.contains("502"),
		"{}",
		err.message
	);
}

#[tokio::test]
async fn deploy_wait_with_settle_exits_3_on_a_rollback() {
	let (url, _) =
		stand_in(json!({ "id": 99, "status": "stopped", "health": "rolled_back" })).await;
	let wait = Wait { settle: 1, ..WAIT };
	let err = commands::deploy(
		&ctx(&url, "pk_test_x"),
		&Place::default(),
		"web",
		wait,
		false,
	)
	.await
	.unwrap_err();
	assert_eq!(err.code, 3);
	assert!(err.message.contains("rolled back"), "{}", err.message);
}

#[tokio::test]
async fn an_unknown_app_or_project_is_a_usage_error_and_a_bad_token_says_log_in() {
	let (url, _) = stand_in(Value::Null).await;
	let c = ctx(&url, "pk_test_x");
	let err = commands::deploy(&c, &Place::default(), "nope", WAIT, false)
		.await
		.unwrap_err();
	assert_eq!(err.code, 2);
	assert!(
		err.message.contains("no application nope"),
		"{}",
		err.message
	);
	let place = Place {
		team: None,
		project: Some("missing".into()),
	};
	let err = commands::deploy(&c, &place, "web", WAIT, false)
		.await
		.unwrap_err();
	assert!(err.message.contains("chiko-web (3)"), "{}", err.message);

	let err = commands::apps(&ctx(&url, "pk_wrong"), &Place::default(), true)
		.await
		.unwrap_err();
	assert_eq!(err.code, 1);
	assert!(
		err.message.contains("401") && err.message.contains("podkit login"),
		"{}",
		err.message
	);
}

#[tokio::test]
async fn env_push_sends_the_file_skips_blanks_and_prunes() {
	let (url, seen) = stand_in(Value::Null).await;
	let dir = std::env::temp_dir().join(format!("podkit-cli-push-{}", std::process::id()));
	std::fs::create_dir_all(&dir).unwrap();
	let file = dir.join(".env");
	// SECRET is empty, as a pulled file leaves it: it must not be blanked.
	std::fs::write(&file, "KEEP=\"a # b\"\nNEW=1\nSECRET=\n").unwrap();
	let push = Push {
		file: file.to_str().unwrap(),
		yes: true,
		prune: true,
		allow_empty: false,
		dry_run: false,
	};
	commands::env_push(&ctx(&url, "pk_test_x"), &Place::default(), "web", push)
		.await
		.unwrap();
	{
		let seen = seen.lock().unwrap();
		assert_eq!(seen.bulk, ["KEEP=\"a # b\"\nNEW=\"1\"\n"]);
		assert_eq!(seen.calls, ["DELETE env 2"]);
	}

	// A dry run sends nothing.
	let (url, seen) = stand_in(Value::Null).await;
	let push = Push {
		file: file.to_str().unwrap(),
		yes: false,
		prune: true,
		allow_empty: false,
		dry_run: true,
	};
	commands::env_push(&ctx(&url, "pk_test_x"), &Place::default(), "web", push)
		.await
		.unwrap();
	assert!(seen.lock().unwrap().bulk.is_empty());

	// pull refuses to overwrite without --force, and writes a private file with it.
	let c = ctx(&url, "pk_test_x");
	let path = file.to_str().unwrap();
	let err = commands::env_pull(&c, &Place::default(), "web", path, false)
		.await
		.unwrap_err();
	assert_eq!(err.code, 2);
	commands::env_pull(&c, &Place::default(), "web", path, true)
		.await
		.unwrap();
	let pulled = std::fs::read_to_string(&file).unwrap();
	assert!(pulled.contains("\nKEEP=\nOLD=\nSECRET=\n"), "{pulled}");
	#[cfg(unix)]
	{
		use std::os::unix::fs::PermissionsExt as _;
		let mode = std::fs::metadata(&file).unwrap().permissions().mode();
		assert_eq!(mode & 0o777, 0o600);
	}
	std::fs::remove_dir_all(&dir).unwrap();
}
