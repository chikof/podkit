//! Private access through Tailscale: the sidecar's deploy request, the write-only auth key, the
//! jobs that start and remove it (against a fake agent), Headscale URLs, tenancy and the pages.
//! `cargo test -p podkit-core -p podkit-ui --all-features -- tailscale`
//! Joining a real tailnet needs an account, so that test is `#[ignore]`d: see `docs/tailscale.md`.

use std::time::Duration;

use axum::http::StatusCode;
use serde_json::{Value, json};
use sqlx::PgPool;
use tokio_util::sync::CancellationToken;

use super::run;
use super::spec::{self, Node};
use crate::ids::ApplicationId;
use crate::jobs::{self, JobFailure, JobKind};
use crate::tests::tailscale::{KEY, Setup, sidecar};
use crate::tests::{call, count};

fn env<'a>(spec: &'a proto::DeploySpec, key: &str) -> Option<&'a str> {
	spec.env
		.iter()
		.find(|(k, _)| k == key)
		.map(|(_, v)| v.as_str())
}

#[test]
fn tailscale_sidecar_spec_joins_the_network_keeps_state_on_a_volume_and_serves_the_app() {
	let spec = spec::build_spec(&sidecar(None, &[], "shop"));

	// The official image; userspace networking needs no capabilities.
	assert_eq!(spec.service, "ts-app-7");
	assert!(matches!(
		&spec.source,
		proto::Source::Image { reference, .. } if reference == "docker.io/tailscale/tailscale:stable"
	));
	assert_eq!(env(&spec, "TS_USERSPACE"), Some("true"));
	assert_eq!(env(&spec, "TS_HOSTNAME"), Some("shop"));
	assert_eq!(env(&spec, "TS_AUTHKEY"), Some(KEY));
	assert_eq!(env(&spec, "TS_STATE_DIR"), Some("/var/lib/tailscale"));
	assert_eq!(env(&spec, "TS_AUTH_ONCE"), Some("true"));
	assert_eq!(env(&spec, "TS_SERVE_CONFIG"), Some("/config/serve.json"));
	// Nothing extra for the Tailscale service itself.
	assert_eq!(env(&spec, "TS_EXTRA_ARGS"), None);

	// On the application's network, with no public route and no published port.
	assert_eq!(spec.attach.len(), 1);
	assert_eq!(spec.attach[0].name, "pk-env-9");
	assert!(spec.routing.is_none());
	assert!(spec.container_port.is_none());
	assert!(spec.published_ports.is_empty());

	// The node's identity survives a restart: it is on a named volume.
	assert_eq!(spec.volumes.len(), 1);
	assert_eq!(spec.volumes[0].name, "ts-app-7-state");
	assert_eq!(spec.volumes[0].path, "/var/lib/tailscale");
	assert!(!spec.volumes[0].read_only);

	// The serve config is a file that proxies HTTPS to the application's stable alias.
	assert_eq!(spec.options.files.len(), 1);
	assert_eq!(spec.options.files[0].path, "/config/serve.json");
	let serve: Value = serde_json::from_str(&spec.options.files[0].content).unwrap();
	assert_eq!(serve["TCP"]["443"]["HTTPS"], json!(true));
	assert_eq!(
		serve["Web"]["${TS_CERT_DOMAIN}:443"]["Handlers"]["/"]["Proxy"],
		json!("http://pk-app-7:3000")
	);
	// The state volume has one writer at a time.
	assert!(matches!(spec.strategy, proto::Strategy::Recreate));
}

#[test]
fn tailscale_sidecar_passes_the_login_server_and_tags_as_extra_args() {
	let tags = vec!["tag:podkit".to_owned(), "tag:web".to_owned()];
	let spec = spec::build_spec(&sidecar(Some("https://hs.example.com"), &tags, "shop"));
	assert_eq!(
		env(&spec, "TS_EXTRA_ARGS"),
		Some("--login-server=https://hs.example.com --advertise-tags=tag:podkit,tag:web")
	);
	let only_tags = spec::build_spec(&sidecar(None, &tags, "shop"));
	assert_eq!(
		env(&only_tags, "TS_EXTRA_ARGS"),
		Some("--advertise-tags=tag:podkit,tag:web")
	);
}

