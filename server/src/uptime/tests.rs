//! Uptime: probes against stub servers, flap protection, rollups, notifications, the public
//! status page and tenancy. `cargo test -p server --all-features -- uptime`
//! The live test (a real whoami app) is `#[ignore]`: `-- --ignored uptime_live`.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::get;
use serde_json::json;
use sqlx::PgPool;
use time::macros::datetime;
use time::{Duration as Span, OffsetDateTime};
use tokio::net::TcpListener;

use super::probe::{self, HttpSpec, Outcome};
use super::service::{self, CheckInput};
use super::{
	DOWN_AFTER, State as CheckState, Step, UP_AFTER, advance, day_stats, maintain, prune, rollup,
	runner,
};
use crate::ids::{TeamId, UptimeCheckId, UserId};
use crate::net::outbound::Outbound;
use crate::notifications::channel::{ChannelFields, ChannelKind};
use crate::notifications::event::{EventKind, Severity};
use crate::notifications::service::{self as notify, RuleInput};
use crate::state::AppState;
use crate::tests::{call, count, state, user_id, user_with_team};

// --- stubs --------------------------------------------------------------------------------------

/// A web server on a free port. `/toggle` answers 200 while `up` is set and 503 after.
struct Web {
	base: String,
	up: Arc<AtomicBool>,
}

async fn web() -> Web {
	let up = Arc::new(AtomicBool::new(true));
	let app = Router::new()
		.route("/ok", get(|| async { "hello from the podkit stub" }))
		.route(
			"/broken",
			get(|| async { (StatusCode::SERVICE_UNAVAILABLE, "maintenance") }),
		)
		.route("/missing", get(|| async { StatusCode::NOT_FOUND }))
		.route(
			"/to-ok",
			get(|| async { axum::response::Redirect::temporary("/ok") }),
		)
		.route(
			"/to-metadata",
			get(|| async {
				axum::response::Redirect::temporary("http://169.254.169.254/latest/meta-data/")
			}),
		)
		.route(
			"/loop",
			get(|| async { axum::response::Redirect::temporary("/loop") }),
		)
		.route(
			"/slow",
			get(|| async {
				tokio::time::sleep(Duration::from_secs(3)).await;
				"late"
			}),
		)
		.route(
			"/toggle",
			get(|State(up): State<Arc<AtomicBool>>| async move {
				if up.load(Ordering::SeqCst) {
					(StatusCode::OK, "alive")
				} else {
					(StatusCode::SERVICE_UNAVAILABLE, "down")
				}
			}),
		)
		.with_state(up.clone());
	let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
	let base = format!("http://{}", listener.local_addr().unwrap());
	tokio::spawn(async move {
		axum::serve(listener, app).await.unwrap();
	});
	Web { base, up }
}

fn install_crypto() {
	let _ = rustls::crypto::ring::default_provider().install_default();
}

fn client() -> Outbound {
	install_crypto();
	let out = Outbound::new(false, Vec::new()).unwrap();
	out.loopback_ok(true);
	out
}

fn spec(url: String) -> HttpSpec {
	HttpSpec {
		url,
		head: false,
		expected_status: 200,
		keyword: None,
		timeout: Duration::from_secs(5),
	}
}

// --- a world with a team and a rule that wants every uptime event ------------------------------

struct World {
	s: AppState,
	app: Router,
	token: String,
	team: TeamId,
	user: UserId,
}

async fn world(db: PgPool) -> World {
	install_crypto();
	let s = state(db.clone());
	let app = crate::routes::routes(s.clone()).await.unwrap();
	let (token, team) = user_with_team(&app, "alice").await;
	let user = UserId(user_id(&db, "alice").await);
	World {
		s,
		app,
		token,
		team: TeamId(team),
		user,
	}
}

impl World {
	fn db(&self) -> &PgPool {
		&self.s.db
	}

	/// A rule that sends the uptime events to a webhook nobody listens on: the event is stored
	/// either way, the rule is what makes the (info) recovery worth keeping.
	async fn want_uptime_events(&self) {
		let channel = notify::create_channel(
			&self.s,
			self.user,
			self.team,
			"hook",
			ChannelKind::Webhook,
			&ChannelFields {
				url: "http://127.0.0.1:9/hook".to_owned(),
				signing_secret: "s3cret-value".to_owned(),
				..ChannelFields::default()
			},
		)
		.await
		.unwrap();
		notify::create_rule(
			self.db(),
			self.user,
			self.team,
			RuleInput {
				name: "uptime".to_owned(),
				kinds: [
					EventKind::UptimeDown,
					EventKind::UptimeUp,
					EventKind::CertExpiring,
				]
				.iter()
				.map(|k| k.as_str().to_owned())
				.collect(),
				channel_ids: vec![channel.id.0],
				project_id: None,
				environment_id: None,
				min_severity: Severity::Info,
				enabled: true,
			},
		)
		.await
		.unwrap();
	}

	async fn check(&self, name: &str, url: &str) -> super::Check {
		service::create(&self.s, self.user, self.team, &http_input(name, url))
			.await
			.unwrap()
	}

	async fn events(&self, kind: &str) -> i64 {
		sqlx::query_scalar("SELECT COUNT(*) FROM notification_events WHERE kind = $1")
			.bind(kind)
			.fetch_one(self.db())
			.await
			.unwrap()
	}

	/// Makes every check due and runs one pass.
	async fn pass(&self) -> usize {
		sqlx::query("UPDATE uptime_checks SET next_check_at = NOW() - INTERVAL '1 second'")
			.execute(self.db())
			.await
			.unwrap();
		runner::tick(&self.s, OffsetDateTime::now_utc())
			.await
			.unwrap()
	}

	async fn reload(&self, id: UptimeCheckId) -> super::Check {
		service::check(self.db(), self.user, self.team, id)
			.await
			.unwrap()
	}
}

fn http_input(name: &str, url: &str) -> CheckInput {
	CheckInput {
		name: name.to_owned(),
		kind: "http".to_owned(),
		url: url.to_owned(),
		method: "GET".to_owned(),
		expected_status: 200,
		keyword: String::new(),
		host: String::new(),
		port: 0,
		timeout_secs: 2,
		interval_secs: 30,
		tls_warn_days: None,
		enabled: true,
		status_visible: false,
		status_name: String::new(),
	}
}

// --- flap protection --------------------------------------------------------------------------

