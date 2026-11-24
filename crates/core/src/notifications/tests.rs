//! Notifications: formatting per channel against stub servers, rule matching and scoping, dedup,
//! quiet hours, flap detection, metric alerts, retries and dead letters, secrets and tenancy.
//! `cargo test -p podkit-core --all-features -- notifications`

use std::collections::HashMap;
use std::fmt::Write as _;
use std::sync::atomic::{AtomicU16, Ordering};
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::extract::State;
use axum::http::{Request, StatusCode};
use hmac::{Hmac, KeyInit, Mac};
use serde_json::{Value, json};
use sha2::Sha256;
use sqlx::PgPool;
use time::macros::datetime;
use time::{Duration, OffsetDateTime};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

use super::alerts;
use super::channel::{ChannelFields, ChannelKind};
use super::delivery::{self, MAX_ATTEMPTS, RATE_LIMIT_PER_MINUTE};
use super::event::{Event, EventKind, Severity};
use super::pipeline::{self, Outcome};
use super::service::{self, AlertInput, RuleInput, SmtpInput};
use super::settings::Settings;
use crate::ids::{EnvironmentId, NotificationChannelId, ProjectId, TeamId, UserId};
use crate::state::AppState;
use crate::tests::{call, count, state, user_id, user_with_team};

// --- stubs --------------------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct Captured {
	path: String,
	headers: HashMap<String, String>,
	body: Vec<u8>,
}

impl Captured {
	fn json(&self) -> Value {
		serde_json::from_slice(&self.body).expect("a JSON body")
	}

	fn header(&self, name: &str) -> &str {
		self.headers
			.get(&name.to_ascii_lowercase())
			.map_or("", String::as_str)
	}
}

/// An HTTP server that records every request and answers with a status the test can change.
struct Stub {
	url: String,
	got: Arc<Mutex<Vec<Captured>>>,
	status: Arc<AtomicU16>,
}

impl Stub {
	fn requests(&self) -> Vec<Captured> {
		self.got.lock().unwrap().clone()
	}

	fn answer(&self, status: u16) {
		self.status.store(status, Ordering::SeqCst);
	}
}

#[derive(Clone)]
struct StubState {
	got: Arc<Mutex<Vec<Captured>>>,
	status: Arc<AtomicU16>,
}

async fn record(State(st): State<StubState>, req: Request<Body>) -> (StatusCode, &'static str) {
	let (parts, body) = req.into_parts();
	let body = to_bytes(body, usize::MAX).await.unwrap().to_vec();
	let headers = parts
		.headers
		.iter()
		.map(|(k, v)| (k.as_str().to_owned(), v.to_str().unwrap_or("").to_owned()))
		.collect();
	st.got.lock().unwrap().push(Captured {
		path: parts.uri.path().to_owned(),
		headers,
		body,
	});
	let status = StatusCode::from_u16(st.status.load(Ordering::SeqCst)).unwrap();
	(status, if status.is_success() { "ok" } else { "nope" })
}

async fn stub() -> Stub {
	let st = StubState {
		got: Arc::default(),
		status: Arc::new(AtomicU16::new(200)),
	};
	let app = Router::new().fallback(record).with_state(st.clone());
	let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
	let url = format!("http://{}", listener.local_addr().unwrap());
	tokio::spawn(async move {
		axum::serve(listener, app).await.unwrap();
	});
	Stub {
		url,
		got: st.got,
		status: st.status,
	}
}

/// A minimal SMTP server: accepts messages and keeps the whole transcript.
async fn smtp_stub() -> (u16, Arc<Mutex<Vec<String>>>) {
	let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
	let port = listener.local_addr().unwrap().port();
	let log: Arc<Mutex<Vec<String>>> = Arc::default();
	let kept = log.clone();
	tokio::spawn(async move {
		loop {
			let Ok((socket, _)) = listener.accept().await else {
				return;
			};
			let log = kept.clone();
			tokio::spawn(async move {
				let (read, mut write) = socket.into_split();
				let mut lines = BufReader::new(read).lines();
				write.write_all(b"220 stub ESMTP\r\n").await.unwrap();
				let mut data = false;
				while let Ok(Some(line)) = lines.next_line().await {
					log.lock().unwrap().push(line.clone());
					if data {
						if line == "." {
							data = false;
							write.write_all(b"250 queued\r\n").await.unwrap();
						}
						continue;
					}
					let upper = line.to_ascii_uppercase();
					let reply: &[u8] = if upper.starts_with("EHLO") {
						b"250-stub\r\n250 8BITMIME\r\n"
					} else if upper.starts_with("DATA") {
						data = true;
						b"354 go ahead\r\n"
					} else if upper.starts_with("QUIT") {
						let _ = write.write_all(b"221 bye\r\n").await;
						return;
					} else {
						b"250 OK\r\n"
					};
					write.write_all(reply).await.unwrap();
				}
			});
		}
	});
	(port, log)
}

// --- a team to work in ----------------------------------------------------------------------------

struct World {
	s: AppState,
	app: Router,
	token: String,
	team: TeamId,
	user: UserId,
}

const PUBLIC: &str = "https://pk.example";

async fn world(db: PgPool) -> World {
	let mut s = state(db.clone());
	let mut config = (*s.config).clone();
	config.public_url = Some(PUBLIC.to_owned());
	s.config = Arc::new(config);
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

	async fn channel(
		&self,
		name: &str,
		kind: ChannelKind,
		fields: ChannelFields,
	) -> NotificationChannelId {
		service::create_channel(&self.s, self.user, self.team, name, kind, &fields)
			.await
			.unwrap()
			.id
	}

	async fn rule(
		&self,
		name: &str,
		kinds: &[EventKind],
		channels: &[NotificationChannelId],
		scope: (Option<ProjectId>, Option<EnvironmentId>),
		min: Severity,
	) {
		service::create_rule(
			self.db(),
			self.user,
			self.team,
			RuleInput {
				name: name.to_owned(),
				kinds: kinds.iter().map(|k| k.as_str().to_owned()).collect(),
				channel_ids: channels.iter().map(|c| c.0).collect(),
				project_id: scope.0.map(|p| p.0),
				environment_id: scope.1.map(|e| e.0),
				min_severity: min,
				enabled: true,
			},
		)
		.await
		.unwrap();
	}

	fn event(&self, kind: EventKind, subject: &str, title: &str) -> Event {
		Event::new(self.team, kind, subject, title)
	}

	async fn project(&self, name: &str) -> (ProjectId, EnvironmentId) {
		let project = crypto::generate_id();
		sqlx::query("INSERT INTO projects (id, team_id, name, slug) VALUES ($1, $2, $3, $3)")
			.bind(project)
			.bind(self.team)
			.bind(name)
			.execute(self.db())
			.await
			.unwrap();
		// Creating a project (even by SQL) gives it its default environment.
		let env: i64 = sqlx::query_scalar("SELECT id FROM environments WHERE project_id = $1")
			.bind(project)
			.fetch_one(self.db())
			.await
			.unwrap();
		(ProjectId(project), EnvironmentId(env))
	}

	async fn server(&self, name: &str) -> i64 {
		let id = crypto::generate_id();
		sqlx::query(
			"INSERT INTO servers (id, team_id, name, hostname, podman_socket_path, is_local, status)
			VALUES ($1, $2, $3, 'localhost', '/run/podman.sock', TRUE, 'active')",
		)
		.bind(id)
		.bind(self.team)
		.bind(name)
		.execute(self.db())
		.await
		.unwrap();
		id
	}

	async fn deliveries(&self, status: &str) -> i64 {
		sqlx::query_scalar("SELECT COUNT(*) FROM notification_deliveries WHERE status = $1")
			.bind(status)
			.fetch_one(self.db())
			.await
			.unwrap()
	}

	async fn tick(&self, now: OffsetDateTime) -> delivery::Report {
		delivery::tick(&self.s, now).await.unwrap()
	}
}

