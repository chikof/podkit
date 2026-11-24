//! Raw-SQL helpers for the projects grid and environment tests: arranging rows the pages
//! then show. They live here because the UI never touches the database, tests included.

use sqlx::PgPool;

pub async fn env_id(db: &PgPool, project: i64, slug: &str) -> i64 {
	sqlx::query_scalar("SELECT id FROM environments WHERE project_id = $1 AND slug = $2")
		.bind(project)
		.bind(slug)
		.fetch_one(db)
		.await
		.unwrap()
}

pub async fn environment_name(db: &PgPool, id: i64) -> String {
	sqlx::query_scalar("SELECT name FROM environments WHERE id = $1")
		.bind(id)
		.fetch_one(db)
		.await
		.unwrap()
}

/// Adds one Redis database per name to `project`, on the server of database 6001 (see
/// `seed_app_and_database`).
pub async fn add_redis_databases(db: &PgPool, project: i64, names: &[&str]) {
	for (i, name) in names.iter().enumerate() {
		sqlx::query(
			"INSERT INTO databases (id, project_id, server_id, name, engine, version, credentials)
			 SELECT $1, $2, server_id, $3, 'redis', '7', 'x' FROM databases WHERE id = 6001",
		)
		.bind(7000 + i64::try_from(i).unwrap())
		.bind(project)
		.bind(name)
		.execute(db)
		.await
		.unwrap();
	}
}

pub async fn set_database_name(db: &PgPool, name: &str) {
	sqlx::query("UPDATE databases SET name = $1")
		.bind(name)
		.execute(db)
		.await
		.unwrap();
}

pub async fn set_database_status(db: &PgPool, status: &str) {
	sqlx::query("UPDATE databases SET status = $1")
		.bind(status)
		.execute(db)
		.await
		.unwrap();
}

pub async fn move_databases_to(db: &PgPool, environment: i64) {
	sqlx::query("UPDATE databases SET environment_id = $1")
		.bind(environment)
		.execute(db)
		.await
		.unwrap();
}

pub async fn delete_databases(db: &PgPool) {
	sqlx::query("DELETE FROM databases")
		.execute(db)
		.await
		.unwrap();
}

/// Names every application `name` and builds it without a Dockerfile, so it has neither a
/// git host's mark nor Docker's.
pub async fn set_application_nixpacks(db: &PgPool, name: &str) {
	sqlx::query("UPDATE applications SET build_strategy = 'nixpacks', name = $1")
		.bind(name)
		.execute(db)
		.await
		.unwrap();
}

/// Every deployment is running and finished.
pub async fn finish_deployments(db: &PgPool) {
	sqlx::query("UPDATE deployments SET status = 'running', finished_at = NOW()")
		.execute(db)
		.await
		.unwrap();
}

/// One sampler reading of a resource, taken `minutes_ago` minutes ago.
pub async fn seed_resource_sample(
	db: &PgPool,
	kind: &str,
	id: i64,
	cpu: Option<f64>,
	mem: i64,
	minutes_ago: i32,
) {
	sqlx::query(
		"INSERT INTO resource_metrics (kind, resource_id, at, rollup, cpu, mem)
		 VALUES ($1, $2, now() - make_interval(mins => $3), FALSE, $4, $5)",
	)
	.bind(kind)
	.bind(id)
	.bind(minutes_ago)
	.bind(cpu)
	.bind(mem)
	.execute(db)
	.await
	.unwrap();
}

/// The id of the only environment clone.
pub async fn only_clone_id(db: &PgPool) -> i64 {
	sqlx::query_scalar("SELECT id FROM environment_clones")
		.fetch_one(db)
		.await
		.unwrap()
}
