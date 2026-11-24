//! Database-row helpers for the web UI tests, which may not use `sqlx`
//! themselves.

use sqlx::PgPool;

/// The id of `team`'s local server.
pub async fn local_server(db: &PgPool, team: i64) -> i64 {
	sqlx::query_scalar("SELECT id FROM servers WHERE team_id = $1")
		.bind(team)
		.fetch_one(db)
		.await
		.unwrap()
}

/// The id of the database called `name`.
pub async fn database_id(db: &PgPool, name: &str) -> i64 {
	sqlx::query_scalar("SELECT id FROM databases WHERE name = $1")
		.bind(name)
		.fetch_one(db)
		.await
		.unwrap()
}

/// The kinds of the jobs queued for `database`, oldest first.
pub async fn job_kinds(db: &PgPool, database: i64) -> Vec<String> {
	sqlx::query_scalar("SELECT kind FROM jobs WHERE target_id = $1 ORDER BY id")
		.bind(database)
		.fetch_all(db)
		.await
		.unwrap()
}

/// Puts the database in `status`, with or without a container.
pub async fn set_state(db: &PgPool, database: i64, status: &str, container: Option<&str>) {
	sqlx::query("UPDATE databases SET status = $2, container_id = $3 WHERE id = $1")
		.bind(database)
		.bind(status)
		.bind(container)
		.execute(db)
		.await
		.unwrap();
}

/// A finished or running lifecycle job of `database`.
pub async fn add_job(db: &PgPool, database: i64, kind: &str, status: &str) {
	sqlx::query(
		"INSERT INTO jobs (id, kind, server_id, target_id, lock_key, status)
		 VALUES ($1, $2, (SELECT server_id FROM databases WHERE id = $3), $3, $4, $5)",
	)
	.bind(database + 1000)
	.bind(kind)
	.bind(database)
	.bind(format!("db:{database}"))
	.bind(status)
	.execute(db)
	.await
	.unwrap();
}

/// A database's stored backup settings: target, schedule, keep and whether a
/// next run is set.
pub async fn backup_settings(
	db: &PgPool,
	database: i64,
) -> (Option<i64>, Option<String>, i32, bool) {
	sqlx::query_as(
		"SELECT backup_target_id, backup_schedule, backup_keep, backup_next_run_at IS NOT NULL
		 FROM databases WHERE id = $1",
	)
	.bind(database)
	.fetch_one(db)
	.await
	.unwrap()
}

/// Registers a backup target of `team` directly and returns its id.
pub async fn add_backup_target(db: &PgPool, id: i64, team: i64, name: &str) -> i64 {
	sqlx::query_scalar(
		"INSERT INTO backup_targets (id, team_id, name, endpoint, region, bucket, access_key_id, secret_access_key)
		 VALUES ($1, $2, $3, 'https://s3.example.com', 'us-east-1', 'b', 'k', ''::bytea) RETURNING id",
	)
	.bind(id)
	.bind(team)
	.bind(name)
	.fetch_one(db)
	.await
	.unwrap()
}

/// The status of the database.
pub async fn status_of(db: &PgPool, database: i64) -> String {
	sqlx::query_scalar("SELECT status FROM databases WHERE id = $1")
		.bind(database)
		.fetch_one(db)
		.await
		.unwrap()
}

/// Sets the status of every job queued for `database`.
pub async fn set_jobs(db: &PgPool, database: i64, status: &str) {
	sqlx::query("UPDATE jobs SET status = $2 WHERE target_id = $1")
		.bind(database)
		.bind(status)
		.execute(db)
		.await
		.unwrap();
}

/// Removes every job queued for `database`.
pub async fn delete_jobs(db: &PgPool, database: i64) {
	sqlx::query("DELETE FROM jobs WHERE target_id = $1")
		.bind(database)
		.execute(db)
		.await
		.unwrap();
}

/// Inserts a backup of `database` in `status` and returns its id. `target` is
/// `None` for a backup whose target was deleted; `by` is who started it.
#[allow(clippy::too_many_arguments)]
pub async fn add_backup(
	db: &PgPool,
	id: i64,
	database: i64,
	target: Option<i64>,
	status: &str,
	size: Option<i64>,
	error: Option<&str>,
	by: Option<i64>,
) -> i64 {
	sqlx::query_scalar(
		"INSERT INTO backups (id, database_id, target_id, status, size_bytes, error, triggered_by,
			object_key, finished_at)
		 VALUES ($1, $2, $3, $4, $5, $6, $7, NULL,
			CASE WHEN $4 IN ('succeeded', 'failed') THEN now() END) RETURNING id",
	)
	.bind(id)
	.bind(database)
	.bind(target)
	.bind(status)
	.bind(size)
	.bind(error)
	.bind(by)
	.fetch_one(db)
	.await
	.unwrap()
}

/// How many backups of `database` exist.
pub async fn backup_count(db: &PgPool, database: i64) -> i64 {
	sqlx::query_scalar("SELECT count(*) FROM backups WHERE database_id = $1")
		.bind(database)
		.fetch_one(db)
		.await
		.unwrap()
}

/// How many backup targets exist, in any team.
pub async fn target_count(db: &PgPool) -> i64 {
	sqlx::query_scalar("SELECT count(*) FROM backup_targets")
		.fetch_one(db)
		.await
		.unwrap()
}

/// The id of the user called `name`.
pub async fn user_id(db: &PgPool, name: &str) -> i64 {
	sqlx::query_scalar("SELECT id FROM users WHERE name = $1")
		.bind(name)
		.fetch_one(db)
		.await
		.unwrap()
}

/// Points `database` at `target` for its backups.
pub async fn set_backup_target(db: &PgPool, database: i64, target: i64) {
	sqlx::query("UPDATE databases SET backup_target_id = $2 WHERE id = $1")
		.bind(database)
		.bind(target)
		.execute(db)
		.await
		.unwrap();
}

/// The stored (encrypted) secret of the only backup target, as text.
pub async fn stored_secret(db: &PgPool) -> Vec<u8> {
	sqlx::query_scalar("SELECT secret_access_key FROM backup_targets LIMIT 1")
		.fetch_one(db)
		.await
		.unwrap()
}

/// Removes `database`'s backup target.
pub async fn clear_backup_target(db: &PgPool, database: i64) {
	sqlx::query("UPDATE databases SET backup_target_id = NULL WHERE id = $1")
		.bind(database)
		.execute(db)
		.await
		.unwrap();
}

/// The environment `database` lives in.
pub async fn database_environment(db: &PgPool, database: i64) -> i64 {
	sqlx::query_scalar("SELECT environment_id FROM databases WHERE id = $1")
		.bind(database)
		.fetch_one(db)
		.await
		.unwrap()
}

/// Records one live metrics sample for `database`.
pub async fn add_metric_sample(db: &PgPool, database: i64, cpu: f64, mem: i64) {
	sqlx::query(
		"INSERT INTO resource_metrics (kind, resource_id, at, rollup, cpu, mem) \
		 VALUES ('database', $1, now(), false, $2, $3)",
	)
	.bind(database)
	.bind(cpu)
	.bind(mem)
	.execute(db)
	.await
	.unwrap();
}
