//! Server firewall tests: what the form may ask for, the policy the helper is given, the apply
//! with its confirm window (against the helper's real engine on a fake host), the revert when
//! podkit cannot reach the server again, permissions, another team's server, and escaping.
//!
//! `cargo test -p server --all-features -- firewall`

use std::net::IpAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::Router;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{StatusCode, header};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use podkit_firewall::engine::{Active, Engine, Host};
use podkit_firewall::protocol::{Request, Response};
use serde_json::json;
use sqlx::PgPool;

use super::service::{self, Helper};
use super::{Rule, RuleInput, Settings, parse_ssh_sources, policy, rules, settings};
use crate::error::ServerError;
use crate::ids::{FirewallRuleId, ServerId, TeamId, UserId};
use crate::state::AppState;
use crate::tests::{count, local_server_id, member_role, revoke_permission, state, user_id};
use crate::ui::testing::{
	assert_contains, assert_not_contains, get_html, post_signals, ui_session,
};

// --- the form ------------------------------------------------------------------------------

fn input(proto: &str, port: &str, source: &str, note: &str) -> RuleInput {
	RuleInput {
		proto: proto.to_owned(),
		port: port.to_owned(),
		source: source.to_owned(),
		note: note.to_owned(),
	}
}

#[test]
fn firewall_an_allow_is_checked_and_its_range_normalized() {
	let ok = input("udp", " 51820 ", "10.0.0.7/24", " wireguard ")
		.parse()
		.unwrap();
	assert_eq!(ok.port, 51820);
	assert_eq!(ok.source.as_deref(), Some("10.0.0.0/24"));
	assert_eq!(ok.note, "wireguard");
	// A bare protocol field is TCP, an empty source is anyone.
	let any = input("", "5432", "", "").parse().unwrap();
	assert_eq!(any.proto, podkit_firewall::policy::Proto::Tcp);
	assert_eq!(any.source, None);
	for bad in [
		input("icmp", "1", "", ""),
		input("tcp", "0", "", ""),
		input("tcp", "65536", "", ""),
		input("tcp", "http", "", ""),
		input("tcp", "80", "10.0.0.0/33", ""),
		input("tcp", "80", "not-an-address", ""),
		input("tcp", "80", "", &"x".repeat(81)),
		input("tcp", "80", "", "a\nb"),
	] {
		assert!(
			matches!(bad.parse(), Err(ServerError::Validation(_))),
			"{bad:?}"
		);
	}
}

#[test]
fn firewall_ssh_ranges_are_deduplicated_and_must_parse() {
	assert_eq!(
		parse_ssh_sources("10.0.0.7/24, 203.0.113.9\n10.0.0.0/24").unwrap(),
		["10.0.0.0/24", "203.0.113.9/32"]
	);
	assert!(parse_ssh_sources("").unwrap().is_empty());
	assert!(parse_ssh_sources("10.0.0.0/8 nope").is_err());
}

#[test]
fn firewall_the_stored_rows_become_the_helpers_policy() {
	let rows = vec![
		Rule {
			id: FirewallRuleId(1),
			proto: "tcp".to_owned(),
			port: 5432,
			source: Some("10.0.0.0/24".to_owned()),
			note: String::new(),
		},
		Rule {
			id: FirewallRuleId(2),
			proto: "udp".to_owned(),
			port: 51820,
			source: None,
			note: String::new(),
		},
	];
	let settings = Settings {
		ssh_sources: vec!["203.0.113.0/24".to_owned()],
		..Settings::default()
	};
	let p = policy(2222, &settings, &rows).unwrap();
	assert_eq!(p.ssh_port, 2222);
	assert_eq!(p.ssh_sources.len(), 1);
	assert_eq!(p.allows.len(), 2);
	assert_eq!(p.allows[1].proto.as_str(), "udp");
	p.validate().unwrap();
	assert!(policy(0, &settings, &rows).unwrap().validate().is_err());
}

// --- a fake server: the helper's real engine on a host that only remembers ----------------

#[derive(Default)]
struct HostState {
	applied: Vec<String>,
	saved: Option<Active>,
}

