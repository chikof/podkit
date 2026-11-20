//! API tests of infrastructure as code: export, plan, apply and the team-level import.

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use serde_json::{Value, json};
use tower::ServiceExt;

use crate::tests::{
	PgPool, app, call, count, db_test, member_role, revoke_permission, user_with_team,
};

/// What the tests send for a request body that is not JSON.
async fn text(
	app: &Router,
	method: &str,
	uri: &str,
	token: &str,
	body: &str,
) -> (StatusCode, String) {
	let req = Request::builder()
		.method(method)
		.uri(uri)
		.header(header::AUTHORIZATION, format!("Bearer {token}"))
		.header(header::CONTENT_TYPE, "application/toml")
		.body(Body::from(body.to_owned()))
		.unwrap();
	let resp = app.clone().oneshot(req).await.unwrap();
	let status = resp.status();
	let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
		.await
		.unwrap();
	(status, String::from_utf8_lossy(&bytes).into_owned())
}

fn json_of(text: &str) -> Value {
	serde_json::from_str(text).unwrap_or(Value::Null)
}

struct World {
	app: Router,
	db: PgPool,
	token: String,
	team: i64,
	project: i64,
	server: String,
}

impl World {
	async fn new(db: PgPool, name: &str) -> Self {
		let app = app(db.clone()).await;
		let (token, team) = user_with_team(&app, name).await;
		let (s, project) = call(
			&app,
			"POST",
			&format!("/api/v1/teams/{team}/projects"),
			Some(&token),
			Some(json!({ "name": "Shop", "slug": "shop" })),
		)
		.await;
		assert!(s.is_success(), "{s} {project}");
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
			db,
			token,
			team,
			project: project["id"].as_i64().unwrap(),
			server: servers[0]["name"].as_str().unwrap().to_owned(),
		}
	}

	fn url(&self, rest: &str) -> String {
		format!(
			"/api/v1/teams/{}/projects/{}/{rest}",
			self.team, self.project
		)
	}

	async fn export(&self) -> String {
		let (s, body) = text(&self.app, "GET", &self.url("export"), &self.token, "").await;
		assert_eq!(s, StatusCode::OK, "{body}");
		body
	}

	async fn send(
		&self,
		rest: &str,
		file: &str,
		secrets: &Value,
		flags: Value,
	) -> (StatusCode, Value) {
		let mut body = json!({ "file": file, "secrets": secrets });
		for (k, v) in flags.as_object().unwrap() {
			body[k] = v.clone();
		}
		call(
			&self.app,
			"POST",
			&self.url(rest),
			Some(&self.token),
			Some(body),
		)
		.await
	}

	async fn plan(&self, file: &str, secrets: &Value, prune: bool) -> Value {
		let (s, body) = self
			.send("plan", file, secrets, json!({ "prune": prune }))
			.await;
		assert_eq!(s, StatusCode::OK, "{body}");
		body
	}

	async fn apply(&self, file: &str, secrets: &Value, prune: bool) -> (StatusCode, Value) {
		self.send("apply", file, secrets, json!({ "prune": prune }))
			.await
	}
}

fn secrets() -> Value {
	json!({
		"production/web/DATABASE_URL": "postgres://app:hunter2-very-secret@db/app",
		"production/web/SESSION_KEY": "session-key-0xCAFE",
		"production/queue/BROKER_PASSWORD": "broker-pass-0xBEEF",
	})
}

/// A project of every kind this build can create without a live server.
fn file(server: &str) -> String {
	format!(
		r#"version = 1

[projects.shop]
name = "Shop"

[projects.shop.environments.production]
name = "production"

[projects.shop.environments.production.apps.web]
server = "{server}"
slug = "web"
repo = "https://github.com/example/web.git"
ref = "main"
port = 8080
memory_mb = 512
domains = ["shop.example.com", "www.example.com"]
volumes = ["/data"]

[projects.shop.environments.production.apps.web.env]
DATABASE_URL = "${{secret:production/web/DATABASE_URL}}"
SESSION_KEY = "${{secret:production/web/SESSION_KEY}}"

[projects.shop.environments.production.apps.cache]
server = "{server}"
slug = "cache"
image = "redis:7"
port = 6379

[projects.shop.environments.production.databases.main]
server = "{server}"
engine = "postgres"
memory_mb = 1024

[projects.shop.environments.production.stacks.queue]
server = "{server}"
compose = """
services:
  broker:
    image: rabbitmq:3
    environment:
      RABBITMQ_DEFAULT_PASS: ${{BROKER_PASSWORD}}
"""

[projects.shop.environments.production.stacks.queue.env]
BROKER_PASSWORD = "${{secret:production/queue/BROKER_PASSWORD}}"

[projects.shop.environments.staging]
name = "Staging"
"#
	)
}

