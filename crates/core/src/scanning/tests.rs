//! Scanning tests. The parsing of Grype's report is tested in the agent
//! (`cargo test -p agent -- scan`); the same fixture feeds these, which run
//! the server's half against a fake agent on the local socket.
//!
//! `cargo test -p podkit-core --all-features -- scanning`

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use axum::response::IntoResponse;
use axum::routing::post;
use serde_json::{Value, json};
use sqlx::PgPool;
use time::{Duration, OffsetDateTime};
use tokio_util::sync::CancellationToken;
use tower::ServiceExt;

use super::service::{self, NewIgnore};
use super::store::{self, FindingFilter};
use super::{
	Badge, BadgeState, Counts, ResourceKind, ScanStatus, Severity, Subject, Target, is_act_now,
};
use crate::error::ServerError;
use crate::ids::{ApplicationId, DatabaseId, ProjectId, ScanId, ServerId, TeamId};
use crate::jobs::{self, JobKind};
use crate::tests::scanning::{DIGEST, Fx, IMAGE, SBOM, fake_agent, result};
use crate::tests::{call, count, member_role, revoke_permission, user_with_team};

fn ignore_new() -> NewIgnore {
	NewIgnore {
		vuln_id: "CVE-2022-0001".to_owned(),
		package: None,
		reason: "because".to_owned(),
		days: 7,
	}
}

#[test]
fn scanning_act_now_is_kev_or_likely_and_severe() {
	assert!(is_act_now(true, None, None), "KEV alone is enough");
	assert!(
		is_act_now(false, Some(0.10), Some(7.0)),
		"both thresholds met exactly"
	);
	assert!(is_act_now(false, Some(0.9), Some(9.8)));
	assert!(
		!is_act_now(false, Some(0.099), Some(9.8)),
		"EPSS just under"
	);
	assert!(!is_act_now(false, Some(0.9), Some(6.9)), "CVSS just under");
	assert!(!is_act_now(false, Some(0.9), None), "no CVSS is not severe");
	assert!(!is_act_now(false, None, Some(9.8)), "no EPSS is not likely");
	assert!(!is_act_now(false, None, None));
}

#[test]
fn scanning_counts_the_fixture_by_severity() {
	let findings = result().findings;
	assert_eq!(findings.len(), 5);
	let counts = Counts::of(&findings);
	assert_eq!(
		counts,
		Counts {
			critical: 1,
			high: 1,
			medium: 1,
			low: 1,
			unknown: 1,
			act_now: 2
		}
	);
	assert_eq!(counts.worst(), Some(Severity::Critical));
	assert_eq!(counts.total(), 5);
	assert_eq!(
		counts.summary(),
		"1 critical, 1 high, 1 medium, 1 low, 1 unknown (2 act now)"
	);
	assert_eq!(Counts::default().summary(), "no known vulnerabilities");
	assert_eq!(Badge::found(Counts::default()).state, BadgeState::Clean);
	assert_eq!(
		Badge::found(Counts {
			medium: 2,
			..Counts::default()
		})
		.state,
		BadgeState::Found(Severity::Medium)
	);
}

#[sqlx::test]
async fn scanning_a_job_stores_findings_counts_and_the_sbom(db: PgPool) {
	let f = Fx::new(db).await;
	let calls = fake_agent(&f.state);
	let scan = f.queue(IMAGE).await;
	assert_eq!(f.scan(scan).await.status, ScanStatus::Queued);

	f.run(scan).await.unwrap();

	let stored = f.scan(scan).await;
	assert_eq!(stored.status, ScanStatus::Succeeded);
	assert_eq!(stored.image_digest, DIGEST);
	assert_eq!((stored.critical, stored.high, stored.act_now), (1, 1, 2));
	assert_eq!(stored.db_built.as_deref(), Some("2026-11-19T18:41:05Z"));
	assert!(stored.sbom_bytes.is_some());
	assert!(stored.finished_at.is_some());
	assert_eq!(count(&f.db, "scan_findings").await, 5);

	// The agent was asked for the image, with the server's caps.
	let sent = calls.scans();
	assert_eq!(sent.len(), 1);
	assert_eq!(sent[0]["image"], IMAGE);
	assert_eq!(sent[0]["cpus"], 1.0);
	assert_eq!(sent[0]["memory_mb"], 1024);

	// What the SBOM endpoint serves is what Syft produced.
	let packed = store::sbom_of(&f.db, scan).await.unwrap().unwrap();
	assert_eq!(super::decompress(&packed).unwrap(), SBOM);
}

