//! Log retention and search: the merge of several agents' answers, the settings and what
//! becomes of the `Authorization` header, the search through the API against an agent with a real
//! log store, and the hand-over of settings on the sampler's tick.
//! `cargo test -p podkit-core --all-features -- log_search`

use axum::Router;
use axum::http::StatusCode;
use proto::{LogLevel, LogRecord, LogSearchResult, Stream};
use serde_json::{Value, json};
use sqlx::PgPool;

use super::service::{self, SettingsInput};
use super::{ExportKind, Line, merge};
use crate::ids::{ServerId, TeamId, UserId};
use crate::state::AppState;
use crate::tests::log_search::{StubAgent, add_member, now_ms, sealed_header, stub_agent};
use crate::tests::{
	call, count, local_server_id, project_id, seed_app_and_database, state, user_id, user_with_team,
};

fn rec(ts: i64, svc: &str, line: &str) -> LogRecord {
	LogRecord {
		ts,
		stream: Stream::Stdout,
		dep: None,
		level: None,
		line: line.to_owned(),
		svc: svc.to_owned(),
	}
}

fn answer(records: Vec<LogRecord>) -> LogSearchResult {
	LogSearchResult {
		records,
		partial: false,
		truncated: false,
	}
}

fn texts(lines: &[Line]) -> Vec<&str> {
	lines.iter().map(|l| l.record.line.as_str()).collect()
}

// --- the merge ---------------------------------------------------------------------------------

#[test]
fn log_search_merge_interleaves_the_agents_by_time() {
	let a = answer(vec![rec(10, "app-1", "a10"), rec(30, "app-1", "a30")]);
	let b = answer(vec![rec(20, "app-2", "b20"), rec(40, "app-2", "b40")]);
	let merged = merge(vec![(ServerId(1), a), (ServerId(2), b)], 10, true);
	assert_eq!(texts(&merged.lines), ["a10", "b20", "a30", "b40"]);
	assert!(!merged.truncated && !merged.partial);
}

#[test]
fn log_search_merge_cuts_from_the_end_the_search_asked_for() {
	let a = answer(vec![rec(10, "app-1", "a10"), rec(30, "app-1", "a30")]);
	let b = answer(vec![rec(20, "app-2", "b20"), rec(40, "app-2", "b40")]);
	let parts = || vec![(ServerId(1), a.clone()), (ServerId(2), b.clone())];

	let newest = merge(parts(), 3, true);
	assert_eq!(texts(&newest.lines), ["b20", "a30", "b40"]);
	assert!(newest.truncated);

	let oldest = merge(parts(), 3, false);
	assert_eq!(texts(&oldest.lines), ["a10", "b20", "a30"]);
	assert!(oldest.truncated);
}

#[test]
fn log_search_merge_orders_a_tie_by_server_then_resource_and_keeps_an_agents_own_order() {
	let a = answer(vec![
		rec(5, "app-9", "first"),
		rec(5, "app-9", "second"),
		rec(5, "app-3", "other"),
	]);
	let b = answer(vec![rec(5, "app-1", "elsewhere")]);
	// Whichever order the agents answered in, the same list comes out.
	let one = merge(
		vec![(ServerId(2), b.clone()), (ServerId(1), a.clone())],
		10,
		true,
	);
	let two = merge(vec![(ServerId(1), a), (ServerId(2), b)], 10, true);
	assert_eq!(texts(&one.lines), ["other", "first", "second", "elsewhere"]);
	assert_eq!(texts(&one.lines), texts(&two.lines));
}

#[test]
fn log_search_merge_says_when_an_agent_stopped_early() {
	let partial = LogSearchResult {
		records: vec![rec(1, "app-1", "x")],
		partial: true,
		truncated: false,
	};
	let merged = merge(
		vec![(ServerId(1), partial), (ServerId(2), answer(vec![]))],
		5,
		true,
	);
	assert!(merged.partial);
	assert!(!merged.truncated);
}

// --- validating the settings ---------------------------------------------------------------------

fn input() -> SettingsInput {
	SettingsInput {
		retention_days: 7,
		max_mb: 1024,
		export_kind: String::new(),
		export_url: String::new(),
		export_tenant: String::new(),
		export_authorization: String::new(),
		clear_authorization: false,
	}
}

#[test]
fn log_search_settings_have_sane_bounds() {
	for (days, mb) in [(0, 1024), (3651, 1024), (7, 15), (7, 2_000_000)] {
		let bad = SettingsInput {
			retention_days: days,
			max_mb: mb,
			..input()
		};
		assert!(service::validate(&bad).is_err(), "{days} days, {mb} MB");
	}
	assert!(service::validate(&input()).is_ok());
}

