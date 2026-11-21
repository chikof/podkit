//! Access-rule tests: the lists and limits are checked, the rules reach the agent as the
//! middlewares Traefik is given, internal-only leaves the ingress, a basic-auth hash is never
//! shown, the warnings find published ports, and another team's rules are not there.
//!
//! `cargo test -p server --all-features -- access_rules`

use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Bytes;
use axum::http::{StatusCode, header};
use axum::response::IntoResponse;
use axum::routing::post;
use serde_json::{Value, json};
use sqlx::PgPool;
use tokio_util::sync::CancellationToken;

use super::policy::{self, Exposure};
use super::service::{self, RulesInput};
use super::{ForDeploy, Rules, for_deploy};
use crate::error::ServerError;
use crate::ids::{ApplicationId, EnvironmentId, ProjectId, ServerId, TeamId, UserId};
use crate::jobs::{self, JobKind};
use crate::networks::{self, ResourceKind};
use crate::state::AppState;
use crate::tests::{call, local_server_id, project_id, state, user_id};
use crate::ui::testing::{
	assert_contains, assert_not_contains, get_html, page_main, post_signals, ui_session,
};

const PASSWORD: &str = "hunter2-hunter2";

// --- the lists and limits ----------------------------------------------------------------------

fn lists(allow: &[&str], deny: &[&str]) -> Rules {
	Rules {
		allow_cidrs: allow.iter().map(ToString::to_string).collect(),
		deny_cidrs: deny.iter().map(ToString::to_string).collect(),
		..Rules::default()
	}
}

#[test]
fn access_rules_allowlist_cidrs_are_validated_and_stored_in_their_normal_form() {
	let mut ok = lists(
		&["10.0.0.7/24", "203.0.113.9", "2001:db8::/32", "10.0.0.0/24"],
		&[],
	);
	ok.validate().unwrap();
	// Host bits are cleared, a bare address is a range of one, duplicates go.
	assert_eq!(
		ok.allow_cidrs,
		["10.0.0.0/24", "2001:db8::/32", "203.0.113.9/32"]
	);
	for bad in [
		"nope",
		"10.0.0.0/33",
		"10.0.0/8",
		"10.0.0.0/8/9",
		"::1/129",
		"10.0.0.0/",
		"1.1.1.1;2.2.2.2",
	] {
		let err = lists(&[bad], &[]).validate().unwrap_err();
		assert!(
			matches!(&err, ServerError::Validation(m) if m.contains("not an IP address")),
			"{bad}: {err:?}"
		);
		assert!(lists(&[], &[bad]).validate().is_err(), "{bad} as a deny");
	}
}

#[test]
fn access_rules_lists_that_lock_everyone_out_or_grow_too_large_are_refused() {
	let err = lists(&["10.0.0.0/24"], &["10.0.0.0/8"])
		.validate()
		.unwrap_err();
	assert!(err.to_string().contains("block every address"), "{err}");
	let many: Vec<String> = (0..=super::MAX_ENTRIES)
		.map(|n| format!("10.0.{n}.0/24"))
		.collect();
	let refs: Vec<&str> = many.iter().map(String::as_str).collect();
	assert!(lists(&refs, &[]).validate().is_err());
	// Denying single addresses out of everything splits the space once per bit each.
	let spread: Vec<String> = (0..50).map(|n| format!("{n}.{n}.{n}.{n}")).collect();
	let refs: Vec<&str> = spread.iter().map(String::as_str).collect();
	let err = lists(&[], &refs).validate().unwrap_err();
	assert!(err.to_string().contains("too many pieces"), "{err}");
}

#[test]
fn access_rules_limits_need_sane_numbers_and_a_burst_needs_a_rate() {
	let check = |f: fn(&mut Rules)| {
		let mut r = Rules::default();
		f(&mut r);
		r.validate()
	};
	assert!(check(|r| r.rate_average = Some(5)).is_ok());
	assert!(
		check(|r| {
			r.rate_average = Some(5);
			r.rate_burst = Some(20);
		})
		.is_ok()
	);
	assert!(check(|r| r.rate_average = Some(0)).is_err());
	assert!(check(|r| r.max_body_mb = Some(0)).is_err());
	assert!(check(|r| r.max_body_mb = Some(10_241)).is_err());
	assert!(check(|r| r.rate_burst = Some(5)).is_err());
}

#[test]
fn access_rules_input_reads_lists_in_any_separator_and_names_the_bad_number() {
	let input = RulesInput {
		allow: "10.0.0.0/8, 192.168.0.0/16\n203.0.113.7 ".to_owned(),
		deny: "198.51.100.0/24".to_owned(),
		rate_average: " 5 ".to_owned(),
		max_body_mb: String::new(),
		security_headers: true,
		..RulesInput::default()
	};
	let rules = input.parse(false).unwrap();
	assert_eq!(rules.allow_cidrs.len(), 3);
	assert_eq!(rules.rate_average, Some(5));
	assert_eq!(rules.max_body_mb, None);
	let bad = RulesInput {
		rate_average: "fast".to_owned(),
		..RulesInput::default()
	};
	assert!(
		bad.parse(false)
			.unwrap_err()
			.to_string()
			.contains("rate limit")
	);
}

// --- the middlewares ---------------------------------------------------------------------------