fn count_of(plan: &Value, action: &str) -> usize {
	plan["changes"]
		.as_array()
		.unwrap()
		.iter()
		.filter(|c| c["action"] == action)
		.count()
}

#[db_test]
async fn iac_apply_makes_everything_in_a_file_and_the_export_says_the_same(db: PgPool) {
	let w = World::new(db, "alice").await;
	let text = file(&w.server);
	let plan = w.plan(&text, &secrets(), false).await;
	assert!(plan["issues"].as_array().unwrap().is_empty(), "{plan}");
	// The production environment is there already; staging, two applications, a database and
	// a stack are new.
	assert_eq!(count_of(&plan, "create"), 5, "{plan}");

	let (s, applied) = w.apply(&text, &secrets(), false).await;
	assert_eq!(s, StatusCode::OK, "{applied}");
	assert!(applied["report"]["failed"].is_null(), "{applied}");
	assert_eq!(count(&w.db, "applications").await, 2);
	assert_eq!(count(&w.db, "databases").await, 1);
	assert_eq!(count(&w.db, "stacks").await, 1);
	assert_eq!(count(&w.db, "env_vars").await, 2);
	assert_eq!(count(&w.db, "custom_domains").await, 2);
	assert_eq!(count(&w.db, "volumes").await, 1);

	let exported = w.export().await;
	assert!(exported.contains("[projects.shop.environments.production.apps.web]"));
	assert!(exported.contains("shop.example.com"));
	assert!(exported.contains("${secret:production/web/DATABASE_URL}"));
	assert!(exported.contains("name = \"Staging\""));
	assert_eq!(
		exported,
		w.export().await,
		"exporting twice gives the same bytes"
	);
}

#[db_test]
async fn iac_the_export_never_holds_a_secret(db: PgPool) {
	let w = World::new(db, "alice").await;
	let (s, applied) = w.apply(&file(&w.server), &secrets(), false).await;
	assert_eq!(s, StatusCode::OK, "{applied}");
	let exported = w.export().await;
	for value in secrets().as_object().unwrap().values() {
		let value = value.as_str().unwrap();
		assert!(
			!exported.contains(value),
			"the export holds {value}:\n{exported}"
		);
	}
	assert!(!exported.contains("hunter2"));
	// Nor does a plan or an apply answer with one.
	let plan = w
		.plan(
			&file(&w.server).replace("hunter2", "hunter3"),
			&json!({ "production/web/DATABASE_URL": "postgres://new-secret-value", "production/web/SESSION_KEY": "k", "production/queue/BROKER_PASSWORD": "b" }),
			false,
		)
		.await
		.to_string();
	assert!(!plan.contains("new-secret-value"), "{plan}");
	assert!(
		plan.contains("(secret)"),
		"a changed secret is named, not shown: {plan}"
	);
}

#[db_test]
async fn iac_a_file_exported_and_planned_again_plans_nothing(db: PgPool) {
	let w = World::new(db, "alice").await;
	let (s, applied) = w.apply(&file(&w.server), &secrets(), false).await;
	assert_eq!(s, StatusCode::OK, "{applied}");
	let exported = w.export().await;
	// With no secrets at all: a placeholder stands for what is stored.
	let (s, body) = text(&w.app, "POST", &w.url("plan"), &w.token, &exported).await;
	assert_eq!(s, StatusCode::OK, "{body}");
	let plan = json_of(&body);
	assert_eq!(plan["changes"].as_array().unwrap().len(), 0, "{plan}");
	assert!(plan["issues"].as_array().unwrap().is_empty(), "{plan}");
	// And with them.
	let plan = w.plan(&exported, &secrets(), false).await;
	assert_eq!(plan["changes"].as_array().unwrap().len(), 0, "{plan}");
}

#[db_test]
async fn iac_apply_twice_is_a_no_op(db: PgPool) {
	let w = World::new(db, "alice").await;
	let text = file(&w.server);
	let (s, first) = w.apply(&text, &secrets(), false).await;
	assert_eq!(s, StatusCode::OK, "{first}");
	assert!(!first["report"]["applied"].as_array().unwrap().is_empty());
	let rows = (
		count(&w.db, "applications").await,
		count(&w.db, "env_vars").await,
		count(&w.db, "custom_domains").await,
	);
	let (s, second) = w.apply(&text, &secrets(), false).await;
	assert_eq!(s, StatusCode::OK, "{second}");
	assert!(
		second["report"]["applied"].as_array().unwrap().is_empty(),
		"{second}"
	);
	assert_eq!(
		rows,
		(
			count(&w.db, "applications").await,
			count(&w.db, "env_vars").await,
			count(&w.db, "custom_domains").await,
		)
	);
}

