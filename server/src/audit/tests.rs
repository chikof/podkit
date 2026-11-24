//! The audit log: one event per mutation, the right actor, no secrets, tenancy, permission,
//! retention.
//!
//! `every_mutating_endpoint_*` is the table: an ordered script that calls each mutating
//! signed-in user and again as an API token. [`EXEMPT`] and [`NEEDS_OUTSIDE`] list the handlers
//! the script does not call, each with its reason.

#![allow(
	clippy::many_single_char_names,
	clippy::similar_names,
	reason = "a table of calls reads best with short names"
)]

use std::collections::BTreeSet;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use serde_json::{Value, json};
use sqlx::PgPool;
use tower::ServiceExt;

use crate::tests::{app, user_with_team};

/// Written into every secret the script sends. It must never reach the log.
const SECRET: &str = "PKSECRET-do-not-log";

// --- plumbing -------------------------------------------------------------------------

async fn request(
	app: &Router,
	method: &str,
	uri: &str,
	bearer: Option<&str>,
	headers: &[(&str, &str)],
	body: Option<Value>,
) -> (StatusCode, Value) {
	let mut req = Request::builder().method(method).uri(uri);
	if let Some(t) = bearer {
		req = req.header(header::AUTHORIZATION, format!("Bearer {t}"));
	}
	for (k, v) in headers {
		req = req.header(*k, *v);
	}
	let req = match body {
		Some(b) => req
			.header(header::CONTENT_TYPE, "application/json")
			.body(Body::from(b.to_string())),
		None => req.body(Body::empty()),
	}
	.unwrap();
	let resp = app.clone().oneshot(req).await.unwrap();
	let status = resp.status();
	let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
		.await
		.unwrap();
	(
		status,
		serde_json::from_slice(&bytes).unwrap_or(Value::Null),
	)
}

#[derive(Debug, sqlx::FromRow)]
struct Row {
	team_id: i64,
	actor_kind: String,
	actor_user_id: Option<i64>,
	actor_token_id: Option<i64>,
	action: String,
	summary: String,
	ip: Option<String>,
	user_agent: Option<String>,
}

const COLS: &str =
	"team_id, actor_kind, actor_user_id, actor_token_id, action, summary, ip, user_agent";

async fn total(db: &PgPool) -> i64 {
	sqlx::query_scalar("SELECT COUNT(*) FROM audit_events")
		.fetch_one(db)
		.await
		.unwrap()
}

async fn newest(db: &PgPool) -> Row {
	sqlx::query_as(sqlx::AssertSqlSafe(format!(
		"SELECT {COLS} FROM audit_events ORDER BY id DESC LIMIT 1"
	)))
	.fetch_one(db)
	.await
	.unwrap()
}

const IP: &str = "203.0.113.7";
const UA: &str = "audit-test/1.0";

/// Who makes the calls of a script.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
	/// A signed-in session.
	Session,
	/// A `root` API token made by that session.
	Token,
}

struct World {
	app: Router,
	db: PgPool,
	mode: Mode,
	/// The bearer: the session token, or the API token in [`Mode::Token`].
	bearer: String,
	session: String,
	team: i64,
	user: i64,
	token_id: Option<i64>,
	/// The handlers the script has called.
	called: BTreeSet<&'static str>,
}

impl World {
	async fn new(db: PgPool, mode: Mode) -> Self {
		let app = app(db.clone()).await;
		let (session, team) = user_with_team(&app, "alice").await;
		let user = crate::tests::user_id(&db, "alice").await;
		let mut w = Self {
			app,
			db,
			mode,
			bearer: session.clone(),
			session,
			team,
			user,
			token_id: None,
			called: BTreeSet::new(),
		};
		if mode == Mode::Token {
			let (s, made) = request(
				&w.app,
				"POST",
				&format!("/api/v1/teams/{team}/api-tokens"),
				Some(&w.session),
				&[],
				Some(json!({ "name": "ci", "scopes": ["root"] })),
			)
			.await;
			assert!(s.is_success(), "make the token: {s} {made}");
			w.bearer = made["secret"].as_str().unwrap().to_owned();
			w.token_id = Some(made["id"].as_i64().unwrap());
		}
		w
	}

