//! Raw-SQL helpers for the model create-flow tests: an extra environment to create into,
//! and the environment a model ended up in. They live here because the UI never touches
//! the database, tests included.

use sqlx::PgPool;

/// A second environment (`staging`) of `project`; returns its id.
pub async fn add_environment(db: &PgPool, project: i64) -> i64 {
	let id = crypto::generate_id();
	sqlx::query(
		"INSERT INTO environments (id, project_id, name, slug, position)
		 VALUES ($1, $2, 'Staging', 'staging', 1)",
	)
	.bind(id)
	.bind(project)
	.execute(db)
	.await
	.unwrap();
	id
}

/// The environment that holds the newest model.
pub async fn newest_model_environment(db: &PgPool) -> i64 {
	sqlx::query_scalar("SELECT environment_id FROM models ORDER BY id DESC LIMIT 1")
		.fetch_one(db)
		.await
		.unwrap()
}
