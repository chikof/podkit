//! Volume backups: the archive key and part plan, retention, the restore state machine,
//! the consistency option (the owner is stopped and started around the read), tenancy,
//! permissions, the pages and that no S3 secret is ever rendered.
//!
//! The agent is a fake that records what it is asked, and so is the object store, which
//! answers the three multipart requests the control plane makes itself. One live test
//! (ignored) runs the real agent against Podman and a Garage container.
//!
//! `cargo test -p podkit-core -p podkit-ui --all-features -- volume_backup`

mod live;

use std::sync::atomic::Ordering;

use axum::http::StatusCode;
use serde_json::{Value, json};
use sqlx::PgPool;

use super::{AdvanceError, RestoreState, advance_restore, object_key, plan_parts};
use crate::ids::{TeamId, VolumeBackupId, VolumePolicyId, VolumeRestoreId};
use crate::jobs::{self, JobFailure, JobKind};
use crate::tests::volume_backups::{CONTAINER, Fx, backed_up};
use crate::tests::{call, server_id, user_with_team};

// --- pure pieces ---------------------------------------------------------------------------

#[test]
fn volume_backup_object_key_names_team_policy_and_backup() {
	let key = |prefix: &str| object_key(prefix, TeamId(1), VolumePolicyId(2), VolumeBackupId(3));
	assert_eq!(key(""), "1/volumes/2/3.tar.gz");
	assert_eq!(key("podkit"), "podkit/1/volumes/2/3.tar.gz");
	assert_eq!(key("a/b/"), "a/b/1/volumes/2/3.tar.gz");
}

#[test]
fn volume_backup_parts_stay_under_the_s3_limit_and_cover_the_archive() {
	const MIB: u64 = 1 << 20;
	// A small archive is one part, and S3 accepts a short last part.
	assert_eq!(plan_parts(0), (64 * MIB, 1));
	assert_eq!(plan_parts(1234), (64 * MIB, 1));
	assert_eq!(plan_parts(64 * MIB), (64 * MIB, 1));
	assert_eq!(plan_parts(64 * MIB + 1), (64 * MIB, 2));
	for size in [10 * MIB, 5 << 30, 100 << 30, 1 << 40, 5 << 40] {
		let (part, count) = plan_parts(size);
		assert!(part >= 64 * MIB && part % MIB == 0, "{size}: part {part}");
		assert!(
			count >= 1 && u64::from(count) <= 10_000,
			"{size}: {count} parts"
		);
		assert!(
			part * u64::from(count) >= size,
			"{size}: parts cover the archive"
		);
	}
}

#[test]
fn volume_backup_restore_flow_runs_in_order_and_ends_in_a_final_state() {
	use RestoreState::{Cancelled, Failed, Queued, Restoring, Starting, Stopping, Succeeded};
	let all = [
		Queued, Stopping, Restoring, Starting, Succeeded, Failed, Cancelled,
	];
	let allowed = |from: RestoreState, to: RestoreState| {
		matches!(
			(from, to),
			(Queued, Stopping)
				| (Stopping, Restoring)
				| (Restoring, Starting)
				| (Starting, Succeeded)
				| (Queued | Stopping | Restoring | Starting, Failed | Cancelled)
		)
	};
	for from in all {
		for to in all {
			assert_eq!(
				from.can_transition_to(to),
				allowed(from, to),
				"{from:?} -> {to:?}"
			);
		}
	}
	for done in [Succeeded, Failed, Cancelled] {
		assert!(done.is_final());
		assert!(
			all.iter().all(|to| !done.can_transition_to(*to)),
			"{done:?} is the end"
		);
	}
}

// --- backups -------------------------------------------------------------------------------