	/// Calls one mutating endpoint and checks that it wrote exactly one event, by the right
	/// actor, and returns the response.
	async fn step(
		&mut self,
		handler: &'static str,
		method: &str,
		path: &str,
		body: Option<Value>,
		action: &str,
	) -> Value {
		self.step_in(handler, method, path, body, action, Some(self.team))
			.await
	}

	/// [`step`](Self::step) for an event that belongs to another team, or to any (`None`).
	async fn step_in(
		&mut self,
		handler: &'static str,
		method: &str,
		path: &str,
		body: Option<Value>,
		action: &str,
		team: Option<i64>,
	) -> Value {
		let before = total(&self.db).await;
		let (s, out) = request(
			&self.app,
			method,
			&format!("/api/v1{path}"),
			Some(&self.bearer),
			&[("x-forwarded-for", IP), ("user-agent", UA)],
			body,
		)
		.await;
		assert!(s.is_success(), "{handler}: {method} {path} gave {s}: {out}");
		self.called.insert(handler);
		let made = total(&self.db).await - before;
		assert_eq!(made, 1, "{handler}: one event, not {made}");
		let e = newest(&self.db).await;
		assert_eq!(e.action, action, "{handler}: action");
		if let Some(team) = team {
			assert_eq!(e.team_id, team, "{handler}: team");
		}
		assert_eq!(e.actor_user_id, Some(self.user), "{handler}: the user");
		match self.mode {
			Mode::Session => {
				assert_eq!(e.actor_kind, "user", "{handler}: actor kind");
				assert_eq!(e.actor_token_id, None, "{handler}: no token");
			}
			Mode::Token => {
				assert_eq!(e.actor_kind, "token", "{handler}: actor kind");
				assert_eq!(e.actor_token_id, self.token_id, "{handler}: the token");
			}
		}
		assert_eq!(e.ip.as_deref(), Some(IP), "{handler}: ip");
		assert_eq!(e.user_agent.as_deref(), Some(UA), "{handler}: user agent");
		assert!(!e.summary.is_empty(), "{handler}: summary");
		out
	}

	/// A call made to set the scene, not part of the table.
	async fn scene(&self, method: &str, path: &str, body: Option<Value>) -> Value {
		let (s, out) = request(
			&self.app,
			method,
			&format!("/api/v1{path}"),
			Some(&self.session),
			&[],
			body,
		)
		.await;
		assert!(s.is_success(), "scene {method} {path}: {s} {out}");
		out
	}

	async fn sql(&self, sql: &'static str, args: &[i64]) {
		let mut q = sqlx::query(sql);
		for a in args {
			q = q.bind(*a);
		}
		q.execute(&self.db).await.unwrap();
	}

	async fn one(&self, sql: &'static str, args: &[i64]) -> i64 {
		let mut q = sqlx::query_scalar(sql);
		for a in args {
			q = q.bind(*a);
		}
		q.fetch_one(&self.db).await.unwrap()
	}
}

fn id(v: &Value) -> i64 {
	v["id"].as_i64().unwrap_or_else(|| panic!("no id in {v}"))
}

// --- endpoints the script does not call ------------------------------------------------

/// Mutating handlers that change nothing podkit stores, or belong to no team.
const EXEMPT: &[(&str, &str)] = &[
	(
		"auth/routes::register",
		"makes an account, which belongs to no team",
	),
	(
		"auth/routes::login",
		"opens a session, which belongs to no team",
	),
	(
		"auth/routes::logout",
		"ends a session, which belongs to no team",
	),
	(
		"avatars/routes::put_me",
		"the caller's own picture; it belongs to no team",
	),
	(
		"avatars/routes::delete_me",
		"the caller's own picture; it belongs to no team",
	),
	("iac/routes::plan", "a plan compares and changes nothing"),
	(
		"iac/routes::import_plan",
		"a plan compares and changes nothing",
	),
	(
		"registries/routes::test_new",
		"tries a login without storing anything",
	),
	(
		"backups/targets::test",
		"probes a bucket without storing anything",
	),
	(
		"git_providers/routes::manifest",
		"builds a form for GitHub; the app is stored (and logged) when GitHub calls back",
	),
	(
		"git_providers/hooks::github",
		"signed by GitHub, not by a person; the deploy it starts is logged as `system`, like the webhook step",
	),
];

