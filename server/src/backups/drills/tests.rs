//! Restore drills: the verdict, the copy's limits and request, the schedule, the scheduler
//! and how a failed job lands on the drill row.
//!
//! What runs in the copy (start, readiness, restore, count, removal) is the agent's and
//! needs Podman; its pure parts are tested in `agent/src/drill.rs`.
//!
//! `cargo test -p server -- drills`

use sqlx::PgPool;
use time::OffsetDateTime;

use super::schedule::{next_due, normalize};
use super::{DrillStatus, judge, limits, request};
use crate::databases::{Credentials, Database, DatabaseStatus, EngineKind};
use crate::error::ServerError;
use crate::ids::{DatabaseId, EnvironmentId, ProjectId, RestoreDrillId, ServerId};
use crate::jobs::{JobFailure, JobKind, fail_target};
use crate::tests::state;

// --- pure pieces ---------------------------------------------------------------------------

#[test]
fn drill_passes_with_tables_and_at_least_the_source_count() {
	assert_eq!(judge(3, None), Ok(()));
	assert_eq!(judge(3, Some(3)), Ok(()));
	// A table created between the count and the dump is not a failure.
	assert_eq!(judge(4, Some(3)), Ok(()));
}

#[test]
fn drill_fails_on_an_empty_or_short_copy() {
	let empty = judge(0, None).unwrap_err();
	assert!(empty.contains("no tables"), "{empty}");
	assert!(judge(0, Some(5)).unwrap_err().contains("no tables"));
	let short = judge(2, Some(5)).unwrap_err();
	assert!(
		short.contains("2 tables") && short.contains("had 5"),
		"{short}"
	);
}

#[test]
fn drill_of_a_database_that_was_empty_may_come_back_empty() {
	assert_eq!(judge(0, Some(0)), Ok(()));
	// A nonsense negative source count is ignored, not trusted.
	assert_eq!(judge(1, Some(-4)), Ok(()));
}

#[test]
fn drill_copy_limits_follow_the_database_within_bounds() {
	assert_eq!(limits(None, None), (1024 << 20, 1.0));
	assert_eq!(limits(Some(2048), Some(1.5)), (2048 << 20, 1.5));
	assert_eq!(limits(Some(64), Some(0.1)), (256 << 20, 0.25));
	assert_eq!(limits(Some(65_536), Some(16.0)), (4096 << 20, 2.0));
	assert_eq!(limits(None, Some(f32::NAN)), (1024 << 20, 1.0));
}

#[test]
fn drill_schedule_blank_means_off() {
	assert_eq!(normalize(None), None);
	assert_eq!(normalize(Some("  ")), None);
	assert_eq!(normalize(Some(" 0 4 * * 0 ")).as_deref(), Some("0 4 * * 0"));
}

#[test]
fn drill_schedule_next_run_or_a_validation_error() {
	let now = OffsetDateTime::from_unix_timestamp(1_763_380_800).unwrap(); // 2025-11-17 12:00 UTC, a Monday
	assert_eq!(next_due(None, now).unwrap(), None);
	let next = next_due(Some("0 4 * * 0"), now).unwrap().unwrap();
	assert_eq!(next.weekday(), time::Weekday::Sunday);
	assert_eq!((next.hour(), next.minute()), (4, 0));
	assert!(next > now);
	assert!(matches!(
		next_due(Some("every week"), now),
		Err(ServerError::Validation(m)) if m.contains("invalid drill schedule")
	));
}

fn database(engine: EngineKind, version: &str) -> Database {
	Database {
		id: DatabaseId(7),
		project_id: ProjectId(1),
		environment_id: EnvironmentId(1),
		server_id: ServerId(1),
		name: "main".to_owned(),
		engine,
		version: version.to_owned(),
		credentials: Vec::new(),
		public_port: Some(15432),
		memory_limit_mb: Some(512),
		cpu_limit: None,
		status: DatabaseStatus::Running,
		status_message: None,
		container_id: Some("abc".to_owned()),
		backup_target_id: None,
		backup_schedule: None,
		backup_keep: 7,
		backup_next_run_at: None,
	}
}