#[test]
fn log_search_an_export_address_is_a_plain_http_url_without_credentials() {
	let with = |kind: &str, url: &str| SettingsInput {
		export_kind: kind.to_owned(),
		export_url: url.to_owned(),
		..input()
	};
	for bad in [
		"",
		"loki.example.com",
		"ftp://loki.example.com/push",
		"https://user:pass@loki.example.com/push",
		"https://loki.example.com/pu sh",
		"javascript:alert(1)",
	] {
		assert!(service::validate(&with("loki", bad)).is_err(), "{bad:?}");
	}
	assert!(service::validate(&with("nope", "https://loki.example.com/")).is_err());
	let ok = service::validate(&with("otlp", "http://collector:4318/v1/logs")).unwrap();
	assert_eq!(ok.export.unwrap().0, ExportKind::Otlp);
}

#[test]
fn log_search_no_forwarding_drops_the_header_whatever_was_typed() {
	let v = service::validate(&SettingsInput {
		export_authorization: "Bearer leftover".to_owned(),
		..input()
	})
	.unwrap();
	assert!(v.export.is_none() && v.authorization.is_none() && v.clear_authorization);
}

// --- a world to search -----------------------------------------------------------------------------

struct World {
	s: AppState,
	app: Router,
	token: String,
	team: i64,
	user: UserId,
	agent: StubAgent,
}

async fn world(db: PgPool) -> World {
	let s = state(db.clone());
	let app = crate::routes::routes(s.clone()).await.unwrap();
	let (token, team) = user_with_team(&app, "alice").await;
	let (st, project) = call(
		&app,
		"POST",
		&format!("/api/v1/teams/{team}/projects"),
		Some(&token),
		Some(json!({ "name": "shop", "slug": "shop" })),
	)
	.await;
	assert!(st.is_success(), "{project}");
	seed_app_and_database(&db, team, project_id(&db, "shop").await).await;
	let agent = stub_agent(&s);
	World {
		user: UserId(user_id(&db, "alice").await),
		s,
		app,
		token,
		team,
		agent,
	}
}

impl World {
	fn api(&self, rest: &str) -> String {
		format!("/api/v1/teams/{}/logs{rest}", self.team)
	}

	async fn search(&self, body: Value) -> (StatusCode, Value) {
		call(
			&self.app,
			"POST",
			&self.api("/search"),
			Some(&self.token),
			Some(body),
		)
		.await
	}
}

fn lines(body: &Value) -> Vec<String> {
	body["lines"]
		.as_array()
		.unwrap()
		.iter()
		.map(|l| l["line"].as_str().unwrap().to_owned())
		.collect()
}

// --- searching -------------------------------------------------------------------------------------------

#[sqlx::test]
async fn log_search_finds_the_lines_of_two_deployments_in_time_order(db: PgPool) {
	let w = world(db).await;
	let t = now_ms();
	let line = |svc: &str, ago: i64, dep: Option<i64>, level: Option<LogLevel>, text: &str| {
		w.agent.line(svc, t - ago, dep, level, text);
	};
	line(
		"app-5001",
		5000,
		Some(1),
		Some(LogLevel::Info),
		"deploy one: listening",
	);
	line(
		"app-5001",
		4000,
		Some(1),
		Some(LogLevel::Error),
		"deploy one: crashed",
	);
	line(
		"app-5001",
		2000,
		Some(2),
		Some(LogLevel::Info),
		"deploy two: listening",
	);
	line(
		"app-5001",
		1000,
		Some(2),
		Some(LogLevel::Warn),
		"deploy two: slow request",
	);
	line(
		"db-6001",
		3000,
		None,
		None,
		"db: ready to accept connections",
	);

	let (s, body) = w.search(json!({ "services": ["app-5001"] })).await;
	assert_eq!(s, StatusCode::OK, "{body}");
	assert_eq!(
		lines(&body),
		[
			"deploy one: listening",
			"deploy one: crashed",
			"deploy two: listening",
			"deploy two: slow request"
		]
	);
	assert_eq!(body["lines"][0]["name"], "Storefront");
	assert_eq!(body["lines"][0]["deployment"], 1);
	assert_eq!(body["lines"][3]["level"], "warn");

	// No resources named: all of the team's, still in order.
	let (_, body) = w.search(json!({})).await;
	assert_eq!(lines(&body).len(), 5);
	assert_eq!(lines(&body)[2], "db: ready to accept connections");
}