#[sqlx::test]
async fn volume_backup_streams_the_archive_to_s3_and_records_the_manifest(db: PgPool) {
	let f = Fx::new(db).await;
	let (s, settings) = f.configure(json!({})).await;
	assert_eq!(s, StatusCode::OK, "{settings}");

	let (id, result) = f.back_up().await;
	result.unwrap();

	let (status, error, _, entries) = f.backup_row(id).await;
	assert_eq!(
		(status.as_str(), error, entries),
		("succeeded", None, Some(3))
	);
	let (_, listed) = f.call("GET", "", None).await;
	let row = &listed[0];
	assert_eq!(row["status"], "succeeded");
	assert_eq!(row["size_bytes"], 1234);
	assert_eq!(row["raw_bytes"], 4096);
	assert_eq!(row["entries"], 3);
	assert_eq!(row["sha256"], "ab".repeat(32));
	let key = row["object_key"].as_str().unwrap();
	assert!(
		key.starts_with(&format!("pre/{}/volumes/", f.team)) && key.ends_with(".tar.gz"),
		"{key}"
	);

	// The agent archived the volume, then sent one part; the control plane made the
	// multipart requests with the key it recorded.
	assert_eq!(
		f.agent.calls(),
		[
			format!("dump {}", f.volume_name),
			"upload 1 part(s) of 67108864".to_owned()
		]
	);
	assert_eq!(
		f.s3.calls(),
		[format!("create {key}"), format!("complete {key} parts=1")]
	);
}

#[sqlx::test]
async fn volume_backup_live_mode_keeps_the_app_running_and_says_so(db: PgPool) {
	let f = Fx::new(db).await;
	f.configure(json!({ "consistency": "live" })).await;

	let (id, result) = f.back_up().await;
	result.unwrap();

	assert!(
		f.agent
			.calls()
			.iter()
			.all(|c| !c.starts_with("stop") && !c.starts_with("start")),
		"{:?}",
		f.agent.calls()
	);
	let (_, _, warning, _) = f.backup_row(id).await;
	assert!(warning.unwrap().contains("web kept running"));
}

#[sqlx::test]
async fn volume_backup_stop_mode_stops_the_app_for_the_read_and_starts_it_before_the_upload(
	db: PgPool,
) {
	let f = Fx::new(db).await;
	f.configure(json!({ "consistency": "stop" })).await;

	let (id, result) = f.back_up().await;
	result.unwrap();

	assert_eq!(
		f.agent.calls(),
		[
			format!("stop {CONTAINER}"),
			format!("dump {}", f.volume_name),
			format!("start {CONTAINER}"),
			"upload 1 part(s) of 67108864".to_owned(),
		]
	);
	// A consistent copy has nothing to warn about.
	let (status, _, warning, _) = f.backup_row(id).await;
	assert_eq!((status.as_str(), warning), ("succeeded", None));
}

#[sqlx::test]
async fn volume_backup_stop_mode_starts_the_app_again_when_the_read_fails(db: PgPool) {
	let f = Fx::new(db).await;
	f.configure(json!({ "consistency": "stop" })).await;
	f.agent.fail_dump.store(true, Ordering::SeqCst);

	let (id, result) = f.back_up().await;

	assert!(matches!(result, Err(JobFailure::Failed(m)) if m.contains("boom")));
	assert_eq!(
		f.agent.calls(),
		[
			format!("stop {CONTAINER}"),
			format!("dump {}", f.volume_name),
			format!("start {CONTAINER}")
		]
	);
	let (status, error, _, _) = f.backup_row(id).await;
	assert_eq!(status, "failed");
	assert!(error.unwrap().contains("boom"));
	assert!(f.s3.calls().is_empty(), "nothing was uploaded");
}