#[db_test]
async fn iac_a_plan_names_update_and_delete_and_changes_nothing(db: PgPool) {
	let w = World::new(db, "alice").await;
	let text = file(&w.server);
	let (s, _) = w.apply(&text, &secrets(), false).await;
	assert_eq!(s, StatusCode::OK);

	let edited = text
		.replace("port = 8080", "port = 9090")
		.replace("domains = [\"shop.example.com\", \"www.example.com\"]", "domains = [\"shop.example.com\"]")
		.replace(
			"[projects.shop.environments.production.apps.cache]\nserver = \"{server}\"\nslug = \"cache\"\nimage = \"redis:7\"\nport = 6379\n"
				.replace("{server}", &w.server)
				.as_str(),
			"",
		);
	let before = (
		count(&w.db, "applications").await,
		count(&w.db, "custom_domains").await,
	);

	let off = w.plan(&edited, &secrets(), false).await;
	let changes = off["changes"].as_array().unwrap();
	assert_eq!(changes.len(), 1, "{off}");
	assert_eq!(changes[0]["action"], "update");
	assert_eq!(changes[0]["name"], "web");
	assert_eq!(changes[0]["will_redeploy"], true);
	let fields: Vec<_> = changes[0]["fields"]
		.as_array()
		.unwrap()
		.iter()
		.map(|f| f["field"].as_str().unwrap())
		.collect();
	assert_eq!(fields, ["port"], "{off}");
	// Pruning is off: what the file leaves out is listed and left alone.
	let extra: Vec<_> = off["extra"]
		.as_array()
		.unwrap()
		.iter()
		.map(|e| (e["name"].as_str().unwrap(), e["fields"].clone()))
		.collect();
	assert!(
		extra.iter().any(|(n, f)| *n == "cache" && f == &json!([])),
		"{off}"
	);
	assert!(
		extra
			.iter()
			.any(|(n, f)| *n == "web" && f == &json!(["domain.www.example.com"])),
		"{off}"
	);

	let on = w.plan(&edited, &secrets(), true).await;
	assert_eq!(count_of(&on, "delete"), 1, "{on}");
	let web = on["changes"]
		.as_array()
		.unwrap()
		.iter()
		.find(|c| c["name"] == "web")
		.unwrap();
	assert!(
		web["fields"]
			.as_array()
			.unwrap()
			.iter()
			.any(|f| f["field"] == "domain.www.example.com" && f["to"].is_null()),
		"{on}"
	);
	assert_eq!(
		before,
		(
			count(&w.db, "applications").await,
			count(&w.db, "custom_domains").await
		),
		"planning changed nothing"
	);
}

#[db_test]
async fn iac_apply_without_prune_never_deletes_and_with_it_does(db: PgPool) {
	let w = World::new(db, "alice").await;
	let text = file(&w.server);
	let (s, _) = w.apply(&text, &secrets(), false).await;
	assert_eq!(s, StatusCode::OK);
	let without_cache = text.replace(
		&format!(
			"[projects.shop.environments.production.apps.cache]\nserver = \"{}\"\nslug = \"cache\"\nimage = \"redis:7\"\nport = 6379\n",
			w.server
		),
		"",
	);
	assert_ne!(without_cache, text);

	let (s, done) = w.apply(&without_cache, &secrets(), false).await;
	assert_eq!(s, StatusCode::OK, "{done}");
	assert_eq!(count(&w.db, "applications").await, 2, "kept: {done}");

	let (s, done) = w.apply(&without_cache, &secrets(), true).await;
	assert_eq!(s, StatusCode::OK, "{done}");
	assert_eq!(count(&w.db, "applications").await, 1, "{done}");
	let again = w.plan(&without_cache, &secrets(), true).await;
	assert_eq!(again["changes"].as_array().unwrap().len(), 0, "{again}");
}

