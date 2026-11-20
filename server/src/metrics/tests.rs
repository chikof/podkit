//! Metrics tests: the pure parts on literals, retention and range queries on
//! synthetic rows, and the endpoints through the router.

use axum::http::StatusCode;
use proto::{ContainerStats, DiskStats, HostStats, ResourceRef, Stats};
use serde_json::{Value, json};
use sqlx::PgPool;
use time::{Duration, OffsetDateTime};

use super::*;
use crate::ids::ServerId;
use crate::tests::dbs::local_server;
use crate::tests::{app, call, user_with_team};

fn container(
	kind: &str,
	id: i64,
	service: Option<&str>,
	cpu: Option<f64>,
	mem: u64,
) -> ContainerStats {
	ContainerStats {
		name: format!("podkit-{kind}-{id}"),
		resource: ResourceRef {
			kind: kind.to_string(),
			id,
			service: service.map(str::to_string),
		},
		cpu_percent: cpu,
		mem_used: mem,
	}
}

fn host(cpu: f64) -> HostStats {
	HostStats {
		cpu_percent: Some(cpu),
		mem_used: 4_000,
		mem_total: 16_000,
		disks: vec![DiskStats {
			path: "/data".to_string(),
			used: 10,
			free: 90,
			total: 100,
		}],
		net_rx: 111,
		net_tx: 222,
		gpu_util: Some(50.0),
		vram_used: Some(2_000),
	}
}

#[test]
fn metrics_range_parses_the_three_windows() {
	assert_eq!(Range::parse("1h"), Some(Range::Hour));
	assert_eq!(Range::parse("24h"), Some(Range::Day));
	assert_eq!(Range::parse("7d"), Some(Range::Week));
	assert_eq!(Range::parse("1d"), None);
	assert_eq!(Range::parse(""), None);
}

#[test]
fn metrics_aggregate_sums_the_services_of_a_stack() {
	let containers = [
		container("stack", 7, Some("web"), Some(1.5), 100),
		container("stack", 7, Some("db"), Some(2.0), 300),
		container("application", 7, None, None, 50),
		container("stack", 8, Some("web"), None, 10),
		container("stack", 7, Some("cache"), None, 1),
	];
	let samples = aggregate(&containers);
	let find = |kind: &str, id: i64| {
		samples
			.iter()
			.find(|s| s.kind == kind && s.id == id)
			.unwrap()
			.clone()
	};
	// A stack and an application with the same id stay apart.
	assert_eq!(samples.len(), 3);
	assert_eq!(find("stack", 7).cpu, Some(3.5));
	assert_eq!(find("stack", 7).mem, 401);
	assert_eq!(find("application", 7).cpu, None);
	assert_eq!(find("stack", 8).cpu, None);
}

fn server_point(at: i64, cpu: Option<f64>) -> ServerPoint {
	ServerPoint {
		at,
		cpu,
		mem_used: at,
		mem_total: 1000,
		disk_used: 5,
		disk_total: 10,
		net_rx: at * 10,
		net_tx: at * 20,
		gpu_util: None,
		vram_used: None,
	}
}

#[test]
fn metrics_downsample_leaves_short_series_alone() {
	let points: Vec<_> = (0..300)
		.map(|n| server_point(1000 + n * 12, Some(1.0)))
		.collect();
	assert_eq!(downsample(points.clone(), 1000, 4600, MAX_POINTS), points);
}

#[test]
fn metrics_downsample_caps_orders_and_averages() {
	// A day of 30 s samples: 2880 points, cpu equal to the index modulo 10.
	let from = 1_000_000;
	let to = from + 24 * 3600;
	let points: Vec<_> = (0..=2880)
		.map(|n| {
			server_point(
				from + n * 30,
				Some(f64::from(u32::try_from(n % 10).unwrap())),
			)
		})
		.collect();
	let out = downsample(points.clone(), from, to, MAX_POINTS);
	assert!(!out.is_empty() && out.len() <= MAX_POINTS, "{}", out.len());
	assert!(out.windows(2).all(|w| w[0].at < w[1].at), "in order");
	// Each bucket is 288 s: about 9.6 samples whose cpu averages near 4.5.
	for p in &out[..out.len() - 1] {
		let cpu = p.cpu.unwrap();
		assert!((3.0..=6.0).contains(&cpu), "{cpu}");
	}
	// Counters keep the newest value of the bucket, so they never go backwards.
	assert!(out.windows(2).all(|w| w[0].net_rx <= w[1].net_rx));
	let last = out.last().unwrap();
	assert_eq!(last.at, points.last().unwrap().at);
	// A gap leaves a gap, not a made-up point.
	let gappy: Vec<_> = (0..400)
		.map(|n| server_point(if n < 200 { n } else { n + 10_000 }, None))
		.collect();
	let out = downsample(gappy, 0, 20_000, MAX_POINTS);
	assert!(out.len() <= MAX_POINTS);
	assert!(out.iter().all(|p| p.cpu.is_none()));
}