#[sqlx::test]
async fn log_search_filters_by_text_regex_level_deployment_and_range(db: PgPool) {
	let w = world(db).await;
	let t = now_ms();
	w.agent.line(
		"app-5001",
		t - 9000,
		Some(1),
		Some(LogLevel::Info),
		"GET /a 200",
	);
	w.agent.line(
		"app-5001",
		t - 8000,
		Some(1),
		Some(LogLevel::Error),
		"GET /b 500",
	);
	w.agent.line(
		"app-5001",
		t - 7000,
		Some(2),
		Some(LogLevel::Info),
		"GET /c 200",
	);
	w.agent.line(
		"app-5001",
		t - 6000,
		Some(2),
		Some(LogLevel::Warn),
		"GET /d 429",
	);

	let q = |extra: Value| {
		let mut base = json!({ "services": ["app-5001"] });
		base.as_object_mut()
			.unwrap()
			.extend(extra.as_object().unwrap().clone());
		base
	};
	let (_, b) = w.search(q(json!({ "text": "get /c" }))).await;
	assert_eq!(lines(&b), ["GET /c 200"], "plain text ignores case");
	let (_, b) = w
		.search(q(json!({ "text": "[45]\\d\\d$", "regex": true })))
		.await;
	assert_eq!(lines(&b), ["GET /b 500", "GET /d 429"]);
	let (_, b) = w.search(q(json!({ "levels": ["warn", "error"] }))).await;
	assert_eq!(lines(&b), ["GET /b 500", "GET /d 429"]);
	let (_, b) = w.search(q(json!({ "deployment": 2 }))).await;
	assert_eq!(lines(&b), ["GET /c 200", "GET /d 429"]);
	let (_, b) = w
		.search(q(json!({ "since": t - 8500, "until": t - 6500 })))
		.await;
	assert_eq!(lines(&b), ["GET /b 500", "GET /c 200"]);
	let (_, b) = w.search(q(json!({ "limit": 2 }))).await;
	assert_eq!(lines(&b), ["GET /c 200", "GET /d 429"], "the newest two");
	assert_eq!(b["truncated"], true);
	let (_, b) = w.search(q(json!({ "limit": 2, "newest": false }))).await;
	assert_eq!(lines(&b), ["GET /a 200", "GET /b 500"], "or the oldest two");
}

#[sqlx::test]
async fn log_search_a_bad_expression_or_range_is_refused(db: PgPool) {
	let w = world(db).await;
	let (s, body) = w
		.search(json!({ "services": ["app-5001"], "text": "(", "regex": true }))
		.await;
	assert!(s.is_client_error(), "{s} {body}");
	let (s, _) = w.search(json!({ "since": 100, "until": 50 })).await;
	assert!(s.is_client_error(), "{s}");
	let long = "x".repeat(501);
	let (s, _) = w.search(json!({ "text": long })).await;
	assert!(s.is_client_error(), "{s}");
}

#[sqlx::test]
async fn log_search_another_teams_resource_is_not_found_and_the_agent_is_not_asked(db: PgPool) {
	let w = world(db.clone()).await;
	w.agent.line(
		"app-5001",
		now_ms() - 1000,
		None,
		None,
		"alice's secret line",
	);
	let (bob, bob_team) = user_with_team(&w.app, "bob").await;
	let theirs = format!("/api/v1/teams/{bob_team}/logs");

	// Bob names alice's application through his own team: not found.
	let (s, body) = call(
		&w.app,
		"POST",
		&format!("{theirs}/search"),
		Some(&bob),
		Some(json!({ "services": ["app-5001"] })),
	)
	.await;
	assert_eq!(s, StatusCode::NOT_FOUND, "{body}");
	assert!(!body.to_string().contains("secret"));

	// Bob names nothing: his own resources, which are none. Never "everything the agent has".
	let (s, body) = call(
		&w.app,
		"POST",
		&format!("{theirs}/search"),
		Some(&bob),
		Some(json!({})),
	)
	.await;
	assert_eq!(s, StatusCode::OK);
	assert_eq!(body["lines"], json!([]));
	assert!(
		w.agent.searches().is_empty(),
		"the agent was asked: {:?}",
		w.agent.searches()
	);

	// His sources list is his own too.
	let (_, sources) = call(
		&w.app,
		"GET",
		&format!("{theirs}/sources"),
		Some(&bob),
		None,
	)
	.await;
	assert_eq!(sources, json!([]));

	// And through alice's team path he is not a member.
	let (s, _) = call(
		&w.app,
		"POST",
		&w.api("/search"),
		Some(&bob),
		Some(json!({ "services": ["app-5001"] })),
	)
	.await;
	assert!(
		s == StatusCode::FORBIDDEN || s == StatusCode::NOT_FOUND,
		"{s}"
	);
}