#[db_test]
async fn iac_a_changed_secret_updates_and_a_missing_one_is_an_issue(db: PgPool) {
	let w = World::new(db, "alice").await;
	let text = file(&w.server);
	let (s, _) = w.apply(&text, &secrets(), false).await;
	assert_eq!(s, StatusCode::OK);

	let mut rotated = secrets();
	rotated["production/web/SESSION_KEY"] = json!("rotated-key");
	let plan = w.plan(&text, &rotated, false).await;
	assert_eq!(plan["changes"].as_array().unwrap().len(), 1, "{plan}");
	assert_eq!(plan["changes"][0]["fields"][0]["field"], "env.SESSION_KEY");
	let (s, _) = w.apply(&text, &rotated, false).await;
	assert_eq!(s, StatusCode::OK);
	let after = w.plan(&text, &rotated, false).await;
	assert_eq!(after["changes"].as_array().unwrap().len(), 0, "{after}");

	// A new variable with no value to fill it in cannot be applied.
	let with_new = text.replace(
		"SESSION_KEY = ",
		"NEW_ONE = \"${secret:production/web/NEW_ONE}\"\nSESSION_KEY = ",
	);
	let plan = w.plan(&with_new, &rotated, false).await;
	let issues = plan["issues"].as_array().unwrap();
	assert_eq!(issues.len(), 1, "{plan}");
	assert!(
		issues[0]["message"]
			.as_str()
			.unwrap()
			.contains("production/web/NEW_ONE"),
		"{plan}"
	);
	let vars = count(&w.db, "env_vars").await;
	let (s, refused) = w.apply(&with_new, &rotated, false).await;
	assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{refused}");
	assert_eq!(count(&w.db, "env_vars").await, vars);
}

#[db_test]
async fn iac_a_missing_server_key_or_registry_is_a_clear_plan_error(db: PgPool) {
	let w = World::new(db, "alice").await;
	let bad = file(&w.server)
		.replace(
			"slug = \"web\"",
			"slug = \"web\"\ndeploy_key = \"no-such-key\"",
		)
		.replace(
			"image = \"redis:7\"",
			"image = \"redis:7\"\nregistry = \"no-such-registry\"",
		)
		.replace("engine = \"postgres\"", "engine = \"postgres\"\n")
		.replace(
			&format!("server = \"{}\"\nengine", w.server),
			"server = \"ghost-server\"\nengine",
		);
	let plan = w.plan(&bad, &secrets(), false).await;
	let messages: Vec<String> = plan["issues"]
		.as_array()
		.unwrap()
		.iter()
		.map(|i| {
			format!(
				"{}: {}",
				i["path"].as_str().unwrap(),
				i["message"].as_str().unwrap()
			)
		})
		.collect();
	assert!(
		messages
			.iter()
			.any(|m| m.contains("deploy_key") && m.contains("no SSH key named \"no-such-key\"")),
		"{messages:?}"
	);
	assert!(
		messages
			.iter()
			.any(|m| m.contains("registry") && m.contains("no registry named \"no-such-registry\"")),
		"{messages:?}"
	);
	assert!(
		messages
			.iter()
			.any(|m| m.contains("no server named \"ghost-server\"")),
		"{messages:?}"
	);
	let (s, refused) = w.apply(&bad, &secrets(), false).await;
	assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{refused}");
	assert_eq!(count(&w.db, "applications").await, 0, "nothing was made");
	assert_eq!(count(&w.db, "environments").await, 1);
}

#[db_test]
async fn iac_what_cannot_change_in_place_is_an_issue(db: PgPool) {
	let w = World::new(db, "alice").await;
	let text = file(&w.server);
	let (s, _) = w.apply(&text, &secrets(), false).await;
	assert_eq!(s, StatusCode::OK);
	let plan = w
		.plan(
			&text.replace("engine = \"postgres\"", "engine = \"mysql\""),
			&secrets(),
			false,
		)
		.await;
	let issues = plan["issues"].as_array().unwrap();
	assert!(
		issues.iter().any(|i| i["message"]
			.as_str()
			.unwrap()
			.contains("cannot be changed in place")),
		"{plan}"
	);
}

#[db_test]
async fn iac_a_file_with_an_unknown_key_or_version_is_refused(db: PgPool) {
	let w = World::new(db, "alice").await;
	let (s, body) = w
		.send(
			"plan",
			"version = 1\n[projects.shop]\nname = \"x\"\nsurprise = 1\n",
			&json!({}),
			json!({}),
		)
		.await;
	assert_eq!(s, StatusCode::BAD_REQUEST, "{body}");
	assert!(
		body["error"].as_str().unwrap().contains("surprise"),
		"{body}"
	);
	let (s, body) = w.send("plan", "version = 9\n", &json!({}), json!({})).await;
	assert_eq!(s, StatusCode::BAD_REQUEST, "{body}");
	assert!(
		body["error"].as_str().unwrap().contains("version 9"),
		"{body}"
	);
	let (s, _) = w
		.send("plan", "this is not toml [", &json!({}), json!({}))
		.await;
	assert_eq!(s, StatusCode::BAD_REQUEST);
}