fn webhook(url: &str, secret: &str) -> ChannelFields {
	ChannelFields {
		url: url.to_owned(),
		signing_secret: secret.to_owned(),
		..ChannelFields::default()
	}
}

fn url_only(url: &str) -> ChannelFields {
	ChannelFields {
		url: url.to_owned(),
		..ChannelFields::default()
	}
}

const NONE: (Option<ProjectId>, Option<EnvironmentId>) = (None, None);

fn now() -> OffsetDateTime {
	OffsetDateTime::now_utc()
}

// --- formatting, one channel at a time --------------------------------------------------------------

#[sqlx::test]
async fn notifications_discord_gets_an_embed_without_mentions(db: PgPool) {
	let w = world(db).await;
	let hook = stub().await;
	let c = w
		.channel(
			"chat",
			ChannelKind::Discord,
			url_only(&format!("{}/api/webhooks/1/tok3n", hook.url)),
		)
		.await;
	w.rule(
		"fails",
		&[EventKind::DeployFailed],
		&[c],
		NONE,
		Severity::Info,
	)
	.await;

	let event = w
		.event(
			EventKind::DeployFailed,
			"application:1",
			"Deploy of web failed",
		)
		.body("Build exited with 1")
		.link("/teams/1/projects/2/apps/3");
	pipeline::route(w.db(), event, now()).await.unwrap();
	assert_eq!(w.tick(now()).await.sent, 1);

	let got = hook.requests();
	assert_eq!(got.len(), 1);
	assert_eq!(got[0].path, "/api/webhooks/1/tok3n");
	let body = got[0].json();
	assert_eq!(body["embeds"][0]["title"], "Deploy of web failed");
	assert_eq!(body["embeds"][0]["description"], "Build exited with 1");
	assert_eq!(
		body["embeds"][0]["url"],
		format!("{PUBLIC}/teams/1/projects/2/apps/3")
	);
	assert_eq!(body["allowed_mentions"]["parse"], json!([]));
}

#[sqlx::test]
async fn notifications_slack_gets_blocks_with_a_link(db: PgPool) {
	let w = world(db).await;
	let hook = stub().await;
	let c = w
		.channel(
			"chat",
			ChannelKind::Slack,
			url_only(&format!("{}/services/T/B/x", hook.url)),
		)
		.await;
	w.rule(
		"fails",
		&[EventKind::DeployFailed],
		&[c],
		NONE,
		Severity::Info,
	)
	.await;
	let event = w
		.event(
			EventKind::DeployFailed,
			"application:1",
			"Deploy of <web> failed",
		)
		.link("/teams/1/projects/2/apps/3");
	pipeline::route(w.db(), event, now()).await.unwrap();
	w.tick(now()).await;

	let got = hook.requests();
	assert_eq!(got[0].path, "/services/T/B/x");
	let body = got[0].json().to_string();
	assert!(body.contains("Deploy of &lt;web&gt; failed"), "{body}");
	assert!(
		body.contains(&format!(
			"<{PUBLIC}/teams/1/projects/2/apps/3|Open in podkit>"
		)),
		"{body}"
	);
	assert!(body.contains("\"blocks\""), "{body}");
}

#[sqlx::test]
async fn notifications_telegram_posts_html_to_the_bot_api(db: PgPool) {
	let w = world(db).await;
	let api = stub().await;
	let c = w
		.channel(
			"bot",
			ChannelKind::Telegram,
			ChannelFields {
				token: "123:ABC".to_owned(),
				chat_id: "-1001".to_owned(),
				api_base: api.url.clone(),
				..ChannelFields::default()
			},
		)
		.await;
	w.rule(
		"fails",
		&[EventKind::DeployFailed],
		&[c],
		NONE,
		Severity::Info,
	)
	.await;
	pipeline::route(
		w.db(),
		w.event(EventKind::DeployFailed, "a:1", "Deploy of <web> failed"),
		now(),
	)
	.await
	.unwrap();
	w.tick(now()).await;

	let got = api.requests();
	assert_eq!(got[0].path, "/bot123:ABC/sendMessage");
	let body = got[0].json();
	assert_eq!(body["chat_id"], "-1001");
	assert_eq!(body["parse_mode"], "HTML");
	assert!(
		body["text"]
			.as_str()
			.unwrap()
			.contains("<b>Deploy of &lt;web&gt; failed</b>")
	);
}

#[sqlx::test]
async fn notifications_ntfy_publishes_json_with_priority_and_token(db: PgPool) {
	let w = world(db).await;
	let ntfy = stub().await;
	let c = w
		.channel(
			"phone",
			ChannelKind::Ntfy,
			ChannelFields {
				server: ntfy.url.clone(),
				topic: "podkit-alerts".to_owned(),
				token: "tk_secret".to_owned(),
				..ChannelFields::default()
			},
		)
		.await;
	w.rule(
		"down",
		&[EventKind::ServerUnreachable],
		&[c],
		NONE,
		Severity::Info,
	)
	.await;
	let event = w
		.event(
			EventKind::ServerUnreachable,
			"server:1",
			"Server nas is unreachable",
		)
		.link("/teams/1/servers/1");
	pipeline::route(w.db(), event, now()).await.unwrap();
	w.tick(now()).await;

	let got = ntfy.requests();
	assert_eq!(got.len(), 1);
	assert_eq!(got[0].path, "/");
	assert_eq!(got[0].header("authorization"), "Bearer tk_secret");
	let body = got[0].json();
	assert_eq!(body["topic"], "podkit-alerts");
	assert_eq!(body["title"], "Server nas is unreachable");
	assert_eq!(body["priority"], 5);
	assert_eq!(body["click"], format!("{PUBLIC}/teams/1/servers/1"));
}

#[sqlx::test]
async fn notifications_webhook_is_signed_with_hmac_sha256(db: PgPool) {
	let w = world(db).await;
	let hook = stub().await;
	let c = w
		.channel(
			"ops",
			ChannelKind::Webhook,
			webhook(&format!("{}/in", hook.url), "s3cret-signing"),
		)
		.await;
	w.rule(
		"fails",
		&[EventKind::DeployFailed],
		&[c],
		NONE,
		Severity::Info,
	)
	.await;
	pipeline::route(
		w.db(),
		w.event(EventKind::DeployFailed, "a:1", "Deploy failed"),
		now(),
	)
	.await
	.unwrap();
	w.tick(now()).await;

	let got = &hook.requests()[0];
	let mut mac = Hmac::<Sha256>::new_from_slice(b"s3cret-signing").unwrap();
	mac.update(&got.body);
	let mut expected = String::new();
	for byte in mac.finalize().into_bytes() {
		let _ = write!(expected, "{byte:02x}");
	}
	assert_eq!(
		got.header("x-podkit-signature"),
		format!("sha256={expected}")
	);
	assert_eq!(got.header("x-podkit-event"), "deploy_failed");
	let body = got.json();
	assert_eq!(body["event"], "deploy_failed");
	assert_eq!(body["severity"], "warning");
	assert_eq!(body["team_id"], w.team.0);
}