/// Feeds `runs` through [`advance`] and returns the state and every step that was not `Nothing`.
fn feed(start: CheckState, runs: &[bool]) -> (CheckState, Vec<Step>) {
	let (mut state, mut fails, mut oks) = (start, 0, 0);
	let mut steps = Vec::new();
	for ok in runs {
		let (s, f, o, step) = advance(state, fails, oks, *ok);
		(state, fails, oks) = (s, f, o);
		if step != Step::Nothing {
			steps.push(step);
		}
	}
	(state, steps)
}

#[test]
fn uptime_one_bad_answer_does_not_make_a_check_down() {
	let (state, steps) = feed(
		CheckState::Up,
		&[false, true, false, true, false, false, true],
	);
	assert_eq!((state, steps), (CheckState::Up, vec![]));
}

#[test]
fn uptime_a_check_is_down_after_the_failures_in_a_row_and_says_so_once() {
	let runs = vec![false; DOWN_AFTER as usize + 4];
	let (state, steps) = feed(CheckState::Up, &runs);
	assert_eq!((state, steps), (CheckState::Down, vec![Step::WentDown]));
	let (state, _) = feed(CheckState::Up, &runs[..DOWN_AFTER as usize - 1]);
	assert_eq!(state, CheckState::Up, "one short of the limit");
}

#[test]
fn uptime_a_lucky_answer_in_an_outage_is_not_recovery() {
	let mut runs = vec![false; DOWN_AFTER as usize];
	runs.extend([true, false, true]);
	let (state, steps) = feed(CheckState::Up, &runs);
	assert_eq!((state, steps), (CheckState::Down, vec![Step::WentDown]));
	let mut runs = vec![false; DOWN_AFTER as usize];
	runs.extend(vec![true; UP_AFTER as usize]);
	let (state, steps) = feed(CheckState::Up, &runs);
	assert_eq!(
		(state, steps),
		(CheckState::Up, vec![Step::WentDown, Step::WentUp])
	);
}

#[test]
fn uptime_the_first_good_answer_is_up_without_a_word() {
	let (state, steps) = feed(CheckState::Unknown, &[true]);
	assert_eq!((state, steps), (CheckState::Up, vec![]));
	let (state, steps) = feed(CheckState::Unknown, &vec![false; DOWN_AFTER as usize]);
	assert_eq!((state, steps), (CheckState::Down, vec![Step::WentDown]));
}

// --- the probes -------------------------------------------------------------------------------

#[tokio::test]
async fn uptime_http_check_judges_status_keyword_and_timeout() {
	let web = web().await;
	let client = client();

	let good = probe::http(&client, &spec(format!("{}/ok", web.base))).await;
	assert!(good.ok, "{good:?}");
	assert_eq!(good.status_code, Some(200));
	assert!(good.latency_ms.is_some());

	let wrong = probe::http(&client, &spec(format!("{}/broken", web.base))).await;
	assert!(!wrong.ok);
	assert_eq!(wrong.status_code, Some(503));
	assert_eq!(wrong.error.as_deref(), Some("answered 503, expected 200"));

	// Any answer below 400 (the setting auto checks use).
	let mut any = spec(format!("{}/ok", web.base));
	any.expected_status = 0;
	assert!(probe::http(&client, &any).await.ok);
	any.url = format!("{}/missing", web.base);
	let missing = probe::http(&client, &any).await;
	assert_eq!(missing.error.as_deref(), Some("answered 404"));

	// Another expected status.
	let mut not_found = spec(format!("{}/missing", web.base));
	not_found.expected_status = 404;
	assert!(probe::http(&client, &not_found).await.ok);

	// A keyword: found, then not found.
	let mut keyword = spec(format!("{}/ok", web.base));
	keyword.keyword = Some("podkit stub".to_owned());
	assert!(probe::http(&client, &keyword).await.ok);
	keyword.keyword = Some("something else".to_owned());
	let absent = probe::http(&client, &keyword).await;
	assert!(!absent.ok);
	assert_eq!(
		absent.error.as_deref(),
		Some("the keyword is not in the response")
	);

	// HEAD works too.
	let mut head = spec(format!("{}/ok", web.base));
	head.head = true;
	assert!(probe::http(&client, &head).await.ok);

	// A timeout.
	let mut slow = spec(format!("{}/slow", web.base));
	slow.timeout = Duration::from_secs(1);
	let late = probe::http(&client, &slow).await;
	assert!(!late.ok);
	assert_eq!(late.error.as_deref(), Some("no answer within 1 s"));
}

#[tokio::test]
async fn uptime_http_check_follows_a_few_redirects_and_vets_every_hop() {
	let web = web().await;
	let client = client();
	// A redirect to a page of the same site is followed ...
	let followed = probe::http(&client, &spec(format!("{}/to-ok", web.base))).await;
	assert!(followed.ok, "{followed:?}");
	// ... a loop is cut ...
	let looped = probe::http(&client, &spec(format!("{}/loop", web.base))).await;
	assert_eq!(looped.error.as_deref(), Some("too many redirects"));
	// ... and a redirect to the metadata address is refused at the hop, however fine the
	// first address was.
	let hop = probe::http(&client, &spec(format!("{}/to-metadata", web.base))).await;
	assert!(!hop.ok);
	assert!(
		hop.error
			.as_deref()
			.unwrap_or_default()
			.contains("cannot connect"),
		"{hop:?}"
	);
}

#[tokio::test]
async fn uptime_probes_go_through_the_outbound_policy() {
	install_crypto();
	// The ordinary test policy lets 127.0.0.1 through for the stubs; this one does not.
	let strict = Outbound::new(true, Vec::new()).unwrap();
	let web = web().await;
	let refused = probe::http(&strict, &spec(format!("{}/ok", web.base))).await;
	assert!(!refused.ok);
	assert_eq!(
		refused.error.as_deref(),
		Some("cannot connect: that address cannot be reached from here")
	);
	assert_eq!(refused.status_code, None, "nothing was requested");
	for url in [
		"http://169.254.169.254/latest/meta-data/",
		"http://[::ffff:127.0.0.1]:8080/",
		"http://0.0.0.0/",
	] {
		let out = probe::http(&strict, &spec(url.to_owned())).await;
		assert!(!out.ok, "{url}");
		assert!(
			out.error
				.as_deref()
				.unwrap_or_default()
				.contains("cannot connect"),
			"{url}: {out:?}"
		);
	}
	// A TCP check and a certificate look are held to it too.
	let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
	let port = listener.local_addr().unwrap().port();
	let tcp = probe::tcp(&strict, "127.0.0.1", port, Duration::from_secs(2)).await;
	assert!(!tcp.ok);
	assert!(
		tokio::time::timeout(Duration::from_millis(200), listener.accept())
			.await
			.is_err(),
		"the port was connected to"
	);
	assert!(
		probe::tls_expiry(&strict, "127.0.0.1", port, Duration::from_secs(2))
			.await
			.is_err()
	);
}