fn label<'a>(labels: &'a [(String, String)], key: &str) -> Option<&'a str> {
	labels
		.iter()
		.find(|(k, _)| k == key)
		.map(|(_, v)| v.as_str())
}

#[test]
fn access_rules_each_option_becomes_its_traefik_middleware() {
	let rules = Rules {
		allow_cidrs: vec!["10.0.0.0/8".to_owned()],
		deny_cidrs: vec![],
		rate_average: Some(5),
		rate_burst: Some(10),
		max_body_mb: Some(2),
		security_headers: true,
		internal_only: false,
	};
	let wire = rules.to_wire(vec!["ann:$2y$10$x".to_owned()]).unwrap();
	assert_eq!(wire.max_body_bytes, Some(2 * 1024 * 1024));
	let labels = agent::ingress::access_labels("app-1", &wire, true, true).unwrap();
	let m = |k: &str| label(&labels, &format!("traefik.http.middlewares.app-1-{k}"));
	assert_eq!(m("allow.ipallowlist.sourcerange"), Some("10.0.0.0/8"));
	assert_eq!(m("rate.ratelimit.average"), Some("5"));
	assert_eq!(m("rate.ratelimit.burst"), Some("10"));
	assert_eq!(m("auth.basicauth.users"), Some("ann:$2y$10$x"));
	assert_eq!(m("body.buffering.maxrequestbodybytes"), Some("2097152"));
	assert_eq!(m("headers.headers.framedeny"), Some("true"));
	assert_eq!(m("headers.headers.stsseconds"), Some("31536000"));
	assert_eq!(m("headers.headers.contenttypenosniff"), Some("true"));
	assert_eq!(
		m("headers.headers.referrerpolicy"),
		Some("strict-origin-when-cross-origin")
	);
	assert!(label(&labels, "traefik.http.routers.app-1.middlewares").is_some());
	assert!(label(&labels, "traefik.http.routers.app-1-secure.middlewares").is_some());
}

#[test]
fn access_rules_a_denylist_is_sent_as_the_allowed_remainder() {
	let wire = lists(&[], &["203.0.113.7"]).to_wire(vec![]).unwrap();
	let labels = agent::ingress::access_labels("app-1", &wire, true, false).unwrap();
	let range = label(
		&labels,
		"traefik.http.middlewares.app-1-allow.ipallowlist.sourcerange",
	)
	.unwrap();
	let ranges: Vec<proto::cidr::Cidr> = range
		.split(',')
		.map(|r| proto::cidr::Cidr::parse(r).unwrap())
		.collect();
	let at = |ip: &str| ranges.iter().any(|r| r.contains_ip(ip.parse().unwrap()));
	assert!(!at("203.0.113.7"));
	assert!(at("203.0.113.8") && at("8.8.8.8") && at("::1"));
}

#[test]
fn access_rules_the_defaults_are_only_the_security_headers() {
	let wire = Rules::default().to_wire(vec![]).unwrap();
	assert!(wire.security_headers);
	assert!(wire.allow.is_empty() && wire.basic_auth.is_empty() && wire.rate_average.is_none());
	let off = Rules {
		security_headers: false,
		..Rules::default()
	};
	assert!(
		off.to_wire(vec![]).is_none(),
		"nothing to write, no middleware"
	);
}

// --- the stack under test ----------------------------------------------------------------------