#[test]
fn metrics_downsample_keeps_a_point_at_the_end_of_the_window() {
	let from = 0;
	let to = 3000;
	let points: Vec<_> = (0..=3000).map(|n| server_point(n, None)).collect();
	let out = downsample(points, from, to, 300);
	assert_eq!(out.len(), 300);
	assert_eq!(out.last().unwrap().at, 3000);
}

async fn local(db: &PgPool, team: i64) -> ServerId {
	ServerId(local_server(db, team).await)
}

/// A raw row every 30 s from `hours` ago until now, cpu 10 and memory 1000.
async fn seed_raw(db: &PgPool, server: ServerId, hours: i32) {
	sqlx::query(
		"INSERT INTO server_metrics
			(server_id, at, cpu, mem_used, mem_total, disk_used, disk_total, net_rx, net_tx)
		 SELECT $1, t, 10, 1000, 4000, 1, 2, EXTRACT(EPOCH FROM t)::BIGINT, 0
		 FROM generate_series(NOW() - make_interval(hours => $2), NOW() - INTERVAL '1 second',
			INTERVAL '30 seconds') AS t",
	)
	.bind(server.0)
	.bind(hours)
	.execute(db)
	.await
	.unwrap();
	sqlx::query(
		"INSERT INTO resource_metrics (kind, resource_id, at, cpu, mem)
		 SELECT 'application', 42, t, 5, 500
		 FROM generate_series(NOW() - make_interval(hours => $1), NOW() - INTERVAL '1 second',
			INTERVAL '30 seconds') AS t",
	)
	.bind(hours)
	.execute(db)
	.await
	.unwrap();
}

async fn rows(db: &PgPool, table: &str, rollup: bool) -> i64 {
	sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
		"SELECT COUNT(*) FROM {table} WHERE rollup = $1"
	)))
	.bind(rollup)
	.fetch_one(db)
	.await
	.unwrap()
}

#[sqlx::test]
async fn metrics_rollup_averages_five_minute_buckets(db: PgPool) {
	let app = app(db.clone()).await;
	let (_, team) = user_with_team(&app, "alice").await;
	let server = local(&db, team).await;
	seed_raw(&db, server, 2).await;
	let raw = rows(&db, "server_metrics", false).await;
	assert!(raw >= 240, "{raw}");

	rollup(&db, OffsetDateTime::now_utc()).await.unwrap();
	let rolled = rows(&db, "server_metrics", true).await;
	// Two hours of 5-minute buckets, the current one still open.
	assert!((23..=25).contains(&rolled), "{rolled}");
	assert_eq!(
		rows(&db, "server_metrics", false).await,
		raw,
		"raw rows stay"
	);
	let (cpu, mem, net): (f64, i64, i64) = sqlx::query_as(
		"SELECT cpu, mem_used, net_rx FROM server_metrics WHERE rollup ORDER BY at LIMIT 1",
	)
	.fetch_one(&db)
	.await
	.unwrap();
	assert!((cpu - 10.0).abs() < 1e-9);
	assert_eq!(mem, 1000);
	assert!(net > 0);
	let resource_rolled = rows(&db, "resource_metrics", true).await;
	assert_eq!(resource_rolled, rolled);

	// Running it again adds nothing.
	rollup(&db, OffsetDateTime::now_utc()).await.unwrap();
	assert_eq!(rows(&db, "server_metrics", true).await, rolled);
}