#[test]
fn drill_request_uses_the_same_engine_and_version_and_nothing_public() {
	let credentials = Credentials {
		username: "app".to_owned(),
		password: "pw".to_owned(),
		database: "appdb".to_owned(),
	};
	let req = request(
		RestoreDrillId(9),
		&database(EngineKind::Postgres, "17"),
		&credentials,
		"https://s3.example/x".to_owned(),
	);
	assert_eq!(req.drill_id, 9);
	assert_eq!(req.engine, proto::Engine::Postgres);
	assert_eq!(req.image, "docker.io/library/postgres:17-alpine");
	assert!(
		req.env
			.contains(&("POSTGRES_USER".to_owned(), "app".to_owned()))
	);
	assert!(
		req.env
			.contains(&("POSTGRES_DB".to_owned(), "appdb".to_owned()))
	);
	assert_eq!(req.memory_bytes, 512 << 20);
	assert_eq!(req.cpu_cores, 1.0);
	assert_eq!(req.ready_timeout_s, super::READY_TIMEOUT_S);
	// The request has no port, network or volume field: the agent makes the copy private.
	let json = serde_json::to_value(&req).unwrap();
	for field in ["port", "network", "volume", "published_ports"] {
		assert!(json.get(field).is_none(), "{field} in {json}");
	}
}

#[test]
fn redis_and_deleting_databases_cannot_be_drilled() {
	assert!(super::drillable(&database(EngineKind::Postgres, "18")).is_ok());
	assert!(super::drillable(&database(EngineKind::Redis, "8")).is_err());
	let mut deleting = database(EngineKind::Mysql, "9.7");
	deleting.status = DatabaseStatus::Deleting;
	assert!(super::drillable(&deleting).is_err());
}

// --- the store and the scheduler -----------------------------------------------------------

const TEAM: i64 = 1;
const PROJECT: i64 = 3;
const SERVER: i64 = 4;
const DB: i64 = 5;
const TARGET: i64 = 6;

/// A team with a running Postgres database and a backup target.
async fn seed(db: &PgPool) {
	for (sql, binds) in [
		("INSERT INTO teams (id, name, slug, logo) VALUES ($1, 't', 't', '')", vec![TEAM]),
		(
			"INSERT INTO projects (id, team_id, name, slug) VALUES ($1, $2, 'p', 'p')",
			vec![PROJECT, TEAM],
		),
		(
			"INSERT INTO servers (id, team_id, name, hostname, podman_socket_path, is_local)
			VALUES ($1, $2, 'local', 'localhost', '/run/podman.sock', TRUE)",
			vec![SERVER, TEAM],
		),
		(
			"INSERT INTO databases (id, project_id, server_id, name, engine, version, credentials, status)
			VALUES ($1, $2, $3, 'main', 'postgres', '18', '\\x00', 'running')",
			vec![DB, PROJECT, SERVER],
		),
		(
			"INSERT INTO backup_targets (id, team_id, name, endpoint, region, bucket, access_key_id, secret_access_key)
			VALUES ($1, $2, 's3', 'https://s3.example.com', 'us-east-1', 'b', 'k', '\\x00')",
			vec![TARGET, TEAM],
		),
	] {
		let mut query = sqlx::query(sql);
		for bind in binds {
			query = query.bind(bind);
		}
		query.execute(db).await.unwrap();
	}
}

async fn add_backup(db: &PgPool, id: i64, status: &str, source_tables: Option<i64>) {
	sqlx::query(
		"INSERT INTO backups (id, database_id, target_id, status, object_key, source_tables, finished_at)
		VALUES ($1, $2, $3, $4, 'k', $5, NOW())",
	)
	.bind(id)
	.bind(DB)
	.bind(TARGET)
	.bind(status)
	.bind(source_tables)
	.execute(db)
	.await
	.unwrap();
}

async fn make_due(db: &PgPool) {
	sqlx::query(
		"UPDATE databases SET drill_schedule = '0 4 * * 0',
			drill_next_run_at = NOW() - INTERVAL '1 minute' WHERE id = $1",
	)
	.bind(DB)
	.execute(db)
	.await
	.unwrap();
}

async fn drills(db: &PgPool) -> Vec<(String, Option<i64>, Option<i64>, Option<String>)> {
	sqlx::query_as("SELECT status, backup_id, source_tables, error FROM restore_drills ORDER BY id")
		.fetch_all(db)
		.await
		.unwrap()
}