#[tokio::test]
async fn uptime_errors_never_carry_the_address() {
	let web = web().await;
	let client = client();
	// Nothing listens on port 1.
	let refused = probe::http(
		&client,
		&spec("http://127.0.0.1:1/secret-path?token=abc".into()),
	)
	.await;
	assert!(!refused.ok);
	let text = refused.error.unwrap();
	for leak in ["127.0.0.1", "secret-path", "token", "abc"] {
		assert!(!text.contains(leak), "{leak} in {text:?}");
	}
	let wrong = probe::http(&client, &spec(format!("{}/broken", web.base))).await;
	assert!(!wrong.error.unwrap().contains(&web.base));
}

#[tokio::test]
async fn uptime_tcp_check_connects_or_does_not() {
	let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
	let port = listener.local_addr().unwrap().port();
	let open = probe::tcp(&client(), "127.0.0.1", port, Duration::from_secs(2)).await;
	assert!(open.ok, "{open:?}");
	drop(listener);
	let closed = probe::tcp(&client(), "127.0.0.1", port, Duration::from_secs(2)).await;
	assert!(!closed.ok);
	assert_eq!(
		closed.error.as_deref(),
		Some("cannot connect: connection refused")
	);
}

// A self-signed ECDSA P-256 certificate for `localhost`, valid until 2030-01-01 (a UTCTime), and
// its key; and another valid until 2126-01-01 (a GeneralizedTime). Made with
// `openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:prime256v1 -not_before 20260101000000Z
//  -not_after <date>`.
const CERT_2030: &str = "MIIBpDCCAUugAwIBAgIUI5X15QQzGEdWQjwFXW2y64YsXtMwCgYIKoZIzj0EAwIwHTEbMBkGA1UEAwwScG9ka2l0LXVwdGltZS10ZXN0MB4XDTI2MDEwMTAwMDAwMFoXDTMwMDEwMTAwMDAwMFowHTEbMBkGA1UEAwwScG9ka2l0LXVwdGltZS10ZXN0MFkwEwYHKoZIzj0CAQYIKoZIzj0DAQcDQgAEa/CZLSlyLpk42xvc14yvAE0yoKFN7UVOFnQR9OHYsH8JonvqgxOXCrBsuMeprABpahdLrBE+VqTGgLs00dQKc6NpMGcwHQYDVR0OBBYEFMByxwbhNMM7iDneP0FpHt3n7xRcMB8GA1UdIwQYMBaAFMByxwbhNMM7iDneP0FpHt3n7xRcMA8GA1UdEwEB/wQFMAMBAf8wFAYDVR0RBA0wC4IJbG9jYWxob3N0MAoGCCqGSM49BAMCA0cAMEQCIAy283pZkii3tEpSULVimDevHcp7MXcbcdte+QUmoCFeAiA9ACEqkEa1FQ5rD28epFi20Xc8hT+U0ipdW9inTMWfsA==";
const KEY_2030: &str = "MIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQgidXw4jKzNAtxMCp5mU4AecZNRVOnjOInsDkFpcMs37qhRANCAARr8JktKXIumTjbG9zXjK8ATTKgoU3tRU4WdBH04diwfwmie+qDE5cKsGy4x6msAGlqF0usET5WpMaAuzTR1Apz";
const CERT_2126: &str = "MIIBkjCCATegAwIBAgIUUbnitUANISUnycIyQrJesgYI17swCgYIKoZIzj0EAwIwHTEbMBkGA1UEAwwScG9ka2l0LXVwdGltZS10ZXN0MCAXDTI2MDEwMTAwMDAwMFoYDzIxMjYwMTAxMDAwMDAwWjAdMRswGQYDVQQDDBJwb2RraXQtdXB0aW1lLXRlc3QwWTATBgcqhkjOPQIBBggqhkjOPQMBBwNCAARAFLTwywdBtjQE878BEYCJirNbtkOZObVOAYANPwrp8kvBX4RvIdDWnaeCw5Nolpwf1lhk7j/vKAtMszcKAJ7bo1MwUTAdBgNVHQ4EFgQUy3OnJoP9eJSYJ6luNy0ODVm/QnUwHwYDVR0jBBgwFoAUy3OnJoP9eJSYJ6luNy0ODVm/QnUwDwYDVR0TAQH/BAUwAwEB/zAKBggqhkjOPQQDAgNJADBGAiEApDI+dNsz7JFRb6A5kj5OHrRALffr5Rmg+0Yi/FjF/c0CIQDFu4dUnYxnPjKhitFsBi3IATu5RiN7N0ADlL6P2hUlsQ==";

fn der(b64: &str) -> Vec<u8> {
	use base64::Engine as _;
	base64::engine::general_purpose::STANDARD
		.decode(b64)
		.unwrap()
}

#[test]
fn uptime_the_expiry_is_read_from_the_certificate() {
	let not_after = probe::not_after(&der(CERT_2030)).expect("a readable certificate");
	assert_eq!(not_after, datetime!(2030-01-01 00:00 UTC));
	let now = datetime!(2029-12-20 00:00 UTC);
	assert_eq!(probe::days_left(not_after, now), 12);
	assert_eq!(probe::days_left(now, not_after), -12);
	// Garbage is not a certificate.
	assert_eq!(probe::not_after(b"not a certificate"), None);
	assert_eq!(probe::not_after(&[]), None);
	let mut cut = der(CERT_2030);
	cut.truncate(40);
	assert_eq!(probe::not_after(&cut), None);
}

#[test]
fn uptime_both_time_formats_of_a_certificate_are_read() {
	// UTCTime has two year digits (below 50 is 20xx), GeneralizedTime four.
	assert_eq!(
		probe::not_after(&der(CERT_2030)),
		Some(datetime!(2030-01-01 00:00 UTC))
	);
	assert_eq!(
		probe::not_after(&der(CERT_2126)),
		Some(datetime!(2126-01-01 00:00 UTC))
	);
}