#[sqlx::test]
async fn scanning_a_failed_scan_says_why_and_touches_nothing_else(db: PgPool) {
	let f = Fx::new(db).await;
	// No agent at all: the scan fails, and that is all that happens.
	let scan = f.queue(IMAGE).await;
	let failure = f.run(scan).await.unwrap_err();
	assert!(matches!(failure, crate::jobs::JobFailure::Failed(_)));
	jobs::fail_target(&f.state, JobKind::ImageScan, scan.0, &failure).await;

	let stored = f.scan(scan).await;
	assert_eq!(stored.status, ScanStatus::Failed);
	assert!(stored.error.is_some());
	let badge = service::subject_scans(
		&f.db,
		f.alice().await,
		TeamId(f.team),
		ProjectId(f.project),
		Fx::app_subject(),
	)
	.await
	.unwrap()
	.badge;
	assert_eq!(badge.state, BadgeState::Failed);
}

#[sqlx::test]
async fn scanning_reuses_a_scan_of_the_same_digest_for_a_day(db: PgPool) {
	let f = Fx::new(db).await;
	let calls = fake_agent(&f.state);

	let first = f.queue(IMAGE).await;
	f.run(first).await.unwrap();
	assert_eq!(calls.scans().len(), 1);

	// A second resource runs the same image: it shows the first scan, no second scan.
	let second = super::queue(
		&f.state,
		ServerId(f.server),
		Target {
			kind: ResourceKind::Database,
			resource_id: 6001,
			image_ref: "docker.io/library/alpine:3.14",
			deployment: None,
		},
	)
	.await
	.unwrap();
	f.run(second).await.unwrap();
	assert_eq!(calls.scans().len(), 1, "the agent scanned once");
	assert!(
		store::find(&f.db, second).await.unwrap().is_none(),
		"the placeholder is gone"
	);
	let db_scan = store::latest(&f.db, ResourceKind::Database, &[6001])
		.await
		.unwrap();
	assert_eq!(db_scan[&6001].id, first);

	// Past 24 hours the same digest is scanned again.
	sqlx::query("UPDATE image_scans SET finished_at = NOW() - INTERVAL '25 hours' WHERE id = $1")
		.bind(first)
		.execute(&f.db)
		.await
		.unwrap();
	let third = f.queue(IMAGE).await;
	f.run(third).await.unwrap();
	assert_eq!(calls.scans().len(), 2);
	assert!(store::find(&f.db, third).await.unwrap().is_some());

	// "Scan again" skips the reuse even inside the day.
	let fourth = super::queue_fresh(&f.state, ServerId(f.server), Fx::target(IMAGE))
		.await
		.unwrap()
		.unwrap();
	f.run(fourth).await.unwrap();
	assert_eq!(calls.scans().len(), 3);
}