#[sqlx::test]
async fn volume_backup_a_stack_volume_stops_every_running_service(db: PgPool) {
	let f = Fx::new(db).await;
	let (s, stack) = call(
		&f.app,
		"POST",
		&format!("/api/v1/teams/{}/projects/{}/stacks", f.team, f.project),
		Some(&f.token),
		Some(json!({
			"name": "blog", "server_id": server_id(&f.db, f.team).await, "env": {},
			"compose_yaml": "services:\n  web:\n    image: ghcr.io/acme/web:1\n    volumes:\n      - data:/var/lib/web\nvolumes:\n  data:\n",
		})),
	)
	.await;
	assert_eq!(s, StatusCode::CREATED, "{stack}");
	let stack = stack["id"].as_i64().unwrap();
	sqlx::query(
		"UPDATE stack_services SET status = 'running', container_id = 'ctr-stack-1' WHERE stack_id = $1",
	)
	.bind(stack)
	.execute(&f.db)
	.await
	.unwrap();
	let url = |rest: &str| {
		format!(
			"/api/v1/teams/{}/projects/{}/stacks/{stack}/volumes/data/backups{rest}",
			f.team, f.project
		)
	};

	let (s, saved) = call(
		&f.app,
		"PUT",
		&url("/settings"),
		Some(&f.token),
		Some(json!({ "target_id": f.target, "consistency": "stop" })),
	)
	.await;
	assert_eq!(s, StatusCode::OK, "{saved}");
	let (s, backup) = call(&f.app, "POST", &url(""), Some(&f.token), None).await;
	assert_eq!(s, StatusCode::ACCEPTED, "{backup}");
	let id = backup["id"].as_i64().unwrap();
	let job = jobs::latest_for(&f.db, JobKind::VolumeBackup, id)
		.await
		.unwrap()
		.unwrap();
	// Stacks take the same lock their deploys do.
	assert_eq!(job.lock_key, format!("stack:{stack}"));
	f.run(job).await.unwrap();

	assert_eq!(
		f.agent.calls(),
		[
			"stop ctr-stack-1".to_owned(),
			format!("dump podkit-stack-{stack}-data"),
			"start ctr-stack-1".to_owned(),
			"upload 1 part(s) of 67108864".to_owned(),
		]
	);
	// A volume the compose file does not have is not found.
	let (s, _) = call(
		&f.app,
		"GET",
		&format!(
			"/api/v1/teams/{}/projects/{}/stacks/{stack}/volumes/nope/backups",
			f.team, f.project
		),
		Some(&f.token),
		None,
	)
	.await;
	assert_eq!(s, StatusCode::NOT_FOUND);
}

#[sqlx::test]
async fn volume_backup_retention_removes_the_oldest_and_their_objects(db: PgPool) {
	let f = Fx::new(db).await;
	f.configure(json!({ "keep": 2 })).await;

	let mut keys = Vec::new();
	for _ in 0..4 {
		let (id, result) = f.back_up().await;
		result.unwrap();
		let key: String = sqlx::query_scalar("SELECT object_key FROM volume_backups WHERE id = $1")
			.bind(id)
			.fetch_one(&f.db)
			.await
			.unwrap();
		keys.push(key);
	}

	assert_eq!(f.backup_count().await, 2);
	let (_, listed) = f.call("GET", "", None).await;
	let kept: Vec<_> = listed
		.as_array()
		.unwrap()
		.iter()
		.map(|b| b["object_key"].as_str().unwrap().to_owned())
		.collect();
	assert_eq!(
		kept,
		[keys[3].clone(), keys[2].clone()],
		"the newest two stay, newest first"
	);
	let mut deleted = f.s3.deleted();
	deleted.sort();
	assert_eq!(deleted, [keys[0].clone(), keys[1].clone()]);
}

#[sqlx::test]
async fn volume_backup_retention_counts_only_succeeded_backups(db: PgPool) {
	let f = Fx::new(db).await;
	f.configure(json!({ "keep": 1 })).await;
	f.back_up().await.1.unwrap();
	f.agent.fail_dump.store(true, Ordering::SeqCst);
	let (_, failed) = f.back_up().await;
	assert!(failed.is_err());

	// The failure took nobody's place: the one good backup is still there.
	let (_, listed) = f.call("GET", "", None).await;
	let statuses: Vec<_> = listed
		.as_array()
		.unwrap()
		.iter()
		.map(|b| b["status"].as_str().unwrap().to_owned())
		.collect();
	assert_eq!(statuses, ["failed", "succeeded"]);
	assert!(f.s3.deleted().is_empty());
}