/// A TLS server with the 2030 certificate on a free port.
async fn tls_stub() -> u16 {
	use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
	use tokio::io::AsyncWriteExt;
	install_crypto();
	let config = rustls::ServerConfig::builder_with_provider(Arc::new(
		rustls::crypto::ring::default_provider(),
	))
	.with_safe_default_protocol_versions()
	.unwrap()
	.with_no_client_auth()
	.with_single_cert(
		vec![CertificateDer::from(der(CERT_2030))],
		PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(der(KEY_2030))),
	)
	.unwrap();
	let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));
	let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
	let port = listener.local_addr().unwrap().port();
	tokio::spawn(async move {
		while let Ok((tcp, _)) = listener.accept().await {
			let acceptor = acceptor.clone();
			tokio::spawn(async move {
				if let Ok(mut tls) = acceptor.accept(tcp).await {
					let _ = tls.shutdown().await;
				}
			});
		}
	});
	port
}

#[tokio::test]
async fn uptime_tls_expiry_is_read_from_a_live_handshake() {
	let port = tls_stub().await;
	let at = probe::tls_expiry(&client(), "localhost", port, Duration::from_secs(5))
		.await
		.unwrap();
	assert_eq!(at, datetime!(2030-01-01 00:00 UTC));
	// Not TLS at all.
	let web = web().await;
	let plain: u16 = web.base.rsplit(':').next().unwrap().parse().unwrap();
	let err = probe::tls_expiry(&client(), "127.0.0.1", plain, Duration::from_secs(2))
		.await
		.unwrap_err();
	assert!(!err.contains("127.0.0.1"), "{err}");
	// Nothing there.
	assert!(
		probe::tls_expiry(&client(), "127.0.0.1", 1, Duration::from_secs(2))
			.await
			.is_err()
	);
}

// --- validation -------------------------------------------------------------------------------

#[test]
fn uptime_input_is_validated() {
	let ok = http_input("site", "https://example.com/health");
	assert!(service::validate(&ok).is_ok());
	let bad = |f: &dyn Fn(&mut CheckInput)| {
		let mut i = http_input("site", "https://example.com/");
		f(&mut i);
		service::validate(&i).is_err()
	};
	assert!(bad(&|i| i.interval_secs = 29), "30 seconds at the least");
	assert!(bad(&|i| i.timeout_secs = 31), "timeout above the interval");
	assert!(bad(&|i| i.name = "  ".into()));
	assert!(bad(&|i| i.url = "ftp://example.com/".into()));
	assert!(
		bad(&|i| i.url = "https://user:pass@example.com/".into()),
		"no login in a URL"
	);
	assert!(bad(&|i| i.url = "not a url".into()));
	assert!(bad(&|i| i.method = "POST".into()));
	assert!(bad(&|i| i.expected_status = 99));
	assert!(bad(&|i| {
		i.method = "HEAD".into();
		i.keyword = "x".into();
	}));
	assert!(bad(&|i| i.kind = "icmp".into()));
	let mut tcp = http_input("db", "");
	tcp.kind = "tcp".into();
	tcp.host = "nas".into();
	tcp.port = 5432;
	assert!(
		service::validate(&tcp).is_ok(),
		"one label is fine on a homelab"
	);
	tcp.host = "bad host!".into();
	assert!(service::validate(&tcp).is_err());
	tcp.host = "nas".into();
	tcp.port = 70_000;
	assert!(service::validate(&tcp).is_err());
	// The certificate warning only applies to https.
	let mut plain = http_input("site", "http://example.com/");
	plain.tls_warn_days = Some(14);
	assert_eq!(service::validate(&plain).unwrap().tls_warn_days, None);
}

// --- rollups ----------------------------------------------------------------------------------

async fn result(db: &PgPool, id: UptimeCheckId, at: OffsetDateTime, ok: bool, ms: i32) {
	sqlx::query(
		"INSERT INTO uptime_results (check_id, at, ok, latency_ms, status_code, error)
		VALUES ($1, $2, $3, $4, NULL, NULL)",
	)
	.bind(id)
	.bind(at)
	.bind(ok)
	.bind(ok.then_some(ms))
	.execute(db)
	.await
	.unwrap();
}

/// `2026-11-<d> <h>:00 UTC`.
fn day(d: u8, h: u8) -> OffsetDateTime {
	datetime!(2026-11-01 00:00 UTC) + Span::days(i64::from(d) - 1) + Span::hours(i64::from(h))
}

#[sqlx::test]
async fn uptime_raw_results_live_seven_days_and_the_daily_figures_ninety(db: PgPool) {
	let w = world(db).await;
	let check = w.check("site", "http://example.com/").await;
	// 2026-11-15: four good runs and one bad one.
	for h in 0..4 {
		result(w.db(), check.id, day(15, h), true, 100 + i32::from(h) * 10).await;
	}
	result(w.db(), check.id, day(15, 5), false, 0).await;
	// 2026-11-20: all good.
	result(w.db(), check.id, day(20, 1), true, 50).await;
	result(w.db(), check.id, day(20, 2), true, 70).await;
	// A very old daily row, and one that is still in range.
	sqlx::query(
		"INSERT INTO uptime_daily (check_id, day, total, ok)
		VALUES ($1, '2026-07-01', 10, 10), ($1, '2026-09-01', 10, 9)",
	)
	.bind(check.id)
	.execute(w.db())
	.await
	.unwrap();

	rollup(w.db(), day(20, 12)).await.unwrap();
	let row: (i32, i32, Option<i32>) = sqlx::query_as(
		"SELECT total, ok, avg_latency_ms FROM uptime_daily
		WHERE check_id = $1 AND day = '2026-11-15'",
	)
	.bind(check.id)
	.fetch_one(w.db())
	.await
	.unwrap();
	assert_eq!(row, (5, 4, Some(115)), "rolled up from the raw rows");

	// Five days on, the raw rows of the 15th are past seven days and go; the day's figures stay.
	let later = day(25, 12);
	prune(w.db(), later).await.unwrap();
	assert_eq!(
		count(w.db(), "uptime_results").await,
		2,
		"only the 20th's rows are left"
	);
	let stats = day_stats(w.db(), &[check.id], 90, later).await.unwrap();
	let days = &stats[&check.id];
	let of = |d: &str| {
		days.iter()
			.find(|s| s.day.to_string() == d)
			.map(|s| (s.total, s.ok))
	};
	assert_eq!(of("2026-11-15"), Some((5, 4)));
	assert_eq!(
		of("2026-11-20"),
		Some((2, 2)),
		"recent days come from the raw rows"
	);
	assert_eq!(of("2026-09-01"), Some((10, 9)));
	assert_eq!(of("2026-07-01"), None, "older than 90 days");
	let old: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM uptime_daily WHERE day = '2026-07-01'")
		.fetch_one(w.db())
		.await
		.unwrap();
	assert_eq!(old, 0, "pruned");
	let total_ok: i32 = days.iter().map(|d| d.ok).sum();
	let total: i32 = days.iter().map(|d| d.total).sum();
	assert_eq!((total, total_ok), (17, 15));
	assert!((super::uptime_percent(days).unwrap() - 15.0 * 100.0 / 17.0).abs() < 1e-9);
}

