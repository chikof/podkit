//! Tests of the richer charts: counters to rates, the extra series' storage, container events and
//! deploy markers, the team's busiest resources, and the endpoints' tenancy.

use axum::http::StatusCode;
use proto::{
	ContainerEvent, ContainerEventKind, DbStat, HistogramBucket, HttpStats, ResourceRef, Stats,
	VolumeStats, VramStats,
};
use serde_json::json;
use sqlx::PgPool;
use time::{Duration, OffsetDateTime};

use super::collect::Memory;
use super::events::{self, DeployMarker};
use super::ext::{self, ExtPoint, ExtSamples};
use super::overview::{Busy, TOP, rank};
use super::tests::{Owned, own_everything};
use super::{Kind, Range};
use crate::tests::dbs::local_server;
use crate::tests::{app, call, insert_deployment, user_with_team};

fn resource(kind: &str, id: i64) -> ResourceRef {
	ResourceRef {
		kind: kind.to_owned(),
		id,
		service: None,
	}
}

fn counters(id: i64, r2: f64, r4: f64, r5: f64, buckets: [(Option<f64>, f64); 3]) -> HttpStats {
	HttpStats {
		resource: Some(resource("application", id)),
		r2xx: r2,
		r4xx: r4,
		r5xx: r5,
		bytes_in: r2 * 100.0,
		bytes_out: r2 * 1000.0,
		buckets: buckets
			.into_iter()
			.map(|(le, count)| HistogramBucket { le, count })
			.collect(),
		..HttpStats::default()
	}
}

fn stats_with(http: Vec<HttpStats>) -> Stats {
	Stats {
		http,
		..Stats::default()
	}
}

// ---- Counters to rates, in the sampler's memory ----------------------------------------

#[test]
fn http_metrics_the_first_reading_only_remembers_and_the_second_draws_a_point() {
	let mut memory = Memory::default();
	let mut out = ExtSamples::new();
	let first = stats_with(vec![counters(
		7,
		100.0,
		10.0,
		0.0,
		[(Some(0.1), 100.0), (Some(1.0), 110.0), (None, 110.0)],
	)]);
	memory.http(1_000, &first, &mut out);
	assert!(out.is_empty(), "no rate from one reading: {out:?}");

	// Thirty seconds later: 90 more 2xx, 30 more 4xx and 3 5xx.
	let second = stats_with(vec![counters(
		7,
		190.0,
		40.0,
		3.0,
		[(Some(0.1), 190.0), (Some(1.0), 240.0), (None, 243.0)],
	)]);
	memory.http(1_030, &second, &mut out);
	let p = &out[&("application".to_owned(), 7)];
	assert_eq!(p.r2xx_rate, Some(3.0));
	assert_eq!(p.r4xx_rate, Some(1.0));
	assert_eq!(p.r5xx_rate, Some(0.1));
	assert!((p.req_rate.unwrap() - 123.0 / 30.0).abs() < 1e-9);
	assert!((p.bytes_out_rate.unwrap() - 90.0 * 1000.0 / 30.0).abs() < 1e-9);
	// 123 new requests: 90 under 100 ms, 30 more under 1 s, 3 slower. p50 is inside the first
	// bucket; p95 is the largest finite bound's neighbourhood.
	assert!(p.p50_ms.unwrap() < 100.0);
	assert!(p.p95_ms.unwrap() > 100.0 && p.p95_ms.unwrap() <= 1000.0);
}

#[test]
fn http_metrics_a_long_silence_is_not_averaged_into_a_rate() {
	let mut memory = Memory::default();
	let mut out = ExtSamples::new();
	let reading = |n: f64| {
		stats_with(vec![counters(
			1,
			n,
			0.0,
			0.0,
			[(Some(1.0), n), (Some(2.0), n), (None, n)],
		)])
	};
	memory.http(0, &reading(10.0), &mut out);
	memory.http(3_600, &reading(500.0), &mut out);
	assert!(out.is_empty(), "an hour apart: {out:?}");
	memory.http(3_630, &reading(530.0), &mut out);
	assert_eq!(out[&("application".to_owned(), 1)].r2xx_rate, Some(1.0));
}

