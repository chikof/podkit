//! What the scanning tests share with the tests of the Security pages: the fixture image's
//! report, a fake agent that answers scans with it, a team with something to scan, and the
//! SQL the page tests read and bend.

use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Bytes;
use axum::routing::post;
use proto::ScanResult;
use serde_json::{Value, json};
use sqlx::PgPool;
use tokio_util::sync::CancellationToken;

use super::{call, seed_app_and_database, server_id, state, user_id, user_with_team};
use crate::ids::{ApplicationId, ScanId, ServerId, TeamId, UserId};
use crate::jobs::{self, JobKind};
use crate::scanning::store;
use crate::scanning::{Counts, ResourceKind, Subject, Target};
use crate::state::AppState;

pub const REPORT: &str = include_str!("../../../../agent/tests/fixtures/grype-report.json");
pub const SBOM: &str = include_str!("../../tests/fixtures/sbom.cdx.json");
pub const DIGEST: &str = "sha256:1111111111111111111111111111111111111111111111111111111111111111";
pub const IMAGE: &str = "docker.io/library/alpine:3.14";

/// What Grype and Syft would answer for the fixture image.
pub fn result() -> ScanResult {
	let (findings, db_built) = agent::scan::parse_grype(REPORT).unwrap();
	ScanResult {
		image_id: DIGEST.to_owned(),
		sbom: Some(SBOM.to_owned()),
		findings,
		db_built,
		tools: "syft@1.57.2 grype@0.122.1".to_owned(),
	}
}

#[derive(Clone, Default)]
pub struct Calls {
	scans: Arc<Mutex<Vec<Value>>>,
}

impl Calls {
	pub fn scans(&self) -> Vec<Value> {
		self.scans.lock().unwrap().clone()
	}
}

/// A fake agent on the local server's socket: resolves every image to [`DIGEST`],
/// answers scans with [`result`] (without an SBOM for a rescan), and records them.
pub fn fake_agent(state: &AppState) -> Calls {
	let calls = Calls::default();
	let scans = calls.scans.clone();
	let router = Router::new()
		.route(
			"/v1/scan/resolve",
			post(|| async { axum::Json(json!({ "image_id": DIGEST })) }),
		)
		.route(
			"/v1/scan",
			post(move |body: Bytes| async move {
				let request: Value = serde_json::from_slice(&body).unwrap();
				let mut answer = result();
				if request.get("image").is_none_or(Value::is_null) {
					answer.sbom = None;
					answer.image_id = String::new();
				}
				scans.lock().unwrap().push(request);
				axum::Json(answer)
			}),
		);
	let listener = tokio::net::UnixListener::bind(&state.config.local_agent_socket).unwrap();
	tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
	calls
}

pub struct Fx {
	pub app: Router,
	pub state: AppState,
	pub db: PgPool,
	pub token: String,
	pub team: i64,
	pub project: i64,
	pub server: i64,
}

impl Fx {
	pub async fn new(db: PgPool) -> Self {
		let state = state(db.clone());
		let app = crate::routes::routes(state.clone()).await.unwrap();
		Self::over(db, state, app).await
	}

	/// [`Fx::new`] over a router somebody else built on `state`: `podkit-ui` passes the one
	/// with the pages.
	pub async fn over(db: PgPool, state: AppState, app: Router) -> Self {
		let (token, team) = user_with_team(&app, "alice").await;
		let (s, project) = call(
			&app,
			"POST",
			&format!("/api/v1/teams/{team}/projects"),
			Some(&token),
			Some(json!({ "name": "web", "slug": "web" })),
		)
		.await;
		assert!(s.is_success(), "create project: {s}");
		let project = project["id"].as_i64().unwrap();
		seed_app_and_database(&db, team, project).await;
		let server = server_id(&db, team).await;
		Self {
			app,
			state,
			db,
			token,
			team,
			project,
			server,
		}
	}

	pub fn url(&self, tail: &str) -> String {
		format!("/api/v1/teams/{}/security{tail}", self.team)
	}

	pub fn target(image: &str) -> Target<'_> {
		Target {
			kind: ResourceKind::Application,
			resource_id: 5001,
			image_ref: image,
			deployment: None,
		}
	}

	/// Queues a scan of the seeded application's image, as a deploy does.
	pub async fn queue(&self, image: &str) -> ScanId {
		crate::scanning::queue(&self.state, ServerId(self.server), Self::target(image))
			.await
			.expect("a scan is queued")
	}

	/// Runs a scan's job the way the worker does.
	pub async fn run(&self, scan: ScanId) -> Result<(), crate::jobs::JobFailure> {
		let job = jobs::latest_for(&self.db, JobKind::ImageScan, scan.0)
			.await
			.unwrap()
			.expect("a scan job is queued");
		let mut log = self.state.logs.open(job.id).await.unwrap();
		let outcome =
			crate::scanning::run::run(&self.state, &job, &CancellationToken::new(), &mut log).await;
		log.finish().await;
		outcome
	}

	pub async fn scan(&self, id: ScanId) -> store::Scan {
		store::find(&self.db, id).await.unwrap().unwrap()
	}

	pub async fn counts(&self, id: ScanId) -> Counts {
		store::effective_counts(&self.db, TeamId(self.team), &[id])
			.await
			.unwrap()
			.get(&id)
			.copied()
			.unwrap_or_default()
	}

	pub async fn alice(&self) -> UserId {
		UserId(user_id(&self.db, "alice").await)
	}

	pub fn app_subject() -> Subject {
		Subject::Application(ApplicationId(5001))
	}
}

// --- what the page tests read and bend, kept here so the UI never touches the database ---

/// The id of the first scan stored.
pub async fn first_scan_id(db: &PgPool) -> i64 {
	sqlx::query_scalar("SELECT id FROM image_scans ORDER BY id LIMIT 1")
		.fetch_one(db)
		.await
		.unwrap()
}

/// The id of the first ignore stored.
pub async fn first_ignore_id(db: &PgPool) -> i64 {
	sqlx::query_scalar("SELECT id FROM scan_ignores ORDER BY id LIMIT 1")
		.fetch_one(db)
		.await
		.unwrap()
}

/// Makes the scanner's link of a finding something hostile.
pub async fn set_finding_url(db: &PgPool, vuln: &str, url: &str) {
	sqlx::query("UPDATE scan_findings SET url = $2 WHERE vuln_id = $1")
		.bind(vuln)
		.bind(url)
		.execute(db)
		.await
		.unwrap();
}

/// Makes the package name of a finding something hostile.
pub async fn set_finding_package(db: &PgPool, vuln: &str, package: &str) {
	sqlx::query("UPDATE scan_findings SET package = $2 WHERE vuln_id = $1")
		.bind(vuln)
		.bind(package)
		.execute(db)
		.await
		.unwrap();
}

/// A server's scanning switch and caps as stored: on, CPUs, MiB.
pub async fn server_scan_settings(db: &PgPool, server: i64) -> (bool, f64, i32) {
	sqlx::query_as("SELECT scan_enabled, scan_cpus, scan_memory_mb FROM servers WHERE id = $1")
		.bind(server)
		.fetch_one(db)
		.await
		.unwrap()
}