#[sqlx::test]
async fn uptime_a_rollup_is_idempotent_and_leaves_pruned_days_alone(db: PgPool) {
	let w = world(db).await;
	let check = w.check("site", "http://example.com/").await;
	let now = day(20, 12);
	result(w.db(), check.id, now - Span::hours(2), true, 10).await;
	rollup(w.db(), now).await.unwrap();
	rollup(w.db(), now).await.unwrap();
	let n: (i32,) = sqlx::query_as("SELECT total FROM uptime_daily WHERE day = '2026-11-20'")
		.fetch_one(w.db())
		.await
		.unwrap();
	assert_eq!(n.0, 1);
	// A final row for a day whose raw rows are partly gone is not overwritten by a smaller count.
	sqlx::query(
		"INSERT INTO uptime_daily (check_id, day, total, ok) VALUES ($1, '2026-11-13', 99, 98)",
	)
	.bind(check.id)
	.execute(w.db())
	.await
	.unwrap();
	result(w.db(), check.id, day(13, 23), true, 10).await;
	rollup(w.db(), now).await.unwrap();
	let n: (i32,) =
		sqlx::query_scalar::<_, i32>("SELECT total FROM uptime_daily WHERE day = '2026-11-13'")
			.fetch_one(w.db())
			.await
			.map(|t| (t,))
			.unwrap();
	assert_eq!(n.0, 99);
	maintain(w.db()).await;
}

// --- the runner, incidents and notifications --------------------------------------------------

#[sqlx::test]
async fn uptime_down_and_up_are_noticed_once_each_with_flap_protection(db: PgPool) {
	let w = world(db).await;
	w.want_uptime_events().await;
	let web = web().await;
	let check = w.check("site", &format!("{}/toggle", web.base)).await;

	assert_eq!(w.pass().await, 1);
	let c = w.reload(check.id).await;
	assert_eq!(c.state(), CheckState::Up, "a good first answer is up");
	assert!(c.last_latency_ms.is_some());
	assert_eq!(
		w.events("uptime_down").await + w.events("uptime_up").await,
		0
	);

	// Two bad answers: still up, nothing said.
	web.up.store(false, Ordering::SeqCst);
	for _ in 0..DOWN_AFTER - 1 {
		w.pass().await;
	}
	assert_eq!(w.reload(check.id).await.state(), CheckState::Up);
	assert_eq!(w.events("uptime_down").await, 0);
	// A good one in between starts the count over.
	web.up.store(true, Ordering::SeqCst);
	w.pass().await;
	web.up.store(false, Ordering::SeqCst);
	for _ in 0..DOWN_AFTER - 1 {
		w.pass().await;
	}
	assert_eq!(w.reload(check.id).await.state(), CheckState::Up);

	// The last bad one in a row: down, one incident, one notification.
	w.pass().await;
	let c = w.reload(check.id).await;
	assert_eq!(c.state(), CheckState::Down);
	assert_eq!(c.last_error.as_deref(), Some("answered 503, expected 200"));
	assert_eq!(w.events("uptime_down").await, 1);
	let title: String =
		sqlx::query_scalar("SELECT title FROM notification_events WHERE kind = 'uptime_down'")
			.fetch_one(w.db())
			.await
			.unwrap();
	assert_eq!(title, "site is down");
	// More bad answers do not repeat it.
	w.pass().await;
	w.pass().await;
	assert_eq!(w.events("uptime_down").await, 1);
	assert_eq!(count(w.db(), "uptime_incidents").await, 1);
	let open = |db: &PgPool| {
		let db = db.clone();
		async move {
			sqlx::query_scalar::<_, i64>(
				"SELECT COUNT(*) FROM uptime_incidents WHERE ended_at IS NULL",
			)
			.fetch_one(&db)
			.await
			.unwrap()
		}
	};
	assert_eq!(open(w.db()).await, 1);

	// One good answer is not enough to be up; the second is.
	web.up.store(true, Ordering::SeqCst);
	w.pass().await;
	assert_eq!(w.reload(check.id).await.state(), CheckState::Down);
	assert_eq!(w.events("uptime_up").await, 0);
	w.pass().await;
	assert_eq!(w.reload(check.id).await.state(), CheckState::Up);
	assert_eq!(w.events("uptime_up").await, 1);
	assert_eq!(open(w.db()).await, 0, "the incident is closed");
	// The history has every run, good and bad.
	assert!(count(w.db(), "uptime_results").await >= 11);
	let incidents = service::incidents(w.db(), w.user, w.team, check.id, 10)
		.await
		.unwrap();
	assert_eq!(incidents.len(), 1);
	assert!(incidents[0].ended_at.is_some());
}

#[sqlx::test]
async fn uptime_a_failing_check_is_asked_again_soon_but_a_healthy_one_waits(db: PgPool) {
	let w = world(db).await;
	let web = web().await;
	let mut input = http_input("site", &format!("{}/toggle", web.base));
	input.interval_secs = 600;
	let check = service::create(&w.s, w.user, w.team, &input).await.unwrap();
	let wait = |db: PgPool, id: UptimeCheckId| async move {
		sqlx::query_scalar::<_, f64>(
			"SELECT EXTRACT(EPOCH FROM next_check_at - last_checked_at)::float8
			FROM uptime_checks WHERE id = $1",
		)
		.bind(id)
		.fetch_one(&db)
		.await
		.unwrap()
	};
	w.pass().await;
	let secs = wait(w.db().clone(), check.id).await;
	assert!((secs - 600.0).abs() < 1.0, "healthy: the interval, {secs}");
	web.up.store(false, Ordering::SeqCst);
	w.pass().await;
	let secs = wait(w.db().clone(), check.id).await;
	assert!((secs - 30.0).abs() < 1.0, "failing: retried soon, {secs}");
}