#[derive(Clone, Default)]
struct FakeHost {
	state: Arc<Mutex<HostState>>,
	/// What the open SSH sessions would be.
	peers: Arc<Mutex<Vec<IpAddr>>>,
}

impl Host for FakeHost {
	fn nft_apply(&self, script: &str) -> Result<(), String> {
		self.state.lock().unwrap().applied.push(script.to_owned());
		Ok(())
	}
	fn ssh_peers(&self, _: u16) -> Vec<IpAddr> {
		self.peers.lock().unwrap().clone()
	}
	fn load(&self) -> Option<Active> {
		self.state.lock().unwrap().saved.clone()
	}
	fn save(&self, active: Option<&Active>) -> Result<(), String> {
		self.state.lock().unwrap().saved = active.cloned();
		Ok(())
	}
}

struct Server {
	engine: Arc<Mutex<Engine<FakeHost>>>,
	host: FakeHost,
}

#[derive(Clone)]
struct Fake {
	engine: Arc<Mutex<Engine<FakeHost>>>,
	/// While set, the agent answers its health check; applying may clear it.
	reachable: Arc<AtomicBool>,
	/// Answer every firewall request as a server without the helper.
	absent: bool,
	/// Cut podkit off as soon as a ruleset is applied.
	cut_after_apply: bool,
}