#[sqlx::test]
async fn notifications_email_goes_through_the_teams_smtp_server(db: PgPool) {
	let w = world(db).await;
	let (port, log) = smtp_stub().await;
	service::save_smtp(
		&w.s,
		w.user,
		w.team,
		SmtpInput {
			host: "127.0.0.1".to_owned(),
			port: i32::from(port),
			security: "none".to_owned(),
			username: String::new(),
			password: String::new(),
			from_addr: "podkit <podkit@example.com>".to_owned(),
		},
	)
	.await
	.unwrap();
	let c = w
		.channel(
			"mail",
			ChannelKind::Email,
			ChannelFields {
				to: "ops@example.com".to_owned(),
				..ChannelFields::default()
			},
		)
		.await;
	w.rule(
		"fails",
		&[EventKind::DeployFailed],
		&[c],
		NONE,
		Severity::Info,
	)
	.await;
	pipeline::route(
		w.db(),
		w.event(EventKind::DeployFailed, "a:1", "Deploy of web failed"),
		now(),
	)
	.await
	.unwrap();
	let report = w.tick(now()).await;
	assert_eq!(report.sent, 1, "{report:?}");

	let transcript = log.lock().unwrap().join("\n");
	assert!(
		transcript.contains("MAIL FROM:<podkit@example.com>"),
		"{transcript}"
	);
	assert!(
		transcript.contains("RCPT TO:<ops@example.com>"),
		"{transcript}"
	);
	assert!(
		transcript.contains("Subject: [podkit] Deploy of web failed"),
		"{transcript}"
	);
}

#[sqlx::test]
async fn notifications_an_email_channel_without_smtp_reports_why(db: PgPool) {
	let w = world(db).await;
	let c = w
		.channel(
			"mail",
			ChannelKind::Email,
			ChannelFields {
				to: "ops@example.com".to_owned(),
				..ChannelFields::default()
			},
		)
		.await;
	let outcome = service::test_channel(&w.s, w.user, w.team, c)
		.await
		.unwrap();
	assert!(!outcome.ok);
	assert!(
		outcome.message.contains("no SMTP server"),
		"{}",
		outcome.message
	);
}

// --- rules ----------------------------------------------------------------------------------------------

async fn matched(
	w: &World,
	kind: EventKind,
	severity: Severity,
	scope: (Option<ProjectId>, Option<EnvironmentId>),
) -> Vec<String> {
	pipeline::matching_channels(w.db(), w.team, kind, severity, scope.0, scope.1)
		.await
		.unwrap()
		.into_iter()
		.map(|c| c.name)
		.collect()
}

#[sqlx::test]
async fn notifications_rules_match_by_kind_scope_and_severity(db: PgPool) {
	let w = world(db).await;
	let (p1, e1) = w.project("one").await;
	let (p2, _e2) = w.project("two").await;
	let hook = stub().await;
	let mut ch = HashMap::new();
	for name in ["project", "team", "critical", "off", "other", "env"] {
		ch.insert(
			name,
			w.channel(
				name,
				ChannelKind::Discord,
				url_only(&format!("{}/{name}", hook.url)),
			)
			.await,
		);
	}
	w.rule(
		"p1 only",
		&[EventKind::DeployFailed],
		&[ch["project"]],
		(Some(p1), None),
		Severity::Info,
	)
	.await;
	w.rule(
		"everywhere",
		&[EventKind::DeployFailed, EventKind::Rollback],
		&[ch["team"]],
		NONE,
		Severity::Warning,
	)
	.await;
	w.rule(
		"critical",
		&[EventKind::DeployFailed],
		&[ch["critical"]],
		NONE,
		Severity::Critical,
	)
	.await;
	w.rule(
		"p1 env",
		&[EventKind::DeployFailed],
		&[ch["env"]],
		(Some(p1), Some(e1)),
		Severity::Info,
	)
	.await;
	w.rule(
		"backups",
		&[EventKind::BackupFailed],
		&[ch["other"]],
		NONE,
		Severity::Info,
	)
	.await;
	// A rule that is off sends nothing.
	w.rule(
		"disabled",
		&[EventKind::DeployFailed],
		&[ch["off"]],
		NONE,
		Severity::Info,
	)
	.await;
	let rules = service::rules(w.db(), w.user, w.team).await.unwrap();
	let disabled = rules.iter().find(|r| r.name == "disabled").unwrap();
	service::set_rule_enabled(w.db(), w.user, w.team, disabled.id, false)
		.await
		.unwrap();

	let mut got = matched(
		&w,
		EventKind::DeployFailed,
		Severity::Warning,
		(Some(p1), Some(e1)),
	)
	.await;
	got.sort();
	assert_eq!(got, ["env", "project", "team"]);
	assert_eq!(
		matched(
			&w,
			EventKind::DeployFailed,
			Severity::Warning,
			(Some(p2), None)
		)
		.await,
		["team"]
	);
	assert_eq!(
		matched(&w, EventKind::DeployFailed, Severity::Warning, NONE).await,
		["team"]
	);
	let mut crit = matched(
		&w,
		EventKind::DeployFailed,
		Severity::Critical,
		(Some(p2), None),
	)
	.await;
	crit.sort();
	assert_eq!(crit, ["critical", "team"]);
	// Below the minimum severity, and a kind nobody listed.
	let nothing = Vec::<String>::new();
	assert_eq!(
		matched(
			&w,
			EventKind::DeployFailed,
			Severity::Info,
			(Some(p2), None)
		)
		.await,
		nothing
	);
	assert_eq!(
		matched(&w, EventKind::ServerRecovered, Severity::Info, NONE).await,
		nothing
	);
	// Turning a channel off silences it everywhere.
	service::update_channel(
		&w.s,
		w.user,
		w.team,
		ch["team"],
		service::ChannelEdit {
			enabled: Some(false),
			..service::ChannelEdit::default()
		},
	)
	.await
	.unwrap();
	assert_eq!(
		matched(
			&w,
			EventKind::DeployFailed,
			Severity::Warning,
			(Some(p2), None)
		)
		.await,
		nothing
	);
}

#[sqlx::test]
async fn notifications_a_rule_must_name_events_and_own_channels(db: PgPool) {
	let w = world(db.clone()).await;
	let (_token_b, team_b) = user_with_team(&w.app, "bob").await;
	let bob = UserId(user_id(&db, "bob").await);
	let theirs = service::create_channel(
		&w.s,
		bob,
		TeamId(team_b),
		"theirs",
		ChannelKind::Discord,
		&url_only("https://example.com/h"),
	)
	.await
	.unwrap();
	let mine = w
		.channel(
			"mine",
			ChannelKind::Discord,
			url_only("https://example.com/m"),
		)
		.await;
	let input = |kinds: Vec<&str>, channels: Vec<i64>| RuleInput {
		name: "r".to_owned(),
		kinds: kinds.into_iter().map(str::to_owned).collect(),
		channel_ids: channels,
		project_id: None,
		environment_id: None,
		min_severity: Severity::Info,
		enabled: true,
	};
	let status = |r: Result<crate::ids::NotificationRuleId, crate::error::ServerError>| {
		r.unwrap_err().status()
	};
	assert_eq!(
		status(service::create_rule(w.db(), w.user, w.team, input(vec![], vec![mine.0])).await),
		StatusCode::BAD_REQUEST
	);
	assert_eq!(
		status(
			service::create_rule(w.db(), w.user, w.team, input(vec!["deploy_failed"], vec![]))
				.await
		),
		StatusCode::BAD_REQUEST
	);
	assert_eq!(
		status(
			service::create_rule(w.db(), w.user, w.team, input(vec!["nope"], vec![mine.0])).await
		),
		StatusCode::BAD_REQUEST
	);
	// Another team's channel does not exist.
	assert_eq!(
		status(
			service::create_rule(
				w.db(),
				w.user,
				w.team,
				input(vec!["deploy_failed"], vec![theirs.id.0])
			)
			.await
		),
		StatusCode::NOT_FOUND
	);
	// Nor does another team's project.
	let mut scoped = input(vec!["deploy_failed"], vec![mine.0]);
	scoped.project_id = Some(crypto::generate_id());
	assert_eq!(
		status(service::create_rule(w.db(), w.user, w.team, scoped).await),
		StatusCode::NOT_FOUND
	);
	assert_eq!(count(w.db(), "notification_rules").await, 0);
}