/// Mutating handlers that do change something but need what a test server cannot give: an
/// agent, Hugging Face, an SSH host, a git provider. Their service functions record the event;
/// this list keeps the reason visible.
const NEEDS_OUTSIDE: &[(&str, &str)] = &[
	(
		"applications/routes::stop",
		"needs a running container on an agent",
	),
	("volumes/routes::remove", "removes the volume on the agent"),
	(
		"model_files/routes::create",
		"checks the size against Hugging Face and the disk through the agent",
	),
	(
		"model_files/routes::retry",
		"checks the size against Hugging Face and the disk through the agent",
	),
	(
		"model_files/routes::remove",
		"removes the volume on the agent",
	),
	(
		"networks/routes::create",
		"makes the network through the agent",
	),
	("networks/routes::import", "asks the agent for the network"),
	(
		"networks/routes::migrate_server",
		"moves containers through the agent",
	),
	("registries/routes::test", "logs in from the agent"),
	(
		"ssh_keys/routes::rotate",
		"logs in to the servers over SSH; see `rotating_a_key_is_one_event`",
	),
	(
		"git_providers/routes::create",
		"tries the token against the provider first",
	),
	(
		"git_providers/routes::test",
		"asks the provider who the token is",
	),
	(
		"scanning/routes::scan_again",
		"needs a finished scan and Grype on an agent",
	),
];

// --- the script -----------------------------------------------------------------------------