#[db_test]
async fn iac_a_member_who_may_not_create_cannot_apply_and_nothing_changes(db: PgPool) {
	let w = World::new(db.clone(), "alice").await;
	let role = member_role(&db, w.team, "alice").await;
	// Alice may read and update everything, but not create an application.
	revoke_permission(&db, role, "create", "application").await;
	let (s, body) = w.apply(&file(&w.server), &secrets(), false).await;
	assert_eq!(s, StatusCode::FORBIDDEN, "{body}");
	assert_eq!(count(&db, "applications").await, 0);
	assert_eq!(
		count(&db, "databases").await,
		0,
		"not even what she may create"
	);
	assert_eq!(count(&db, "environments").await, 1);
	// She can still ask what it would do.
	let plan = w.plan(&file(&w.server), &secrets(), false).await;
	assert!(plan["changes"].as_array().unwrap().len() > 3);
}

#[db_test]
async fn iac_a_member_who_may_not_read_cannot_export_or_plan(db: PgPool) {
	let w = World::new(db.clone(), "alice").await;
	let role = member_role(&db, w.team, "alice").await;
	revoke_permission(&db, role, "read", "stack").await;
	let (s, _) = text(&w.app, "GET", &w.url("export"), &w.token, "").await;
	assert_eq!(s, StatusCode::FORBIDDEN);
	let (s, _) = w.send("plan", "version = 1\n", &json!({}), json!({})).await;
	assert_eq!(s, StatusCode::FORBIDDEN);
}

#[db_test]
async fn iac_another_team_gets_404_everywhere(db: PgPool) {
	let w = World::new(db.clone(), "alice").await;
	let app = &w.app;
	let (b_token, b_team) = user_with_team(app, "bob").await;
	let theirs = format!("/api/v1/teams/{b_team}/projects/{}", w.project);
	let (s, _) = text(app, "GET", &format!("{theirs}/export"), &b_token, "").await;
	assert_eq!(s, StatusCode::NOT_FOUND);
	for verb in ["plan", "apply"] {
		let (s, _) = call(
			app,
			"POST",
			&format!("{theirs}/{verb}"),
			Some(&b_token),
			Some(json!({ "file": file(&w.server) })),
		)
		.await;
		assert_eq!(s, StatusCode::NOT_FOUND, "{verb}");
	}
	// And a team that is not his at all.
	let alien = format!("/api/v1/teams/{}/projects/{}/export", w.team, w.project);
	let (s, _) = text(app, "GET", &alien, &b_token, "").await;
	assert!(
		s == StatusCode::NOT_FOUND || s == StatusCode::FORBIDDEN,
		"{s}"
	);
	assert_eq!(count(&db, "applications").await, 0);
}

#[db_test]
async fn iac_a_read_token_plans_and_cannot_apply_or_prune(db: PgPool) {
	let w = World::new(db, "alice").await;
	let make = |scopes: Value| {
		let w = &w;
		async move {
			let (s, body) = call(
				&w.app,
				"POST",
				&format!("/api/v1/teams/{}/api-tokens", w.team),
				Some(&w.token),
				Some(json!({ "name": format!("tok-{scopes}"), "scopes": scopes })),
			)
			.await;
			assert!(s.is_success(), "{body}");
			body["secret"].as_str().unwrap().to_owned()
		}
	};
	let read = make(json!(["read"])).await;
	let write = make(json!(["read", "write"])).await;
	let body = |extra: Value| {
		let mut b = json!({ "file": file(&w.server), "secrets": secrets() });
		for (k, v) in extra.as_object().unwrap() {
			b[k] = v.clone();
		}
		Some(b)
	};
	let (s, plan) = call(&w.app, "POST", &w.url("plan"), Some(&read), body(json!({}))).await;
	assert_eq!(s, StatusCode::OK, "{plan}");
	let (s, _) = text(&w.app, "GET", &w.url("export"), &read, "").await;
	assert_eq!(s, StatusCode::OK);
	let (s, denied) = call(
		&w.app,
		"POST",
		&w.url("apply"),
		Some(&read),
		body(json!({})),
	)
	.await;
	assert_eq!(s, StatusCode::FORBIDDEN, "{denied}");
	let (s, denied) = call(
		&w.app,
		"POST",
		&w.url("apply"),
		Some(&write),
		body(json!({ "prune": true })),
	)
	.await;
	assert_eq!(s, StatusCode::FORBIDDEN, "pruning needs delete: {denied}");
	let (s, ok) = call(
		&w.app,
		"POST",
		&w.url("apply"),
		Some(&write),
		body(json!({})),
	)
	.await;
	assert_eq!(s, StatusCode::OK, "{ok}");
}