#[sqlx::test]
async fn metrics_retention_keeps_raw_a_day_and_rollups_a_week(db: PgPool) {
	let app = app(db.clone()).await;
	let (_, team) = user_with_team(&app, "alice").await;
	let server = local(&db, team).await;
	// Three days of raw rows, rolled up, then pruned.
	seed_raw(&db, server, 72).await;
	let now = OffsetDateTime::now_utc();
	rollup(&db, now).await.unwrap();
	// An old rollup that is past its week.
	sqlx::query(
		"INSERT INTO server_metrics
			(server_id, at, rollup, mem_used, mem_total, disk_used, disk_total, net_rx, net_tx)
		 VALUES ($1, NOW() - INTERVAL '8 days', TRUE, 1, 1, 1, 1, 1, 1)",
	)
	.bind(server.0)
	.execute(&db)
	.await
	.unwrap();
	sqlx::query(
		"INSERT INTO resource_metrics (kind, resource_id, at, rollup, mem)
		 VALUES ('model', 1, NOW() - INTERVAL '8 days', TRUE, 1)",
	)
	.execute(&db)
	.await
	.unwrap();

	prune(&db, now).await.unwrap();

	let oldest_raw: OffsetDateTime =
		sqlx::query_scalar("SELECT MIN(at) FROM server_metrics WHERE NOT rollup")
			.fetch_one(&db)
			.await
			.unwrap();
	assert!(
		oldest_raw >= now - RAW_RETENTION,
		"raw older than a day survived"
	);
	let (oldest_rollup, newest_rollup): (OffsetDateTime, OffsetDateTime) =
		sqlx::query_as("SELECT MIN(at), MAX(at) FROM server_metrics WHERE rollup")
			.fetch_one(&db)
			.await
			.unwrap();
	assert!(
		oldest_rollup >= now - ROLLUP_RETENTION,
		"rollup past a week survived"
	);
	// The 3-day-old raw rows live on as rollups.
	assert!(oldest_rollup < now - Duration::days(2), "{oldest_rollup}");
	assert!(newest_rollup > now - Duration::minutes(10));
	assert_eq!(
		sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM resource_metrics WHERE kind = 'model'")
			.fetch_one(&db)
			.await
			.unwrap(),
		0
	);
	let resource_oldest: OffsetDateTime =
		sqlx::query_scalar("SELECT MIN(at) FROM resource_metrics WHERE NOT rollup")
			.fetch_one(&db)
			.await
			.unwrap();
	assert!(resource_oldest >= now - RAW_RETENTION);

	// A rollup is not rebuilt from the thinned raw rows: re-running keeps the
	// bucket at the edge of the day as it was.
	let before = rows(&db, "server_metrics", true).await;
	rollup(&db, now).await.unwrap();
	assert_eq!(rows(&db, "server_metrics", true).await, before);
}

fn assert_series(points: &[ServerPoint], now: i64, window: i64) {
	assert!(!points.is_empty());
	assert!(points.len() <= MAX_POINTS, "{} points", points.len());
	assert!(points.windows(2).all(|w| w[0].at < w[1].at), "in order");
	assert!(points.iter().all(|p| p.at <= now && p.at >= now - window));
}

#[sqlx::test]
async fn metrics_ranges_return_at_most_300_points_in_order(db: PgPool) {
	let app = app(db.clone()).await;
	let (_, team) = user_with_team(&app, "alice").await;
	let server = local(&db, team).await;
	seed_raw(&db, server, 72).await;
	let now = OffsetDateTime::now_utc();
	rollup(&db, now).await.unwrap();
	prune(&db, now).await.unwrap();
	let secs = now.unix_timestamp();

	let hour = server_series(&db, server, Range::Hour, now).await.unwrap();
	// An hour of 30 s samples is 120 points: no averaging.
	assert_series(&hour, secs, 3600);
	assert!(hour.len() >= 119, "{}", hour.len());

	let day = server_series(&db, server, Range::Day, now).await.unwrap();
	assert_series(&day, secs, 24 * 3600);
	assert_eq!(day.len(), MAX_POINTS.min(day.len()));
	assert!(day.len() > 250, "{}", day.len());

	let week = server_series(&db, server, Range::Week, now).await.unwrap();
	assert_series(&week, secs, 7 * 24 * 3600);
	// Three days exist, and only the rollups reach back past the first day.
	assert!(week[0].at < secs - 2 * 24 * 3600, "{}", secs - week[0].at);
	assert!(
		week.last().unwrap().at > secs - 600,
		"the raw tail is there"
	);
	assert!(week.iter().all(|p| p.cpu == Some(10.0)));

	let resource = resource_series(&db, Kind::Application, 42, Range::Week, now)
		.await
		.unwrap();
	assert!(!resource.is_empty() && resource.len() <= MAX_POINTS);
	assert!(resource.windows(2).all(|w| w[0].at < w[1].at));
	assert!(resource.iter().all(|p| p.mem == 500 && p.cpu == Some(5.0)));
	// Another resource's id is empty, not an error.
	assert!(
		resource_series(&db, Kind::Application, 43, Range::Hour, now)
			.await
			.unwrap()
			.is_empty()
	);
	assert!(
		resource_series(&db, Kind::Database, 42, Range::Hour, now)
			.await
			.unwrap()
			.is_empty()
	);
}