#[allow(clippy::too_many_lines, reason = "one table, read top to bottom")]
async fn script(w: &mut World) {
	let t = w.team;

	// A second person to add to the team.
	let (s, _) = request(
		&w.app,
		"POST",
		"/api/v1/auth/register",
		None,
		&[],
		Some(
			json!({ "name": "bob", "email": "bob@example.com", "password": "correct horse battery" }),
		),
	)
	.await;
	assert!(s.is_success());
	let bob = crate::tests::user_id(&w.db, "bob").await;

	// teams, members
	w.step_in(
		"teams/routes::create",
		"POST",
		"/teams",
		Some(json!({ "name": "Second", "slug": "second" })),
		"team.create",
		// The event belongs to the team that was made.
		None,
	)
	.await;
	w.step(
		"members/routes::add",
		"POST",
		&format!("/teams/{t}/members"),
		Some(json!({ "email": "bob@example.com", "role_id": 2 })),
		"member.add",
	)
	.await;
	w.step(
		"members/routes::update_role",
		"PATCH",
		&format!("/teams/{t}/members/{bob}"),
		Some(json!({ "role_id": 1 })),
		"member.update",
	)
	.await;

	// projects and environments
	let p = id(&w
		.step(
			"projects/routes::create",
			"POST",
			&format!("/teams/{t}/projects"),
			Some(json!({ "name": "Web", "slug": "web" })),
			"project.create",
		)
		.await);
	let envs = w
		.scene(
			"GET",
			&format!("/teams/{t}/projects/{p}/environments"),
			None,
		)
		.await;
	let prod = id(&envs[0]);
	let staging = id(&w
		.step(
			"environments/routes::create",
			"POST",
			&format!("/teams/{t}/projects/{p}/environments"),
			Some(json!({ "name": "Staging", "slug": "staging" })),
			"environment.create",
		)
		.await);
	w.step(
		"environments/routes::update",
		"PATCH",
		&format!("/teams/{t}/projects/{p}/environments/{staging}"),
		Some(json!({ "name": "Stage" })),
		"environment.update",
	)
	.await;
	w.step(
		"environments/routes::reorder",
		"PUT",
		&format!("/teams/{t}/projects/{p}/environments/order"),
		Some(json!({ "ids": [staging, prod] })),
		"environment.reorder",
	)
	.await;
	w.step(
		"environments/routes::remove",
		"DELETE",
		&format!("/teams/{t}/projects/{p}/environments/{staging}"),
		None,
		"environment.delete",
	)
	.await;

	// ssh keys, registries, servers
	let key = id(&w
		.step(
			"ssh_keys/routes::create",
			"POST",
			&format!("/teams/{t}/ssh-keys"),
			Some(json!({ "name": "deploy" })),
			"ssh_key.create",
		)
		.await);
	w.step(
		"ssh_keys/routes::remove",
		"DELETE",
		&format!("/teams/{t}/ssh-keys/{key}"),
		None,
		"ssh_key.delete",
	)
	.await;
	let reg = id(&w
		.step(
			"registries/routes::create",
			"POST",
			&format!("/teams/{t}/registries"),
			Some(json!({
				"name": "hub", "url": "https://registry.example.com",
				"username": "alice", "password": format!("{SECRET}-registry"),
			})),
			"registry.create",
		)
		.await);
	w.step(
		"registries/routes::update",
		"PATCH",
		&format!("/teams/{t}/registries/{reg}"),
		Some(
			json!({ "name": "hub2", "username": "alice", "password": format!("{SECRET}-registry2") }),
		),
		"registry.update",
	)
	.await;
	w.step(
		"registries/routes::remove",
		"DELETE",
		&format!("/teams/{t}/registries/{reg}"),
		None,
		"registry.delete",
	)
	.await;
	let remote = id(&w
		.step(
			"servers/routes::create",
			"POST",
			&format!("/teams/{t}/servers"),
			Some(json!({
				"name": "edge", "hostname": "edge.example.com", "ssh_user": "root",
				"generate_key": true,
			})),
			"server.create",
		)
		.await);
	w.step(
		"servers/routes::bootstrap",
		"POST",
		&format!("/teams/{t}/servers/{remote}/bootstrap"),
		None,
		"server.bootstrap",
	)
	.await;
	w.step(
		"scanning/routes::put_settings",
		"PUT",
		&format!("/teams/{t}/security/servers/{remote}"),
		Some(json!({ "enabled": true, "cpus": 1.0, "memory_mb": 1024 })),
		"scan.settings",
	)
	.await;
	w.step(
		"servers/routes::remove",
		"DELETE",
		&format!("/teams/{t}/servers/{remote}"),
		None,
		"server.delete",
	)
	.await;
	let local = w
		.one(
			"SELECT id FROM servers WHERE team_id = $1 AND is_local",
			&[t],
		)
		.await;

	// an application and what hangs off it
	let a = w
		.step(
			"applications/routes::create",
			"POST",
			&format!("/teams/{t}/projects/{p}/applications"),
			Some(json!({
				"name": "Whoami", "slug": "whoami", "server_id": local,
				"image": "traefik/whoami:v1", "container_port": 80,
			})),
			"app.create",
		)
		.await;
	let app_id = id(&a);
	let webhook_secret = a["webhook_secret"].as_str().unwrap().to_owned();
	let base = format!("/teams/{t}/projects/{p}/applications/{app_id}");
	w.step(
		"applications/routes::update",
		"PATCH",
		&base,
		Some(json!({
			"name": "Whoami 2", "slug": "whoami", "server_id": local,
			"image": "traefik/whoami:v1.1", "container_port": 80,
		})),
		"app.update",
	)
	.await;
	w.step(
		"env_vars/routes::upsert",
		"POST",
		&format!("{base}/env-vars"),
		Some(json!({ "key": "DATABASE_URL", "value": format!("{SECRET}-env") })),
		"env_var.set",
	)
	.await;
	w.step(
		"env_vars/routes::bulk",
		"POST",
		&format!("{base}/env-vars/bulk"),
		Some(json!({ "text": format!("BULK_KEY={SECRET}-bulk\n") })),
		"env_var.bulk_set",
	)
	.await;
	let vars = w.scene("GET", &format!("{base}/env-vars"), None).await;
	w.step(
		"env_vars/routes::remove",
		"DELETE",
		&format!("{base}/env-vars/{}", id(&vars[0])),
		None,
		"env_var.delete",
	)
	.await;
	let dom = id(&w
		.step(
			"domains/routes::create",
			"POST",
			&format!("{base}/domains"),
			Some(json!({ "hostname": "whoami.example.com" })),
			"domain.create",
		)
		.await);
	w.step(
		"domains/routes::remove",
		"DELETE",
		&format!("{base}/domains/{dom}"),
		None,
		"domain.delete",
	)
	.await;
	w.step(
		"volumes/routes::create",
		"POST",
		&format!("{base}/volumes"),
		Some(json!({ "mount_path": "/data" })),
		"volume.create",
	)
	.await;
	let dep = id(&w
		.step(
			"deployments/routes::create",
			"POST",
			&format!("{base}/deployments"),
			None,
			"app.deploy",
		)
		.await);
	w.step(
		"deployments/routes::cancel",
		"POST",
		&format!("{base}/deployments/{dep}/cancel"),
		None,
		"deployment.cancel",
	)
	.await;
	// A build that left an image behind, so there is something to roll back to and restart.
	w.sql(
		"UPDATE deployments SET image_tag = 'whoami:1', status = 'stopped' WHERE id = $1",
		&[dep],
	)
	.await;
	w.step(
		"deployments/routes::rollback",
		"POST",
		&format!("{base}/deployments/{dep}/rollback"),
		None,
		"app.rollback",
	)
	.await;
	w.step(
		"applications/routes::restart",
		"POST",
		&format!("{base}/restart"),
		None,
		"app.restart",
	)
	.await;
	// The same image, promoted to the copy of the application in another environment.
	let qa = id(&w
		.scene(
			"POST",
			&format!("/teams/{t}/projects/{p}/environments"),
			Some(json!({ "name": "QA", "slug": "qa" })),
		)
		.await);
	w.scene(
		"POST",
		&format!("/teams/{t}/projects/{p}/applications"),
		Some(json!({
			"name": "Whoami", "slug": "whoami-qa", "server_id": local,
			"image": "traefik/whoami:v1", "container_port": 80, "environment_id": qa,
		})),
	)
	.await;
	w.sql(
		"UPDATE deployments SET status = 'running' WHERE id = $1",
		&[dep],
	)
	.await;
	w.step(
		"applications/routes::promote",
		"POST",
		&format!("{base}/promote"),
		Some(json!({ "environment_id": qa })),
		"app.promote",
	)
	.await;
	// The push webhook has no session: podkit itself is the actor.
	let before = total(&w.db).await;
	let (s, out) = request(
		&w.app,
		"POST",
		&format!("/api/v1/webhooks/deploy/{app_id}"),
		None,
		&[("x-webhook-secret", &webhook_secret)],
		Some(json!({})),
	)
	.await;
	assert!(s.is_success(), "webhook: {s} {out}");
	assert_eq!(total(&w.db).await - before, 1, "the webhook is one event");
	let e = newest(&w.db).await;
	assert_eq!(
		(e.action.as_str(), e.actor_kind.as_str()),
		("app.deploy", "system")
	);
	assert_eq!((e.actor_user_id, e.actor_token_id), (None, None));
	w.called.insert("webhooks/routes::deploy");

	// tokens
	let tok = id(&w
		.step(
			"auth/tokens/routes::create",
			"POST",
			&format!("/teams/{t}/api-tokens"),
			Some(json!({ "name": "other", "scopes": ["read"] })),
			"api_token.create",
		)
		.await);
	w.step(
		"auth/tokens/routes::revoke",
		"DELETE",
		&format!("/teams/{t}/api-tokens/{tok}"),
		None,
		"api_token.revoke",
	)
	.await;

	w.step(
		"applications/routes::remove",
		"DELETE",
		&base,
		None,
		"app.delete",
	)
	.await;
	w.step(
		"projects/routes::remove",
		"DELETE",
		&format!("/teams/{t}/projects/{p}"),
		None,
		"project.delete",
	)
	.await;
}