#[sqlx::test]
async fn drill_candidate_is_the_newest_succeeded_backup(db: PgPool) {
	seed(&db).await;
	assert!(
		super::candidate(&db, DatabaseId(DB))
			.await
			.unwrap()
			.is_none()
	);
	add_backup(&db, 10, "succeeded", Some(4)).await;
	add_backup(&db, 11, "failed", None).await;
	let c = super::candidate(&db, DatabaseId(DB))
		.await
		.unwrap()
		.unwrap();
	assert_eq!(c.id.0, 10);
	assert_eq!(c.source_tables, Some(4));
}

#[sqlx::test]
async fn drill_scheduler_queues_a_due_drill_once(db: PgPool) {
	seed(&db).await;
	add_backup(&db, 10, "succeeded", Some(4)).await;
	make_due(&db).await;
	let s = state(db.clone());

	super::schedule::sweep(&s).await;
	assert_eq!(
		drills(&db).await,
		[("queued".to_owned(), Some(10), Some(4), None)]
	);
	let (kind, lock): (JobKind, String) =
		sqlx::query_as("SELECT kind, lock_key FROM jobs WHERE kind = 'restore_drill'")
			.fetch_one(&db)
			.await
			.unwrap();
	assert_eq!(kind, JobKind::RestoreDrill);
	assert_eq!(lock, format!("drill:{DB}"));
	let ahead: bool =
		sqlx::query_scalar("SELECT drill_next_run_at > NOW() FROM databases WHERE id = $1")
			.bind(DB)
			.fetch_one(&db)
			.await
			.unwrap();
	assert!(ahead, "the schedule moved on");

	// Due again while the first is still queued: skipped, not stacked.
	make_due(&db).await;
	super::schedule::sweep(&s).await;
	assert_eq!(drills(&db).await.len(), 1);
}

#[sqlx::test]
async fn drill_scheduler_records_a_failure_when_there_is_no_backup(db: PgPool) {
	seed(&db).await;
	add_backup(&db, 11, "failed", None).await;
	make_due(&db).await;

	super::schedule::sweep(&state(db.clone())).await;
	assert_eq!(
		drills(&db).await,
		[(
			"failed".to_owned(),
			None,
			None,
			Some(super::NO_BACKUP.to_owned())
		)]
	);
	let jobs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM jobs")
		.fetch_one(&db)
		.await
		.unwrap();
	assert_eq!(jobs, 0);
}

#[sqlx::test]
async fn drill_scheduler_leaves_databases_without_a_schedule_alone(db: PgPool) {
	seed(&db).await;
	add_backup(&db, 10, "succeeded", None).await;
	super::schedule::sweep(&state(db.clone())).await;
	assert!(drills(&db).await.is_empty(), "drills are off by default");
}

#[sqlx::test]
async fn failed_drill_job_marks_the_drill_failed_and_keeps_the_backup(db: PgPool) {
	seed(&db).await;
	add_backup(&db, 10, "succeeded", Some(2)).await;
	let s = state(db.clone());
	let database = crate::databases::find_by_id(&db, DatabaseId(DB))
		.await
		.unwrap()
		.unwrap();
	let drill = super::queue(&s, &database, None).await.unwrap();
	assert_eq!(drill.status, DrillStatus::Queued);
	assert!(matches!(
		super::queue(&s, &database, None).await,
		Err(ServerError::Conflict(_))
	));

	fail_target(
		&s,
		JobKind::RestoreDrill,
		drill.id.0,
		&JobFailure::Failed("restore failed: psql exited with code 3".into()),
	)
	.await;
	let row = super::find_by_id(&db, drill.id).await.unwrap().unwrap();
	assert_eq!(row.status, DrillStatus::Failed);
	assert_eq!(
		row.error.as_deref(),
		Some("restore failed: psql exited with code 3")
	);
	assert!(row.finished_at.is_some());
	let (backup, database_status): (String, String) = sqlx::query_as(
		"SELECT b.status, d.status FROM backups b JOIN databases d ON d.id = b.database_id
		WHERE b.id = 10",
	)
	.fetch_one(&db)
	.await
	.unwrap();
	assert_eq!(
		(backup.as_str(), database_status.as_str()),
		("succeeded", "running")
	);

	// A later failure report does not rewrite a finished drill.
	fail_target(
		&s,
		JobKind::RestoreDrill,
		drill.id.0,
		&JobFailure::Cancelled,
	)
	.await;
	let row = super::find_by_id(&db, drill.id).await.unwrap().unwrap();
	assert_eq!(
		row.error.as_deref(),
		Some("restore failed: psql exited with code 3")
	);
}
