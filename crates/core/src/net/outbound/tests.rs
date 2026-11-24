//! The instance's outbound policy: the defaults and the setting, the notification channels and
//! the call-site guard. The mechanism itself (every blocked range, IPv4-in-IPv6, rebinding, a
//! redirect) is tested in `crates/outbound`; what is here is what podkit does with it.

use std::collections::HashMap;
use std::future::Future;
use std::io;
use std::net::IpAddr;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::http::{HeaderMap, StatusCode, header};
use axum::routing::any;
use outbound::Lookup;
use tokio::net::TcpListener;

use super::{Source, default_private, resolve, set_private, status};
use crate::error::ServerError;
use crate::ids::{TeamId, UserId};
use crate::net::outbound::Outbound;
use crate::notifications::channel::{ChannelFields, ChannelKind};
use crate::notifications::service as notifications;
use crate::state::{AppState, Registration};
use crate::tests::{PgPool, db_test, state, user_id, user_with_team};

/// Answers names from a table; the second and later answers of a name may differ.
struct Names(Mutex<HashMap<String, Vec<Vec<IpAddr>>>>);

impl Names {
	fn new(table: &[(&str, &[&[&str]])]) -> Arc<Self> {
		Arc::new(Self(Mutex::new(
			table
				.iter()
				.map(|(name, answers)| {
					(
						(*name).to_owned(),
						answers
							.iter()
							.map(|a| a.iter().map(|ip| ip.parse().unwrap()).collect())
							.collect(),
					)
				})
				.collect(),
		)))
	}
}

impl Lookup for Names {
	fn lookup<'a>(
		&'a self,
		host: &'a str,
	) -> Pin<Box<dyn Future<Output = io::Result<Vec<IpAddr>>> + Send + 'a>> {
		let mut table = self.0.lock().unwrap();
		let answer = table.get_mut(host).and_then(|answers| {
			if answers.len() > 1 {
				Some(answers.remove(0))
			} else {
				answers.first().cloned()
			}
		});
		Box::pin(async move { answer.ok_or_else(|| io::Error::other("no such name")) })
	}
}

fn strict(allow_private: bool, names: Arc<Names>) -> Outbound {
	Outbound::with_lookup(allow_private, Vec::new(), names).unwrap()
}

struct World {
	st: AppState,
	admin: UserId,
	other: UserId,
	team: TeamId,
}

/// An instance with an administrator (the first account) and a second member of a team.
async fn world(db: PgPool) -> World {
	let st = state(db.clone());
	let app = crate::routes::routes(st.clone()).await.unwrap();
	let (_, team) = user_with_team(&app, "admin").await;
	let _ = user_with_team(&app, "other").await;
	World {
		admin: UserId(user_id(&db, "admin").await),
		other: UserId(user_id(&db, "other").await),
		team: TeamId(team),
		st,
	}
}

fn hook(url: &str) -> ChannelFields {
	ChannelFields {
		url: url.to_owned(),
		signing_secret: "signing-secret-1".to_owned(),
		..ChannelFields::default()
	}
}

async fn make(w: &World, url: &str) -> Result<crate::notifications::channel::Channel, ServerError> {
	static N: AtomicUsize = AtomicUsize::new(0);
	notifications::create_channel(
		&w.st,
		w.admin,
		w.team,
		&format!("c-{}", N.fetch_add(1, Ordering::SeqCst)),
		ChannelKind::Webhook,
		&hook(url),
	)
	.await
}

#[derive(Clone)]
struct Seen(Arc<AtomicUsize>);

async fn stub(status: StatusCode, body: &'static str, location: Option<String>) -> (String, Seen) {
	let seen = Seen(Arc::new(AtomicUsize::new(0)));
	let counter = seen.clone();
	let handler = move |_h: HeaderMap| {
		let location = location.clone();
		let counter = counter.clone();
		async move {
			counter.0.fetch_add(1, Ordering::SeqCst);
			let mut headers = HeaderMap::new();
			if let Some(l) = location {
				headers.insert(header::LOCATION, l.parse().unwrap());
			}
			(status, headers, body)
		}
	};
	let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
	let url = format!("http://{}", listener.local_addr().unwrap());
	tokio::spawn(async move {
		axum::serve(listener, Router::new().fallback(any(handler)))
			.await
			.unwrap();
	});
	(url, seen)
}

#[test]
fn outbound_private_defaults_to_allow_for_a_homelab_and_deny_for_a_hosted_instance() {
	assert!(default_private(Registration::FirstUser));
	assert!(default_private(Registration::Closed));
	assert!(!default_private(Registration::Open));
}