#[sqlx::test]
async fn every_mutating_endpoint_writes_one_event_as_a_user(db: PgPool) {
	let mut w = World::new(db, Mode::Session).await;
	script(&mut w).await;
}

#[sqlx::test]
async fn every_mutating_endpoint_writes_one_event_as_a_token(db: PgPool) {
	let mut w = World::new(db, Mode::Token).await;
	script(&mut w).await;
}

// --- secrets, tenancy, permission ----------------------------------------------------------

#[sqlx::test]
async fn secrets_never_reach_the_log(db: PgPool) {
	let mut w = World::new(db, Mode::Session).await;
	script(&mut w).await;
	let leaked: i64 = sqlx::query_scalar(
		"SELECT COUNT(*) FROM audit_events WHERE diff::text LIKE '%PKSECRET%' \
		 OR summary LIKE '%PKSECRET%'",
	)
	.fetch_one(&w.db)
	.await
	.unwrap();
	assert_eq!(leaked, 0, "a secret value reached the log");
	// And the API does not hand one out either.
	let (s, body) = request(
		&w.app,
		"GET",
		&format!("/api/v1/teams/{}/audit?limit=1000", w.team),
		Some(&w.session),
		&[],
		None,
	)
	.await;
	assert!(s.is_success(), "{s} {body}");
	assert!(!body.to_string().contains("PKSECRET"), "{body}");
	// The env var event says the value changed, and shows •••.
	let masked: i64 = sqlx::query_scalar(
		"SELECT COUNT(*) FROM audit_events WHERE action = 'env_var.set' AND diff::text LIKE '%•••%'",
	)
	.fetch_one(&w.db)
	.await
	.unwrap();
	assert_eq!(masked, 1);
}