#[sqlx::test]
async fn scanning_an_ignore_hides_a_finding_until_it_expires(db: PgPool) {
	let f = Fx::new(db).await;
	fake_agent(&f.state);
	let scan = f.queue(IMAGE).await;
	f.run(scan).await.unwrap();
	let (user, team) = (f.alice().await, TeamId(f.team));
	assert_eq!(f.counts(scan).await.critical, 1);

	// Ignoring the KEV finding (any package) for 30 days removes it from the counts.
	let ignore = service::ignore(
		&f.db,
		user,
		team,
		NewIgnore {
			reason: "not reachable from outside".to_owned(),
			days: 30,
			..ignore_new()
		},
	)
	.await
	.unwrap();
	let counts = f.counts(scan).await;
	assert_eq!((counts.critical, counts.act_now), (0, 1));

	// The table hides it unless asked, and says it is ignored when shown.
	let (view, shown) = service::findings(&f.db, user, team, scan, FindingFilter::default())
		.await
		.unwrap();
	assert_eq!(shown.len(), 4);
	assert_eq!(view.badge.state, BadgeState::Found(Severity::High));
	let (_, all) = service::findings(
		&f.db,
		user,
		team,
		scan,
		FindingFilter {
			show_ignored: true,
			..FindingFilter::default()
		},
	)
	.await
	.unwrap();
	assert_eq!(all.len(), 5);
	assert!(
		all.iter()
			.find(|x| x.vuln_id == "CVE-2022-0001")
			.unwrap()
			.ignored
	);

	// Once it has expired the finding is back.
	sqlx::query("UPDATE scan_ignores SET expires_at = NOW() - INTERVAL '1 minute' WHERE id = $1")
		.bind(ignore)
		.execute(&f.db)
		.await
		.unwrap();
	assert_eq!(f.counts(scan).await.critical, 1);

	// An ignore for another package does not hide this one.
	service::ignore(
		&f.db,
		user,
		team,
		NewIgnore {
			package: Some("some-other-package".to_owned()),
			..ignore_new()
		},
	)
	.await
	.unwrap();
	assert_eq!(f.counts(scan).await.critical, 1);

	// The reason and the period are required.
	for bad in [
		NewIgnore {
			reason: "  ".to_owned(),
			..ignore_new()
		},
		NewIgnore {
			days: 0,
			..ignore_new()
		},
		NewIgnore {
			days: 366,
			..ignore_new()
		},
		NewIgnore {
			vuln_id: String::new(),
			..ignore_new()
		},
	] {
		assert!(matches!(
			service::ignore(&f.db, user, team, bad).await,
			Err(ServerError::Validation(_))
		));
	}
}

#[sqlx::test]
async fn scanning_the_deploy_does_not_wait_for_the_scan(db: PgPool) {
	use crate::{applications, deployments};

	let f = Fx::new(db).await;
	// An agent that deploys but cannot scan at all (the scan routes do not exist).
	let deployer = Router::new().route(
		"/v1/deploy",
		post(|| async {
			let event = json!({ "type": "deployed", "container_id": "c1", "image": IMAGE,
				"commit_sha": null });
			(
				[(header::CONTENT_TYPE, "application/x-ndjson")],
				format!("{event}\n"),
			)
				.into_response()
		}),
	);
	let listener = tokio::net::UnixListener::bind(&f.state.config.local_agent_socket).unwrap();
	tokio::spawn(async move { axum::serve(listener, deployer).await.unwrap() });

	let (s, created) = call(
		&f.app,
		"POST",
		&format!(
			"/api/v1/teams/{}/projects/{}/applications",
			f.team, f.project
		),
		Some(&f.token),
		Some(
			json!({ "name": "w", "slug": "w", "server_id": f.server, "container_port": 80,
			"image": IMAGE }),
		),
	)
	.await;
	assert_eq!(s, StatusCode::OK, "{created}");
	let application =
		applications::find_by_id(&f.db, ApplicationId(created["id"].as_i64().unwrap()))
			.await
			.unwrap()
			.unwrap();
	let queued = deployments::queue(&f.state, &application, None, None)
		.await
		.unwrap();
	let job = jobs::latest_for(&f.db, JobKind::Deploy, queued.id.0)
		.await
		.unwrap()
		.unwrap();
	let mut log = f.state.logs.open(job.id).await.unwrap();

	// Running the deploy job returns with the deployment live, and the scan only queued.
	deployments::run::run(&f.state, &job, &CancellationToken::new(), &mut log)
		.await
		.unwrap();
	log.finish().await;
	let deployment = deployments::find_by_id(&f.db, queued.id)
		.await
		.unwrap()
		.unwrap();
	assert_eq!(deployment.status, deployments::DeploymentStatus::Running);

	let scans: Vec<(ScanId, ScanStatus, String)> =
		sqlx::query_as("SELECT id, status, image_ref FROM image_scans")
			.fetch_all(&f.db)
			.await
			.unwrap();
	assert_eq!(scans.len(), 1);
	assert_eq!(scans[0].1, ScanStatus::Queued, "nobody ran the scan");
	assert_eq!(scans[0].2, IMAGE);
	let linked: Option<i64> = sqlx::query_scalar(
		"SELECT deployment_id FROM scan_links WHERE resource_kind = 'application' AND scan_id = $1",
	)
	.bind(scans[0].0)
	.fetch_one(&f.db)
	.await
	.unwrap();
	assert_eq!(
		linked,
		Some(queued.id.0),
		"the result attaches to the deployment"
	);
	let waiting: i64 = sqlx::query_scalar(
		"SELECT COUNT(*) FROM jobs WHERE kind = 'image_scan' AND status = 'queued'",
	)
	.fetch_one(&f.db)
	.await
	.unwrap();
	assert_eq!(waiting, 1);
}