#[test]
fn outbound_private_setting_parses_allow_and_deny_only() {
	assert_eq!(super::parse("allow"), Ok(true));
	assert_eq!(super::parse(" Deny "), Ok(false));
	assert!(super::parse("maybe").is_err());
	assert!(super::parse("").is_err());
}

#[db_test]
async fn outbound_private_is_decided_by_the_env_then_the_setting_then_the_default(db: PgPool) {
	for (registration, expected) in [
		(Registration::FirstUser, true),
		(Registration::Open, false),
		(Registration::Closed, true),
	] {
		let s = resolve(&db, registration, None).await.unwrap();
		assert_eq!((s.allow_private, s.source), (expected, Source::Default));
	}
	// The variable beats everything.
	let s = resolve(&db, Registration::Open, Some(true)).await.unwrap();
	assert_eq!((s.allow_private, s.source), (true, Source::Env));
	// A stored setting beats the default.
	sqlx::query("INSERT INTO instance_settings (key, value) VALUES ('outbound_private', 'deny')")
		.execute(&db)
		.await
		.unwrap();
	let s = resolve(&db, Registration::FirstUser, None).await.unwrap();
	assert_eq!((s.allow_private, s.source), (false, Source::Setting));
	let s = resolve(&db, Registration::FirstUser, Some(true))
		.await
		.unwrap();
	assert_eq!((s.allow_private, s.source), (true, Source::Env));
}

#[db_test]
async fn outbound_private_toggle_is_the_administrators_and_applies_at_once(db: PgPool) {
	let w = world(db.clone()).await;
	// Tests run as an open instance: private is off.
	assert!(!w.st.outbound.allow_private());
	assert!(!status(&w.st).await.unwrap().allow_private);

	let refused = set_private(&w.st, w.other, w.team, true).await;
	assert!(
		matches!(refused, Err(ServerError::Forbidden)),
		"{refused:?}"
	);
	assert!(!w.st.outbound.allow_private());

	set_private(&w.st, w.admin, w.team, true).await.unwrap();
	assert!(w.st.outbound.allow_private());
	let s = status(&w.st).await.unwrap();
	assert_eq!((s.allow_private, s.source), (true, Source::Setting));

	// It is kept: a restart reads it back.
	let again = resolve(&db, Registration::Open, None).await.unwrap();
	assert_eq!((again.allow_private, again.source), (true, Source::Setting));

	let audited: i64 =
		sqlx::query_scalar("SELECT COUNT(*) FROM audit_events WHERE action = 'outbound.update'")
			.fetch_one(&db)
			.await
			.unwrap();
	assert_eq!(audited, 1, "a change is in the audit log");

	set_private(&w.st, w.admin, w.team, false).await.unwrap();
	assert!(!w.st.outbound.allow_private());
}

#[db_test]
async fn outbound_private_toggle_does_nothing_while_the_variable_pins_it(db: PgPool) {
	let mut w = world(db).await;
	let mut config = (*w.st.config).clone();
	config.outbound_private = Some(false);
	w.st.config = Arc::new(config);
	let err = set_private(&w.st, w.admin, w.team, true).await.unwrap_err();
	assert!(matches!(err, ServerError::Validation(m) if m.contains("OUTBOUND_PRIVATE")));
	assert!(!w.st.outbound.allow_private());
}