#[sqlx::test]
async fn log_search_asks_for_the_teams_resources_only(db: PgPool) {
	let w = world(db).await;
	let (s, _) = w.search(json!({})).await;
	assert_eq!(s, StatusCode::OK);
	let asked = w.agent.searches();
	assert_eq!(asked.len(), 1);
	let mut services = asked[0].services.clone();
	services.sort();
	assert_eq!(services, ["app-5001", "db-6001"]);
}

#[sqlx::test]
async fn log_search_sources_name_the_resources_with_their_projects(db: PgPool) {
	let w = world(db).await;
	let (s, sources) = call(&w.app, "GET", &w.api("/sources"), Some(&w.token), None).await;
	assert_eq!(s, StatusCode::OK);
	let names: Vec<(&str, &str, &str)> = sources
		.as_array()
		.unwrap()
		.iter()
		.map(|s| {
			(
				s["service"].as_str().unwrap(),
				s["name"].as_str().unwrap(),
				s["project"].as_str().unwrap(),
			)
		})
		.collect();
	assert_eq!(
		names,
		[
			("app-5001", "Storefront", "shop"),
			("db-6001", "orders", "shop")
		]
	);
}

// --- the settings -----------------------------------------------------------------------------------------------

const HEADER: &str = "Bearer glc_super_secret_value";

fn settings_body() -> Value {
	json!({
		"retention_days": 14, "max_mb": 2048, "export_kind": "loki",
		"export_url": "https://loki.example.com/loki/api/v1/push",
		"export_tenant": "homelab", "export_authorization": HEADER,
	})
}

#[sqlx::test]
async fn log_search_settings_default_then_save_and_reach_the_agent(db: PgPool) {
	let w = world(db).await;
	let t = Some(w.token.as_str());
	let (s, body) = call(&w.app, "GET", &w.api("/settings"), t, None).await;
	assert_eq!(s, StatusCode::OK);
	assert_eq!(body["retention_days"], 7);
	assert_eq!(body["max_mb"], 1024);
	assert_eq!(body["export"], Value::Null);

	let (s, body) = call(&w.app, "PUT", &w.api("/settings"), t, Some(settings_body())).await;
	assert_eq!(s, StatusCode::OK, "{body}");
	assert_eq!(body["retention_days"], 14);
	assert_eq!(body["export"]["kind"], "loki");
	assert_eq!(body["export"]["tenant"], "homelab");
	assert_eq!(body["export"]["has_authorization"], true);

	// The agent got them at once: days, bytes, the header opened and the labels.
	let configs = w.agent.configs();
	assert_eq!(configs.len(), 1, "{configs:?}");
	let c = &configs[0];
	assert_eq!((c.retention_days, c.max_bytes), (14, 2048 << 20));
	let export = c.export.as_ref().unwrap();
	assert_eq!(export.authorization.as_deref(), Some(HEADER));
	assert_eq!(export.tenant.as_deref(), Some("homelab"));
	assert!(
		export
			.labels
			.contains(&("team".to_owned(), "alice".to_owned()))
	);
}

#[sqlx::test]
async fn log_search_the_authorization_header_is_sealed_and_never_shown(db: PgPool) {
	let w = world(db.clone()).await;
	let t = Some(w.token.as_str());
	call(&w.app, "PUT", &w.api("/settings"), t, Some(settings_body())).await;

	// Stored sealed, not as text.
	let sealed = sealed_header(&db, w.team)
		.await
		.expect("a header is stored");
	assert!(!String::from_utf8_lossy(&sealed).contains("glc_super_secret"));
	assert_eq!(w.s.secrets.decrypt(&sealed).unwrap(), HEADER);

	// Never in an answer.
	let (_, got) = call(&w.app, "GET", &w.api("/settings"), t, None).await;
	assert!(!got.to_string().contains("glc_super_secret"), "{got}");

	// Never in the audit log; the change is there.
	let diff: Value = sqlx::query_scalar(
		"SELECT diff FROM audit_events WHERE action = 'log_settings.update' ORDER BY id DESC LIMIT 1",
	)
	.fetch_one(&db)
	.await
	.unwrap();
	assert!(!diff.to_string().contains("glc_super_secret"), "{diff}");
	assert_eq!(diff["retention_days"]["to"], 14);
	assert_eq!(
		diff["export_url"],
		"https://loki.example.com/loki/api/v1/push"
	);

	// Saving again with the field empty keeps the header...
	let mut again = settings_body();
	again["export_authorization"] = json!("");
	again["retention_days"] = json!(30);
	let (s, body) = call(&w.app, "PUT", &w.api("/settings"), t, Some(again.clone())).await;
	assert_eq!(s, StatusCode::OK, "{body}");
	assert_eq!(body["export"]["has_authorization"], true);
	assert_eq!(
		w.s.secrets
			.decrypt(&sealed_header(&db, w.team).await.unwrap())
			.unwrap(),
		HEADER
	);

	// ...asking to forget it drops it...
	again["clear_authorization"] = json!(true);
	let (_, body) = call(&w.app, "PUT", &w.api("/settings"), t, Some(again)).await;
	assert_eq!(body["export"]["has_authorization"], false);
	assert!(sealed_header(&db, w.team).await.is_none());

	// ...and so does turning forwarding off.
	call(&w.app, "PUT", &w.api("/settings"), t, Some(settings_body())).await;
	assert!(sealed_header(&db, w.team).await.is_some());
	let off = json!({ "retention_days": 7, "max_mb": 1024, "export_kind": "" });
	let (_, body) = call(&w.app, "PUT", &w.api("/settings"), t, Some(off)).await;
	assert_eq!(body["export"], Value::Null);
	assert!(sealed_header(&db, w.team).await.is_none());
}