#[sqlx::test]
async fn scanning_a_server_with_scanning_off_queues_nothing(db: PgPool) {
	let f = Fx::new(db).await;
	let (user, team) = (f.alice().await, TeamId(f.team));
	service::update_settings(
		&f.db,
		user,
		team,
		ServerId(f.server),
		service::Settings {
			enabled: false,
			cpus: 1.0,
			memory_mb: 1024,
		},
	)
	.await
	.unwrap();
	let queued = super::queue(&f.state, ServerId(f.server), Fx::target(IMAGE)).await;
	assert_eq!(queued, None);
	assert_eq!(count(&f.db, "image_scans").await, 0);
	assert_eq!(count(&f.db, "jobs").await, 0);

	// The caps are validated.
	for (cpus, memory_mb) in [(0.0, 1024), (9.0, 1024), (1.0, 100), (1.0, 99_999)] {
		let refused = service::update_settings(
			&f.db,
			user,
			team,
			ServerId(f.server),
			service::Settings {
				enabled: true,
				cpus,
				memory_mb,
			},
		)
		.await;
		assert!(
			matches!(refused, Err(ServerError::Validation(_))),
			"{cpus} {memory_mb}"
		);
	}
	let (s, body) = call(
		&f.app,
		"GET",
		&f.url(&format!("/servers/{}", f.server)),
		Some(&f.token),
		None,
	)
	.await;
	assert_eq!(s, StatusCode::OK);
	assert_eq!(body["enabled"], false);
}

#[sqlx::test]
async fn scanning_a_new_server_scans_with_one_cpu_and_one_gigabyte(db: PgPool) {
	let f = Fx::new(db).await;
	let (s, body) = call(
		&f.app,
		"GET",
		&f.url(&format!("/servers/{}", f.server)),
		Some(&f.token),
		None,
	)
	.await;
	assert_eq!(s, StatusCode::OK);
	assert_eq!(
		body,
		json!({ "enabled": true, "cpus": 1.0, "memory_mb": 1024 })
	);
}