#[sqlx::test]
async fn volume_backup_a_second_backup_waits_for_the_first(db: PgPool) {
	let f = Fx::new(db).await;
	f.configure(json!({})).await;
	let (s, _) = f.call("POST", "", None).await;
	assert_eq!(s, StatusCode::ACCEPTED);
	let (s, body) = f.call("POST", "", None).await;
	assert_eq!(s, StatusCode::CONFLICT, "{body}");
	assert_eq!(f.backup_count().await, 1);
}

#[sqlx::test]
async fn volume_backup_without_a_destination_is_refused(db: PgPool) {
	let f = Fx::new(db).await;
	let (s, body) = f.call("POST", "", None).await;
	assert_eq!(s, StatusCode::BAD_REQUEST, "{body}");
	assert_eq!(f.backup_count().await, 0);
}

#[sqlx::test]
async fn volume_backup_settings_are_validated(db: PgPool) {
	let f = Fx::new(db).await;
	for (extra, what) in [
		(json!({ "keep": 0 }), "keep below 1"),
		(json!({ "keep": 400 }), "keep above 365"),
		(
			json!({ "schedule": "every day" }),
			"a schedule that is not cron",
		),
		(
			json!({ "schedule": "0 0 3 * * *" }),
			"a schedule with seconds",
		),
		(
			json!({ "target_id": null, "schedule": "0 3 * * *" }),
			"a schedule without a destination",
		),
	] {
		let (s, body) = f.configure(extra).await;
		assert_eq!(s, StatusCode::BAD_REQUEST, "{what}: {body}");
	}
	let (s, saved) = f
		.configure(json!({ "schedule": "0 3 * * *", "keep": 3 }))
		.await;
	assert_eq!(s, StatusCode::OK, "{saved}");
	assert_eq!(saved["schedule"], "0 3 * * *");
	assert_eq!(saved["keep"], 3);
	assert!(
		saved["next_run_at"].is_string(),
		"the next run is computed: {saved}"
	);
}

#[sqlx::test]
async fn volume_backup_scheduler_queues_due_volumes_once(db: PgPool) {
	let f = Fx::new(db).await;
	f.configure(json!({ "schedule": "* * * * *" })).await;
	sqlx::query("UPDATE volume_backup_policies SET next_run_at = NOW() - INTERVAL '1 minute'")
		.execute(&f.db)
		.await
		.unwrap();

	super::schedule::sweep(&f.state).await;
	assert_eq!(f.backup_count().await, 1);
	let by: Option<i64> = sqlx::query_scalar("SELECT triggered_by FROM volume_backups")
		.fetch_one(&f.db)
		.await
		.unwrap();
	assert_eq!(by, None, "a scheduled backup has no user");
	let ahead: bool = sqlx::query_scalar("SELECT next_run_at > NOW() FROM volume_backup_policies")
		.fetch_one(&f.db)
		.await
		.unwrap();
	assert!(ahead);

	// Not due any more, and the first is still queued: nothing piles up.
	super::schedule::sweep(&f.state).await;
	assert_eq!(f.backup_count().await, 1);
}

#[sqlx::test]
async fn volume_backup_restore_needs_the_volume_typed(db: PgPool) {
	let f = Fx::new(db).await;
	let backup = backed_up(&f).await;

	for confirm in ["", "/dat", "/DATA", "data"] {
		let (s, body) = f
			.restore(backup, json!({ "mode": "same", "confirm": confirm }))
			.await;
		assert_eq!(s, StatusCode::BAD_REQUEST, "{confirm:?}: {body}");
	}
	let restores: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM volume_restores")
		.fetch_one(&f.db)
		.await
		.unwrap();
	assert_eq!(restores, 0, "nothing was queued");
	assert!(f.agent.calls().is_empty());
}