#[db_test]
async fn iac_the_round_trip_into_an_empty_team_exports_the_same_file(db: PgPool) {
	let a = World::new(db.clone(), "alice").await;
	let (s, _) = a
		.apply(&without_domains(&file(&a.server)), &secrets(), false)
		.await;
	assert_eq!(s, StatusCode::OK);
	let first = a.export().await;

	let bob = World::new_empty(db.clone(), "bob").await;
	let (s, imported) = call(
		&bob.app,
		"POST",
		&format!("/api/v1/teams/{}/import/apply", bob.team),
		Some(&bob.token),
		Some(json!({ "file": first, "secrets": secrets() })),
	)
	.await;
	assert_eq!(s, StatusCode::OK, "{imported}");
	let made = imported["projects"][0]["project_id"].as_i64().unwrap();

	let (s, second) = text(
		&bob.app,
		"GET",
		&format!("/api/v1/teams/{}/projects/{made}/export", bob.team),
		&bob.token,
		"",
	)
	.await;
	assert_eq!(s, StatusCode::OK, "{second}");
	assert_eq!(first, second, "identical apart from ids and secrets");
	// The new project is a plan of nothing against the file it came from.
	let (_, plan) = call(
		&bob.app,
		"POST",
		&format!("/api/v1/teams/{}/projects/{made}/plan", bob.team),
		Some(&bob.token),
		Some(json!({ "file": first, "secrets": secrets() })),
	)
	.await;
	assert_eq!(plan["changes"].as_array().unwrap().len(), 0, "{plan}");
}

impl World {
	/// A team with no project yet.
	async fn new_empty(db: PgPool, name: &str) -> Self {
		let app = app(db.clone()).await;
		let (token, team) = user_with_team(&app, name).await;
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
			db,
			token,
			team,
			project: 0,
			server: servers[0]["name"].as_str().unwrap().to_owned(),
		}
	}
}

#[db_test]
async fn iac_an_import_makes_new_projects_and_refuses_a_slug_that_exists(db: PgPool) {
	let w = World::new(db.clone(), "alice").await;
	let text = file(&w.server);
	// The team already has `shop`.
	let (s, body) = call(
		&w.app,
		"POST",
		&format!("/api/v1/teams/{}/import/plan", w.team),
		Some(&w.token),
		Some(json!({ "file": text, "secrets": secrets() })),
	)
	.await;
	assert_eq!(s, StatusCode::OK, "{body}");
	let issues = body["projects"][0]["plan"]["issues"].as_array().unwrap();
	assert!(
		issues
			.iter()
			.any(|i| i["message"].as_str().unwrap().contains("already exists")),
		"{body}"
	);
	let projects = count(&db, "projects").await;
	let (s, refused) = call(
		&w.app,
		"POST",
		&format!("/api/v1/teams/{}/import/apply", w.team),
		Some(&w.token),
		Some(json!({ "file": text, "secrets": secrets() })),
	)
	.await;
	assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{refused}");
	assert_eq!(count(&db, "projects").await, projects, "nothing was made");

	let renamed = without_domains(&text)
		.replace("[projects.shop", "[projects.shop2")
		.replace("name = \"Shop\"", "name = \"Shop two\"")
		.replace("slug = \"web\"", "slug = \"web2\"")
		.replace("slug = \"cache\"", "slug = \"cache2\"");
	let (s, done) = call(
		&w.app,
		"POST",
		&format!("/api/v1/teams/{}/import/apply", w.team),
		Some(&w.token),
		Some(json!({ "file": renamed, "secrets": secrets() })),
	)
	.await;
	assert_eq!(s, StatusCode::OK, "{done}");
	assert_eq!(count(&db, "projects").await, projects + 1);
	assert_eq!(
		count(&db, "applications").await,
		2,
		"web and cache of the new project (shop is empty)"
	);
}

#[db_test]
async fn iac_a_failing_change_stops_the_apply_and_the_report_says_where(db: PgPool) {
	let w = World::new(db.clone(), "alice").await;
	let (s, _) = w.apply(&file(&w.server), &secrets(), false).await;
	assert_eq!(s, StatusCode::OK);
	// A second project that wants the same hostnames: the install allows each only once.
	let (_, other) = call(
		&w.app,
		"POST",
		&format!("/api/v1/teams/{}/projects", w.team),
		Some(&w.token),
		Some(json!({ "name": "Other", "slug": "other" })),
	)
	.await;
	let other = other["id"].as_i64().unwrap();
	let clash = file(&w.server)
		.replace("slug = \"web\"", "slug = \"web3\"")
		.replace("slug = \"cache\"", "slug = \"cache3\"")
		.replace("[projects.shop", "[projects.other");
	let (s, done) = call(
		&w.app,
		"POST",
		&format!("/api/v1/teams/{}/projects/{other}/apply", w.team),
		Some(&w.token),
		Some(json!({ "file": clash, "secrets": secrets() })),
	)
	.await;
	assert_eq!(s, StatusCode::CONFLICT, "{done}");
	let failed = &done["report"]["failed"];
	assert_eq!(failed["name"], "web", "{done}");
	assert!(
		failed["message"].as_str().unwrap().contains("hostname"),
		"{done}"
	);
	assert!(
		!done["report"]["applied"].as_array().unwrap().is_empty(),
		"what ran before it is listed: {done}"
	);
}