#[test]
fn tailscale_headscale_login_server_urls_are_normalized_or_refused() {
	let ok = |s: &str| spec::normalize_login_server(s).unwrap();
	// Empty means the Tailscale service itself.
	assert_eq!(ok(""), None);
	assert_eq!(ok("   "), None);
	assert_eq!(
		ok(" https://hs.example.com/ ").as_deref(),
		Some("https://hs.example.com")
	);
	// A homelab Headscale is often plain HTTP on a private address, with a port or a path.
	assert_eq!(
		ok("http://192.168.1.5:8080").as_deref(),
		Some("http://192.168.1.5:8080")
	);
	assert_eq!(
		ok("https://example.com/headscale/").as_deref(),
		Some("https://example.com/headscale")
	);
	for bad in [
		"hs.example.com",
		"ftp://hs.example.com",
		"https://",
		"https://user:pw@hs.example.com",
		"https://hs.example.com/?x=1",
		"https://hs example.com",
		"https://hs.example.com --advertise-exit-node",
		"javascript:alert(1)",
	] {
		assert!(
			spec::normalize_login_server(bad).is_err(),
			"{bad:?} should be refused"
		);
	}
}

#[test]
fn tailscale_tags_are_parsed_normalized_and_bounded() {
	assert_eq!(
		spec::parse_tags("tag:podkit, web  tag:podkit\nops").unwrap(),
		["tag:podkit", "tag:web", "tag:ops"]
	);
	assert!(spec::parse_tags("").unwrap().is_empty());
	for bad in ["Tag:Upper", "tag:a;rm", "tag:", "tag:1abc", "tag:a_b"] {
		assert!(spec::parse_tags(bad).is_err(), "{bad:?} should be refused");
	}
	let many = (0..11)
		.map(|n| format!("t{n}"))
		.collect::<Vec<_>>()
		.join(" ");
	assert!(spec::parse_tags(&many).is_err());
}

#[test]
fn tailscale_node_names_and_auth_keys_are_checked() {
	assert!(spec::valid_hostname("shop-2"));
	for bad in ["", "-a", "a-", "Shop", "a.b", "a_b", &"a".repeat(64)] {
		assert!(!spec::valid_hostname(bad), "{bad:?}");
	}
	assert_eq!(spec::default_hostname("My_App.v2"), "my-app-v2");
	assert!(spec::check_auth_key(KEY).is_ok());
	// Headscale pre-auth keys are plain hex.
	assert!(spec::check_auth_key("0123456789abcdef0123456789abcdef").is_ok());
	assert!(spec::check_auth_key("two words").is_err());
	assert!(spec::check_auth_key("").is_err());
}