#[sqlx::test]
async fn log_search_a_member_reads_but_does_not_change_the_settings(db: PgPool) {
	let w = world(db.clone()).await;
	let (bob, _) = user_with_team(&w.app, "bob").await;
	add_member(&db, w.team, "bob").await;
	let t = Some(bob.as_str());

	let (s, _) = call(&w.app, "GET", &w.api("/settings"), t, None).await;
	assert_eq!(s, StatusCode::OK);
	let (s, _) = call(&w.app, "POST", &w.api("/search"), t, Some(json!({}))).await;
	assert_eq!(s, StatusCode::OK);
	let (s, _) = call(&w.app, "PUT", &w.api("/settings"), t, Some(settings_body())).await;
	assert_eq!(s, StatusCode::FORBIDDEN);
	assert_eq!(count(&db, "log_settings").await, 0);
	assert!(w.agent.configs().is_empty());
}

// --- the hand-over to the agent -----------------------------------------------------------------------------------

async fn local_server(w: &World) -> crate::servers::Server {
	let id = local_server_id(&w.s.db, w.team).await;
	crate::servers::find(&w.s.db, ServerId(id))
		.await
		.unwrap()
		.unwrap()
}

#[sqlx::test]
async fn log_search_the_tick_gives_the_agent_its_settings_once_and_again_when_they_change(
	db: PgPool,
) {
	let w = world(db).await;
	let server = local_server(&w).await;
	let agent = w.s.agents.client(&w.s, &server).await.unwrap();

	super::sync::tick(&w.s, &server, &agent).await;
	super::sync::tick(&w.s, &server, &agent).await;
	let configs = w.agent.configs();
	assert_eq!(configs.len(), 1, "an unchanged setting is not sent twice");
	assert_eq!(
		(configs[0].retention_days, configs[0].max_bytes),
		(7, 1 << 30)
	);
	assert!(configs[0].export.is_none());

	let saved = SettingsInput {
		retention_days: 3,
		..input()
	};
	service::update_settings(&w.s, w.user, TeamId(w.team), &saved)
		.await
		.unwrap();
	super::sync::tick(&w.s, &server, &agent).await;
	let configs = w.agent.configs();
	assert_eq!(
		configs.len(),
		2,
		"the save pushed once, the tick found nothing to add"
	);
	assert_eq!(configs[1].retention_days, 3);
}

#[sqlx::test]
async fn log_search_dropped_lines_tell_the_team_once_per_increase(db: PgPool) {
	let w = world(db.clone()).await;
	let server = local_server(&w).await;
	let agent = w.s.agents.client(&w.s, &server).await.unwrap();
	let events = || count(&db, "notification_events");

	super::sync::tick(&w.s, &server, &agent).await;
	assert_eq!(events().await, 0, "nothing dropped yet");

	w.agent
		.dropped
		.store(40, std::sync::atomic::Ordering::Relaxed);
	super::sync::tick(&w.s, &server, &agent).await;
	assert_eq!(events().await, 1);
	let (kind, title, link): (String, String, Option<String>) =
		sqlx::query_as("SELECT kind, title, link FROM notification_events")
			.fetch_one(&db)
			.await
			.unwrap();
	assert_eq!(kind, "log_export_failing");
	assert!(title.contains("dropping lines"), "{title}");
	assert_eq!(link.unwrap(), format!("/teams/{}/logs", w.team));

	// The same count again: nothing new to say.
	super::sync::tick(&w.s, &server, &agent).await;
	assert_eq!(events().await, 1);
}