#[sqlx::test]
async fn another_team_gets_404_and_a_plain_member_gets_403(db: PgPool) {
	let w = World::new(db, Mode::Session).await;
	let t = w.team;
	let (carol, _) = user_with_team(&w.app, "carol").await;
	for path in [
		format!("/api/v1/teams/{t}/audit"),
		format!("/api/v1/teams/{t}/audit/export"),
	] {
		let (s, _) = request(&w.app, "GET", &path, Some(&carol), &[], None).await;
		assert_eq!(s, StatusCode::NOT_FOUND, "{path}");
	}
	// A member without `read audit`.
	let (bob, _) = user_with_team(&w.app, "bob").await;
	w.scene(
		"POST",
		&format!("/teams/{t}/members"),
		Some(json!({ "email": "bob@example.com", "role_id": 2 })),
	)
	.await;
	let (s, _) = request(
		&w.app,
		"GET",
		&format!("/api/v1/teams/{t}/audit"),
		Some(&bob),
		&[],
		None,
	)
	.await;
	assert_eq!(s, StatusCode::FORBIDDEN);
	let (s, _) = request(
		&w.app,
		"GET",
		&format!("/api/v1/teams/{t}/audit"),
		Some(&w.session),
		&[],
		None,
	)
	.await;
	assert!(s.is_success());
}

#[sqlx::test]
async fn a_token_needs_the_read_scope_and_the_log_is_append_only(db: PgPool) {
	let w = World::new(db, Mode::Session).await;
	let t = w.team;
	w.scene(
		"POST",
		&format!("/teams/{t}/projects"),
		Some(json!({ "name": "Web", "slug": "web" })),
	)
	.await;
	let made = w
		.scene(
			"POST",
			&format!("/teams/{t}/api-tokens"),
			Some(json!({ "name": "r", "scopes": ["read"] })),
		)
		.await;
	let secret = made["secret"].as_str().unwrap();
	let (s, body) = request(
		&w.app,
		"GET",
		&format!("/api/v1/teams/{t}/audit"),
		Some(secret),
		&[],
		None,
	)
	.await;
	assert!(s.is_success(), "{s} {body}");
	assert!(body.to_string().contains("project.create"));
	let changed = sqlx::query("UPDATE audit_events SET summary = 'x'")
		.execute(&w.db)
		.await;
	assert!(changed.is_err(), "an event can be edited");
}

#[test]
fn every_skipped_endpoint_says_why() {
	for (handler, why) in EXEMPT.iter().chain(NEEDS_OUTSIDE) {
		assert!(handler.contains("::") && why.len() > 10, "{handler}");
	}
}

// --- END OF TESTS (new ones go above this line)

/// SEC-06 end to end: the address in the audit log is the socket peer's, whatever header a
/// LAN client sends.
#[sqlx::test]
async fn sec_cleanup_the_audit_address_is_the_socket_peer_not_a_forwarded_header(db: PgPool) {
	use axum::extract::ConnectInfo;

	let app = app(db.clone()).await;
	let (token, team) = user_with_team(&app, "alice").await;
	let from = |peer: &str, name: &str| {
		let mut req = Request::builder()
			.method("POST")
			.uri(format!("/api/v1/teams/{team}/projects"))
			.header(header::AUTHORIZATION, format!("Bearer {token}"))
			.header(header::CONTENT_TYPE, "application/json")
			.header("x-forwarded-for", "203.0.113.9")
			.body(Body::from(
				json!({ "name": name, "slug": name }).to_string(),
			))
			.unwrap();
		req.extensions_mut().insert(ConnectInfo(
			format!("{peer}:51000")
				.parse::<std::net::SocketAddr>()
				.unwrap(),
		));
		req
	};
	for (peer, name) in [
		("192.168.1.50", "lan"),
		("127.0.0.1", "local"),
		("198.51.100.4", "wan"),
	] {
		let resp = app.clone().oneshot(from(peer, name)).await.unwrap();
		assert!(resp.status().is_success(), "{peer}: {}", resp.status());
		assert_eq!(
			newest(&db).await.ip.as_deref(),
			Some(peer),
			"{peer} wrote a forwarded address into the log"
		);
	}
}
