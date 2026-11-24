//! A stand-in agent with a real log store behind it, and the seeding the log search tests share.
//! The store is the agent's own (`agent::logstore`), so a search here runs the real query engine;
//! only Podman and the collector are left out.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use agent::logstore::LogStore;
use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::{get, post, put};
use axum::{Json, Router};
use proto::{LogConfig, LogLevel, LogRecord, LogSearchRequest, LogSearchResult, LogStatus, Stream};

use crate::state::AppState;

/// What the stand-in agent remembers of the control plane's requests.
#[derive(Default)]
pub struct Seen {
	pub searches: Mutex<Vec<LogSearchRequest>>,
	pub configs: Mutex<Vec<LogConfig>>,
}

/// The agent's log store and what was asked of it.
#[derive(Clone)]
pub struct StubAgent {
	pub store: Arc<LogStore>,
	pub seen: Arc<Seen>,
	/// What `/v1/logs/status` says was dropped on the way to the backend.
	pub dropped: Arc<AtomicU64>,
}

#[derive(Clone)]
struct Shared {
	store: Arc<LogStore>,
	seen: Arc<Seen>,
	dropped: Arc<AtomicU64>,
}

async fn search(
	State(a): State<Shared>,
	Json(req): Json<LogSearchRequest>,
) -> Result<Json<LogSearchResult>, (StatusCode, Json<proto::ErrorBody>)> {
	a.seen
		.searches
		.lock()
		.unwrap_or_else(PoisonError::into_inner)
		.push(req.clone());
	a.store.search(req).await.map(Json).map_err(|e| {
		(
			StatusCode::BAD_REQUEST,
			Json(proto::ErrorBody {
				error: e.to_string(),
			}),
		)
	})
}

/// Starts the agent on the app's local agent socket.
pub fn stub_agent(state: &AppState) -> StubAgent {
	let dir = tempfile::tempdir().unwrap().keep();
	let shared = Shared {
		store: Arc::new(LogStore::open(&dir)),
		seen: Arc::new(Seen::default()),
		dropped: Arc::new(AtomicU64::new(0)),
	};
	let app = Router::new()
		.route("/v1/logs/search", post(search))
		.route(
			"/v1/logs/config",
			put(
				|State(a): State<Shared>, Json(config): Json<LogConfig>| async move {
					a.seen
						.configs
						.lock()
						.unwrap_or_else(PoisonError::into_inner)
						.push(config);
					StatusCode::NO_CONTENT
				},
			),
		)
		.route(
			"/v1/logs/status",
			get(|State(a): State<Shared>| async move {
				Json(LogStatus {
					export_dropped: a.dropped.load(Ordering::Relaxed),
					..LogStatus::default()
				})
			}),
		)
		.with_state(shared.clone());
	let listener = tokio::net::UnixListener::bind(&state.config.local_agent_socket).unwrap();
	tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
	StubAgent {
		store: shared.store,
		seen: shared.seen,
		dropped: shared.dropped,
	}
}

impl StubAgent {
	/// Stores one line of `svc` (`app-5001`) as the collector would.
	pub fn line(&self, svc: &str, ts: i64, dep: Option<i64>, level: Option<LogLevel>, text: &str) {
		let record = LogRecord {
			ts,
			stream: Stream::Stdout,
			dep,
			level,
			line: text.to_owned(),
			svc: String::new(),
		};
		self.store.append(svc, &[record]).unwrap();
	}

	pub fn searches(&self) -> Vec<LogSearchRequest> {
		self.seen
			.searches
			.lock()
			.unwrap_or_else(PoisonError::into_inner)
			.clone()
	}

	pub fn configs(&self) -> Vec<LogConfig> {
		self.seen
			.configs
			.lock()
			.unwrap_or_else(PoisonError::into_inner)
			.clone()
	}
}

/// Now, in unix milliseconds.
pub fn now_ms() -> i64 {
	i64::try_from(time::OffsetDateTime::now_utc().unix_timestamp_nanos() / 1_000_000).unwrap()
}

/// The sealed header stored for `team`, if any.
pub async fn sealed_header(db: &sqlx::PgPool, team: i64) -> Option<Vec<u8>> {
	sqlx::query_scalar("SELECT export_authorization FROM log_settings WHERE team_id = $1")
		.bind(team)
		.fetch_optional(db)
		.await
		.unwrap()
		.flatten()
}

/// Makes the user called `name` a plain member (role 2) of `team`.
pub async fn add_member(db: &sqlx::PgPool, team: i64, name: &str) {
	sqlx::query(
		"INSERT INTO team_members (id, team_id, user_id, role_id)
		 SELECT $3, $1, id, 2 FROM users WHERE name = $2",
	)
	.bind(team)
	.bind(name)
	.bind(crypto::generate_id())
	.execute(db)
	.await
	.unwrap();
}