#[test]
fn http_metrics_a_stack_sums_its_services() {
	let service = |name: &str, n: f64| HttpStats {
		resource: Some(ResourceRef {
			kind: "stack".to_owned(),
			id: 3,
			service: Some(name.to_owned()),
		}),
		r2xx: n,
		buckets: vec![
			HistogramBucket {
				le: Some(1.0),
				count: n,
			},
			HistogramBucket { le: None, count: n },
		],
		..HttpStats::default()
	};
	let mut memory = Memory::default();
	let mut out = ExtSamples::new();
	memory.http(
		0,
		&stats_with(vec![service("web", 10.0), service("api", 10.0)]),
		&mut out,
	);
	memory.http(
		10,
		&stats_with(vec![service("web", 20.0), service("api", 40.0)]),
		&mut out,
	);
	assert_eq!(out[&("stack".to_owned(), 3)].r2xx_rate, Some(4.0));
}

#[test]
fn http_metrics_database_operations_become_a_rate_and_a_restart_gives_none() {
	let mut memory = Memory::default();
	let mut out = ExtSamples::new();
	let stat = |ops: i64| DbStat {
		id: 4,
		connections: Some(6),
		size: Some(1 << 20),
		operations: Some(ops),
		memory: None,
	};
	memory.databases(0, &[stat(1000)], &mut out);
	let p = &out[&("database".to_owned(), 4)];
	assert_eq!(
		(p.db_conns, p.db_size, p.db_ops_rate),
		(Some(6), Some(1 << 20), None)
	);
	out.clear();
	memory.databases(30, &[stat(1300)], &mut out);
	assert_eq!(out[&("database".to_owned(), 4)].db_ops_rate, Some(10.0));
	out.clear();
	// The engine restarted: its counter is smaller, which is not a negative rate.
	memory.databases(60, &[stat(50)], &mut out);
	assert_eq!(out[&("database".to_owned(), 4)].db_ops_rate, None);
}

#[test]
fn http_metrics_model_usage_becomes_tokens_and_requests_per_second() {
	let mut memory = Memory::default();
	let mut out = ExtSamples::new();
	memory.models(0, &[(9, 10, 1_000)], &mut out);
	assert!(out.is_empty());
	memory.models(20, &[(9, 14, 1_600)], &mut out);
	let p = &out[&("model".to_owned(), 9)];
	assert_eq!(p.tokens_rate, Some(30.0));
	assert_eq!(p.model_req_rate, Some(0.2));
	out.clear();
	// A totals row that shrank (usage rows deleted) draws nothing.
	memory.models(40, &[(9, 1, 5)], &mut out);
	assert!(out.is_empty());
}

#[test]
fn http_metrics_volumes_and_vram_add_up_per_resource() {
	let stack = |service: &str| ResourceRef {
		kind: "stack".to_owned(),
		id: 3,
		service: Some(service.to_owned()),
	};
	let stats = Stats {
		volumes: vec![
			VolumeStats {
				resource: stack("db"),
				used: 100,
			},
			VolumeStats {
				resource: stack("web"),
				used: 50,
			},
			VolumeStats {
				resource: resource("application", 1),
				used: 7,
			},
		],
		vram: vec![VramStats {
			resource: resource("model", 2),
			used: 1 << 30,
		}],
		..Stats::default()
	};
	let mut out = ExtSamples::new();
	super::collect::volumes_and_vram(&stats, &mut out);
	assert_eq!(out[&("stack".to_owned(), 3)].volume_bytes, Some(150));
	assert_eq!(out[&("application".to_owned(), 1)].volume_bytes, Some(7));
	assert_eq!(out[&("model".to_owned(), 2)].vram_bytes, Some(1 << 30));
}