#[db_test]
async fn outbound_a_channel_cannot_point_at_a_blocked_address_and_private_follows_the_setting(
	db: PgPool,
) {
	let mut w = world(db).await;
	w.st.outbound = strict(
		false,
		Names::new(&[
			("hook.example.com", &[&["93.184.216.34"]]),
			("lan.example.com", &[&["192.168.1.20"]]),
			("sneaky.example.com", &[&["127.0.0.1"]]),
			("meta.example.com", &[&["::ffff:169.254.169.254"]]),
		]),
	);
	for blocked in [
		"http://127.0.0.1:2375/containers/prune",
		"http://[::1]:8080/",
		"http://[::ffff:127.0.0.1]/",
		"http://169.254.169.254/latest/meta-data/",
		"http://[fe80::1]/",
		"http://0.0.0.0/",
		"http://224.0.0.1/",
		"http://192.0.2.1/",
		"http://sneaky.example.com/",
		"http://meta.example.com/",
		"http://nothing-resolves.example.com/",
	] {
		let err = make(&w, blocked).await.unwrap_err();
		assert!(
			matches!(&err, ServerError::Validation(m) if m.contains("does not send requests")),
			"{blocked}: {err:?}"
		);
	}
	assert!(make(&w, "ftp://hook.example.com/").await.is_err());
	// Private addresses: refused while the setting is off ...
	for private in [
		"http://10.0.0.5/admin",
		"http://192.168.1.20:8081/",
		"http://[fd12::5]/",
		"http://100.64.0.9/",
		"http://lan.example.com/",
	] {
		assert!(make(&w, private).await.is_err(), "{private}");
	}
	// ... and accepted once an administrator allows them.
	w.st.outbound.set_allow_private(true);
	for private in [
		"http://10.0.0.5/admin",
		"http://192.168.1.20:8081/",
		"http://[fd12::5]/",
		"http://100.64.0.9/",
		"http://lan.example.com/",
	] {
		assert!(make(&w, private).await.is_ok(), "{private}");
	}
	// Never the always-blocked ones, whatever the setting.
	assert!(make(&w, "http://127.0.0.1:2375/").await.is_err());
	assert!(make(&w, "http://169.254.169.254/").await.is_err());
	assert!(make(&w, "http://hook.example.com/hook").await.is_ok());
}

#[db_test]
async fn outbound_a_name_that_changes_its_answer_is_refused_when_it_is_used(db: PgPool) {
	let mut w = world(db).await;
	// Public when the channel is made, the metadata address afterwards.
	w.st.outbound = strict(
		true,
		Names::new(&[(
			"shifty.example.com",
			&[&["93.184.216.34"], &["169.254.169.254"]],
		)]),
	);
	let channel = make(&w, "http://shifty.example.com/hook").await.unwrap();
	let outcome = notifications::test_channel(&w.st, w.admin, w.team, channel.id)
		.await
		.unwrap();
	assert!(!outcome.ok);
	assert_eq!(outcome.message, "cannot connect");
}

#[db_test]
async fn outbound_the_test_button_shows_a_status_and_never_the_answer(db: PgPool) {
	let w = world(db).await;
	let (url, _) = stub(StatusCode::FORBIDDEN, "INTERNAL-ONLY-SECRET-BODY", None).await;
	let channel = make(&w, &url).await.unwrap();
	let outcome = notifications::test_channel(&w.st, w.admin, w.team, channel.id)
		.await
		.unwrap();
	assert!(!outcome.ok);
	assert_eq!(outcome.message, "HTTP 403");
	// And it is not kept in the channel's record either.
	let kept: Option<String> =
		sqlx::query_scalar("SELECT last_test_error FROM notification_channels WHERE id = $1")
			.bind(channel.id)
			.fetch_one(&w.st.db)
			.await
			.unwrap();
	assert_eq!(kept.as_deref(), Some("HTTP 403"));
}

#[db_test]
async fn outbound_a_failed_connection_says_only_that_it_failed(db: PgPool) {
	let w = world(db).await;
	let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
	let port = listener.local_addr().unwrap().port();
	drop(listener);
	let channel = make(&w, &format!("http://127.0.0.1:{port}/secret-path"))
		.await
		.unwrap();
	let outcome = notifications::test_channel(&w.st, w.admin, w.team, channel.id)
		.await
		.unwrap();
	assert_eq!(outcome.message, "cannot connect");
}

#[db_test]
async fn outbound_a_redirect_is_not_followed_by_a_notification(db: PgPool) {
	let w = world(db).await;
	let (target, reached) = stub(StatusCode::OK, "ok", None).await;
	let (front, _) = stub(StatusCode::FOUND, "", Some(format!("{target}/inner"))).await;
	let channel = make(&w, &front).await.unwrap();
	let outcome = notifications::test_channel(&w.st, w.admin, w.team, channel.id)
		.await
		.unwrap();
	assert_eq!(outcome.message, "HTTP 302");
	assert_eq!(reached.0.load(Ordering::SeqCst), 0);
}

#[db_test]
async fn outbound_smtp_host_goes_through_the_policy(db: PgPool) {
	let mut w = world(db).await;
	w.st.outbound = strict(
		false,
		Names::new(&[("mail.example.com", &[&["10.0.0.25"]])]),
	);
	let save = |host: &str| {
		let w = &w;
		let input = notifications::SmtpInput {
			host: host.to_owned(),
			port: 25,
			security: "none".to_owned(),
			from_addr: "podkit@example.com".to_owned(),
			..notifications::SmtpInput::default()
		};
		async move { notifications::save_smtp(&w.st, w.admin, w.team, input).await }
	};
	for host in ["127.0.0.1", "169.254.169.254", "[::1]", "mail.example.com"] {
		assert!(save(host).await.is_err(), "{host}");
	}
	w.st.outbound.set_allow_private(true);
	assert!(save("mail.example.com").await.is_ok());
	assert!(save("127.0.0.1").await.is_err());
}