struct Setup {
	app: Router,
	state: AppState,
	db: PgPool,
	cookie: String,
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
		let (cookie, team) = ui_session(&app, "alice").await;
		let (_, login) = call(
			&app,
			"POST",
			"/api/v1/auth/login",
			None,
			Some(json!({ "email": "alice@example.com", "password": "correct horse battery" })),
		)
		.await;
		let (status, _, _) = post_signals(
			&app,
			&format!("/teams/{team}/projects"),
			Some(&cookie),
			&json!({ "name": "Shop", "slug": "shop" }),
		)
		.await;
		assert_eq!(status, StatusCode::OK);
		let project = project_id(&db, "shop").await;
		let server = local_server_id(&db, team).await;
		Self {
			app,
			state,
			db,
			cookie,
			token: login["token"].as_str().unwrap().to_owned(),
			team,
			project,
			server,
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

	fn base(&self, app: i64) -> String {
		format!("/teams/{}/projects/{}/apps/{app}", self.team, self.project)
	}

	async fn alice(&self) -> UserId {
		UserId(user_id(&self.db, "alice").await)
	}

	async fn save(&self, app: i64, input: &RulesInput) -> Result<(), ServerError> {
		service::save(
			&self.db,
			self.alice().await,
			TeamId(self.team),
			ProjectId(self.project),
			ApplicationId(app),
			input,
		)
		.await
	}

	async fn internal(&self, app: i64, on: bool) {
		service::set_internal(
			&self.db,
			self.alice().await,
			TeamId(self.team),
			ProjectId(self.project),
			ApplicationId(app),
			on,
		)
		.await
		.unwrap();
	}

	async fn add_user(&self, app: i64, name: &str, password: &str) -> Result<(), ServerError> {
		service::set_password(
			&self.db,
			self.alice().await,
			TeamId(self.team),
			ProjectId(self.project),
			ApplicationId(app),
			name,
			password,
		)
		.await
	}

	/// Queues a deploy and runs its job the way the worker does.
	async fn deploy(&self, app: i64) {
		let stored = crate::applications::find_by_id(&self.db, ApplicationId(app))
			.await
			.unwrap()
			.unwrap();
		let queued = crate::deployments::queue(&self.state, &stored, None, None)
			.await
			.unwrap();
		let job = jobs::latest_for(&self.db, JobKind::Deploy, queued.id.0)
			.await
			.unwrap()
			.expect("a deploy job is queued");
		let mut log = self.state.logs.open(job.id).await.unwrap();
		let result =
			crate::deployments::run::run(&self.state, &job, &CancellationToken::new(), &mut log)
				.await;
		log.finish().await;
		result.unwrap_or_else(|e| panic!("the deploy failed: {e:?}"));
	}

	/// The `routing` of the spec the next deploy of the application hands the agent.
	async fn routing_of_deploy(&self, app: i64, agent: &Arc<Mutex<Vec<Value>>>) -> Value {
		self.deploy(app).await;
		agent.lock().unwrap().last().expect("the agent was asked")["routing"].clone()
	}
}

/// A fake agent on the local server's socket: records every deploy spec and answers healthy.
fn fake_agent(state: &AppState) -> Arc<Mutex<Vec<Value>>> {
	let seen = Arc::new(Mutex::new(Vec::new()));
	let recorder = seen.clone();
	let app = Router::new().route(
		"/v1/deploy",
		post(move |body: Bytes| async move {
			recorder
				.lock()
				.unwrap()
				.push(serde_json::from_slice(&body).unwrap());
			let lines = [
				json!({ "type": "phase", "phase": "starting" }),
				json!({ "type": "deployed", "container_id": "c-1", "image": "docker.io/traefik/whoami:v1.10", "commit_sha": null }),
			];
			let body = lines.iter().fold(String::new(), |mut out, e| {
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

fn form(extra: &Value) -> Value {
	let mut base = json!({
		"acc_allow": "", "acc_deny": "", "acc_rate": "", "acc_burst": "", "acc_body": "",
		"acc_headers": true,
	});
	for (k, v) in extra.as_object().unwrap() {
		base[k] = v.clone();
	}
	base
}

// --- deploys -----------------------------------------------------------------------------------

#[sqlx::test]
async fn access_rules_a_new_application_gets_the_security_headers_and_nothing_else(db: PgPool) {
	let s = Setup::new(db).await;
	let id = s.application("web1").await;
	let agent = fake_agent(&s.state);
	let routing = s.routing_of_deploy(id, &agent).await;
	let access = &routing["access"];
	assert_eq!(access["security_headers"], true, "{routing}");
	assert_eq!(access["allow"], json!([]));
	assert_eq!(access["basic_auth"], json!([]));
	assert!(access["rate_average"].is_null());
	assert!(!routing["http_hosts"].as_array().unwrap().is_empty());
}

#[sqlx::test]
async fn access_rules_an_application_from_before_the_feature_deploys_unchanged(db: PgPool) {
	let s = Setup::new(db).await;
	let id = s.application("old1").await;
	// What the migration did for every application that existed: a row, headers off.
	sqlx::query("INSERT INTO access_rules (application_id, security_headers) VALUES ($1, FALSE)")
		.bind(id)
		.execute(&s.db)
		.await
		.unwrap();
	let agent = fake_agent(&s.state);
	let routing = s.routing_of_deploy(id, &agent).await;
	assert!(
		routing.get("access").is_none_or(Value::is_null),
		"{routing}"
	);
}

#[sqlx::test]
async fn access_rules_internal_only_removes_every_router(db: PgPool) {
	let s = Setup::new(db).await;
	let id = s.application("web2").await;
	let agent = fake_agent(&s.state);
	assert!(s.routing_of_deploy(id, &agent).await.is_object());

	s.internal(id, true).await;
	let routing = s.routing_of_deploy(id, &agent).await;
	assert!(
		routing.is_null(),
		"no routing at all, so no router label: {routing}"
	);
	let spec = agent.lock().unwrap().last().cloned().unwrap();
	// It still has its networks: only the public route is gone.
	assert!(
		spec["attach"].as_array().is_some_and(|a| !a.is_empty()),
		"{spec}"
	);

	s.internal(id, false).await;
	assert!(s.routing_of_deploy(id, &agent).await.is_object());
}

#[sqlx::test]
async fn access_rules_the_deploy_hands_the_agent_the_rules_and_the_hashes(db: PgPool) {
	let s = Setup::new(db).await;
	let id = s.application("web3").await;
	s.save(
		id,
		&RulesInput {
			allow: "10.0.0.0/8".to_owned(),
			rate_average: "5".to_owned(),
			rate_burst: "10".to_owned(),
			max_body_mb: "2".to_owned(),
			security_headers: true,
			..RulesInput::default()
		},
	)
	.await
	.unwrap();
	s.add_user(id, "ann", PASSWORD).await.unwrap();
	let agent = fake_agent(&s.state);
	let routing = s.routing_of_deploy(id, &agent).await;
	let access = &routing["access"];
	assert_eq!(access["allow"], json!(["10.0.0.0/8"]));
	assert_eq!(access["rate_average"], 5);
	assert_eq!(access["rate_burst"], 10);
	assert_eq!(access["max_body_bytes"], 2 * 1024 * 1024);
	let entry = access["basic_auth"][0].as_str().unwrap();
	let (user, hash) = entry.split_once(':').unwrap();
	assert_eq!(user, "ann");
	assert!(hash.starts_with("$2y$10$"), "{hash}");
	assert!(bcrypt::verify(PASSWORD, hash).unwrap());
	assert!(!bcrypt::verify("wrong password", hash).unwrap());
	// What the agent turns that into must be what Traefik reads.
	let wire: proto::AccessRules = serde_json::from_value(access.clone()).unwrap();
	let labels = agent::ingress::access_labels("app-1", &wire, true, false).unwrap();
	assert!(
		label(
			&labels,
			"traefik.http.middlewares.app-1-auth.basicauth.users"
		)
		.unwrap()
		.starts_with("ann:$2y$")
	);
}

// --- the password is write-only ----------------------------------------------------------------

#[sqlx::test]
async fn access_rules_basic_auth_hashes_and_passwords_are_never_rendered(db: PgPool) {
	let s = Setup::new(db).await;
	let id = s.application("web4").await;
	let base = s.base(id);
	let (status, _, body) = post_signals(
		&s.app,
		&format!("{base}/access/users"),
		Some(&s.cookie),
		&json!({ "acc_user": "ann", "acc_pass": PASSWORD }),
	)
	.await;
	assert_eq!(status, StatusCode::OK, "{body}");
	assert_contains(&body, "ann");
	let hash: String =
		sqlx::query_scalar("SELECT password_hash FROM access_users WHERE application_id = $1")
			.bind(id)
			.fetch_one(&s.db)
			.await
			.unwrap();
	assert!(hash.starts_with("$2y$"));

	let mut seen = vec![body];
	for page in ["/domains", "/security", "/networks", ""] {
		let (status, _, html) = get_html(&s.app, &format!("{base}{page}"), Some(&s.cookie)).await;
		assert_eq!(status, StatusCode::OK, "{page}");
		seen.push(html);
	}
	let (_, _, networks) = get_html(
		&s.app,
		&format!("/teams/{}/servers/{}/networks", s.team, s.server),
		Some(&s.cookie),
	)
	.await;
	seen.push(networks);
	for body in &seen {
		assert_not_contains(body, &hash);
		assert_not_contains(body, "$2y$");
		assert_not_contains(body, PASSWORD);
	}
	let (_, _, domains) = get_html(&s.app, &format!("{base}/domains"), Some(&s.cookie)).await;
	assert_contains(page_main(&domains), "ann");

	// Nor does the audit log keep them, though it records who added the user.
	let events: Vec<String> = sqlx::query_scalar(
		"SELECT action || ' ' || COALESCE(diff::text, '') FROM audit_events WHERE action LIKE 'app.access%'",
	)
	.fetch_all(&s.db)
	.await
	.unwrap();
	assert!(
		events
			.iter()
			.any(|e| e.contains("app.access_user.set") && e.contains("ann")),
		"{events:?}"
	);
	assert!(
		events
			.iter()
			.all(|e| !e.contains("$2y$") && !e.contains(PASSWORD)),
		"{events:?}"
	);
}

#[sqlx::test]
async fn access_rules_users_are_added_replaced_and_removed(db: PgPool) {
	let s = Setup::new(db).await;
	let id = s.application("web5").await;
	let app = ApplicationId(id);
	let hash_of = || async {
		sqlx::query_scalar::<_, String>(
			"SELECT password_hash FROM access_users WHERE application_id = $1",
		)
		.bind(id)
		.fetch_one(&s.db)
		.await
		.unwrap()
	};
	s.add_user(id, "ann", PASSWORD).await.unwrap();
	let first = hash_of().await;
	// The same name again sets a new password.
	s.add_user(id, "ann", "another-password").await.unwrap();
	assert_eq!(super::usernames(&s.db, app).await.unwrap(), ["ann"]);
	let second = hash_of().await;
	assert_ne!(first, second);
	assert!(bcrypt::verify("another-password", &second).unwrap());

	let long = "x".repeat(73);
	for (name, password) in [
		("", PASSWORD),
		("a b", PASSWORD),
		("a:b", PASSWORD),
		("ann2", "short"),
		("ann2", long.as_str()),
	] {
		let err = s.add_user(id, name, password).await.unwrap_err();
		assert!(
			matches!(err, ServerError::Validation(_)),
			"{name:?}: {err:?}"
		);
	}
	for n in 0..19 {
		s.add_user(id, &format!("user{n}"), PASSWORD).await.unwrap();
	}
	assert!(s.add_user(id, "one-too-many", PASSWORD).await.is_err());

	let (alice, team, project) = (s.alice().await, TeamId(s.team), ProjectId(s.project));
	service::remove_user(&s.db, alice, team, project, app, "ann")
		.await
		.unwrap();
	assert!(
		service::remove_user(&s.db, alice, team, project, app, "ann")
			.await
			.is_err()
	);
	assert_eq!(super::usernames(&s.db, app).await.unwrap().len(), 19);
}

// --- the pages ---------------------------------------------------------------------------------

#[sqlx::test]
async fn access_rules_the_domains_tab_saves_and_shows_the_rules_back(db: PgPool) {
	let s = Setup::new(db).await;
	let id = s.application("web6").await;
	let base = s.base(id);
	let (status, _, html) = get_html(&s.app, &format!("{base}/domains"), Some(&s.cookie)).await;
	assert_eq!(status, StatusCode::OK);
	let main = page_main(&html);
	for needle in [
		"Access rules",
		"Public access",
		"Basic auth",
		"Security headers",
		"Allow only these addresses",
	] {
		assert_contains(main, needle);
	}
	assert_eq!(
		main.matches("<h1").count(),
		1,
		"one h1: the application's name"
	);

	let (status, _, body) = post_signals(
		&s.app,
		&format!("{base}/access"),
		Some(&s.cookie),
		&form(&json!({
			"acc_allow": "10.0.0.7/24\n203.0.113.9", "acc_deny": "198.51.100.0/24",
			"acc_rate": "5", "acc_burst": "10", "acc_body": 3, "acc_headers": false,
		})),
	)
	.await;
	assert_eq!(status, StatusCode::OK, "{body}");
	assert_contains(&body, "Saved. Changes apply on the next deploy.");
	// The form is told the normal form it was stored in.
	assert_contains(&body, "10.0.0.0/24");
	let rules = super::rules(&s.db, ApplicationId(id))
		.await
		.unwrap()
		.unwrap();
	assert_eq!(rules.allow_cidrs, ["10.0.0.0/24", "203.0.113.9/32"]);
	assert_eq!(rules.deny_cidrs, ["198.51.100.0/24"]);
	assert_eq!(
		(rules.rate_average, rules.rate_burst, rules.max_body_mb),
		(Some(5), Some(10), Some(3))
	);
	assert!(!rules.security_headers);

	let (_, _, html) = get_html(&s.app, &format!("{base}/domains"), Some(&s.cookie)).await;
	assert_contains(&html, "10.0.0.0/24");
	assert_contains(&html, "198.51.100.0/24");

	// The change is in the audit log.
	let actions: Vec<String> =
		sqlx::query_scalar("SELECT action FROM audit_events WHERE action = 'app.access'")
			.fetch_all(&s.db)
			.await
			.unwrap();
	assert_eq!(actions.len(), 1);

	// And the Security tab says what is on.
	let (_, _, security) = get_html(&s.app, &format!("{base}/security"), Some(&s.cookie)).await;
	assert_contains(&security, "IP allow and deny lists");
	assert_contains(&security, "Change access on the Domains tab");
}

#[sqlx::test]
async fn access_rules_a_bad_range_is_refused_and_nothing_is_stored(db: PgPool) {
	let s = Setup::new(db).await;
	let id = s.application("web7").await;
	let (status, _, body) = post_signals(
		&s.app,
		&format!("{}/access", s.base(id)),
		Some(&s.cookie),
		&form(&json!({ "acc_allow": "10.0.0.0/8\nnot-an-address" })),
	)
	.await;
	assert_eq!(
		status,
		StatusCode::OK,
		"an action reports its error on the page"
	);
	assert_contains(&body, "not an IP address");
	assert!(
		super::rules(&s.db, ApplicationId(id))
			.await
			.unwrap()
			.is_none()
	);
	let (_, _, body) = post_signals(
		&s.app,
		&format!("{}/access", s.base(id)),
		Some(&s.cookie),
		&form(&json!({ "acc_burst": "5" })),
	)
	.await;
	assert_contains(&body, "needs a rate limit");
}

#[sqlx::test]
async fn access_rules_the_internal_only_switch_is_an_action_with_an_audit_entry(db: PgPool) {
	let s = Setup::new(db).await;
	let id = s.application("web8").await;
	let base = s.base(id);
	let internal = || async {
		super::rules(&s.db, ApplicationId(id))
			.await
			.unwrap()
			.unwrap()
			.internal_only
	};
	let (status, _, body) = post_signals(
		&s.app,
		&format!("{base}/access/internal/on"),
		Some(&s.cookie),
		&json!({}),
	)
	.await;
	assert_eq!(status, StatusCode::OK, "{body}");
	assert_contains(&body, "Internal only");
	assert!(internal().await);
	// Saving the rules does not turn it off again.
	post_signals(
		&s.app,
		&format!("{base}/access"),
		Some(&s.cookie),
		&form(&json!({ "acc_rate": "2" })),
	)
	.await;
	assert!(internal().await);
	let ForDeploy { internal_only, .. } = for_deploy(&s.db, ApplicationId(id)).await.unwrap();
	assert!(internal_only);
	let (_, _, html) = get_html(&s.app, &format!("{base}/security"), Some(&s.cookie)).await;
	assert_contains(&html, "no public route");
	post_signals(
		&s.app,
		&format!("{base}/access/internal/off"),
		Some(&s.cookie),
		&json!({}),
	)
	.await;
	assert!(!internal().await);
	let n: i64 =
		sqlx::query_scalar("SELECT COUNT(*) FROM audit_events WHERE action = 'app.internal_only'")
			.fetch_one(&s.db)
			.await
			.unwrap();
	assert_eq!(n, 2);
}

// --- tenancy and permissions -------------------------------------------------------------------

#[sqlx::test]
async fn access_rules_another_teams_application_is_a_404_everywhere(db: PgPool) {
	let s = Setup::new(db).await;
	let id = s.application("web10").await;
	s.add_user(id, "ann", PASSWORD).await.unwrap();
	let (bob, bob_team) = ui_session(&s.app, "bob").await;
	let bob_id = UserId(user_id(&s.db, "bob").await);

	// Through the service: the application is not there for them.
	let (team, project, app) = (TeamId(bob_team), ProjectId(s.project), ApplicationId(id));
	let get = service::get(&s.db, bob_id, team, project, app).await;
	assert!(
		matches!(get, Err(ServerError::ApplicationNotFound)),
		"{get:?}"
	);
	let save = service::save(&s.db, bob_id, team, project, app, &RulesInput::default()).await;
	assert!(
		matches!(save, Err(ServerError::ApplicationNotFound)),
		"{save:?}"
	);
	assert!(matches!(
		service::set_internal(&s.db, bob_id, team, project, app, true).await,
		Err(ServerError::ApplicationNotFound)
	));
	assert!(matches!(
		service::set_password(&s.db, bob_id, team, project, app, "eve", PASSWORD).await,
		Err(ServerError::ApplicationNotFound)
	));
	assert!(matches!(
		service::remove_user(&s.db, bob_id, team, project, app, "ann").await,
		Err(ServerError::ApplicationNotFound)
	));

	// Through forged requests, with their own session and their own team in the path.
	let base = format!("/teams/{bob_team}/projects/{}/apps/{id}", s.project);
	let (status, _, _) = get_html(&s.app, &format!("{base}/domains"), Some(&bob)).await;
	assert_eq!(status, StatusCode::NOT_FOUND);
	let forged = [
		("access", form(&json!({ "acc_allow": "0.0.0.0/0" }))),
		("access/internal/on", json!({})),
		(
			"access/users",
			json!({ "acc_user": "eve", "acc_pass": PASSWORD }),
		),
		("access/users/ann/delete", json!({})),
	];
	for (path, signals) in &forged {
		let (_, _, body) =
			post_signals(&s.app, &format!("{base}/{path}"), Some(&bob), signals).await;
		assert_contains(&body, "no longer exists");
	}
	// And with alice's team in the path, which they are not a member of.
	let base = s.base(id);
	let (status, _, _) = get_html(&s.app, &format!("{base}/domains"), Some(&bob)).await;
	assert_eq!(status, StatusCode::NOT_FOUND);
	for (path, signals) in &forged {
		// An action answers in the page's own words, not with a status.
		let (_, _, body) =
			post_signals(&s.app, &format!("{base}/{path}"), Some(&bob), signals).await;
		assert_contains(&body, "no longer exists");
	}

	// Nothing of alice's changed.
	assert_eq!(
		super::usernames(&s.db, ApplicationId(id)).await.unwrap(),
		["ann"]
	);
	assert!(
		super::rules(&s.db, ApplicationId(id))
			.await
			.unwrap()
			.is_none()
	);
}

#[sqlx::test]
async fn access_rules_changing_them_needs_update_on_domains_and_a_forged_post_is_refused(
	db: PgPool,
) {
	let s = Setup::new(db).await;
	let id = s.application("web11").await;
	let role = crate::tests::member_role(&s.db, s.team, "alice").await;
	crate::tests::revoke_permission(&s.db, role, "update", "domain").await;
	let base = s.base(id);

	// They can still read the rules, but the form is read-only and has no save bar.
	let (status, _, html) = get_html(&s.app, &format!("{base}/domains"), Some(&s.cookie)).await;
	assert_eq!(status, StatusCode::OK);
	assert_contains(&html, "You can read these rules but not change them.");
	assert_not_contains(&html, "ui-save-bar");
	assert_not_contains(&html, "Make internal only");

	let forged = [
		("access", form(&json!({ "acc_rate": "1" }))),
		("access/internal/on", json!({})),
		(
			"access/users",
			json!({ "acc_user": "eve", "acc_pass": PASSWORD }),
		),
	];
	for (path, signals) in &forged {
		let (_, _, body) =
			post_signals(&s.app, &format!("{base}/{path}"), Some(&s.cookie), signals).await;
		assert_contains(&body.to_lowercase(), "permission");
	}
	assert!(
		super::rules(&s.db, ApplicationId(id))
			.await
			.unwrap()
			.is_none()
	);
	assert!(
		super::usernames(&s.db, ApplicationId(id))
			.await
			.unwrap()
			.is_empty()
	);
}

// --- the warnings and the network policy -------------------------------------------------------

async fn seed_database(s: &Setup, id: i64, name: &str, port: Option<i32>) {
	sqlx::query(
		"INSERT INTO databases (id, project_id, server_id, name, engine, version, credentials, public_port) \
		 VALUES ($1, $2, $3, $4, 'postgres', '17', 'x', $5)",
	)
	.bind(id)
	.bind(s.project)
	.bind(s.server)
	.bind(name)
	.bind(port)
	.execute(&s.db)
	.await
	.unwrap();
}

#[sqlx::test]
async fn access_rules_a_published_port_is_a_warning_with_a_link_to_the_fix(db: PgPool) {
	let s = Setup::new(db).await;
	seed_database(&s, 7001, "orders", Some(25432)).await;
	seed_database(&s, 7002, "private", None).await;

	let warnings = policy::port_warnings(&s.db, TeamId(s.team), ServerId(s.server))
		.await
		.unwrap();
	assert_eq!(warnings.len(), 1);
	assert_eq!(
		(
			warnings[0].name.as_str(),
			warnings[0].port,
			warnings[0].kind
		),
		("orders", 25432, ResourceKind::Database)
	);

	let (_, _, html) = get_html(
		&s.app,
		&format!("/teams/{}/servers/{}/networks", s.team, s.server),
		Some(&s.cookie),
	)
	.await;
	assert_contains(&html, "orders publishes host port 25432.");
	assert_contains(
		&html,
		&format!("/teams/{}/projects/{}/databases/7001", s.team, s.project),
	);
	assert_not_contains(&html, "private publishes");

	// On the database's own Security tab too, and not for the one without a port.
	let db_page = |id: i64| {
		format!(
			"/teams/{}/projects/{}/databases/{id}/security",
			s.team, s.project
		)
	};
	let (_, _, html) = get_html(&s.app, &db_page(7001), Some(&s.cookie)).await;
	assert_contains(&html, "publishes host port 25432");
	let (_, _, html) = get_html(&s.app, &db_page(7002), Some(&s.cookie)).await;
	assert_not_contains(&html, "publishes host port");

	// Another team's server has none of them.
	let (_, bob_team) = ui_session(&s.app, "bob").await;
	let theirs = local_server_id(&s.db, bob_team).await;
	assert!(
		policy::port_warnings(&s.db, TeamId(bob_team), ServerId(theirs))
			.await
			.unwrap()
			.is_empty()
	);
	assert!(
		policy::port_warnings(&s.db, TeamId(bob_team), ServerId(s.server))
			.await
			.unwrap()
			.is_empty()
	);
}

#[sqlx::test]
async fn access_rules_the_policy_follows_network_memberships_and_exposure(db: PgPool) {
	let s = Setup::new(db).await;
	let public = s.application("front").await;
	let hidden = s.application("worker").await;
	let secret = s.application("admin").await;
	s.internal(hidden, true).await;
	s.save(
		secret,
		&RulesInput {
			rate_average: "3".to_owned(),
			security_headers: true,
			..RulesInput::default()
		},
	)
	.await
	.unwrap();
	seed_database(&s, 7003, "orders", Some(25433)).await;
	seed_database(&s, 7004, "cache", None).await;
	let environment: i64 =
		sqlx::query_scalar("SELECT environment_id FROM applications WHERE id = $1")
			.bind(public)
			.fetch_one(&s.db)
			.await
			.unwrap();
	let network =
		networks::ensure_environment(&s.db, EnvironmentId(environment), ServerId(s.server))
			.await
			.unwrap();
	for (kind, id) in [
		(ResourceKind::Application, public),
		(ResourceKind::Application, hidden),
		(ResourceKind::Application, secret),
		(ResourceKind::Database, 7003),
		(ResourceKind::Database, 7004),
	] {
		networks::upsert_member(&s.db, network.id, kind, id, &[])
			.await
			.unwrap();
	}

	let views = networks::service::list(
		&s.state,
		s.alice().await,
		TeamId(s.team),
		ServerId(s.server),
	)
	.await
	.unwrap();
	let built = policy::build(&s.db, TeamId(s.team), ServerId(s.server), views)
		.await
		.unwrap();
	let net = built
		.networks
		.iter()
		.find(|n| n.network.id == network.id)
		.unwrap();
	let exposure = |name: &str| {
		net.members
			.iter()
			.find(|m| m.member.name == name)
			.unwrap()
			.exposure
	};
	assert_eq!(exposure("front"), Exposure::Public { restricted: false });
	assert_eq!(exposure("worker"), Exposure::InternalOnly);
	assert_eq!(exposure("admin"), Exposure::Public { restricted: true });
	assert_eq!(exposure("orders"), Exposure::PublishedPort(25433));
	assert_eq!(exposure("cache"), Exposure::Private);
	assert_eq!(built.warnings.len(), 1);

	let (_, _, html) = get_html(
		&s.app,
		&format!("/teams/{}/servers/{}/networks", s.team, s.server),
		Some(&s.cookie),
	)
	.await;
	let main = page_main(&html);
	for needle in [
		"Who can reach what",
		"These 5 resources reach each other by name.",
		"Internal only",
		"Public, with rules",
		"Port 25433 open",
		"Private",
	] {
		assert_contains(main, needle);
	}
	assert_eq!(main.matches("<h1").count(), 1);
}

// --- live --------------------------------------------------------------------------------------

/// The real agent in this process, a real Podman and Traefik: the rules are written as labels
/// and Traefik enforces them. An allow list without 127.0.0.1 answers 403, basic auth answers
/// 401 without a login and 200 with one, and the rate limit answers 429 after the burst.
///
/// Needs the network (traefik, traefik/whoami) and a `podman.sock`; binds port 20717 and
/// 20718 on the host. It removes the containers it made, `podkit-traefik` and `app-<id>`, so
/// do not run it on a machine whose own podkit uses that Podman.
/// `cargo test -p server --all-features -- --ignored access_rules_live`
#[sqlx::test]
#[ignore = "pulls traefik and traefik/whoami from docker.io with a real podman.sock, binds ports 20717 and 20718"]
async fn access_rules_live_traefik_enforces_the_rules(db: PgPool) {
	use std::time::Duration;

	const PORT: u16 = 20717;
	let mut state = state(db.clone());
	{
		let config = Arc::make_mut(&mut state.config);
		config.ingress_port = PORT;
		config.https_port = PORT + 1;
	}
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
	let listener = tokio::net::UnixListener::bind(&state.config.local_agent_socket).unwrap();
	tokio::spawn(async move {
		axum::serve(listener, agent::routes::router(ctx))
			.await
			.unwrap();
	});

	let s = Setup::with_state(db, state).await;
	// whoami listens on 80.
	let (status, created) = call(
		&s.app,
		"POST",
		&format!(
			"/api/v1/teams/{}/projects/{}/applications",
			s.team, s.project
		),
		Some(&s.token),
		Some(json!({
			"name": "acc-live", "slug": "acc-live", "server_id": s.server, "container_port": 80,
			"image": "docker.io/traefik/whoami",
		})),
	)
	.await;
	assert_eq!(status, StatusCode::OK, "{created}");
	let id = created["id"].as_i64().unwrap();
	let host = created["url"].as_str().unwrap().to_owned();

	let client = reqwest::Client::new();
	let request = |auth: Option<(&'static str, &'static str)>| {
		let mut req = client
			.get(format!("http://127.0.0.1:{PORT}/"))
			.header("Host", host.clone())
			.timeout(Duration::from_secs(3));
		if let Some((user, password)) = auth {
			req = req.basic_auth(user, Some(password));
		}
		req
	};
	let status_of = |auth: Option<(&'static str, &'static str)>| {
		let req = request(auth);
		async move { req.send().await.map(|r| r.status()) }
	};
	let wait_for = |want: StatusCode, auth: Option<(&'static str, &'static str)>| {
		let status_of = &status_of;
		async move {
			let mut last = None;
			for _ in 0..60 {
				last = status_of(auth).await.ok();
				if last == Some(want) {
					return;
				}
				tokio::time::sleep(Duration::from_millis(500)).await;
			}
			panic!("wanted {want}, the last answer was {last:?}");
		}
	};

	let outcome = std::panic::AssertUnwindSafe(async {
		let (user, team, project, application) = (
			s.alice().await,
			TeamId(s.team),
			ProjectId(s.project),
			ApplicationId(id),
		);
		let apply = |allow: &'static str, rate: &'static str, burst: &'static str| {
			let (db, input) = (
				s.db.clone(),
				RulesInput {
					allow: allow.to_owned(),
					rate_average: rate.to_owned(),
					rate_burst: burst.to_owned(),
					security_headers: true,
					..RulesInput::default()
				},
			);
			async move {
				service::save(&db, user, team, project, application, &input)
					.await
					.unwrap();
			}
		};

		// The headers only: it answers, and carries them.
		s.deploy(id).await;
		wait_for(StatusCode::OK, None).await;
		let answer = request(None).send().await.unwrap();
		let header = |name: &str| {
			answer
				.headers()
				.get(name)
				.map(|v| v.to_str().unwrap().to_owned())
		};
		assert_eq!(header("x-frame-options").as_deref(), Some("DENY"));
		assert_eq!(header("x-content-type-options").as_deref(), Some("nosniff"));

		// An allow list that leaves out the caller: 403.
		apply("203.0.113.0/24", "", "").await;
		s.deploy(id).await;
		wait_for(StatusCode::FORBIDDEN, None).await;

		// The caller allowed, with a login: 401 without it, 200 with it, 401 with a wrong one.
		// Rootless Podman forwards the published port, so Traefik may see the caller as the
		// forwarder (a private address) rather than 127.0.0.1: allow every private range.
		apply(
			"127.0.0.0/8 10.0.0.0/8 172.16.0.0/12 192.168.0.0/16 169.254.0.0/16",
			"",
			"",
		)
		.await;
		service::set_password(&s.db, user, team, project, application, "ann", PASSWORD)
			.await
			.unwrap();
		s.deploy(id).await;
		wait_for(StatusCode::UNAUTHORIZED, None).await;
		wait_for(StatusCode::OK, Some(("ann", PASSWORD))).await;
		assert_eq!(
			status_of(Some(("ann", "wrong-password"))).await.unwrap(),
			StatusCode::UNAUTHORIZED
		);

		// One request a second with a burst of two, no login: 429 once the burst is spent.
		service::remove_user(&s.db, user, team, project, application, "ann")
			.await
			.unwrap();
		apply("", "1", "2").await;
		s.deploy(id).await;
		wait_for(StatusCode::OK, None).await;
		let mut statuses = Vec::new();
		for _ in 0..12 {
			statuses.push(status_of(None).await.unwrap());
		}
		assert!(
			statuses.contains(&StatusCode::TOO_MANY_REQUESTS),
			"{statuses:?}"
		);

		// Internal only: no router, so Traefik has nothing to answer with.
		s.internal(id, true).await;
		s.deploy(id).await;
		wait_for(StatusCode::NOT_FOUND, None).await;
	});
	let result = futures_util::FutureExt::catch_unwind(outcome).await;

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
	if let Err(panic) = result {
		std::panic::resume_unwind(panic);
	}
}