#[sqlx::test]
async fn scanning_the_daily_rescan_reads_the_stored_sbom_once_a_day(db: PgPool) {
	let f = Fx::new(db).await;
	let calls = fake_agent(&f.state);
	let scan = f.queue(IMAGE).await;
	f.run(scan).await.unwrap();
	// Rescans are for what is running.
	sqlx::query("UPDATE databases SET status = 'running' WHERE id = 6001")
		.execute(&f.db)
		.await
		.unwrap();
	store::link(&f.db, ResourceKind::Database, 6001, scan, None)
		.await
		.unwrap();
	let before = f.scan(scan).await;

	let now = OffsetDateTime::now_utc();
	assert_eq!(
		super::queue::queue_due_rescans(&f.state, now)
			.await
			.unwrap(),
		1
	);
	// Due once a day: asking again, even an hour later, queues nothing.
	assert_eq!(
		super::queue::queue_due_rescans(&f.state, now + Duration::hours(1))
			.await
			.unwrap(),
		0
	);

	let job = jobs::latest_for(&f.db, JobKind::ImageRescan, f.server)
		.await
		.unwrap()
		.unwrap();
	let mut log = f.state.logs.open(job.id).await.unwrap();
	super::run::run(&f.state, &job, &CancellationToken::new(), &mut log)
		.await
		.unwrap();
	log.finish().await;

	// The agent got the SBOM and no image to read.
	let sent = calls.scans();
	assert_eq!(sent.len(), 2);
	assert!(sent[1].get("image").is_none_or(Value::is_null));
	assert_eq!(sent[1]["sbom"], SBOM);
	let after = f.scan(scan).await;
	assert!(after.rescanned_at.is_some());
	assert_eq!(
		after.finished_at, before.finished_at,
		"the scan keeps its own date"
	);
	assert_eq!(
		after.sbom_bytes, before.sbom_bytes,
		"the stored SBOM is kept"
	);
	assert_eq!(count(&f.db, "scan_findings").await, 5);

	// Twenty-four hours on it is due again.
	sqlx::query("UPDATE jobs SET status = 'succeeded'")
		.execute(&f.db)
		.await
		.unwrap();
	assert_eq!(
		super::queue::queue_due_rescans(&f.state, now + Duration::hours(24))
			.await
			.unwrap(),
		1
	);
}

#[sqlx::test]
async fn scanning_other_teams_cannot_see_scans_sboms_or_ignores(db: PgPool) {
	let f = Fx::new(db).await;
	fake_agent(&f.state);
	let scan = f.queue(IMAGE).await;
	f.run(scan).await.unwrap();
	let (user, team) = (f.alice().await, TeamId(f.team));
	let ignore = service::ignore(&f.db, user, team, ignore_new())
		.await
		.unwrap();

	let (bob, bob_team) = user_with_team(&f.app, "bob").await;
	let theirs = |tail: &str| format!("/api/v1/teams/{bob_team}/security{tail}");
	for (method, path) in [
		("GET", format!("/scans/{scan}")),
		("GET", format!("/scans/{scan}/sbom")),
		("DELETE", format!("/ignores/{ignore}")),
	] {
		let (s, _) = call(&f.app, method, &theirs(&path), Some(&bob), None).await;
		assert_eq!(s, StatusCode::NOT_FOUND, "{method} {path}");
	}
	// Nor are they listed, nor the application's tab, nor the server's settings.
	let (_, list) = call(&f.app, "GET", &theirs("/ignores"), Some(&bob), None).await;
	assert_eq!(list, json!([]));
	let (s, _) = call(
		&f.app,
		"GET",
		&theirs(&format!("/projects/{}/applications/5001", f.project)),
		Some(&bob),
		None,
	)
	.await;
	assert_eq!(s, StatusCode::NOT_FOUND);
	let (s, _) = call(
		&f.app,
		"GET",
		&theirs(&format!("/servers/{}", f.server)),
		Some(&bob),
		None,
	)
	.await;
	assert_eq!(s, StatusCode::NOT_FOUND);
	let (_, overview) = call(&f.app, "GET", &theirs(""), Some(&bob), None).await;
	assert_eq!(overview, json!([]));
	// Bob's refused delete changed nothing.
	assert_eq!(count(&f.db, "scan_ignores").await, 1);

	// Alice sees it all, including the overview row of the application.
	let (s, body) = call(
		&f.app,
		"GET",
		&f.url(&format!("/scans/{scan}")),
		Some(&f.token),
		None,
	)
	.await;
	assert_eq!(s, StatusCode::OK, "{body}");
	assert_eq!(
		body["findings"].as_array().unwrap().len(),
		4,
		"one is ignored"
	);
	let (_, overview) = call(&f.app, "GET", &f.url(""), Some(&f.token), None).await;
	let app_row = overview
		.as_array()
		.unwrap()
		.iter()
		.find(|r| r["kind"] == "application")
		.unwrap();
	assert_eq!(app_row["badge"]["state"], "high");
	assert_eq!(app_row["badge"]["act_now"], true);
}