#[sqlx::test]
async fn metrics_record_stores_the_host_and_each_resource(db: PgPool) {
	let app = app(db.clone()).await;
	let (_, team) = user_with_team(&app, "alice").await;
	let server = local(&db, team).await;
	let at = OffsetDateTime::now_utc() - Duration::seconds(30);
	let stats = Stats {
		host: host(12.5),
		containers: vec![
			container("application", 1, None, Some(1.0), 10),
			container("stack", 2, Some("a"), Some(1.0), 10),
			container("stack", 2, Some("b"), Some(2.0), 20),
		],
		..Stats::default()
	};
	record(&db, server, at, &stats).await.unwrap();
	// The same instant twice is one row.
	record(&db, server, at, &stats).await.unwrap();

	let now = OffsetDateTime::now_utc();
	let points = server_series(&db, server, Range::Hour, now).await.unwrap();
	assert_eq!(points.len(), 1);
	let p = &points[0];
	assert_eq!(p.cpu, Some(12.5));
	assert_eq!((p.mem_used, p.mem_total), (4_000, 16_000));
	assert_eq!((p.disk_used, p.disk_total), (10, 100));
	assert_eq!((p.net_rx, p.net_tx), (111, 222));
	assert_eq!((p.gpu_util, p.vram_used), (Some(50.0), Some(2_000)));
	let stack = resource_series(&db, Kind::Stack, 2, Range::Hour, now)
		.await
		.unwrap();
	assert_eq!(stack.len(), 1);
	assert_eq!((stack[0].cpu, stack[0].mem), (Some(3.0), 30));
	assert_eq!(rows(&db, "resource_metrics", false).await, 2);

	// A host without a GPU or disks still records.
	let bare = Stats {
		host: HostStats::default(),
		containers: vec![],
		..Stats::default()
	};
	record(&db, server, at + Duration::seconds(10), &bare)
		.await
		.unwrap();
	let points = server_series(&db, server, Range::Hour, now).await.unwrap();
	assert_eq!(points.len(), 2);
	assert_eq!((points[1].cpu, points[1].gpu_util), (None, None));
}

/// Everything team alice owns that has a metrics endpoint.
pub(super) struct Owned {
	pub(super) team: i64,
	pub(super) project: i64,
	pub(super) server: i64,
	pub(super) application: i64,
	pub(super) database: i64,
	pub(super) model: i64,
	pub(super) stack: i64,
}

pub(super) async fn own_everything(
	app: &axum::Router,
	db: &PgPool,
	token: &str,
	team: i64,
) -> Owned {
	let server = local_server(db, team).await;
	let (s, project) = call(
		app,
		"POST",
		&format!("/api/v1/teams/{team}/projects"),
		Some(token),
		Some(json!({ "name": "web", "slug": "web" })),
	)
	.await;
	assert!(s.is_success(), "{project}");
	let project = project["id"].as_i64().unwrap();
	let (s, application) = call(
		app,
		"POST",
		&format!("/api/v1/teams/{team}/projects/{project}/applications"),
		Some(token),
		Some(json!({
			"name": "whoami", "slug": "whoami", "server_id": server,
			"repo_url": "https://github.com/traefik/whoami", "git_ref": "master",
			"container_port": 80,
		})),
	)
	.await;
	assert!(s.is_success(), "{application}");
	let database = crypto::generate_id();
	sqlx::query(
		"INSERT INTO databases (id, project_id, server_id, name, engine, version, credentials)
		 VALUES ($1, $2, $3, 'main', 'postgres', '18', ''::bytea)",
	)
	.bind(database)
	.bind(project)
	.bind(server)
	.execute(db)
	.await
	.unwrap();
	let files = crypto::generate_id();
	sqlx::query(
		"INSERT INTO model_files
			(id, team_id, server_id, repo, revision, selection, total_bytes, downloaded_bytes,
			 status, volume_name)
		 VALUES ($1, $2, $3, 'acme/open', 'main', '{}'::jsonb, 1, 1, 'ready', $4)",
	)
	.bind(files)
	.bind(team)
	.bind(server)
	.bind(format!("model-files-{files}"))
	.execute(db)
	.await
	.unwrap();
	let model = crypto::generate_id();
	sqlx::query(
		"INSERT INTO models
			(id, project_id, server_id, model_files_id, name, slug, engine, image, port,
			 served_name, context_size)
		 VALUES ($1, $2, $3, $4, 'qwen', $5, 'ollama', 'img', 11434, 'qwen', 4096)",
	)
	.bind(model)
	.bind(project)
	.bind(server)
	.bind(files)
	.bind(format!("qwen-{model}"))
	.execute(db)
	.await
	.unwrap();
	let stack = crypto::generate_id();
	sqlx::query(
		"INSERT INTO stacks (id, project_id, server_id, name, slug, compose_yaml, env)
		 VALUES ($1, $2, $3, 'shop', 'shop', 'services: {}', ''::bytea)",
	)
	.bind(stack)
	.bind(project)
	.bind(server)
	.execute(db)
	.await
	.unwrap();
	Owned {
		team,
		project,
		server,
		application: application["id"].as_i64().unwrap(),
		database,
		model,
		stack,
	}
}