// --- noise control ----------------------------------------------------------------------------------------

#[sqlx::test]
async fn notifications_repeats_collapse_into_one_message_with_a_count(db: PgPool) {
	let w = world(db).await;
	let hook = stub().await;
	let c = w
		.channel(
			"chat",
			ChannelKind::Discord,
			url_only(&format!("{}/h", hook.url)),
		)
		.await;
	w.rule(
		"fails",
		&[EventKind::DeployFailed],
		&[c],
		NONE,
		Severity::Info,
	)
	.await;
	let t0 = now();
	let fail = || {
		w.event(
			EventKind::DeployFailed,
			"application:7",
			"Deploy of web failed",
		)
	};

	let first = pipeline::route(w.db(), fail(), t0).await.unwrap();
	assert!(
		matches!(first, Outcome::Recorded { deliveries: 1, .. }),
		"{first:?}"
	);
	for minutes in [1, 2] {
		let again = pipeline::route(w.db(), fail(), t0 + Duration::minutes(minutes))
			.await
			.unwrap();
		assert!(matches!(again, Outcome::Collapsed(_)), "{again:?}");
	}
	// Another subject is another event.
	let other = pipeline::route(
		w.db(),
		w.event(
			EventKind::DeployFailed,
			"application:8",
			"Deploy of api failed",
		),
		t0,
	)
	.await
	.unwrap();
	assert!(matches!(other, Outcome::Recorded { .. }));

	assert_eq!(
		w.tick(t0 + Duration::minutes(3)).await.sent,
		2,
		"one message per distinct event"
	);
	assert_eq!(count(w.db(), "notification_events").await, 2);
	let counted: i32 = sqlx::query_scalar(
		"SELECT count FROM notification_events WHERE event_key = 'deploy_failed:application:7'",
	)
	.fetch_one(w.db())
	.await
	.unwrap();
	assert_eq!(counted, 3);

	// When the window has ended, one follow-up tells how often it happened, and only once.
	assert_eq!(w.tick(t0 + Duration::minutes(5)).await.sent, 0);
	assert_eq!(w.tick(t0 + Duration::minutes(13)).await.sent, 1);
	assert_eq!(w.tick(t0 + Duration::minutes(30)).await.sent, 0);
	let last = hook.requests().pop().unwrap().json();
	assert_eq!(last["embeds"][0]["title"], "Deploy of web failed (×3)");

	// After the window the same event is new again.
	let later = pipeline::route(w.db(), fail(), t0 + Duration::minutes(40))
		.await
		.unwrap();
	assert!(matches!(later, Outcome::Recorded { .. }), "{later:?}");
}

#[sqlx::test]
async fn notifications_quiet_hours_hold_back_all_but_critical_and_send_one_digest(db: PgPool) {
	let w = world(db).await;
	let hook = stub().await;
	let c = w
		.channel(
			"chat",
			ChannelKind::Discord,
			url_only(&format!("{}/h", hook.url)),
		)
		.await;
	w.rule(
		"all",
		&[
			EventKind::DeployFailed,
			EventKind::ServerUnreachable,
			EventKind::BackupFailed,
		],
		&[c],
		NONE,
		Severity::Info,
	)
	.await;
	service::save_settings(
		w.db(),
		w.user,
		w.team,
		&Settings {
			quiet_enabled: true,
			quiet_start_min: 22 * 60,
			quiet_end_min: 7 * 60,
			..Settings::default()
		},
	)
	.await
	.unwrap();

	let night = datetime!(2026-01-01 23:00 UTC);
	pipeline::route(
		w.db(),
		w.event(EventKind::DeployFailed, "a:1", "Deploy of web failed"),
		night,
	)
	.await
	.unwrap();
	pipeline::route(
		w.db(),
		w.event(EventKind::BackupFailed, "d:1", "Backup of db failed"),
		night,
	)
	.await
	.unwrap();
	pipeline::route(
		w.db(),
		w.event(
			EventKind::ServerUnreachable,
			"server:1",
			"Server nas is unreachable",
		),
		night,
	)
	.await
	.unwrap();
	assert_eq!(w.deliveries("deferred").await, 2);

	// During the night only the critical one goes out.
	assert_eq!(w.tick(night + Duration::minutes(5)).await.sent, 1);
	assert_eq!(
		hook.requests()[0].json()["embeds"][0]["title"],
		"Server nas is unreachable"
	);
	assert_eq!(w.tick(night + Duration::hours(3)).await.sent, 0);
	assert_eq!(w.deliveries("deferred").await, 2);

	// When quiet hours end the two fold into one digest.
	let morning = datetime!(2026-01-02 07:30 UTC);
	assert_eq!(w.tick(morning).await.sent, 1);
	let digest = hook.requests().pop().unwrap().json();
	assert_eq!(
		digest["embeds"][0]["title"],
		"2 notifications while you were away"
	);
	let text = digest["embeds"][0]["description"].as_str().unwrap();
	assert!(
		text.contains("Deploy of web failed") && text.contains("Backup of db failed"),
		"{text}"
	);
	assert_eq!(w.deliveries("digested").await, 2);
	assert_eq!(w.deliveries("deferred").await, 0);

	// A single leftover is sent as itself, not as a digest of one.
	let night2 = datetime!(2026-01-02 23:30 UTC);
	pipeline::route(
		w.db(),
		w.event(EventKind::DeployFailed, "a:2", "Deploy of api failed"),
		night2,
	)
	.await
	.unwrap();
	w.tick(datetime!(2026-01-03 08:00 UTC)).await;
	assert_eq!(
		hook.requests().pop().unwrap().json()["embeds"][0]["title"],
		"Deploy of api failed"
	);
}

#[sqlx::test]
async fn notifications_flapping_up_down_pairs_are_muted_until_they_settle(db: PgPool) {
	let w = world(db).await;
	let hook = stub().await;
	let c = w
		.channel(
			"chat",
			ChannelKind::Discord,
			url_only(&format!("{}/h", hook.url)),
		)
		.await;
	w.rule(
		"servers",
		&[EventKind::ServerUnreachable, EventKind::ServerRecovered],
		&[c],
		NONE,
		Severity::Info,
	)
	.await;
	let t0 = now();
	let down = || {
		w.event(
			EventKind::ServerUnreachable,
			"server:1",
			"Server nas is unreachable",
		)
	};
	let up = || w.event(EventKind::ServerRecovered, "server:1", "Server nas is back");

	let mut shape = Vec::new();
	for (i, event) in [down(), up(), down(), up(), down(), up()]
		.into_iter()
		.enumerate()
	{
		let at = t0 + Duration::minutes(i64::try_from(i).unwrap());
		shape.push(match pipeline::route(w.db(), event, at).await.unwrap() {
			Outcome::Recorded { .. } => "recorded",
			Outcome::Collapsed(_) => "collapsed",
			Outcome::Muted => "muted",
			Outcome::Ignored => "ignored",
		});
	}
	// Down, up, a repeated down (same key inside the window), then the fourth change is flagged.
	assert_eq!(
		shape,
		[
			"recorded",
			"recorded",
			"collapsed",
			"recorded",
			"muted",
			"muted"
		]
	);
	let titles: Vec<String> =
		sqlx::query_scalar("SELECT title FROM notification_events ORDER BY id")
			.fetch_all(w.db())
			.await
			.unwrap();
	assert_eq!(titles.len(), 3);
	assert!(titles[2].starts_with("Flapping: "), "{titles:?}");

	// Half an hour of calm later the pair is judged afresh.
	let later = pipeline::route(w.db(), down(), t0 + Duration::minutes(40))
		.await
		.unwrap();
	assert!(matches!(later, Outcome::Recorded { .. }), "{later:?}");
}