#[test]
fn http_metrics_the_event_window_continues_where_the_last_one_ended() {
	let mut memory = Memory::default();
	assert_eq!(memory.event_window(1_000), (940, 1_000));
	memory.events_stored(1_000);
	assert_eq!(memory.event_window(1_030), (1_001, 1_030));
}

// ---- Storage ----------------------------------------------------------------------------

fn http_point(rate: f64) -> ExtPoint {
	ExtPoint {
		req_rate: Some(rate),
		r2xx_rate: Some(rate),
		p95_ms: Some(120.0),
		..ExtPoint::default()
	}
}

async fn store(db: &PgPool, kind: &str, id: i64, at: OffsetDateTime, point: ExtPoint) {
	let mut samples = ExtSamples::new();
	samples.insert((kind.to_owned(), id), point);
	ext::record(db, at, &samples).await.unwrap();
}

#[sqlx::test]
async fn http_metrics_storage_rolls_up_averages_and_prunes(db: PgPool) {
	let now = OffsetDateTime::now_utc();
	// Three raw readings in one finished 5-minute bucket, 40 minutes ago.
	let bucket = {
		let t = now - Duration::minutes(40);
		let secs = t.unix_timestamp();
		OffsetDateTime::from_unix_timestamp(secs - secs % 300).unwrap()
	};
	for (i, rate) in [10.0, 20.0, 30.0].into_iter().enumerate() {
		store(
			&db,
			"application",
			5,
			bucket + Duration::seconds(30 * i64::try_from(i).unwrap()),
			http_point(rate),
		)
		.await;
	}
	// An empty sample stores nothing.
	let mut empty = ExtSamples::new();
	empty.insert(("application".to_owned(), 6), ExtPoint::default());
	ext::record(&db, now, &empty).await.unwrap();
	let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM resource_extra_metrics")
		.fetch_one(&db)
		.await
		.unwrap();
	assert_eq!(rows, 3);

	super::rollup(&db, now).await.unwrap();
	let (rolled, avg, p95): (i64, f64, f64) = sqlx::query_as(
		"SELECT COUNT(*), AVG(req_rate), AVG(p95_ms) FROM resource_extra_metrics WHERE rollup",
	)
	.fetch_one(&db)
	.await
	.unwrap();
	assert_eq!((rolled, avg, p95), (1, 20.0, 120.0));
	// Run again: the bucket is rolled up once.
	super::rollup(&db, now).await.unwrap();
	let again: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM resource_extra_metrics WHERE rollup")
		.fetch_one(&db)
		.await
		.unwrap();
	assert_eq!(again, 1);

	// A week-old raw row and a 10-day-old rollup go; recent rows stay.
	store(
		&db,
		"application",
		5,
		now - Duration::days(2),
		http_point(1.0),
	)
	.await;
	sqlx::query(
		"INSERT INTO resource_extra_metrics (kind, resource_id, at, rollup, req_rate)
		VALUES ('application', 5, $1, TRUE, 1.0)",
	)
	.bind(now - Duration::days(10))
	.execute(&db)
	.await
	.unwrap();
	super::prune(&db, now).await.unwrap();
	let left: Vec<(bool,)> =
		sqlx::query_as("SELECT rollup FROM resource_extra_metrics ORDER BY rollup, at")
			.fetch_all(&db)
			.await
			.unwrap();
	// The three raw rows of 40 minutes ago and the rollup remain; the 2-day-old raw row and
	// the 10-day-old rollup are gone.
	assert_eq!(left.len(), 4, "{left:?}");
}