#[sqlx::test]
async fn volume_backup_restore_in_place_stops_the_app_then_starts_it(db: PgPool) {
	let f = Fx::new(db).await;
	let backup = backed_up(&f).await;

	let (s, restore) = f
		.restore(backup, json!({ "mode": "same", "confirm": "/data" }))
		.await;
	assert_eq!(s, StatusCode::ACCEPTED, "{restore}");
	let id = restore["id"].as_i64().unwrap();
	assert_eq!(restore["status"], "queued");
	assert_eq!(restore["dest_volume"], f.volume_name);
	let job = f.restore_job(id).await;
	assert_eq!(job.lock_key, format!("app:{}", f.application));
	f.run(job).await.unwrap();

	assert_eq!(
		f.agent.calls(),
		[
			format!("stop {CONTAINER}"),
			format!("restore {} wipe=true", f.volume_name),
			format!("sha256 {}", "ab".repeat(32)),
			format!("start {CONTAINER}"),
		]
	);
	assert_eq!(f.restore_status(id).await, ("succeeded".to_owned(), None));
}

#[sqlx::test]
async fn volume_backup_restore_into_a_new_volume_leaves_the_app_running(db: PgPool) {
	let f = Fx::new(db).await;
	let backup = backed_up(&f).await;

	let (s, restore) = f
		.restore(
			backup,
			json!({ "mode": "new", "new_volume": "/restored", "confirm": "/data" }),
		)
		.await;
	assert_eq!(s, StatusCode::ACCEPTED, "{restore}");
	let id = restore["id"].as_i64().unwrap();
	let dest = restore["dest_volume"].as_str().unwrap().to_owned();
	assert_ne!(dest, f.volume_name);
	// The new volume is a volume of the application, mounted from its next deploy.
	let mount: String = sqlx::query_scalar("SELECT mount_path FROM volumes WHERE name = $1")
		.bind(&dest)
		.fetch_one(&f.db)
		.await
		.unwrap();
	assert_eq!(mount, "/restored");
	f.run(f.restore_job(id).await).await.unwrap();

	// Nothing was stopped, and nothing was deleted from the new volume.
	assert_eq!(
		f.agent.calls(),
		[
			format!("restore {dest} wipe=false"),
			format!("sha256 {}", "ab".repeat(32))
		]
	);
	assert_eq!(f.restore_status(id).await.0, "succeeded");
}

#[sqlx::test]
async fn volume_backup_restore_into_a_new_volume_needs_a_free_path(db: PgPool) {
	let f = Fx::new(db).await;
	let backup = backed_up(&f).await;
	for new in [json!("/data"), json!("relative"), json!("/"), Value::Null] {
		let (s, body) = f
			.restore(
				backup,
				json!({ "mode": "new", "new_volume": new, "confirm": "/data" }),
			)
			.await;
		assert!(
			s == StatusCode::BAD_REQUEST || s == StatusCode::CONFLICT,
			"{new}: {s} {body}"
		);
	}
	let volumes: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM volumes")
		.fetch_one(&f.db)
		.await
		.unwrap();
	assert_eq!(volumes, 1, "a refused restore adds no volume");
}

#[sqlx::test]
async fn volume_backup_a_failed_restore_still_starts_the_app(db: PgPool) {
	let f = Fx::new(db).await;
	let backup = backed_up(&f).await;
	let (_, restore) = f
		.restore(backup, json!({ "mode": "same", "confirm": "/data" }))
		.await;
	let id = restore["id"].as_i64().unwrap();
	f.agent.fail_restore.store(true, Ordering::SeqCst);

	let result = f.run(f.restore_job(id).await).await;

	assert!(
		matches!(&result, Err(JobFailure::Failed(m)) if m.contains("disk full")),
		"{result:?}"
	);
	assert_eq!(
		f.agent.calls(),
		[
			format!("stop {CONTAINER}"),
			format!("restore {} wipe=true", f.volume_name),
			format!("sha256 {}", "ab".repeat(32)),
			format!("start {CONTAINER}"),
		]
	);
	let (status, error) = f.restore_status(id).await;
	assert_eq!(status, "failed");
	assert!(error.unwrap().contains("disk full"));
}