async fn firewall(State(f): State<Fake>, body: Bytes) -> axum::response::Response {
	let ctype = [(header::CONTENT_TYPE, "application/json")];
	if f.absent {
		return (ctype, r#"{"kind":"not_installed"}"#).into_response();
	}
	let request: Request = serde_json::from_slice(&body).unwrap();
	let applies = matches!(request, Request::Apply { .. });
	let answer = f.engine.lock().unwrap().handle(request, Instant::now());
	if applies && f.cut_after_apply {
		f.reachable.store(false, Ordering::SeqCst);
	}
	(ctype, serde_json::to_string(&answer).unwrap()).into_response()
}

async fn health(State(f): State<Fake>) -> axum::response::Response {
	if f.reachable.load(Ordering::SeqCst) {
		axum::Json(json!({ "version": "0", "podman_version": "5" })).into_response()
	} else {
		StatusCode::SERVICE_UNAVAILABLE.into_response()
	}
}

/// A fake agent on the local server's socket in front of the helper's engine.
fn serve(state: &AppState, absent: bool, cut_after_apply: bool) -> Server {
	let host = FakeHost::default();
	host.peers
		.lock()
		.unwrap()
		.push("198.51.100.4".parse().unwrap());
	let engine = Arc::new(Mutex::new(Engine::new(host.clone())));
	let fake = Fake {
		engine: engine.clone(),
		reachable: Arc::new(AtomicBool::new(true)),
		absent,
		cut_after_apply,
	};
	let app = Router::new()
		.route("/v1/firewall", post(firewall))
		.route("/v1/health", get(health))
		.with_state(fake);
	let listener = tokio::net::UnixListener::bind(&state.config.local_agent_socket).unwrap();
	tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
	Server { engine, host }
}

struct Setup {
	app: Router,
	state: AppState,
	db: PgPool,
	cookie: String,
	team: i64,
	server: i64,
	user: i64,
}

impl Setup {
	async fn new(db: PgPool) -> Self {
		let state = state(db.clone());
		let app = crate::routes::routes(state.clone()).await.unwrap();
		let (cookie, team) = ui_session(&app, "alice").await;
		let server = local_server_id(&db, team).await;
		let user = user_id(&db, "alice").await;
		Self {
			app,
			state,
			db,
			cookie,
			team,
			server,
			user,
		}
	}

	fn ids(&self) -> (UserId, TeamId, ServerId) {
		(UserId(self.user), TeamId(self.team), ServerId(self.server))
	}

	async fn add(&self, input: &RuleInput) -> Result<FirewallRuleId, ServerError> {
		let (u, t, s) = self.ids();
		service::add(&self.state, u, t, s, input).await
	}

	fn url(&self, tail: &str) -> String {
		format!(
			"/teams/{}/servers/{}/firewall{tail}",
			self.team, self.server
		)
	}

	async fn audited(&self, action: &str) -> i64 {
		sqlx::query_scalar("SELECT COUNT(*) FROM audit_events WHERE action = $1")
			.bind(action)
			.fetch_one(&self.db)
			.await
			.unwrap()
	}
}

fn applied(host: &FakeHost) -> Vec<String> {
	host.state.lock().unwrap().applied.clone()
}

// --- apply with confirm -------------------------------------------------------------------

#[sqlx::test]
async fn firewall_an_apply_that_podkit_survives_is_confirmed_and_kept(db: PgPool) {
	let s = Setup::new(db).await;
	let server = serve(&s.state, false, false);
	s.add(&input("tcp", "5432", "10.0.0.0/24", "db"))
		.await
		.unwrap();
	let (u, t, id) = s.ids();
	service::save_ssh_sources(&s.state, u, t, id, "203.0.113.0/24")
		.await
		.unwrap();

	let preview = service::preview(&s.state, u, t, id).await.unwrap();
	assert!(preview.script.contains("5432"), "{}", preview.script);
	assert!(preview.diff.iter().any(|l| l.op == '+'));
	assert_eq!(preview.keep_sources, ["198.51.100.4"]);
	// Previewing changes nothing on the server.
	assert!(applied(&server.host).is_empty());
	assert!(!settings(&s.db, id).await.unwrap().enabled);

	service::apply(&s.state, u, t, id).await.unwrap();
	let script = applied(&server.host).pop().unwrap();
	assert!(script.contains("inet podkit"), "{script}");
	// SSH is limited to the range, and the session open at the time is let in as well.
	assert!(script.contains("203.0.113.0/24"), "{script}");
	assert!(script.contains("198.51.100.4"), "{script}");
	assert!(settings(&s.db, id).await.unwrap().enabled);
	assert_eq!(s.audited("firewall.apply").await, 1);

	// Confirmed: the helper keeps it, with nothing waiting.
	let view = service::get(&s.state, u, t, id).await.unwrap();
	match view.helper {
		Helper::Ready(status) => {
			assert!(status.enabled);
			assert!(status.pending.is_none());
		}
		other => panic!("{other:?}"),
	}
	// So the timer finds nothing to revert.
	assert!(
		!server
			.engine
			.lock()
			.unwrap()
			.tick(Instant::now() + Duration::from_secs(120))
	);

	// Turning it off removes the table and is audited.
	service::disable(&s.state, u, t, id).await.unwrap();
	assert!(
		applied(&server.host)
			.pop()
			.unwrap()
			.contains("delete table inet podkit")
	);
	assert!(!settings(&s.db, id).await.unwrap().enabled);
	assert_eq!(s.audited("firewall.disable").await, 1);
}

#[sqlx::test]
async fn firewall_an_apply_that_cuts_podkit_off_is_reverted_and_the_team_hears_of_it(db: PgPool) {
	let s = Setup::new(db).await;
	let server = serve(&s.state, false, true);
	s.add(&input("tcp", "5432", "", "")).await.unwrap();
	let (u, t, id) = s.ids();

	let err = service::apply(&s.state, u, t, id).await.unwrap_err();
	assert!(
		matches!(&err, ServerError::Validation(m) if m.contains("reverted")),
		"{err:?}"
	);
	// The helper took the unconfirmed change back at once: no table, nothing pending.
	assert!(
		applied(&server.host)
			.pop()
			.unwrap()
			.contains("delete table inet podkit")
	);
	let status = server
		.engine
		.lock()
		.unwrap()
		.handle(Request::Status, Instant::now());
	assert!(
		matches!(&status, Response::Status(st) if !st.enabled && st.pending.is_none()),
		"{status:?}"
	);
	assert!(!settings(&s.db, id).await.unwrap().enabled);
	assert_eq!(s.audited("firewall.revert").await, 1);
	assert_eq!(s.audited("firewall.apply").await, 0);
	let events: i64 = sqlx::query_scalar(
		"SELECT COUNT(*) FROM notification_events WHERE kind = 'firewall_reverted'",
	)
	.fetch_one(&s.db)
	.await
	.unwrap();
	assert_eq!(events, 1);
}

#[sqlx::test]
async fn firewall_an_unconfirmed_change_is_reverted_by_the_helpers_timer(db: PgPool) {
	let s = Setup::new(db).await;
	let server = serve(&s.state, false, false);
	let (u, t, id) = s.ids();
	// A change applied and left alone, as when podkit dies mid-apply.
	let found = crate::scope::server(&s.db, t, id).await.unwrap();
	let client = s.state.agents.client(&s.state, &found).await.unwrap();
	client
		.firewall(&Request::Apply {
			policy: podkit_firewall::policy::Policy::default(),
			confirm_secs: Some(60),
		})
		.await
		.unwrap();
	let view = service::get(&s.state, u, t, id).await.unwrap();
	assert!(matches!(&view.helper, Helper::Ready(st) if st.pending.is_some()));
	assert!(
		server
			.engine
			.lock()
			.unwrap()
			.tick(Instant::now() + Duration::from_secs(61))
	);
	let view = service::get(&s.state, u, t, id).await.unwrap();
	assert!(matches!(&view.helper, Helper::Ready(st) if !st.enabled && st.pending.is_none()));
}

#[sqlx::test]
async fn firewall_a_server_without_the_helper_is_shown_as_such_and_refuses_changes(db: PgPool) {
	let s = Setup::new(db).await;
	serve(&s.state, true, false);
	let (u, t, id) = s.ids();
	let view = service::get(&s.state, u, t, id).await.unwrap();
	assert!(matches!(view.helper, Helper::NotInstalled));
	for result in [
		service::apply(&s.state, u, t, id).await.map(|_| ()),
		service::preview(&s.state, u, t, id).await.map(|_| ()),
		service::disable(&s.state, u, t, id).await,
	] {
		assert!(
			matches!(&result, Err(ServerError::Validation(m)) if m.contains("not installed")),
			"{result:?}"
		);
	}
	assert!(!settings(&s.db, id).await.unwrap().enabled);

	let (status, _, html) = get_html(&s.app, &s.url(""), Some(&s.cookie)).await;
	assert_eq!(status, StatusCode::OK);
	assert_contains(
		&html,
		"The firewall helper is not installed on this server.",
	);
	assert_contains(&html, "docs/firewall.md");
	// The local server's helper is installed by hand: no install button, and the action refuses.
	assert_not_contains(&html, "Install the helper");
	assert!(service::install_helper(&s.state, u, t, id).await.is_err());
}

#[sqlx::test]
async fn firewall_the_page_shows_the_rules_the_diff_and_escapes_what_was_typed(db: PgPool) {
	let s = Setup::new(db).await;
	serve(&s.state, false, false);
	s.add(&input(
		"tcp",
		"5432",
		"10.0.0.0/24",
		"<script>alert(1)</script>",
	))
	.await
	.unwrap();
	let (status, _, html) = get_html(&s.app, &s.url(""), Some(&s.cookie)).await;
	assert_eq!(status, StatusCode::OK);
	assert_contains(&html, "Always open");
	assert_contains(&html, "10.0.0.0/24");
	assert_contains(&html, "&lt;script&gt;alert(1)&lt;/script&gt;");
	assert_not_contains(&html, "<script>alert(1)");
	assert_eq!(html.matches("<h1").count(), 1);

	let (status, _, body) =
		post_signals(&s.app, &s.url("/preview"), Some(&s.cookie), &json!({})).await;
	assert_eq!(status, StatusCode::OK);
	assert_contains(&body, "5432");
}

#[sqlx::test]
async fn firewall_adding_removing_and_limiting_ssh_are_audited(db: PgPool) {
	let s = Setup::new(db).await;
	serve(&s.state, false, false);
	let (status, _, _) = post_signals(
		&s.app,
		&s.url("/allow"),
		Some(&s.cookie),
		&json!({ "fw_proto": "udp", "fw_port": "51820", "fw_source": "", "fw_note": "vpn" }),
	)
	.await;
	assert_eq!(status, StatusCode::OK);
	let stored = rules(&s.db, ServerId(s.server)).await.unwrap();
	assert_eq!(stored.len(), 1);
	assert_eq!(stored[0].port, 51820);
	assert_eq!(s.audited("firewall.allow.add").await, 1);

	// The same allow twice is refused, and says so.
	let (_, _, body) = post_signals(
		&s.app,
		&s.url("/allow"),
		Some(&s.cookie),
		&json!({ "fw_proto": "udp", "fw_port": "51820", "fw_source": "", "fw_note": "" }),
	)
	.await;
	assert_contains(&body, "already in the list");
	assert_eq!(rules(&s.db, ServerId(s.server)).await.unwrap().len(), 1);

	post_signals(
		&s.app,
		&s.url("/ssh"),
		Some(&s.cookie),
		&json!({ "fw_ssh": "203.0.113.0/24" }),
	)
	.await;
	assert_eq!(
		settings(&s.db, ServerId(s.server))
			.await
			.unwrap()
			.ssh_sources,
		["203.0.113.0/24"]
	);
	assert_eq!(s.audited("firewall.ssh.set").await, 1);

	let id = stored[0].id;
	let (status, _, _) = post_signals(
		&s.app,
		&s.url(&format!("/allow/{id}/delete")),
		Some(&s.cookie),
		&json!({}),
	)
	.await;
	assert_eq!(status, StatusCode::OK);
	assert!(rules(&s.db, ServerId(s.server)).await.unwrap().is_empty());
	assert_eq!(s.audited("firewall.allow.remove").await, 1);
}

// --- who may ------------------------------------------------------------------------------

#[sqlx::test]
async fn firewall_a_member_without_update_server_cannot_change_it_even_with_a_forged_post(
	db: PgPool,
) {
	let s = Setup::new(db).await;
	serve(&s.state, false, false);
	let role = member_role(&s.db, s.team, "alice").await;
	revoke_permission(&s.db, role, "update", "server").await;

	let (status, _, html) = get_html(&s.app, &s.url(""), Some(&s.cookie)).await;
	assert_eq!(status, StatusCode::OK);
	assert_not_contains(&html, "Add allow");
	assert_not_contains(&html, "Preview changes");

	let forged =
		json!({ "fw_proto": "tcp", "fw_port": "22", "fw_source": "", "fw_note": "", "fw_ssh": "" });
	for tail in ["/allow", "/ssh", "/preview", "/apply", "/off", "/install"] {
		post_signals(&s.app, &s.url(tail), Some(&s.cookie), &forged).await;
	}
	assert_eq!(count(&s.db, "server_firewall_rules").await, 0);
	assert_eq!(count(&s.db, "server_firewalls").await, 0);
	assert_eq!(s.audited("firewall.allow.add").await, 0);
	assert_eq!(s.audited("firewall.apply").await, 0);
	let (u, t, id) = s.ids();
	assert!(service::apply(&s.state, u, t, id).await.is_err());
}

#[sqlx::test]
async fn firewall_another_teams_server_is_not_found(db: PgPool) {
	let s = Setup::new(db).await;
	serve(&s.state, false, false);
	let (bob_cookie, bob_team) = ui_session(&s.app, "bob").await;
	let bob = user_id(&s.db, "bob").await;
	// Bob, with his own team in the path and alice's server id.
	let url = format!("/teams/{bob_team}/servers/{}/firewall", s.server);
	let (status, _, _) = get_html(&s.app, &url, Some(&bob_cookie)).await;
	assert_eq!(status, StatusCode::NOT_FOUND);
	let (u, t, id) = (UserId(bob), TeamId(bob_team), ServerId(s.server));
	assert!(matches!(
		service::add(&s.state, u, t, id, &input("tcp", "80", "", "")).await,
		Err(ServerError::ServerNotFound)
	));
	assert!(matches!(
		service::apply(&s.state, u, t, id).await,
		Err(ServerError::ServerNotFound)
	));
	// Alice cannot reach it through bob's team either.
	let (status, _, _) = get_html(&s.app, &url, Some(&s.cookie)).await;
	assert_ne!(status, StatusCode::OK);
	assert_eq!(count(&s.db, "server_firewall_rules").await, 0);
}