#[sqlx::test]
async fn uptime_a_paused_check_does_not_run_and_a_pass_respects_the_claim(db: PgPool) {
	let w = world(db).await;
	let web = web().await;
	let check = w.check("site", &format!("{}/ok", web.base)).await;
	let mut input = http_input("site", &format!("{}/ok", web.base));
	input.enabled = false;
	service::update(&w.s, w.user, w.team, check.id, &input)
		.await
		.unwrap();
	assert_eq!(w.pass().await, 0, "paused");
	input.enabled = true;
	service::update(&w.s, w.user, w.team, check.id, &input)
		.await
		.unwrap();
	assert_eq!(w.pass().await, 1);
	// Right after a run the next one is an interval away.
	assert_eq!(
		runner::tick(&w.s, OffsetDateTime::now_utc()).await.unwrap(),
		0
	);
}

#[sqlx::test]
async fn uptime_a_tcp_check_runs_and_goes_down(db: PgPool) {
	let w = world(db).await;
	w.want_uptime_events().await;
	let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
	let port = i32::from(listener.local_addr().unwrap().port());
	let mut input = http_input("database", "");
	input.kind = "tcp".into();
	input.host = "127.0.0.1".into();
	input.port = port;
	let check = service::create(&w.s, w.user, w.team, &input).await.unwrap();
	w.pass().await;
	assert_eq!(w.reload(check.id).await.state(), CheckState::Up);
	drop(listener);
	for _ in 0..DOWN_AFTER {
		w.pass().await;
	}
	let c = w.reload(check.id).await;
	assert_eq!(c.state(), CheckState::Down);
	assert_eq!(
		c.last_error.as_deref(),
		Some("cannot connect: connection refused")
	);
	assert_eq!(w.events("uptime_down").await, 1);
}

#[sqlx::test]
async fn uptime_an_expiring_certificate_is_warned_about_once(db: PgPool) {
	let w = world(db).await;
	w.want_uptime_events().await;
	let mut input = http_input("secure", "https://example.com/");
	input.tls_warn_days = Some(14);
	let check = service::create(&w.s, w.user, w.team, &input).await.unwrap();
	let now = OffsetDateTime::now_utc();
	let fine = Outcome {
		ok: true,
		latency_ms: Some(5),
		status_code: Some(200),
		error: None,
	};
	// Thirty days left: nothing to say.
	runner::record(&w.s, &check, &fine, Some(Ok(now + Span::days(30))), now)
		.await
		.unwrap();
	assert_eq!(w.events("cert_expiring").await, 0);
	// Five days left: one warning, and the next looks do not repeat it.
	let check = w.reload(check.id).await;
	let soon = now + Span::days(5) + Span::hours(1);
	runner::record(&w.s, &check, &fine, Some(Ok(soon)), now + Span::seconds(1))
		.await
		.unwrap();
	assert_eq!(w.events("cert_expiring").await, 1);
	let check = w.reload(check.id).await;
	assert!(check.tls_warned);
	runner::record(&w.s, &check, &fine, Some(Ok(soon)), now + Span::hours(7))
		.await
		.unwrap();
	assert_eq!(w.events("cert_expiring").await, 1);
	let title: String =
		sqlx::query_scalar("SELECT title FROM notification_events WHERE kind = 'cert_expiring'")
			.fetch_one(w.db())
			.await
			.unwrap();
	assert!(
		title.contains("secure") && title.contains("5 days"),
		"{title}"
	);
	// A renewed certificate re-arms the warning.
	let check = w.reload(check.id).await;
	runner::record(
		&w.s,
		&check,
		&fine,
		Some(Ok(now + Span::days(80))),
		now + Span::hours(14),
	)
	.await
	.unwrap();
	assert!(!w.reload(check.id).await.tls_warned);
}

// --- checks for application domains -------------------------------------------------------------

async fn app_with_domain(w: &World) -> i64 {
	let project = crypto::generate_id();
	sqlx::query("INSERT INTO projects (id, team_id, name, slug) VALUES ($1, $2, 'web', 'web')")
		.bind(project)
		.bind(w.team)
		.execute(w.db())
		.await
		.unwrap();
	let server = crate::tests::local_server_id(w.db(), w.team.0).await;
	crate::tests::seed_application_on(w.db(), project, server).await;
	let app: i64 = crate::tests::application_id(w.db(), "pinned").await;
	crate::tests::seed_deployment(w.db(), app, "running").await;
	sqlx::query(
		"INSERT INTO custom_domains (id, application_id, hostname) VALUES (1, $1, 'app.example.com')",
	)
	.bind(app)
	.execute(w.db())
	.await
	.unwrap();
	app
}

#[sqlx::test]
async fn uptime_every_domain_of_a_running_app_gets_a_check_and_a_deleted_one_stays_deleted(
	db: PgPool,
) {
	let w = world(db).await;
	let app = app_with_domain(&w).await;
	service::reconcile_auto(&w.s).await.unwrap();
	service::reconcile_auto(&w.s).await.unwrap();
	let checks = service::checks(w.db(), w.user, w.team).await.unwrap();
	assert_eq!(
		checks.len(),
		2,
		"the generated name and the custom domain: {checks:?}"
	);
	let generated = checks
		.iter()
		.find(|c| c.target().contains("sslip.io"))
		.unwrap();
	assert!(
		generated
			.target()
			.starts_with("http://pinned.127-0-0-1.sslip.io")
	);
	assert_eq!(generated.expected_status, 0);
	assert_eq!(generated.application_id.map(|a| a.0), Some(app));
	let custom = checks
		.iter()
		.find(|c| c.target().contains("app.example.com"))
		.unwrap();
	assert!(
		custom.target().starts_with("http://"),
		"no ACME here, so no https"
	);

	// Deleting one is remembered.
	service::remove(w.db(), w.user, w.team, custom.id)
		.await
		.unwrap();
	service::reconcile_auto(&w.s).await.unwrap();
	assert_eq!(
		service::checks(w.db(), w.user, w.team).await.unwrap().len(),
		1
	);

	// A domain that is gone takes its check along.
	sqlx::query("DELETE FROM custom_domains")
		.execute(w.db())
		.await
		.unwrap();
	service::reconcile_auto(&w.s).await.unwrap();
	assert_eq!(
		service::checks(w.db(), w.user, w.team).await.unwrap().len(),
		1
	);
}