#[sqlx::test]
async fn volume_backup_a_restore_that_cannot_start_fails_without_touching_the_app(db: PgPool) {
	let f = Fx::new(db).await;
	let backup = backed_up(&f).await;
	let (_, restore) = f
		.restore(backup, json!({ "mode": "same", "confirm": "/data" }))
		.await;
	let id = restore["id"].as_i64().unwrap();
	// A backup with no recorded checksum cannot be checked, so it is not restored.
	sqlx::query("UPDATE volume_backups SET sha256 = NULL")
		.execute(&f.db)
		.await
		.unwrap();

	let result = f.run(f.restore_job(id).await).await;

	assert!(
		matches!(&result, Err(JobFailure::Failed(m)) if m.contains("checksum")),
		"{result:?}"
	);
	assert_eq!(f.restore_status(id).await.0, "failed");
	assert!(f.agent.calls().is_empty(), "nothing was stopped");
}

#[sqlx::test]
async fn volume_backup_the_state_machine_refuses_out_of_order_moves(db: PgPool) {
	let f = Fx::new(db).await;
	let backup = backed_up(&f).await;
	let (_, restore) = f
		.restore(backup, json!({ "mode": "same", "confirm": "/data" }))
		.await;
	let id = VolumeRestoreId(restore["id"].as_i64().unwrap());

	let skip = advance_restore(
		&f.db,
		id,
		RestoreState::Queued,
		RestoreState::Restoring,
		None,
	)
	.await;
	assert!(matches!(skip, Err(AdvanceError::Order { .. })));
	advance_restore(
		&f.db,
		id,
		RestoreState::Queued,
		RestoreState::Stopping,
		None,
	)
	.await
	.unwrap();
	// Someone else moved it already.
	let stale = advance_restore(
		&f.db,
		id,
		RestoreState::Queued,
		RestoreState::Stopping,
		None,
	)
	.await;
	assert!(matches!(
		stale,
		Err(AdvanceError::Moved(RestoreState::Queued))
	));
	advance_restore(
		&f.db,
		id,
		RestoreState::Stopping,
		RestoreState::Failed,
		Some("boom"),
	)
	.await
	.unwrap();
	let after = advance_restore(
		&f.db,
		id,
		RestoreState::Failed,
		RestoreState::Starting,
		None,
	)
	.await;
	assert!(
		matches!(after, Err(AdvanceError::Order { .. })),
		"a final state is the end"
	);
	assert_eq!(
		f.restore_status(id.0).await,
		("failed".to_owned(), Some("boom".to_owned()))
	);
	let finished: bool =
		sqlx::query_scalar("SELECT finished_at IS NOT NULL FROM volume_restores WHERE id = $1")
			.bind(id)
			.fetch_one(&f.db)
			.await
			.unwrap();
	assert!(finished);
}

#[sqlx::test]
async fn volume_backup_a_backup_can_be_deleted_with_its_object(db: PgPool) {
	let f = Fx::new(db).await;
	let backup = backed_up(&f).await;
	let key: String = sqlx::query_scalar("SELECT object_key FROM volume_backups WHERE id = $1")
		.bind(backup)
		.fetch_one(&f.db)
		.await
		.unwrap();

	let (s, body) = f.call("DELETE", &format!("/{backup}"), None).await;
	assert_eq!(s, StatusCode::OK, "{body}");
	assert_eq!(f.s3.deleted(), [key]);
	assert_eq!(f.backup_count().await, 0);
	// Restoring what is gone is not found, not a crash.
	let (s, _) = f
		.restore(backup, json!({ "mode": "same", "confirm": "/data" }))
		.await;
	assert_eq!(s, StatusCode::NOT_FOUND);
}