#[sqlx::test]
async fn http_metrics_series_come_back_oldest_first_within_the_range(db: PgPool) {
	let now = OffsetDateTime::now_utc();
	for (minutes, rate) in [(50, 1.0), (30, 2.0), (10, 3.0)] {
		store(
			&db,
			"application",
			8,
			now - Duration::minutes(minutes),
			http_point(rate),
		)
		.await;
	}
	// Another resource's row, and one too old for the hour.
	store(
		&db,
		"application",
		9,
		now - Duration::minutes(5),
		http_point(99.0),
	)
	.await;
	store(
		&db,
		"application",
		8,
		now - Duration::minutes(90),
		http_point(77.0),
	)
	.await;
	let hour = ext::series(&db, Kind::Application, 8, Range::Hour, now)
		.await
		.unwrap();
	let rates: Vec<Option<f64>> = hour.iter().map(|p| p.req_rate).collect();
	assert_eq!(rates, [Some(1.0), Some(2.0), Some(3.0)]);
	let day = ext::series(&db, Kind::Application, 8, Range::Day, now)
		.await
		.unwrap();
	assert_eq!(day.len(), 4);
	assert!(day.windows(2).all(|w| w[0].at < w[1].at));
}

#[sqlx::test]
async fn http_metrics_events_are_stored_once_and_listed_for_their_resource(db: PgPool) {
	let now = OffsetDateTime::now_utc();
	let at = now.unix_timestamp() - 600;
	let event = |kind, code, container: &str| ContainerEvent {
		at,
		kind,
		resource: resource("application", 7),
		container: container.to_owned(),
		exit_code: code,
	};
	let batch = vec![
		event(ContainerEventKind::Exit, Some(1), "podkit-app-7-1"),
		event(ContainerEventKind::Oom, None, "podkit-app-7-1"),
	];
	events::record(&db, &batch).await.unwrap();
	// The window of the next question overlaps by a second: the same event is not stored twice.
	events::record(&db, &batch).await.unwrap();
	events::record(
		&db,
		&[ContainerEvent {
			resource: resource("application", 8),
			..event(ContainerEventKind::Restart, None, "other")
		}],
	)
	.await
	.unwrap();
	let marks = events::markers(&db, Kind::Application, 7, Range::Hour, now)
		.await
		.unwrap();
	assert_eq!(marks.len(), 2, "{marks:?}");
	assert!(
		marks
			.iter()
			.any(|m| m.event == "exit" && m.exit_code == Some(1))
	);
	assert!(marks.iter().any(|m| m.event == "oom"));
	// Nothing older than the window, nothing of another resource.
	let none = events::markers(
		&db,
		Kind::Application,
		7,
		Range::Hour,
		now - Duration::hours(3),
	)
	.await
	.unwrap();
	assert!(none.is_empty());
	// Old events are pruned with the rollups.
	events::prune(&db, now + Duration::days(8)).await.unwrap();
	let left: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM container_events")
		.fetch_one(&db)
		.await
		.unwrap();
	assert_eq!(left, 0);
}

// ---- Endpoints --------------------------------------------------------------------------