// --- metric alerts -------------------------------------------------------------------------------------------

async fn sample(w: &World, server: i64, at: OffsetDateTime, cpu: f64, disk_used: i64) {
	sqlx::query(
		"INSERT INTO server_metrics (server_id, at, cpu, mem_used, mem_total, disk_used, disk_total, net_rx, net_tx)
		VALUES ($1, $2, $3, 1, 2, $4, 100, 0, 0)",
	)
	.bind(server)
	.bind(at)
	.bind(cpu)
	.bind(disk_used)
	.execute(w.db())
	.await
	.unwrap();
}

fn server_alert(
	name: &str,
	server: i64,
	metric: &str,
	over: f64,
	minutes: i32,
	severity: Severity,
) -> AlertInput {
	AlertInput {
		name: name.to_owned(),
		scope: "server".to_owned(),
		server_id: Some(server),
		resource_kind: None,
		resource_id: None,
		metric: metric.to_owned(),
		threshold: over,
		duration_minutes: minutes,
		severity,
		enabled: true,
	}
}

#[sqlx::test]
async fn notifications_a_metric_alert_fires_after_its_duration_and_recovers(db: PgPool) {
	let w = world(db).await;
	let hook = stub().await;
	let c = w
		.channel(
			"chat",
			ChannelKind::Discord,
			url_only(&format!("{}/h", hook.url)),
		)
		.await;
	w.rule(
		"metrics",
		&[EventKind::MetricAlert, EventKind::MetricRecovered],
		&[c],
		NONE,
		Severity::Info,
	)
	.await;
	let server = w.server("nas").await;
	service::create_alert(
		w.db(),
		w.user,
		w.team,
		server_alert("CPU high", server, "cpu", 90.0, 5, Severity::Warning),
	)
	.await
	.unwrap();

	let t0 = now();
	let at = |m: i64| t0 + Duration::minutes(m);
	// Over for four minutes: still quiet.
	for m in 0..=4 {
		sample(&w, server, at(m), 95.0, 10).await;
		assert_eq!(
			alerts::evaluate(&w.s, at(m)).await.unwrap(),
			0,
			"minute {m}"
		);
	}
	// The fifth minute completes the duration.
	sample(&w, server, at(5), 96.0, 10).await;
	assert_eq!(alerts::evaluate(&w.s, at(5)).await.unwrap(), 1);
	// Still over: no second message.
	sample(&w, server, at(6), 97.0, 10).await;
	assert_eq!(alerts::evaluate(&w.s, at(6)).await.unwrap(), 0);
	// Back under: one recovery.
	sample(&w, server, at(7), 12.0, 10).await;
	assert_eq!(alerts::evaluate(&w.s, at(7)).await.unwrap(), 1);
	sample(&w, server, at(8), 11.0, 10).await;
	assert_eq!(alerts::evaluate(&w.s, at(8)).await.unwrap(), 0);

	w.tick(now() + Duration::seconds(5)).await;
	let titles: Vec<String> = hook
		.requests()
		.iter()
		.map(|r| r.json()["embeds"][0]["title"].as_str().unwrap().to_owned())
		.collect();
	assert_eq!(titles.len(), 2, "{titles:?}");
	assert!(
		titles.iter().any(|t| t.contains("CPU on nas is at 96%")),
		"{titles:?}"
	);
	assert!(
		titles.iter().any(|t| t.contains("is back to 12%")),
		"{titles:?}"
	);
}

#[sqlx::test]
async fn notifications_a_dip_resets_the_clock_and_stale_samples_say_nothing(db: PgPool) {
	let w = world(db).await;
	let server = w.server("nas").await;
	service::create_alert(
		w.db(),
		w.user,
		w.team,
		server_alert("Disk", server, "disk", 90.0, 3, Severity::Critical),
	)
	.await
	.unwrap();
	let t0 = now();
	let at = |m: i64| t0 + Duration::minutes(m);
	sample(&w, server, at(0), 1.0, 95).await;
	alerts::evaluate(&w.s, at(0)).await.unwrap();
	sample(&w, server, at(2), 1.0, 50).await;
	alerts::evaluate(&w.s, at(2)).await.unwrap();
	sample(&w, server, at(3), 1.0, 95).await;
	assert_eq!(
		alerts::evaluate(&w.s, at(3)).await.unwrap(),
		0,
		"the dip started a new count"
	);
	// The newest sample is ten minutes old when judged: it proves nothing.
	assert_eq!(alerts::evaluate(&w.s, at(13)).await.unwrap(), 0);
	sample(&w, server, at(6), 1.0, 95).await;
	assert_eq!(alerts::evaluate(&w.s, at(6)).await.unwrap(), 1);
	// A disk alert is the "disk over a threshold" event.
	let kind: String = sqlx::query_scalar("SELECT kind FROM notification_events")
		.fetch_one(w.db())
		.await
		.unwrap();
	assert_eq!(kind, "disk_high");
}

#[sqlx::test]
async fn notifications_alerts_validate_their_target_and_numbers(db: PgPool) {
	let w = world(db.clone()).await;
	let server = w.server("nas").await;
	let base = server_alert("x", server, "cpu", 90.0, 5, Severity::Warning);
	let status =
		|r: Result<crate::ids::MetricAlertId, crate::error::ServerError>| r.unwrap_err().status();
	let with = |f: &dyn Fn(&mut AlertInput)| {
		let mut a = base.clone();
		f(&mut a);
		a
	};
	for bad in [
		with(&|a| a.threshold = 0.0),
		with(&|a| a.threshold = 150.0),
		with(&|a| a.duration_minutes = 0),
		with(&|a| a.metric = "mem_mb".to_owned()),
		with(&|a| a.metric = "bogus".to_owned()),
	] {
		assert_eq!(
			status(service::create_alert(w.db(), w.user, w.team, bad).await),
			StatusCode::BAD_REQUEST
		);
	}
	// Another team's server and resource do not exist.
	let (_t, team_b) = user_with_team(&w.app, "bob").await;
	let foreign = crypto::generate_id();
	sqlx::query(
		"INSERT INTO servers (id, team_id, name, hostname, podman_socket_path, is_local) VALUES ($1, $2, 'theirs', 'h', '/s', TRUE)",
	)
	.bind(foreign)
	.bind(team_b)
	.execute(&db)
	.await
	.unwrap();
	assert_eq!(
		status(
			service::create_alert(
				w.db(),
				w.user,
				w.team,
				with(&|a| a.server_id = Some(foreign))
			)
			.await
		),
		StatusCode::NOT_FOUND
	);
	let resource = with(&|a| {
		a.scope = "resource".to_owned();
		a.server_id = None;
		a.resource_kind = Some("application".to_owned());
		a.resource_id = Some(1);
		a.metric = "mem_mb".to_owned();
		a.threshold = 512.0;
	});
	assert_eq!(
		status(service::create_alert(w.db(), w.user, w.team, resource).await),
		StatusCode::NOT_FOUND
	);
	assert_eq!(count(w.db(), "metric_alerts").await, 0);
}

// --- delivery: retries, backoff, dead letters, rate limit ------------------------------------------------------