async fn raw_get(f: &Fx, token: &str, uri: &str) -> (StatusCode, axum::http::HeaderMap, Vec<u8>) {
	let response = f
		.app
		.clone()
		.oneshot(
			Request::builder()
				.uri(uri)
				.header(header::AUTHORIZATION, format!("Bearer {token}"))
				.body(Body::empty())
				.unwrap(),
		)
		.await
		.unwrap();
	let (parts, body) = response.into_parts();
	let bytes = axum::body::to_bytes(body, usize::MAX).await.unwrap();
	(parts.status, parts.headers, bytes.to_vec())
}

#[sqlx::test]
async fn scanning_the_sbom_download_needs_read_permission(db: PgPool) {
	let f = Fx::new(db).await;
	fake_agent(&f.state);
	let scan = f.queue(IMAGE).await;
	f.run(scan).await.unwrap();

	let (s, headers, body) = raw_get(&f, &f.token, &f.url(&format!("/scans/{scan}/sbom"))).await;
	assert_eq!(s, StatusCode::OK);
	assert_eq!(
		headers[header::CONTENT_TYPE],
		"application/vnd.cyclonedx+json"
	);
	assert!(
		headers[header::CONTENT_DISPOSITION]
			.to_str()
			.unwrap()
			.starts_with("attachment; filename=\"sbom-")
	);
	assert_eq!(String::from_utf8(body).unwrap(), SBOM);

	// Without the permission the same request is refused, not served.
	let role = member_role(&f.db, f.team, "alice").await;
	revoke_permission(&f.db, role, "read", "scan").await;
	let (s, _, body) = raw_get(&f, &f.token, &f.url(&format!("/scans/{scan}/sbom"))).await;
	assert_eq!(s, StatusCode::FORBIDDEN);
	assert!(!String::from_utf8_lossy(&body).contains("bomFormat"));
	let (s, _, _) = raw_get(&f, &f.token, &f.url(&format!("/scans/{scan}"))).await;
	assert_eq!(s, StatusCode::FORBIDDEN);

	// No token at all is no access either.
	let (s, _) = call(
		&f.app,
		"GET",
		&f.url(&format!("/scans/{scan}/sbom")),
		None,
		None,
	)
	.await;
	assert!(s.is_client_error(), "{s}");
}

#[sqlx::test]
async fn scanning_ignoring_and_settings_need_update_permission(db: PgPool) {
	let f = Fx::new(db).await;
	let role = member_role(&f.db, f.team, "alice").await;
	revoke_permission(&f.db, role, "update", "scan").await;

	let (s, _) = call(
		&f.app,
		"POST",
		&f.url("/ignores"),
		Some(&f.token),
		Some(json!({ "vuln_id": "CVE-1", "reason": "x", "days": 7 })),
	)
	.await;
	assert_eq!(s, StatusCode::FORBIDDEN);
	let (s, _) = call(
		&f.app,
		"PUT",
		&f.url(&format!("/servers/{}", f.server)),
		Some(&f.token),
		Some(json!({ "enabled": false, "cpus": 1.0, "memory_mb": 1024 })),
	)
	.await;
	assert_eq!(s, StatusCode::FORBIDDEN);
	assert_eq!(count(&f.db, "scan_ignores").await, 0);
	let enabled: bool = sqlx::query_scalar("SELECT scan_enabled FROM servers WHERE id = $1")
		.bind(f.server)
		.fetch_one(&f.db)
		.await
		.unwrap();
	assert!(enabled, "the refused request changed nothing");
}