impl Owned {
	/// Every metrics path, under `team` and (for the project's resources) `project`.
	fn paths(&self, team: i64, project: i64) -> [(&'static str, String); 5] {
		let p = format!("/api/v1/teams/{team}/projects/{project}");
		[
			(
				"server",
				format!("/api/v1/teams/{team}/servers/{}/metrics", self.server),
			),
			(
				"application",
				format!("{p}/applications/{}/metrics", self.application),
			),
			(
				"database",
				format!("{p}/databases/{}/metrics", self.database),
			),
			("model", format!("{p}/models/{}/metrics", self.model)),
			("stack", format!("{p}/stacks/{}/metrics", self.stack)),
		]
	}
}

#[sqlx::test]
async fn metrics_endpoints_serve_the_owner_and_hide_from_other_teams(db: PgPool) {
	let app = app(db.clone()).await;
	let (alice, a_team) = user_with_team(&app, "alice").await;
	let (bob, b_team) = user_with_team(&app, "bob").await;
	let owned = own_everything(&app, &db, &alice, a_team).await;
	let b_server = local_server(&db, b_team).await;
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

	// Samples of every resource, as the sampler would have stored them.
	let server = ServerId(owned.server);
	let stats = Stats {
		host: host(20.0),
		containers: vec![
			container("application", owned.application, None, Some(2.0), 64),
			container("database", owned.database, None, Some(2.0), 64),
			container("model", owned.model, None, Some(2.0), 64),
			container("stack", owned.stack, Some("web"), Some(2.0), 64),
		],
		..Stats::default()
	};
	let at = OffsetDateTime::now_utc() - Duration::seconds(30);
	record(&db, server, at, &stats).await.unwrap();

	for (name, path) in owned.paths(owned.team, owned.project) {
		let (status, body) =
			call(&app, "GET", &format!("{path}?range=1h"), Some(&alice), None).await;
		assert_eq!(status, StatusCode::OK, "{name}: {body}");
		let points = body.as_array().unwrap_or_else(|| panic!("{name}: array"));
		assert_eq!(points.len(), 1, "{name}");
		assert!(points[0].get("cpu").is_some(), "{name}: {body}");
		assert!(points[0]["at"].as_i64().is_some(), "{name}");
		// The default range is an hour.
		let (status, again) = call(&app, "GET", &path, Some(&alice), None).await;
		assert_eq!((status, again), (StatusCode::OK, body), "{name}");
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
		// No token.
		let (status, _) = call(&app, "GET", &path, None, None).await;
		assert!(status.is_client_error(), "{name}: {status}");
		// Alice's ids under Bob's team: they do not exist for him.
		for (project, label) in [(b_project, "own project"), (owned.project, "her project")] {
			let foreign = owned
				.paths(b_team, project)
				.into_iter()
				.find(|(n, _)| *n == name)
				.unwrap()
				.1;
			let (status, _) = call(&app, "GET", &foreign, Some(&bob), None).await;
			assert_eq!(
				status,
				StatusCode::NOT_FOUND,
				"{name} via {label}: {foreign}"
			);
		}
		// Her team: he is not a member.
		let (status, _) = call(&app, "GET", &path, Some(&bob), None).await;
		assert_eq!(status, StatusCode::FORBIDDEN, "{name}");
	}
	// Bob's own server answers him, with nothing in it yet.
	let (status, body) = call(
		&app,
		"GET",
		&format!("/api/v1/teams/{b_team}/servers/{b_server}/metrics?range=24h"),
		Some(&bob),
		None,
	)
	.await;
	assert_eq!((status, body), (StatusCode::OK, Value::Array(vec![])));
}