fn extra_paths(owned: &Owned, team: i64, project: i64) -> [(&'static str, String); 4] {
	let p = format!("/api/v1/teams/{team}/projects/{project}");
	[
		(
			"application",
			format!("{p}/applications/{}/metrics/extra", owned.application),
		),
		(
			"database",
			format!("{p}/databases/{}/metrics/extra", owned.database),
		),
		("model", format!("{p}/models/{}/metrics/extra", owned.model)),
		("stack", format!("{p}/stacks/{}/metrics/extra", owned.stack)),
	]
}

#[sqlx::test]
async fn http_metrics_the_extra_endpoints_serve_the_owner_and_hide_from_other_teams(db: PgPool) {
	let app = app(db.clone()).await;
	let (alice, a_team) = user_with_team(&app, "alice").await;
	let (bob, b_team) = user_with_team(&app, "bob").await;
	let owned = own_everything(&app, &db, &alice, a_team).await;
	let _ = local_server(&db, b_team).await;
	let (s, b_project) = call(
		&app,
		"POST",
		&format!("/api/v1/teams/{b_team}/projects"),
		Some(&bob),
		Some(json!({ "name": "mine", "slug": "mine" })),
	)
	.await;
	assert!(s.is_success());
	let b_project = b_project["id"].as_i64().unwrap();

	let now = OffsetDateTime::now_utc();
	for (kind, id) in [
		("application", owned.application),
		("database", owned.database),
		("model", owned.model),
		("stack", owned.stack),
	] {
		store(&db, kind, id, now - Duration::seconds(30), http_point(4.0)).await;
	}
	events::record(
		&db,
		&[ContainerEvent {
			at: now.unix_timestamp() - 60,
			kind: ContainerEventKind::Oom,
			resource: resource("application", owned.application),
			container: "podkit-app".to_owned(),
			exit_code: None,
		}],
	)
	.await
	.unwrap();
	insert_deployment(&db, owned.application, 4242, "running", None, None).await;

	for (name, path) in extra_paths(&owned, owned.team, owned.project) {
		let (status, body) =
			call(&app, "GET", &format!("{path}?range=1h"), Some(&alice), None).await;
		assert_eq!(status, StatusCode::OK, "{name}: {body}");
		assert_eq!(
			body["points"].as_array().unwrap().len(),
			1,
			"{name}: {body}"
		);
		assert_eq!(body["points"][0]["req_rate"], 4.0, "{name}");
		if name == "application" {
			assert_eq!(body["events"][0]["event"], "oom");
			assert_eq!(body["deploys"][0]["status"], "running");
			assert_eq!(body["deploys"][0]["commit"], "abcdef1234567890");
		} else {
			assert!(body["deploys"].as_array().unwrap().is_empty(), "{name}");
		}
		for range in ["24h", "7d"] {
			let (status, _) = call(
				&app,
				"GET",
				&format!("{path}?range={range}"),
				Some(&alice),
				None,
			)
			.await;
			assert_eq!(status, StatusCode::OK, "{name} {range}");
		}
		let (status, _) = call(&app, "GET", &format!("{path}?range=1y"), Some(&alice), None).await;
		assert!(
			status.is_client_error() && status != StatusCode::NOT_FOUND,
			"{name}: {status}"
		);
		let (status, _) = call(&app, "GET", &path, None, None).await;
		assert!(status.is_client_error(), "{name}: {status}");
		// Alice's ids under Bob's team: they do not exist for him.
		for project in [b_project, owned.project] {
			let foreign = extra_paths(&owned, b_team, project)
				.into_iter()
				.find(|(n, _)| *n == name)
				.unwrap()
				.1;
			let (status, _) = call(&app, "GET", &foreign, Some(&bob), None).await;
			assert_eq!(status, StatusCode::NOT_FOUND, "{name}: {foreign}");
		}
		// Her team: he is not a member.
		let (status, _) = call(&app, "GET", &path, Some(&bob), None).await;
		assert_eq!(status, StatusCode::FORBIDDEN, "{name}");
	}
}

#[test]
fn deploy_markers_serialise_with_the_commit_only_when_there_is_one() {
	let with = serde_json::to_value(DeployMarker {
		at: 1,
		id: 2,
		status: "running".to_owned(),
		commit: Some("abc".to_owned()),
	})
	.unwrap();
	assert_eq!(with["commit"], "abc");
	let without = serde_json::to_value(DeployMarker {
		at: 1,
		id: 2,
		status: "failed".to_owned(),
		commit: None,
	})
	.unwrap();
	assert!(without["commit"].is_null());
}

// ---- The busiest resources -------------------------------------------------------------------

fn busy(name: &str, cpu: f64, mem: f64, r5xx: f64, restarts: i64) -> Busy {
	Busy {
		kind: "application".to_owned(),
		id: 1,
		project_id: 1,
		name: name.to_owned(),
		cpu: Some(cpu),
		mem: Some(mem),
		req_rate: Some(10.0),
		r5xx_rate: Some(r5xx),
		restarts,
	}
}

#[test]
fn http_metrics_the_busiest_lists_sort_cap_and_skip_the_idle() {
	let mut rows: Vec<Busy> = (0..8)
		.map(|i| {
			busy(
				&format!("app-{i}"),
				f64::from(i),
				f64::from(i) * 1e6,
				0.0,
				0,
			)
		})
		.collect();
	rows.push(busy("flaky", 0.0, 0.0, 2.5, 4));
	rows.push(busy("sad", 0.0, 0.0, 0.5, 9));
	rows.push(Busy {
		cpu: None,
		mem: None,
		r5xx_rate: None,
		..busy("quiet", 0.0, 0.0, 0.0, 0)
	});
	let top = rank(&rows);
	assert_eq!(top.by_cpu.len(), TOP);
	assert_eq!(top.by_cpu[0].name, "app-7");
	// Zero CPU is not "busy".
	assert!(top.by_cpu.iter().all(|r| r.cpu.unwrap() > 0.0));
	assert_eq!(top.by_memory[0].name, "app-7");
	let errors: Vec<&str> = top.by_errors.iter().map(|r| r.name.as_str()).collect();
	assert_eq!(errors, ["flaky", "sad"]);
	let restarts: Vec<&str> = top.by_restarts.iter().map(|r| r.name.as_str()).collect();
	assert_eq!(restarts, ["sad", "flaky"]);
	assert!(rank(&[busy("idle", 0.0, 0.0, 0.0, 0)]).is_empty());
}

#[sqlx::test]
async fn http_metrics_the_busiest_query_only_sees_the_teams_own_resources(db: PgPool) {
	let app = app(db.clone()).await;
	let (alice, a_team) = user_with_team(&app, "alice").await;
	let (_, b_team) = user_with_team(&app, "bob").await;
	let owned = own_everything(&app, &db, &alice, a_team).await;
	let _ = local_server(&db, b_team).await;
	let now = OffsetDateTime::now_utc();
	for (kind, id) in [("application", owned.application), ("stack", owned.stack)] {
		sqlx::query(
			"INSERT INTO resource_metrics (kind, resource_id, at, cpu, mem) VALUES ($1, $2, $3, 20, 1000)",
		)
		.bind(kind)
		.bind(id)
		.bind(now - Duration::minutes(5))
		.execute(&db)
		.await
		.unwrap();
	}
	store(
		&db,
		"application",
		owned.application,
		now - Duration::minutes(5),
		ExtPoint {
			req_rate: Some(10.0),
			r5xx_rate: Some(2.0),
			..ExtPoint::default()
		},
	)
	.await;
	events::record(
		&db,
		&[ContainerEvent {
			at: now.unix_timestamp() - 120,
			kind: ContainerEventKind::Exit,
			resource: resource("application", owned.application),
			container: "c".to_owned(),
			exit_code: Some(2),
		}],
	)
	.await
	.unwrap();

	let kinds = [Kind::Application, Kind::Database, Kind::Model, Kind::Stack];
	let mine = super::overview::load(&db, owned.team, &kinds)
		.await
		.unwrap();
	assert_eq!(mine.len(), 4, "{mine:?}");
	let app_row = mine.iter().find(|r| r.kind == "application").unwrap();
	assert_eq!(app_row.cpu, Some(20.0));
	assert_eq!(app_row.r5xx_rate, Some(2.0));
	assert_eq!(app_row.restarts, 1);
	let top = rank(&mine);
	assert_eq!(top.by_errors.len(), 1);
	assert_eq!(top.by_restarts.len(), 1);
	// Another team sees none of it.
	let theirs = super::overview::load(&db, b_team, &kinds).await.unwrap();
	assert!(theirs.is_empty(), "{theirs:?}");
	// A kind the person may not read is left out of the query.
	let only_stacks = super::overview::load(&db, owned.team, &[Kind::Stack])
		.await
		.unwrap();
	assert!(only_stacks.iter().all(|r| r.kind == "stack"));
}