#[db_test]
async fn iac_the_export_lists_a_model_and_its_download_by_name(db: PgPool) {
	let w = World::new(db.clone(), "alice").await;
	crate::tests::seed_model(&db, w.team, w.project, "tiny", "stopped", None).await;
	let exported = w.export().await;
	assert!(
		exported.contains("[projects.shop.environments.production.models.tiny]"),
		"{exported}"
	);
	assert!(exported.contains("repo = \"acme/open\""), "{exported}");
	assert!(exported.contains("revision = \"main\""));
	assert!(exported.contains(&format!("server = \"{}\"", w.server)));
	let (s, body) = text(&w.app, "POST", &w.url("plan"), &w.token, &exported).await;
	assert_eq!(s, StatusCode::OK, "{body}");
	assert_eq!(
		json_of(&body)["changes"].as_array().unwrap().len(),
		0,
		"{body}"
	);
	// A model is not edited in place.
	let (_, plan) = w
		.send(
			"plan",
			&exported.replace("context_size = 4096", "context_size = 8192"),
			&json!({}),
			json!({}),
		)
		.await;
	assert!(
		plan["issues"]
			.as_array()
			.unwrap()
			.iter()
			.any(|i| i["message"]
				.as_str()
				.unwrap()
				.contains("cannot be changed in place")),
		"{plan}"
	);
	// One whose download is not on the server is a clear error.
	let (_, plan) = w
		.send(
			"plan",
			&exported.replace("acme/open", "acme/missing"),
			&json!({}),
			json!({}),
		)
		.await;
	assert!(
		plan["issues"]
			.as_array()
			.unwrap()
			.iter()
			.any(|i| i["message"].as_str().unwrap().contains("download")),
		"{plan}"
	);
}

#[db_test]
async fn iac_a_network_with_a_member_of_the_project_is_exported_with_its_members(db: PgPool) {
	let w = World::new(db.clone(), "alice").await;
	let (s, _) = w.apply(&file(&w.server), &secrets(), false).await;
	assert_eq!(s, StatusCode::OK);
	let server: i64 = crate::tests::server_id(&db, w.team).await;
	let app_id = crate::tests::application_id(&db, "web").await;
	let network = crypto::generate_id();
	sqlx::query(
		"INSERT INTO networks (id, team_id, server_id, name, slug, kind, podman_name, internal)
		 VALUES ($1, $2, $3, 'Back', 'back', 'user', $4, TRUE)",
	)
	.bind(network)
	.bind(w.team)
	.bind(server)
	.bind(format!("podkit-net-{network}"))
	.execute(&db)
	.await
	.unwrap();
	sqlx::query(
		"INSERT INTO resource_networks (network_id, resource_kind, resource_id, aliases)
		 VALUES ($1, 'application', $2, ARRAY['web-internal'])",
	)
	.bind(network)
	.bind(app_id)
	.execute(&db)
	.await
	.unwrap();

	let exported = w.export().await;
	assert!(
		exported.contains("[[projects.shop.networks]]"),
		"{exported}"
	);
	assert!(exported.contains("internal = true"));
	assert!(exported.contains("aliases = [\"web-internal\"]") || exported.contains("web-internal"));
	let plan = w.plan(&exported, &secrets(), false).await;
	assert_eq!(plan["changes"].as_array().unwrap().len(), 0, "{plan}");

	// Dropping the member from the file only matters with prune.
	let without = exported
		.split("[[projects.shop.networks]]")
		.next()
		.unwrap()
		.to_owned();
	let off = w.plan(&without, &secrets(), false).await;
	assert_eq!(off["changes"].as_array().unwrap().len(), 0);
	assert_eq!(off["extra"].as_array().unwrap().len(), 1, "{off}");
	let on = w.plan(&without, &secrets(), true).await;
	assert_eq!(count_of(&on, "delete"), 1, "{on}");
}

#[db_test]
async fn iac_a_plan_of_a_stack_names_the_changed_compose_and_variable(db: PgPool) {
	let w = World::new(db, "alice").await;
	let text = file(&w.server);
	let (s, _) = w.apply(&text, &secrets(), false).await;
	assert_eq!(s, StatusCode::OK);
	let edited = text.replace("rabbitmq:3", "rabbitmq:4");
	let plan = w.plan(&edited, &secrets(), false).await;
	let change = &plan["changes"][0];
	assert_eq!(change["kind"], "stack", "{plan}");
	assert_eq!(change["fields"][0]["field"], "compose");
	assert_eq!(change["will_redeploy"], true);
	let (s, done) = w.apply(&edited, &secrets(), false).await;
	assert_eq!(s, StatusCode::OK, "{done}");
	let again = w.plan(&edited, &secrets(), false).await;
	assert_eq!(again["changes"].as_array().unwrap().len(), 0, "{again}");
}