#[sqlx::test]
async fn uptime_a_team_can_opt_out_of_automatic_checks(db: PgPool) {
	let w = world(db).await;
	app_with_domain(&w).await;
	service::set_auto_checks(w.db(), w.user, w.team, false)
		.await
		.unwrap();
	assert!(!service::auto_checks(w.db(), w.user, w.team).await.unwrap());
	service::reconcile_auto(&w.s).await.unwrap();
	assert_eq!(count(w.db(), "uptime_checks").await, 0);
	service::set_auto_checks(w.db(), w.user, w.team, true)
		.await
		.unwrap();
	service::reconcile_auto(&w.s).await.unwrap();
	assert_eq!(count(w.db(), "uptime_checks").await, 2);
}

#[sqlx::test]
async fn uptime_an_application_that_is_not_running_gets_no_check(db: PgPool) {
	let w = world(db).await;
	let app = app_with_domain(&w).await;
	sqlx::query("UPDATE deployments SET status = 'stopped' WHERE application_id = $1")
		.bind(app)
		.execute(w.db())
		.await
		.unwrap();
	service::reconcile_auto(&w.s).await.unwrap();
	assert_eq!(count(w.db(), "uptime_checks").await, 0);
}

// --- the API and tenancy ----------------------------------------------------------------------

fn api(team: i64) -> String {
	format!("/api/v1/teams/{team}/uptime")
}

#[sqlx::test]
async fn uptime_api_creates_lists_updates_and_deletes_checks(db: PgPool) {
	let w = world(db).await;
	let t = Some(w.token.as_str());
	let base = api(w.team.0);
	let (s, created) = call(
		&w.app,
		"POST",
		&format!("{base}/checks"),
		t,
		Some(json!({
			"name": "site", "kind": "http", "url": "https://example.com/health", "interval_secs": 45,
		})),
	)
	.await;
	assert_eq!(s, StatusCode::CREATED, "{created}");
	assert_eq!(created["state"], "unknown");
	assert_eq!(created["interval_secs"], 45);
	let id = created["id"].as_i64().unwrap();

	let (s, list) = call(&w.app, "GET", &format!("{base}/checks"), t, None).await;
	assert_eq!(s, StatusCode::OK);
	assert_eq!(list.as_array().unwrap().len(), 1);

	let (s, dup) = call(
		&w.app,
		"POST",
		&format!("{base}/checks"),
		t,
		Some(json!({ "name": "site", "kind": "tcp", "host": "nas", "port": 22 })),
	)
	.await;
	assert_eq!(s, StatusCode::CONFLICT, "{dup}");

	let (s, bad) = call(
		&w.app,
		"POST",
		&format!("{base}/checks"),
		t,
		Some(json!({
			"name": "fast", "kind": "http", "url": "https://example.com/", "interval_secs": 5,
		})),
	)
	.await;
	assert!(s.is_client_error(), "{s} {bad}");

	let (s, updated) = call(
		&w.app,
		"PUT",
		&format!("{base}/checks/{id}"),
		t,
		Some(json!({
			"name": "site", "kind": "http", "url": "https://example.com/health",
			"interval_secs": 120, "enabled": false,
		})),
	)
	.await;
	assert_eq!(s, StatusCode::OK, "{updated}");
	assert_eq!(updated["interval_secs"], 120);
	assert_eq!(updated["enabled"], false);

	let (s, body) = call(
		&w.app,
		"GET",
		&format!("{base}/checks/{id}/results?range=bogus"),
		t,
		None,
	)
	.await;
	assert!(s.is_client_error(), "{s} {body}");
	let (s, body) = call(
		&w.app,
		"GET",
		&format!("{base}/checks/{id}/results?range=7d"),
		t,
		None,
	)
	.await;
	assert_eq!(s, StatusCode::OK);
	assert_eq!(body["points"], json!([]));
	let (s, body) = call(
		&w.app,
		"GET",
		&format!("{base}/checks/{id}/incidents"),
		t,
		None,
	)
	.await;
	assert_eq!(s, StatusCode::OK);
	assert_eq!(body, json!([]));
	let (s, _) = call(&w.app, "POST", &format!("{base}/checks/{id}/run"), t, None).await;
	assert_eq!(s, StatusCode::ACCEPTED);

	let (s, _) = call(&w.app, "DELETE", &format!("{base}/checks/{id}"), t, None).await;
	assert_eq!(s, StatusCode::NO_CONTENT);
	let (s, _) = call(&w.app, "GET", &format!("{base}/checks/{id}"), t, None).await;
	assert_eq!(s, StatusCode::NOT_FOUND);
}

#[sqlx::test]
async fn uptime_another_teams_checks_are_not_found(db: PgPool) {
	let w = world(db).await;
	let check = w.check("site", "https://example.com/").await;
	let (bob, bob_team) = user_with_team(&w.app, "bob").await;
	let b = Some(bob.as_str());
	let id = check.id.0;

	// Through his own team's path: the id belongs to someone else.
	let mine = api(bob_team);
	for (method, path) in [
		("GET", format!("{mine}/checks/{id}")),
		("GET", format!("{mine}/checks/{id}/results")),
		("GET", format!("{mine}/checks/{id}/incidents")),
		("POST", format!("{mine}/checks/{id}/run")),
		("DELETE", format!("{mine}/checks/{id}")),
	] {
		let (s, _) = call(&w.app, method, &path, b, None).await;
		assert_eq!(s, StatusCode::NOT_FOUND, "{method} {path}");
	}
	let (s, _) = call(
		&w.app,
		"PUT",
		&format!("{mine}/checks/{id}"),
		b,
		Some(json!({ "name": "stolen", "kind": "http", "url": "https://evil.example/" })),
	)
	.await;
	assert_eq!(s, StatusCode::NOT_FOUND);
	// His own list is empty, and nothing changed.
	let (_, list) = call(&w.app, "GET", &format!("{mine}/checks"), b, None).await;
	assert_eq!(list, json!([]));
	assert_eq!(count(w.db(), "uptime_checks").await, 1);
	assert_eq!(w.reload(check.id).await.name, "site");

	// Through the owner's path he is not a member: refused.
	let theirs = api(w.team.0);
	let (s, _) = call(&w.app, "GET", &format!("{theirs}/checks"), b, None).await;
	assert!(
		s == StatusCode::FORBIDDEN || s == StatusCode::NOT_FOUND,
		"{s}"
	);
}