#[sqlx::test]
async fn notifications_failures_retry_with_backoff_then_become_dead_letters(db: PgPool) {
	let w = world(db).await;
	let hook = stub().await;
	hook.answer(500);
	let c = w
		.channel(
			"ops",
			ChannelKind::Webhook,
			webhook(&format!("{}/secret-path-tok", hook.url), "signing-key-1"),
		)
		.await;
	w.rule(
		"fails",
		&[EventKind::DeployFailed],
		&[c],
		NONE,
		Severity::Info,
	)
	.await;
	let t0 = now();
	pipeline::route(
		w.db(),
		w.event(EventKind::DeployFailed, "a:1", "Deploy failed"),
		t0,
	)
	.await
	.unwrap();

	let mut at = t0;
	for attempt in 1..MAX_ATTEMPTS {
		let report = w.tick(at).await;
		assert_eq!(report.retried, 1, "attempt {attempt}: {report:?}");
		let (attempts, next, error): (i32, OffsetDateTime, String) = sqlx::query_as(
			"SELECT attempts, next_attempt_at, last_error FROM notification_deliveries",
		)
		.fetch_one(w.db())
		.await
		.unwrap();
		assert_eq!(attempts, attempt);
		let wait = delivery::backoff(attempt);
		assert_eq!(wait.whole_seconds(), 30 * (1 << (attempt - 1)));
		assert!(
			(next - (at + wait)).abs() < Duration::milliseconds(2),
			"{next} vs {at}+{wait}"
		);
		assert!(error.starts_with("HTTP 500"), "{error}");
		assert!(
			!error.contains("secret-path-tok"),
			"the URL is a secret: {error}"
		);
		// Not due before the backoff has passed.
		assert_eq!(
			w.tick(at + wait - Duration::seconds(1)).await,
			delivery::Report::default()
		);
		at += wait;
	}
	let report = w.tick(at).await;
	assert_eq!(report.dead, 1, "{report:?}");
	assert_eq!(w.deliveries("dead").await, 1);
	assert_eq!(
		hook.requests().len(),
		usize::try_from(MAX_ATTEMPTS).unwrap()
	);

	// The log shows it, and a retry puts it back for a fresh set of attempts.
	let (recent, dead) = service::delivery_log(w.db(), w.user, w.team).await.unwrap();
	assert_eq!((recent.len(), dead.len()), (1, 1));
	hook.answer(200);
	service::retry_delivery(&w.s, w.user, w.team, dead[0].id)
		.await
		.unwrap();
	assert_eq!(w.tick(at + Duration::seconds(1)).await.sent, 1);
	assert_eq!(w.deliveries("sent").await, 1);
	assert_eq!(w.deliveries("dead").await, 0);
}

#[sqlx::test]
async fn notifications_each_channel_is_rate_limited(db: PgPool) {
	let w = world(db).await;
	let hook = stub().await;
	let c = w
		.channel(
			"chat",
			ChannelKind::Discord,
			url_only(&format!("{}/h", hook.url)),
		)
		.await;
	w.rule(
		"fails",
		&[EventKind::DeployFailed],
		&[c],
		NONE,
		Severity::Info,
	)
	.await;
	let t0 = now();
	let total = RATE_LIMIT_PER_MINUTE + 5;
	for i in 0..total {
		pipeline::route(
			w.db(),
			w.event(
				EventKind::DeployFailed,
				&format!("a:{i}"),
				&format!("Deploy {i} failed"),
			),
			t0,
		)
		.await
		.unwrap();
	}
	let report = w.tick(t0).await;
	assert_eq!(
		(i64::try_from(report.sent).unwrap(), report.rate_limited),
		(RATE_LIMIT_PER_MINUTE, 5),
		"{report:?}"
	);
	assert_eq!(w.deliveries("queued").await, 5);
	// Waiting a few seconds is not enough; a minute is.
	assert_eq!(w.tick(t0 + Duration::seconds(20)).await.sent, 0);
	assert_eq!(w.tick(t0 + Duration::seconds(61)).await.sent, 5);
	assert_eq!(i64::try_from(hook.requests().len()).unwrap(), total);
}

// --- the test button -----------------------------------------------------------------------------------------------

#[sqlx::test]
async fn notifications_send_test_reports_and_remembers_the_outcome_without_the_secret(db: PgPool) {
	let w = world(db).await;
	let hook = stub().await;
	let good = w
		.channel(
			"good",
			ChannelKind::Discord,
			url_only(&format!("{}/api/webhooks/9/tok3n-good", hook.url)),
		)
		.await;
	let outcome = service::test_channel(&w.s, w.user, w.team, good)
		.await
		.unwrap();
	assert!(outcome.ok, "{outcome:?}");
	let seen = hook.requests();
	assert_eq!(seen.len(), 1);
	assert_eq!(
		seen[0].json()["embeds"][0]["title"],
		"Test notification from podkit"
	);

	hook.answer(403);
	let refused = service::test_channel(&w.s, w.user, w.team, good)
		.await
		.unwrap();
	assert!(!refused.ok);
	assert!(
		refused.message.starts_with("HTTP 403"),
		"{}",
		refused.message
	);

	// Nothing listens there; the error must not repeat the address.
	let dead = w
		.channel(
			"dead",
			ChannelKind::Webhook,
			webhook("http://127.0.0.1:1/hidden-token-path", "signing-key-2"),
		)
		.await;
	let unreachable = service::test_channel(&w.s, w.user, w.team, dead)
		.await
		.unwrap();
	assert!(!unreachable.ok);
	assert!(
		!unreachable.message.contains("hidden-token-path"),
		"{}",
		unreachable.message
	);

	let channels = service::channels(w.db(), w.user, w.team).await.unwrap();
	let remembered = channels.iter().find(|c| c.id == good).unwrap();
	assert_eq!(remembered.last_test_ok, Some(false));
	assert!(
		remembered
			.last_test_error
			.as_deref()
			.unwrap()
			.starts_with("HTTP 403")
	);
}

// --- secrets and tenancy ----------------------------------------------------------------------------------------------

#[sqlx::test]
async fn notifications_secrets_are_sealed_and_never_returned(db: PgPool) {
	let w = world(db.clone()).await;
	let url = "https://discord.example/api/webhooks/42/SuperSecretToken";
	let base = format!("/api/v1/teams/{}/notifications", w.team);
	let (s, created) = call(
		&w.app,
		"POST",
		&format!("{base}/channels"),
		Some(&w.token),
		Some(json!({ "name": "chat", "kind": "discord", "url": url })),
	)
	.await;
	assert_eq!(s, StatusCode::OK, "{created}");
	let (_, listed) = call(
		&w.app,
		"GET",
		&format!("{base}/channels"),
		Some(&w.token),
		None,
	)
	.await;
	for body in [&created, &listed] {
		let text = body.to_string();
		assert!(
			!text.contains("SuperSecretToken") && !text.contains("webhooks/42"),
			"{text}"
		);
		assert!(
			text.contains("discord.example"),
			"the host is shown: {text}"
		);
	}
	let stored: Vec<u8> = sqlx::query_scalar("SELECT secret FROM notification_channels")
		.fetch_one(&db)
		.await
		.unwrap();
	assert!(!String::from_utf8_lossy(&stored).contains("SuperSecretToken"));
	let config: Value = sqlx::query_scalar("SELECT config FROM notification_channels")
		.fetch_one(&db)
		.await
		.unwrap();
	assert!(!config.to_string().contains("SuperSecretToken"));

	// Editing without a new secret keeps the stored one (it still opens).
	let id = created["id"].as_i64().unwrap();
	let (s, _) = call(
		&w.app,
		"PATCH",
		&format!("{base}/channels/{id}"),
		Some(&w.token),
		Some(json!({ "name": "renamed" })),
	)
	.await;
	assert_eq!(s, StatusCode::OK);
	let channel = delivery::load_channel(&db, NotificationChannelId(id))
		.await
		.unwrap()
		.unwrap();
	assert_eq!(channel.name, "renamed");
	let secret = super::channel::open_secret(&w.s.secrets, &channel).unwrap();
	assert_eq!(secret.url.as_deref(), Some(url));

	// SMTP: the password goes in once and only "has_password" comes out.
	let (s, _) = call(
		&w.app,
		"PUT",
		&format!("{base}/smtp"),
		Some(&w.token),
		Some(json!({
			"host": "smtp.example.com", "port": 587, "security": "starttls",
			"username": "u", "password": "smtp-pass-123", "from_addr": "podkit@example.com"
		})),
	)
	.await;
	assert_eq!(s, StatusCode::OK);
	let (_, smtp) = call(&w.app, "GET", &format!("{base}/smtp"), Some(&w.token), None).await;
	assert_eq!(smtp["has_password"], true);
	assert!(!smtp.to_string().contains("smtp-pass-123"));
	let sealed: Vec<u8> = sqlx::query_scalar("SELECT password FROM notification_smtp")
		.fetch_one(&db)
		.await
		.unwrap();
	assert!(!String::from_utf8_lossy(&sealed).contains("smtp-pass-123"));
}