/// The same file without custom hostnames, which are unique across the whole install.
fn without_domains(text: &str) -> String {
	text.replace(
		"domains = [\"shop.example.com\", \"www.example.com\"]\n",
		"",
	)
}

// ----- the schema and the examples of the docs

const SCHEMA: &str = include_str!("../../../docs/iac/podkit.schema.json");
const README: &str = include_str!("../../../docs/iac/README.md");

fn validator() -> jsonschema::Validator {
	let schema: Value = serde_json::from_str(SCHEMA).expect("the schema is JSON");
	jsonschema::validator_for(&schema).expect("the schema is a schema")
}

fn as_json(toml_text: &str) -> Value {
	let value: toml::Value = toml::from_str(toml_text).expect("TOML");
	serde_json::to_value(value).unwrap()
}

/// The fenced `toml` blocks of the docs: every one is an example of a file.
fn examples() -> Vec<String> {
	let mut out = Vec::new();
	let mut current: Option<String> = None;
	for line in README.lines() {
		match (&mut current, line) {
			(None, "```toml") => current = Some(String::new()),
			(Some(block), "```") => {
				out.push(std::mem::take(block));
				current = None;
			}
			(Some(block), line) => {
				block.push_str(line);
				block.push('\n');
			}
			(None, _) => {}
		}
	}
	out
}

#[test]
fn iac_every_example_in_the_docs_validates_against_the_schema_and_parses() {
	let validator = validator();
	let found = examples();
	assert!(found.len() >= 5, "the docs have examples: {}", found.len());
	for (n, example) in found.iter().enumerate() {
		let errors: Vec<String> = validator
			.iter_errors(&as_json(example))
			.map(|e| format!("{} at {}", e, e.instance_path()))
			.collect();
		assert!(
			errors.is_empty(),
			"example {n} does not validate: {errors:?}\n{example}"
		);
		super::doc::parse(example).unwrap_or_else(|e| panic!("example {n} does not parse: {e}"));
	}
}

#[test]
fn iac_the_schema_refuses_what_podkit_refuses() {
	let validator = validator();
	for (what, bad) in [
		("another version", "version = 2\n"),
		("an unknown key", "version = 1\nsurprise = true\n"),
		(
			"an app with neither repo nor image",
			"version = 1\n[projects.p]\nname = \"P\"\n[projects.p.environments.production]\nname = \"p\"\n[projects.p.environments.production.apps.a]\nserver = \"s\"\nslug = \"a\"\nport = 80\n",
		),
		(
			"an app with both",
			"version = 1\n[projects.p]\nname = \"P\"\n[projects.p.environments.production]\nname = \"p\"\n[projects.p.environments.production.apps.a]\nserver = \"s\"\nslug = \"a\"\nport = 80\nrepo = \"r\"\nimage = \"i\"\n",
		),
		(
			"an engine that does not exist",
			"version = 1\n[projects.p]\nname = \"P\"\n[projects.p.environments.production]\nname = \"p\"\n[projects.p.environments.production.databases.d]\nserver = \"s\"\nengine = \"oracle\"\n",
		),
		(
			"a project that is not a slug",
			"version = 1\n[projects.\"Not A Slug\"]\nname = \"P\"\n",
		),
	] {
		assert!(
			!validator.is_valid(&as_json(bad)),
			"the schema accepts {what}"
		);
	}
}

#[db_test]
async fn iac_a_real_export_validates_against_the_schema_and_the_secrets_file_of_the_docs_parses(
	db: PgPool,
) {
	let w = World::new(db.clone(), "alice").await;
	let (s, _) = w.apply(&file(&w.server), &secrets(), false).await;
	assert_eq!(s, StatusCode::OK);
	crate::tests::seed_model(&db, w.team, w.project, "tiny", "stopped", None).await;
	let exported = w.export().await;
	let errors: Vec<String> = validator()
		.iter_errors(&as_json(&exported))
		.map(|e| format!("{} at {}", e, e.instance_path()))
		.collect();
	assert!(errors.is_empty(), "{errors:?}\n{exported}");
	// The secrets file of the docs is what `Secrets::parse` reads.
	let block = README
		.split("```toml secrets\n")
		.nth(1)
		.and_then(|rest| rest.split("```").next())
		.expect("the docs show a secrets file");
	assert!(!super::objects::Secrets::parse(block).unwrap().0.is_empty());
}