#[sqlx::test]
async fn uptime_the_status_page_settings_are_the_teams_own(db: PgPool) {
	let w = world(db).await;
	let t = Some(w.token.as_str());
	let base = api(w.team.0);
	let (s, page) = call(&w.app, "GET", &format!("{base}/status-page"), t, None).await;
	assert_eq!(s, StatusCode::OK);
	assert_eq!(page["enabled"], false);
	assert_eq!(page["slug"], "alice");
	let (s, page) = call(
		&w.app,
		"PUT",
		&format!("{base}/status-page"),
		t,
		Some(json!({ "enabled": true, "title": "Alice's services" })),
	)
	.await;
	assert_eq!(s, StatusCode::OK, "{page}");
	assert_eq!(page["enabled"], true);
	assert_eq!(page["title"], "Alice's services");
	let (s, settings) = call(&w.app, "GET", &format!("{base}/settings"), t, None).await;
	assert_eq!(s, StatusCode::OK);
	assert_eq!(settings["auto_checks"], true);
}

// --- the public page, service level ----------------------------------------------------------

#[sqlx::test]
async fn uptime_the_public_page_has_only_the_picked_checks_by_their_chosen_names(db: PgPool) {
	let w = world(db).await;
	let mut shown = http_input("web-prod-internal", "https://internal-host.lan:8443/health");
	shown.status_visible = true;
	shown.status_name = "Website".into();
	service::create(&w.s, w.user, w.team, &shown).await.unwrap();
	w.check("secret-db", "http://db.internal.lan:5432/").await;
	let now = OffsetDateTime::now_utc();

	// Off by default: the same answer as a slug nobody has.
	assert!(
		service::public_page(w.db(), "alice", now)
			.await
			.unwrap()
			.is_none()
	);
	service::set_status_page(w.db(), w.user, w.team, true, "")
		.await
		.unwrap();
	assert!(
		service::public_page(w.db(), "nobody", now)
			.await
			.unwrap()
			.is_none()
	);
	let page = service::public_page(w.db(), "alice", now)
		.await
		.unwrap()
		.unwrap();
	assert_eq!(
		page.title, "alice",
		"the team's name when there is no title"
	);
	assert_eq!(page.checks.len(), 1);
	assert_eq!(page.checks[0].name, "Website");
	// Paused checks are left off.
	sqlx::query("UPDATE uptime_checks SET enabled = FALSE WHERE status_visible")
		.execute(w.db())
		.await
		.unwrap();
	let page = service::public_page(w.db(), "alice", now)
		.await
		.unwrap()
		.unwrap();
	assert!(page.checks.is_empty());
}

#[sqlx::test]
async fn uptime_members_read_but_cannot_change(db: PgPool) {
	let w = world(db).await;
	let check = w.check("site", "https://example.com/").await;
	// A user with a role that only reads: made by inserting the membership with role 2.
	let (bob, _) = user_with_team(&w.app, "bob").await;
	let bob_id = UserId(user_id(w.db(), "bob").await);
	sqlx::query("INSERT INTO team_members (id, team_id, user_id, role_id) VALUES ($1, $2, $3, 2)")
		.bind(crypto::generate_id())
		.bind(w.team)
		.bind(bob_id)
		.execute(w.db())
		.await
		.unwrap();
	let b = Some(bob.as_str());
	let base = api(w.team.0);
	let (s, list) = call(&w.app, "GET", &format!("{base}/checks"), b, None).await;
	assert_eq!(s, StatusCode::OK);
	assert_eq!(list.as_array().unwrap().len(), 1);
	let body = Some(json!({ "name": "x", "kind": "tcp", "host": "nas", "port": 22 }));
	for (method, path, body) in [
		("POST", format!("{base}/checks"), body.clone()),
		("PUT", format!("{base}/checks/{}", check.id), body),
		("DELETE", format!("{base}/checks/{}", check.id), None),
		("POST", format!("{base}/checks/{}/run", check.id), None),
		(
			"PUT",
			format!("{base}/status-page"),
			Some(json!({ "enabled": true })),
		),
		(
			"PUT",
			format!("{base}/settings"),
			Some(json!({ "auto_checks": false })),
		),
	] {
		let (s, _) = call(&w.app, method, &path, b, body).await;
		assert_eq!(s, StatusCode::FORBIDDEN, "{method} {path}");
	}
	assert_eq!(count(w.db(), "uptime_checks").await, 1);
}

// --- live ---------------------------------------------------------------------------------------

/// A check against a real whoami app goes up; when the app is stopped it goes down and a
/// notification is stored; when it is started again it is up again. Needs podman and the
/// `docker.io/traefik/whoami` image.
/// `cargo test -p server --all-features -- --ignored uptime_live`
#[sqlx::test]
#[ignore = "needs podman and the whoami image"]
async fn uptime_live_whoami_goes_down_and_up_with_the_app(db: PgPool) {
	use std::process::Command;
	const NAME: &str = "pk-t_5de0c7a9-whoami";
	const PORT: u16 = 21_746;
	let podman = |args: &[&str]| {
		Command::new("podman")
			.args(args)
			.output()
			.expect("podman is installed")
	};
	let run = || {
		podman(&[
			"run",
			"-d",
			"--rm",
			"--name",
			NAME,
			"-p",
			&format!("127.0.0.1:{PORT}:80"),
			"docker.io/traefik/whoami",
		])
	};
	let cleanup = || {
		podman(&["rm", "-f", NAME]);
	};
	cleanup();
	let started = run();
	assert!(
		started.status.success(),
		"{}",
		String::from_utf8_lossy(&started.stderr)
	);

	let w = world(db).await;
	w.want_uptime_events().await;
	let mut input = http_input("whoami", &format!("http://127.0.0.1:{PORT}/"));
	input.keyword = "Hostname".into();
	let check = service::create(&w.s, w.user, w.team, &input).await.unwrap();
	let wait_for = |wanted: CheckState| {
		let (w, id) = (&w, check.id);
		async move {
			for _ in 0..40 {
				w.pass().await;
				if w.reload(id).await.state() == wanted {
					return true;
				}
				tokio::time::sleep(Duration::from_millis(500)).await;
			}
			false
		}
	};
	let came_up = wait_for(CheckState::Up).await;

	podman(&["stop", "-t", "1", NAME]);
	let went_down = wait_for(CheckState::Down).await;
	let down_events = w.events("uptime_down").await;

	let restarted = run();
	let up_again = restarted.status.success() && wait_for(CheckState::Up).await;
	let up_events = w.events("uptime_up").await;
	cleanup();

	assert!(came_up, "the app comes up");
	assert!(went_down, "stopping the app takes the check down");
	assert_eq!(down_events, 1);
	assert!(up_again, "starting it again brings the check back");
	assert_eq!(up_events, 1);
}