#[sqlx::test]
async fn scanning_a_subject_shows_its_scan_and_scans_again_on_request(db: PgPool) {
	let f = Fx::new(db).await;
	fake_agent(&f.state);
	let (user, team, project) = (f.alice().await, TeamId(f.team), ProjectId(f.project));

	// Before any deploy there is nothing, and the tab says so.
	let none = service::subject_scans(&f.db, user, team, project, Fx::app_subject())
		.await
		.unwrap();
	assert!(none.scanning_on);
	assert_eq!(none.badge, Badge::NONE);

	let scan = f.queue(IMAGE).await;
	let waiting = service::subject_scans(&f.db, user, team, project, Fx::app_subject())
		.await
		.unwrap();
	assert_eq!(waiting.badge.state, BadgeState::Scanning);

	f.run(scan).await.unwrap();
	let done = service::subject_scans(&f.db, user, team, project, Fx::app_subject())
		.await
		.unwrap();
	assert_eq!(done.badge.state, BadgeState::Found(Severity::Critical));
	assert_eq!(done.badge.counts.act_now, 2);

	// "Scan again" queues one fresh scan and refuses while nothing was ever scanned.
	assert_eq!(
		service::scan_again(&f.state, user, team, project, Fx::app_subject())
			.await
			.unwrap(),
		1
	);
	let refused = service::scan_again(
		&f.state,
		user,
		team,
		project,
		Subject::Database(DatabaseId(6001)),
	)
	.await;
	assert!(matches!(refused, Err(ServerError::Validation(_))));
}

#[sqlx::test]
async fn scanning_the_api_returns_the_resource_tab_and_filters_findings(db: PgPool) {
	let f = Fx::new(db).await;
	fake_agent(&f.state);
	let scan = f.queue(IMAGE).await;
	f.run(scan).await.unwrap();

	let tab = format!("/projects/{}/applications/5001", f.project);
	let (s, body) = call(&f.app, "GET", &f.url(&tab), Some(&f.token), None).await;
	assert_eq!(s, StatusCode::OK, "{body}");
	assert_eq!(body["badge"]["state"], "critical");
	assert_eq!(body["entries"][0]["scan"]["status"], "succeeded");
	assert_eq!(body["entries"][0]["scan"]["image_ref"], IMAGE);

	let ids = |body: &Value| -> Vec<String> {
		body["findings"]
			.as_array()
			.unwrap()
			.iter()
			.map(|x| x["vuln_id"].as_str().unwrap().to_owned())
			.collect()
	};
	let get = |query: &str| {
		let (app, token, url) = (
			f.app.clone(),
			f.token.clone(),
			f.url(&format!("/scans/{scan}{query}")),
		);
		async move { call(&app, "GET", &url, Some(&token), None).await }
	};
	let (s, body) = get("?act_now=true&severity=critical").await;
	assert_eq!(s, StatusCode::OK);
	assert_eq!(ids(&body), ["CVE-2022-0001"]);
	let (_, body) = get("?act_now=true").await;
	assert_eq!(ids(&body), ["CVE-2022-0001", "GHSA-xxxx-yyyy-zzzz"]);
	let (_, body) = get("?fixable=true").await;
	assert_eq!(ids(&body), ["CVE-2022-0001"]);
	let (s, _) = get("?severity=bogus").await;
	assert!(s.is_client_error());

	let (s, _) = call(
		&f.app,
		"POST",
		&f.url(&format!("{tab}/scan")),
		Some(&f.token),
		None,
	)
	.await;
	assert_eq!(s, StatusCode::ACCEPTED);
}