#[sqlx::test]
async fn notifications_another_teams_things_are_404_and_non_members_are_refused(db: PgPool) {
	let w = world(db.clone()).await;
	let (bob, team_b) = user_with_team(&w.app, "bob").await;
	let mine = w
		.channel(
			"mine",
			ChannelKind::Discord,
			url_only("https://example.com/h"),
		)
		.await;
	w.rule(
		"r",
		&[EventKind::DeployFailed],
		&[mine],
		NONE,
		Severity::Info,
	)
	.await;
	let rule: i64 = sqlx::query_scalar("SELECT id FROM notification_rules")
		.fetch_one(&db)
		.await
		.unwrap();
	let base = format!("/api/v1/teams/{team_b}/notifications");
	let channel = format!("{base}/channels/{}", mine.0);
	let body = json!({ "name": "x", "kinds": ["deploy_failed"], "channel_ids": [mine.0] });

	// Bob is in his own team: alice's ids do not exist there.
	for (method, path) in [
		("PATCH", channel.clone()),
		("DELETE", channel.clone()),
		("POST", format!("{channel}/test")),
		("PATCH", format!("{base}/rules/{rule}")),
		("DELETE", format!("{base}/rules/{rule}")),
	] {
		let (s, _) = call(&w.app, method, &path, Some(&bob), Some(body.clone())).await;
		assert_eq!(s, StatusCode::NOT_FOUND, "{method} {path}");
	}
	let (_, theirs) = call(&w.app, "GET", &format!("{base}/channels"), Some(&bob), None).await;
	assert_eq!(theirs.as_array().unwrap().len(), 0);

	// A rule of Bob's cannot point at Alice's channel.
	let (s, _) = call(
		&w.app,
		"POST",
		&format!("{base}/rules"),
		Some(&bob),
		Some(body),
	)
	.await;
	assert_eq!(s, StatusCode::NOT_FOUND);
	assert_eq!(count(&db, "notification_rules").await, 1);

	// Not a member of alice's team at all: refused, on every route.
	let alice_base = format!("/api/v1/teams/{}/notifications", w.team);
	for path in [
		"channels",
		"rules",
		"alerts",
		"smtp",
		"settings",
		"deliveries",
	] {
		let (s, _) = call(
			&w.app,
			"GET",
			&format!("{alice_base}/{path}"),
			Some(&bob),
			None,
		)
		.await;
		assert_eq!(s, StatusCode::NOT_FOUND, "GET {path}");
	}
	let (s, _) = call(
		&w.app,
		"GET",
		&format!("{alice_base}/feed"),
		Some(&bob),
		None,
	)
	.await;
	assert_eq!(
		s,
		StatusCode::NOT_FOUND,
		"the bell treats an outsider as if the team did not exist"
	);
	let (s, _) = call(
		&w.app,
		"POST",
		&format!("{alice_base}/channels"),
		Some(&bob),
		Some(json!({ "name": "x", "kind": "discord", "url": "https://example.com/h" })),
	)
	.await;
	assert_eq!(s, StatusCode::NOT_FOUND);
	assert_eq!(count(&db, "notification_channels").await, 1);
}

#[sqlx::test]
async fn notifications_a_plain_member_may_read_but_not_change(db: PgPool) {
	let w = world(db.clone()).await;
	let (bob, _) = user_with_team(&w.app, "bob").await;
	let member = user_id(&db, "bob").await;
	// Role 2 is the default member role: read only on notifications.
	sqlx::query("INSERT INTO team_members (id, team_id, user_id, role_id) VALUES ($1, $2, $3, 2)")
		.bind(crypto::generate_id())
		.bind(w.team)
		.bind(member)
		.execute(&db)
		.await
		.unwrap();
	let base = format!("/api/v1/teams/{}/notifications", w.team);
	let (s, _) = call(&w.app, "GET", &format!("{base}/channels"), Some(&bob), None).await;
	assert_eq!(s, StatusCode::OK);
	let (s, _) = call(
		&w.app,
		"POST",
		&format!("{base}/channels"),
		Some(&bob),
		Some(json!({ "name": "x", "kind": "discord", "url": "https://example.com/h" })),
	)
	.await;
	assert_eq!(s, StatusCode::FORBIDDEN);
	let (s, _) = call(
		&w.app,
		"PUT",
		&format!("{base}/settings"),
		Some(&bob),
		Some(json!({
			"dedup_minutes": 5, "quiet_enabled": false, "quiet_start_min": 0,
			"quiet_end_min": 0, "utc_offset_min": 0
		})),
	)
	.await;
	assert_eq!(s, StatusCode::FORBIDDEN);
	assert_eq!(count(&db, "notification_channels").await, 0);
}

// --- the bell ---------------------------------------------------------------------------------------------------------------

#[sqlx::test]
async fn notifications_the_bell_counts_unread_per_person_and_marks_read(db: PgPool) {
	let w = world(db.clone()).await;
	let (_bob_token, _) = user_with_team(&w.app, "bob").await;
	let bob = UserId(user_id(&db, "bob").await);
	sqlx::query("INSERT INTO team_members (id, team_id, user_id, role_id) VALUES ($1, $2, $3, 2)")
		.bind(crypto::generate_id())
		.bind(w.team)
		.bind(bob)
		.execute(&db)
		.await
		.unwrap();

	// Warnings reach the bell with no rule at all; info that nobody asked for does not.
	for (i, kind) in [
		EventKind::DeployFailed,
		EventKind::BackupFailed,
		EventKind::DeploySucceeded,
	]
	.into_iter()
	.enumerate()
	{
		pipeline::route(
			w.db(),
			w.event(kind, &format!("a:{i}"), &format!("Event {i}")),
			now(),
		)
		.await
		.unwrap();
	}
	assert_eq!(count(w.db(), "notification_events").await, 2);
	assert_eq!(
		service::unread_count(w.db(), w.user, w.team).await.unwrap(),
		2
	);
	assert_eq!(service::unread_count(w.db(), bob, w.team).await.unwrap(), 2);

	let feed = service::feed(w.db(), w.user, w.team, 10).await.unwrap();
	assert_eq!(feed.len(), 2);
	assert!(feed.iter().all(|f| f.unread));
	service::mark_read(w.db(), w.user, w.team, feed[0].id)
		.await
		.unwrap();
	assert_eq!(
		service::unread_count(w.db(), w.user, w.team).await.unwrap(),
		1
	);
	assert_eq!(
		service::unread_count(w.db(), bob, w.team).await.unwrap(),
		2,
		"reading is per person"
	);
	service::mark_all_read(w.db(), bob, w.team).await.unwrap();
	assert_eq!(service::unread_count(w.db(), bob, w.team).await.unwrap(), 0);
	assert_eq!(
		service::unread_count(w.db(), w.user, w.team).await.unwrap(),
		1
	);

	// A later event is unread again; an outsider has no bell for this team.
	pipeline::route(
		w.db(),
		w.event(EventKind::DeployFailed, "a:9", "Event 9"),
		now(),
	)
	.await
	.unwrap();
	assert_eq!(service::unread_count(w.db(), bob, w.team).await.unwrap(), 1);
	let (_t, other_team) = user_with_team(&w.app, "carol").await;
	let carol = UserId(user_id(&db, "carol").await);
	assert_eq!(
		service::unread_count(w.db(), carol, w.team).await.unwrap(),
		0
	);
	assert_eq!(
		service::feed(w.db(), carol, w.team, 10)
			.await
			.unwrap_err()
			.status(),
		StatusCode::NOT_FOUND
	);
	assert_eq!(
		service::mark_read(w.db(), carol, TeamId(other_team), feed[1].id)
			.await
			.unwrap_err()
			.status(),
		StatusCode::NOT_FOUND
	);
	assert_eq!(
		service::mark_all_read(w.db(), carol, w.team)
			.await
			.unwrap_err()
			.status(),
		StatusCode::NOT_FOUND
	);
}