#[test]
fn tailscale_status_output_is_read() {
	assert_eq!(
		spec::parse_status(
			r#"{"BackendState":"Running","Self":{"DNSName":"shop.tail1234.ts.net."}}"#
		),
		Some(Node::Running {
			dns_name: "shop.tail1234.ts.net".to_owned()
		})
	);
	assert_eq!(
		spec::parse_status(r#"{"BackendState":"NeedsLogin"}"#),
		Some(Node::NeedsLogin)
	);
	assert_eq!(
		spec::parse_status(r#"{"BackendState":"Starting"}"#),
		Some(Node::Other("Starting".to_owned()))
	);
	assert_eq!(spec::parse_status("not json"), None);
}

#[sqlx::test]
async fn tailscale_settings_refuse_a_bad_login_server_tag_or_key(db: PgPool) {
	let s = Setup::new(db).await;
	for body in [
		json!({ "login_server": "hs.example.com" }),
		json!({ "tags": "tag:Bad!" }),
		json!({ "auth_key": "has spaces" }),
	] {
		let (status, answer) = s.put_settings(body.clone()).await;
		assert!(status.is_client_error(), "{body}: {status} {answer}");
	}
	assert_eq!(count(&s.db, "tailscale_settings").await, 0);
}

// --- enabling, the start job and removal -------------------------------------------------------

#[sqlx::test]
async fn tailscale_enable_needs_a_stored_key_and_a_valid_name(db: PgPool) {
	let s = Setup::new(db).await;
	let (status, body) = s.enable(json!({})).await;
	assert!(status.is_client_error(), "{status} {body}");
	assert!(body.to_string().contains("auth key"));
	s.store_key().await;
	let (status, _) = s.enable(json!({ "hostname": "Not A Name" })).await;
	assert!(status.is_client_error());
	assert_eq!(count(&s.db, "tailscale_exposures").await, 0);
}

#[sqlx::test]
async fn tailscale_start_job_deploys_the_sidecar_and_records_the_node(db: PgPool) {
	let s = Setup::new(db).await;
	s.store_key().await;
	s.put_settings(json!({ "login_server": "http://10.0.0.2:8080", "tags": "tag:podkit" }))
		.await;
	let (status, body) = s.enable(json!({})).await;
	assert_eq!(status, StatusCode::OK, "{body}");
	// The name defaults to the slug, and the sidecar starts in a job.
	assert_eq!(body["hostname"], json!("shop"));
	assert_eq!(body["status"], json!("starting"));
	assert_eq!(body["url"], Value::Null);

	s.run_job(JobKind::TailscaleStart).await.unwrap();

	let deploys = s.agent.deploys.lock().unwrap().clone();
	assert_eq!(deploys.len(), 1);
	let spec = &deploys[0];
	assert_eq!(spec["service"], json!(format!("ts-app-{}", s.application)));
	let env: Vec<(String, String)> = serde_json::from_value(spec["env"].clone()).unwrap();
	let get = |k: &str| env.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str());
	assert_eq!(get("TS_AUTHKEY"), Some(KEY));
	assert_eq!(get("TS_HOSTNAME"), Some("shop"));
	assert_eq!(
		get("TS_EXTRA_ARGS"),
		Some("--login-server=http://10.0.0.2:8080 --advertise-tags=tag:podkit")
	);
	assert_eq!(spec["attach"].as_array().unwrap().len(), 1);
	assert_eq!(
		spec["volumes"][0]["name"],
		json!(format!("ts-app-{}-state", s.application))
	);

	let exposure = s.status().await;
	assert_eq!(exposure["status"], json!("running"));
	assert_eq!(exposure["url"], json!("https://shop.tail1234.ts.net"));
	// The key never comes back out through the exposure, either.
	assert!(!exposure.to_string().contains("tskey"));

	// The application itself now answers to the alias the sidecar proxies to.
	let (status, deployment) = call(
		&s.app,
		"POST",
		&format!(
			"/api/v1/teams/{}/projects/{}/applications/{}/deployments",
			s.team, s.project, s.application
		),
		Some(&s.token),
		None,
	)
	.await;
	assert_eq!(status, StatusCode::ACCEPTED, "{deployment}");
	let job = jobs::latest_for(&s.db, JobKind::Deploy, deployment["id"].as_i64().unwrap())
		.await
		.unwrap()
		.expect("a deploy job is queued");
	let mut log = s.state.logs.open(job.id).await.unwrap();
	crate::deployments::run::run(&s.state, &job, &CancellationToken::new(), &mut log)
		.await
		.unwrap();
	log.finish().await;
	let deploys = s.agent.deploys.lock().unwrap().clone();
	let app_spec = deploys.last().unwrap();
	let alias = format!("pk-app-{}", s.application);
	let attached = app_spec["attach"].as_array().unwrap();
	assert!(
		attached.iter().any(|n| n["aliases"]
			.as_array()
			.is_some_and(|a| a.iter().any(|x| x == &json!(alias)))),
		"{attached:?}"
	);
}

#[sqlx::test]
async fn tailscale_a_refused_auth_key_fails_with_a_reason(db: PgPool) {
	let s = Setup::new(db).await;
	s.store_key().await;
	*s.agent.status.lock().unwrap() = r#"{"BackendState":"NeedsLogin"}"#.to_owned();
	s.enable(json!({})).await;
	let job = jobs::latest_for(&s.db, JobKind::TailscaleStart, s.application)
		.await
		.unwrap()
		.unwrap();
	let mut log = s.state.logs.open(job.id).await.unwrap();
	let app = crate::applications::find_by_id(&s.db, ApplicationId(s.application))
		.await
		.unwrap()
		.unwrap();
	let server = crate::servers::find(&s.db, app.server_id)
		.await
		.unwrap()
		.unwrap();
	let client = s.state.agents.client(&s.state, &server).await.unwrap();
	// The real grace period is 40 seconds; shortened here.
	let result = run::wait_joined(
		&s.state,
		&CancellationToken::new(),
		&mut log,
		app.id,
		&client,
		"side1",
		Duration::from_secs(5),
		Duration::from_millis(5),
		Duration::from_millis(30),
	)
	.await;
	match result {
		Err(JobFailure::Failed(m)) => assert!(m.contains("did not accept the auth key"), "{m}"),
		other => panic!("expected a refused key, got {other:?}"),
	}
}

#[sqlx::test]
async fn tailscale_removal_logs_the_node_out_before_the_sidecar_and_volume_go(db: PgPool) {
	let s = Setup::new(db).await;
	s.store_key().await;
	s.enable(json!({})).await;
	s.run_job(JobKind::TailscaleStart).await.unwrap();
	s.agent.calls.lock().unwrap().clear();

	let (status, body) = call(&s.app, "DELETE", &s.exposure_path(), Some(&s.token), None).await;
	assert_eq!(status, StatusCode::OK, "{body}");
	assert_eq!(body["exposure"]["status"], json!("removing"));

	s.run_job(JobKind::TailscaleRemove).await.unwrap();
	let calls = s.agent.calls.lock().unwrap().clone();
	let id = s.application;
	assert_eq!(
		calls,
		[
			"exec side1 tailscale --socket /tmp/tailscaled.sock logout".to_owned(),
			format!("delete service ts-app-{id}"),
			format!("delete volume ts-app-{id}-state"),
		]
	);
	assert_eq!(count(&s.db, "tailscale_exposures").await, 0);
	assert_eq!(s.status().await, Value::Null);

	let actions: Vec<String> = sqlx::query_scalar("SELECT action FROM audit_events ORDER BY id")
		.fetch_all(&s.db)
		.await
		.unwrap();
	for expected in ["tailscale.enable", "tailscale.disable", "tailscale.removed"] {
		assert!(
			actions.iter().any(|a| a == expected),
			"{expected} in {actions:?}"
		);
	}
}

#[sqlx::test]
async fn tailscale_deleting_the_application_logs_the_node_out_too(db: PgPool) {
	let s = Setup::new(db).await;
	s.store_key().await;
	s.enable(json!({})).await;
	s.run_job(JobKind::TailscaleStart).await.unwrap();
	s.agent.calls.lock().unwrap().clear();

	let (status, body) = call(
		&s.app,
		"DELETE",
		&format!(
			"/api/v1/teams/{}/projects/{}/applications/{}",
			s.team, s.project, s.application
		),
		Some(&s.token),
		None,
	)
	.await;
	assert!(status.is_success(), "{status} {body}");
	let calls = s.agent.calls.lock().unwrap().clone();
	assert!(
		calls.iter().any(|c| c.ends_with("logout")),
		"the node should be logged out: {calls:?}"
	);
	assert!(calls.iter().any(|c| c.starts_with("delete volume ts-app-")));
	assert_eq!(count(&s.db, "tailscale_exposures").await, 0);
}

#[sqlx::test]
async fn tailscale_a_failed_start_is_kept_on_the_exposure_and_tells_the_team(db: PgPool) {
	let s = Setup::new(db).await;
	s.store_key().await;
	s.enable(json!({})).await;
	// The key is forgotten between turning it on and the job running.
	sqlx::query("UPDATE tailscale_settings SET auth_key = NULL")
		.execute(&s.db)
		.await
		.unwrap();
	let err = s.run_job(JobKind::TailscaleStart).await.unwrap_err();
	let JobFailure::Failed(message) = &err else {
		panic!("{err:?}")
	};
	assert!(message.contains("no Tailscale auth key"), "{message}");

	let job = jobs::latest_for(&s.db, JobKind::TailscaleStart, s.application)
		.await
		.unwrap()
		.unwrap();
	jobs::fail_target(&s.state, JobKind::TailscaleStart, s.application, &err).await;
	crate::notifications::hooks::job_finished(&s.state, &job, &Err(err)).await;
	let exposure = s.status().await;
	assert_eq!(exposure["status"], json!("failed"));
	assert!(exposure["error"].as_str().unwrap().contains("auth key"));
	let events: i64 = sqlx::query_scalar(
		"SELECT COUNT(*) FROM notification_events WHERE kind = 'tailnet_failed'",
	)
	.fetch_one(&s.db)
	.await
	.unwrap();
	assert_eq!(events, 1);
}

// --- live --------------------------------------------------------------------------------------

/// Needs a real tailnet: `TS_LIVE_AUTHKEY=tskey-auth-… cargo test -p podkit-core --all-features --
/// --ignored tailscale_live`. The sidecar itself is only proven by reaching the app from a
/// tailnet device; the steps are in `docs/tailscale.md`.
#[test]
#[ignore = "needs a Tailscale account: see docs/tailscale.md"]
fn tailscale_live_key_is_well_formed() {
	let key = std::env::var("TS_LIVE_AUTHKEY").expect("set TS_LIVE_AUTHKEY");
	assert!(spec::check_auth_key(&key).is_ok());
}