// --- tenancy and permissions ---------------------------------------------------------------

#[sqlx::test]
async fn volume_backup_another_teams_volume_is_not_found(db: PgPool) {
	let f = Fx::new(db).await;
	let backup = backed_up(&f).await;
	let (bob, bob_team) = user_with_team(&f.app, "bob").await;
	let (_, bob_project) = call(
		&f.app,
		"POST",
		&format!("/api/v1/teams/{bob_team}/projects"),
		Some(&bob),
		Some(json!({ "name": "mine", "slug": "mine" })),
	)
	.await;
	let bob_project = bob_project["id"].as_i64().unwrap();

	// Bob's own team and project, alice's application and volume ids.
	let theirs = |rest: &str| {
		format!(
			"/api/v1/teams/{bob_team}/projects/{bob_project}/applications/{}/volumes/{}/backups{rest}",
			f.application, f.volume
		)
	};
	for (method, rest, body) in [
		("GET", String::new(), None),
		("POST", String::new(), None),
		("GET", "/settings".to_owned(), None),
		("PUT", "/settings".to_owned(), Some(json!({ "keep": 1 }))),
		("DELETE", format!("/{backup}"), None),
		(
			"POST",
			format!("/{backup}/restore"),
			Some(json!({ "mode": "same", "confirm": "/data" })),
		),
	] {
		let (s, _) = call(&f.app, method, &theirs(&rest), Some(&bob), body).await;
		assert_eq!(s, StatusCode::NOT_FOUND, "{method} {rest}");
	}
	// Alice's own project path with bob's token is no better.
	let (s, _) = call(&f.app, "GET", &f.api(""), Some(&bob), None).await;
	assert!(
		s == StatusCode::NOT_FOUND || s == StatusCode::FORBIDDEN,
		"{s}"
	);

	// Nothing changed: still one backup, and no restore.
	assert_eq!(f.backup_count().await, 1);
	assert!(f.agent.calls().is_empty());
}

#[sqlx::test]
async fn volume_backup_a_destination_of_another_team_is_not_found(db: PgPool) {
	let f = Fx::new(db).await;
	let (_, other_team) = user_with_team(&f.app, "bob").await;
	let foreign = f.state.secrets.encrypt("x").unwrap();
	sqlx::query(
		"INSERT INTO backup_targets (id, team_id, name, endpoint, region, bucket, access_key_id, secret_access_key)
		VALUES (7002, $1, 'theirs', 'http://127.0.0.1:1', 'us-east-1', 'b', 'k', $2)",
	)
	.bind(other_team)
	.bind(foreign)
	.execute(&f.db)
	.await
	.unwrap();

	let (s, body) = f.configure(json!({ "target_id": 7002 })).await;
	assert_eq!(s, StatusCode::NOT_FOUND, "{body}");
	let (s, settings) = f.call("GET", "/settings", None).await;
	assert_eq!(s, StatusCode::OK);
	assert_eq!(settings["target_id"], Value::Null);
}

// --- the destination goes away --------------------------------------------------------------

#[sqlx::test]
async fn volume_backup_deleting_the_destination_keeps_the_history_but_not_the_restore(db: PgPool) {
	let f = Fx::new(db).await;
	let backup = backed_up(&f).await;
	sqlx::query("DELETE FROM backup_targets WHERE id = $1")
		.bind(f.target)
		.execute(&f.db)
		.await
		.unwrap();

	let (_, listed) = f.call("GET", "", None).await;
	assert_eq!(listed[0]["target_id"], Value::Null);
	let (s, body) = f
		.restore(backup, json!({ "mode": "same", "confirm": "/data" }))
		.await;
	assert_eq!(s, StatusCode::BAD_REQUEST, "{body}");
	let (s, _) = f.call("POST", "", None).await;
	assert_eq!(s, StatusCode::BAD_REQUEST, "no destination, no backup");
}
