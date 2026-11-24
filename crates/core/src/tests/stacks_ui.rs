//! Raw-SQL helpers for the template and stack page tests: arranging rows the pages then
//! show. They live here because the UI never touches the database, tests included.

use sqlx::PgPool;

use crate::state::AppState;

pub async fn stack_environment(db: &PgPool) -> i64 {
	sqlx::query_scalar("SELECT environment_id FROM stacks")
		.fetch_one(db)
		.await
		.unwrap()
}

pub async fn stack_status(db: &PgPool, id: i64) -> String {
	sqlx::query_scalar("SELECT status FROM stacks WHERE id = $1")
		.bind(id)
		.fetch_one(db)
		.await
		.unwrap()
}

pub async fn set_stack_status(db: &PgPool, id: i64, status: &str, message: Option<&str>) {
	sqlx::query("UPDATE stacks SET status = $2, status_message = $3 WHERE id = $1")
		.bind(id)
		.bind(status)
		.bind(message)
		.execute(db)
		.await
		.unwrap();
}

/// Makes a service of a stack look deployed: a container, a status and a health text.
pub async fn set_service(
	db: &PgPool,
	stack: i64,
	name: &str,
	status: &str,
	container: Option<&str>,
	health: Option<&str>,
) {
	sqlx::query(
		"UPDATE stack_services SET status = $3, container_id = $4, health = $5
		 WHERE stack_id = $1 AND name = $2",
	)
	.bind(stack)
	.bind(name)
	.bind(status)
	.bind(container)
	.bind(health)
	.execute(db)
	.await
	.unwrap();
}

pub async fn set_service_message(db: &PgPool, stack: i64, name: &str, message: &str) {
	sqlx::query("UPDATE stack_services SET status_message = $3 WHERE stack_id = $1 AND name = $2")
		.bind(stack)
		.bind(name)
		.bind(message)
		.execute(db)
		.await
		.unwrap();
}

pub async fn service_domain(db: &PgPool, stack: i64, name: &str) -> Option<String> {
	sqlx::query_scalar("SELECT domain FROM stack_services WHERE stack_id = $1 AND name = $2")
		.bind(stack)
		.bind(name)
		.fetch_one(db)
		.await
		.unwrap()
}

pub async fn stack_compose(db: &PgPool, id: i64) -> String {
	sqlx::query_scalar("SELECT compose_yaml FROM stacks WHERE id = $1")
		.bind(id)
		.fetch_one(db)
		.await
		.unwrap()
}

pub async fn stack_name(db: &PgPool, id: i64) -> String {
	sqlx::query_scalar("SELECT name FROM stacks WHERE id = $1")
		.bind(id)
		.fetch_one(db)
		.await
		.unwrap()
}

/// What the stack's variable `key` holds, decrypted with the app's own key, to check it never
/// reaches a page.
pub async fn stack_variable(state: &AppState, id: i64, key: &str) -> Option<String> {
	let stack = crate::stacks::find_by_id(&state.db, crate::ids::StackId(id))
		.await
		.unwrap()
		.unwrap();
	crate::stacks::decrypt_env(&state.secrets, &stack)
		.unwrap()
		.remove(key)
}

/// The jobs queued so far, as `kind`s.
pub async fn job_kinds(db: &PgPool) -> Vec<String> {
	sqlx::query_scalar("SELECT kind::text FROM jobs ORDER BY id")
		.fetch_all(db)
		.await
		.unwrap()
}

/// The id of the newest stack.
pub async fn newest_stack(db: &PgPool) -> i64 {
	sqlx::query_scalar("SELECT id FROM stacks ORDER BY id DESC LIMIT 1")
		.fetch_one(db)
		.await
		.unwrap()
}