// --- raising events from the rest of podkit -----------------------------------------------------------------------------------

/// Puts a server, an application and a failed deployment with its job in the team.
async fn seed_deployment(w: &World) -> (ProjectId, EnvironmentId, crate::jobs::Job) {
	let (project, env) = w.project("web").await;
	let server = w.server("nas").await;
	let app = crypto::generate_id();
	sqlx::query(
		"INSERT INTO applications (id, project_id, server_id, name, slug, repo_url, container_port, environment_id)
		VALUES ($1, $2, $3, 'Web', 'web', 'https://example.com/r.git', 3000, $4)",
	)
	.bind(app)
	.bind(project)
	.bind(server)
	.bind(env)
	.execute(w.db())
	.await
	.unwrap();
	let deployment = crypto::generate_id();
	sqlx::query("INSERT INTO deployments (id, application_id, status) VALUES ($1, $2, 'failed')")
		.bind(deployment)
		.bind(app)
		.execute(w.db())
		.await
		.unwrap();
	let job = crate::jobs::Job {
		id: crate::ids::JobId(crypto::generate_id()),
		kind: crate::jobs::JobKind::Deploy,
		server_id: crate::ids::ServerId(server),
		target_id: deployment,
		lock_key: format!("app:{app}"),
		status: crate::jobs::JobStatus::Failed,
		error: Some("boom".to_owned()),
	};
	(project, env, job)
}

#[sqlx::test]
async fn notifications_a_failed_deploy_raises_one_event_scoped_to_its_project(db: PgPool) {
	let w = world(db).await;
	let hook = stub().await;
	let (project, env, job) = seed_deployment(&w).await;
	let c = w
		.channel(
			"chat",
			ChannelKind::Discord,
			url_only(&format!("{}/h", hook.url)),
		)
		.await;
	w.rule(
		"web fails",
		&[EventKind::DeployFailed],
		&[c],
		(Some(project), Some(env)),
		Severity::Info,
	)
	.await;

	let failure = Err(crate::jobs::JobFailure::Failed(
		"image pull failed".to_owned(),
	));
	super::hooks::job_finished(&w.s, &job, &failure).await;
	// A cancelled job is the user's doing and says nothing.
	super::hooks::job_finished(&w.s, &job, &Err(crate::jobs::JobFailure::Cancelled)).await;
	assert_eq!(w.tick(now() + Duration::seconds(1)).await.sent, 1);

	let got = hook.requests();
	assert_eq!(got.len(), 1);
	let body = got[0].json();
	assert_eq!(body["embeds"][0]["title"], "Deploy of Web failed");
	assert_eq!(body["embeds"][0]["description"], "image pull failed");
	assert!(
		body["embeds"][0]["url"]
			.as_str()
			.unwrap()
			.ends_with("/deployments")
	);
	let (kind, p, e): (String, Option<i64>, Option<i64>) =
		sqlx::query_as("SELECT kind, project_id, environment_id FROM notification_events")
			.fetch_one(w.db())
			.await
			.unwrap();
	assert_eq!(
		(kind.as_str(), p, e),
		("deploy_failed", Some(project.0), Some(env.0))
	);

	// A success is info: it needs a rule of its own to be sent.
	super::hooks::job_finished(&w.s, &job, &Ok(())).await;
	assert_eq!(w.tick(now() + Duration::seconds(2)).await.sent, 0);
}

// --- live: a real ntfy server ------------------------------------------------------------------------------------------------

const LIVE_NAME: &str = "pk-t_ff8dba23-ntfy";
const LIVE_PORT: u16 = 23871;

/// Removes the container however the test ends.
struct Container;

impl Drop for Container {
	fn drop(&mut self) {
		let _ = std::process::Command::new(podman())
			.args(["rm", "-f", LIVE_NAME])
			.output();
	}
}

fn podman() -> String {
	std::env::var("PODMAN").unwrap_or_else(|_| "podman".to_owned())
}

/// A failed deploy produces exactly one message on a throwaway `binwiederhier/ntfy` container.
/// `PODMAN=/path/to/podman cargo test -p podkit-core --all-features -- --ignored notifications_live`
#[sqlx::test]
#[ignore = "needs podman and pulls binwiederhier/ntfy"]
async fn notifications_live_a_failed_deploy_sends_exactly_one_ntfy_message(db: PgPool) {
	let _ = rustls::crypto::ring::default_provider().install_default();
	let _cleanup = Container;
	let out = std::process::Command::new(podman())
		.args([
			"run",
			"-d",
			"--rm",
			"--name",
			LIVE_NAME,
			"-p",
			&format!("127.0.0.1:{LIVE_PORT}:80"),
			"docker.io/binwiederhier/ntfy:latest",
			"serve",
		])
		.output()
		.expect("podman runs");
	assert!(
		out.status.success(),
		"{}",
		String::from_utf8_lossy(&out.stderr)
	);
	let server = format!("http://127.0.0.1:{LIVE_PORT}");
	let http = reqwest::Client::new();
	let mut ready = false;
	for _ in 0..60 {
		if let Ok(r) = http.get(format!("{server}/v1/health")).send().await
			&& r.status().is_success()
		{
			ready = true;
			break;
		}
		tokio::time::sleep(std::time::Duration::from_secs(1)).await;
	}
	assert!(ready, "ntfy did not come up");

	let w = world(db).await;
	let (project, env, job) = seed_deployment(&w).await;
	let c = w
		.channel(
			"phone",
			ChannelKind::Ntfy,
			ChannelFields {
				server: server.clone(),
				topic: "podkit-live-test".to_owned(),
				..ChannelFields::default()
			},
		)
		.await;
	w.rule(
		"deploys",
		&[EventKind::DeployFailed],
		&[c],
		(Some(project), Some(env)),
		Severity::Info,
	)
	.await;

	// The same failure reported twice inside the dedup window is still one message.
	let failure = Err(crate::jobs::JobFailure::Failed("boom".to_owned()));
	super::hooks::job_finished(&w.s, &job, &failure).await;
	super::hooks::job_finished(&w.s, &job, &failure).await;
	assert_eq!(w.tick(now() + Duration::seconds(1)).await.sent, 1);
	assert_eq!(w.tick(now() + Duration::seconds(30)).await.sent, 0);

	let polled = http
		.get(format!("{server}/podkit-live-test/json?poll=1"))
		.send()
		.await
		.unwrap()
		.text()
		.await
		.unwrap();
	let messages: Vec<Value> = polled
		.lines()
		.filter_map(|l| serde_json::from_str::<Value>(l).ok())
		.filter(|m| m["event"] == "message")
		.collect();
	assert_eq!(messages.len(), 1, "{polled}");
	assert_eq!(messages[0]["title"], "Deploy of Web failed");
	assert_eq!(messages[0]["message"], "boom");
}