// ---- every user-URL call site uses the policy ---------------------------------------------

fn sources(dir: &std::path::Path, out: &mut Vec<(String, String)>) {
	for entry in std::fs::read_dir(dir).unwrap().flatten() {
		let path = entry.path();
		if path.is_dir() {
			sources(&path, out);
		} else if path.extension().is_some_and(|e| e == "rs") {
			out.push((
				path.to_string_lossy().into_owned(),
				std::fs::read_to_string(&path).unwrap(),
			));
		}
	}
}

/// Not production code: tests, stubs, live tests.
fn is_test_file(path: &str) -> bool {
	let name = path.rsplit('/').next().unwrap_or_default();
	name == "tests.rs"
		|| name.ends_with("_tests.rs")
		|| name == "live.rs"
		|| path.contains("/tests/")
}

/// Code before the first `#[cfg(test)]`: what runs in production.
fn production(text: &str) -> &str {
	text.split("#[cfg(test)]").next().unwrap_or(text)
}

/// Whether `needle` is in `code` and not the end of a longer name (`stats.http` is not `s.http`).
fn has_token(code: &str, needle: &str) -> bool {
	code.match_indices(needle).any(|(at, _)| {
		code[..at]
			.chars()
			.next_back()
			.is_none_or(|c| !(c.is_alphanumeric() || c == '_' || c == '.'))
	})
}

/// The control plane has one plain HTTP client, `state.http`, for the operator's own endpoints
/// (the Hugging Face hub from `HF_ENDPOINT`). A request to an address a person typed must use
/// `state.outbound`: so no other file may build a `reqwest::Client` or reach for `state.http`.
/// A new call site that needs the plain client has to be added here on purpose, with the reason.
#[test]
fn outbound_no_file_but_the_operators_own_endpoints_uses_the_plain_http_client() {
	// file suffix -> why it may.
	const ALLOWED: &[(&str, &str)] = &[
		("src/main.rs", "builds the clients"),
		("src/state.rs", "declares the field"),
		("src/catalog/search.rs", "HF_ENDPOINT, the operator's hub"),
		(
			"src/model_files/service.rs",
			"HF_ENDPOINT, the operator's hub",
		),
		("src/model_files/hf.rs", "HF_ENDPOINT, the operator's hub"),
	];
	// Every layer of the control plane: this crate, and the web UI and `main.rs` above it.
	let mut files = Vec::new();
	for root in ["src", "../ui/src", "../../server/src"] {
		sources(
			&std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(root),
			&mut files,
		);
	}
	assert!(files.len() > 100, "the walk found {} files", files.len());
	let mut offenders = Vec::new();
	for (path, text) in &files {
		if is_test_file(path) || ALLOWED.iter().any(|(suffix, _)| path.ends_with(suffix)) {
			continue;
		}
		for (n, line) in production(text).lines().enumerate() {
			let code = line.split("//").next().unwrap_or_default();
			let plain_state_client = has_token(code, "s.http") || has_token(code, "state.http");
			let builds_a_client = code.contains("reqwest::Client::new")
				|| code.contains("reqwest::Client::builder")
				|| code.contains("reqwest::get(")
				|| code.contains("TcpStream::connect(");
			if plain_state_client || builds_a_client {
				offenders.push(format!("{path}:{}: {}", n + 1, line.trim()));
			}
		}
	}
	assert!(
		offenders.is_empty(),
		"requests to an address a person typed go through `state.outbound` (net::outbound):\n{}",
		offenders.join("\n")
	);
}

/// The same for the agent: its log export is the one request it makes to an address from a
/// team's settings, and it goes through the policy.
#[test]
fn outbound_the_agents_log_export_uses_the_policy() {
	let root =
		std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../agent/src/logstore/export.rs");
	let text = std::fs::read_to_string(root).unwrap();
	let code = production(&text);
	assert!(
		code.contains("Outbound"),
		"the export does not use the policy"
	);
	assert!(
		!code.contains("reqwest::Client"),
		"the export builds its own client"
	);
}
