//! Raw-SQL helpers and a stub server for the notification page tests. They live here because
//! the UI never touches the database, tests included.

use axum::Router;
use sqlx::PgPool;
use tokio::net::TcpListener;

/// The only channel's id.
pub async fn channel_id(db: &PgPool) -> i64 {
	sqlx::query_scalar("SELECT id FROM notification_channels")
		.fetch_one(db)
		.await
		.unwrap()
}

/// The only channel's stored (sealed) secret.
pub async fn channel_secret(db: &PgPool) -> Vec<u8> {
	sqlx::query_scalar("SELECT secret FROM notification_channels")
		.fetch_one(db)
		.await
		.unwrap()
}

pub async fn channel_enabled(db: &PgPool) -> bool {
	sqlx::query_scalar("SELECT enabled FROM notification_channels")
		.fetch_one(db)
		.await
		.unwrap()
}

/// The only rule's id.
pub async fn rule_id(db: &PgPool) -> i64 {
	sqlx::query_scalar("SELECT id FROM notification_rules")
		.fetch_one(db)
		.await
		.unwrap()
}

pub async fn rule_enabled(db: &PgPool) -> bool {
	sqlx::query_scalar("SELECT enabled FROM notification_rules")
		.fetch_one(db)
		.await
		.unwrap()
}

/// The only metric alert's id.
pub async fn alert_id(db: &PgPool) -> i64 {
	sqlx::query_scalar("SELECT id FROM metric_alerts")
		.fetch_one(db)
		.await
		.unwrap()
}

pub async fn alert_enabled(db: &PgPool) -> bool {
	sqlx::query_scalar("SELECT enabled FROM metric_alerts")
		.fetch_one(db)
		.await
		.unwrap()
}

/// The team's noise control row: dedup minutes, quiet on, start, end and offset in minutes.
pub async fn noise(db: &PgPool, team: i64) -> Option<(i32, bool, i32, i32, i32)> {
	sqlx::query_as(
		"SELECT dedup_minutes, quiet_enabled, quiet_start_min, quiet_end_min, utc_offset_min
		FROM notification_settings WHERE team_id = $1",
	)
	.bind(team)
	.fetch_optional(db)
	.await
	.unwrap()
}

pub async fn smtp_password(db: &PgPool) -> Vec<u8> {
	sqlx::query_scalar("SELECT password FROM notification_smtp")
		.fetch_one(db)
		.await
		.unwrap()
}

/// Puts an event in the team's history without going through a rule. `severity` is 0 to 2.
pub async fn seed_event(db: &PgPool, team: i64, title: &str, severity: i16) -> i64 {
	let id = crypto::generate_id();
	sqlx::query(
		"INSERT INTO notification_events (id, team_id, kind, severity, event_key, title, body, link)
		VALUES ($1, $2, 'deploy_failed', $3, $4, $5, 'it broke <b>badly</b>', '/teams/1/projects')",
	)
	.bind(id)
	.bind(team)
	.bind(severity)
	.bind(format!("deploy_failed:{id}"))
	.bind(title)
	.execute(db)
	.await
	.unwrap();
	id
}

/// Adds the user called `name` to `team` in the default member role.
pub async fn add_member(db: &PgPool, team: i64, name: &str) {
	sqlx::query(
		"INSERT INTO team_members (id, team_id, user_id, role_id)
		SELECT $1, $2, id, 2 FROM users WHERE name = $3",
	)
	.bind(crypto::generate_id())
	.bind(team)
	.bind(name)
	.execute(db)
	.await
	.unwrap();
}

/// A server that answers every request with 200, for "Send test". Returns its address.
pub async fn ok_server() -> String {
	let app = Router::new().fallback(|| async { "ok" });
	let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
	let url = format!("http://{}", listener.local_addr().unwrap());
	tokio::spawn(async move {
		axum::serve(listener, app).await.unwrap();
	});
	url
}

/// The id of the channel called `name`.
pub async fn channel_named(db: &PgPool, name: &str) -> i64 {
	sqlx::query_scalar("SELECT id FROM notification_channels WHERE name = $1")
		.bind(name)
		.fetch_one(db)
		.await
		.unwrap()
}

/// How many events the user called `name` has marked read one by one.
pub async fn reads_of(db: &PgPool, name: &str) -> i64 {
	sqlx::query_scalar(
		"SELECT COUNT(*) FROM notification_reads r JOIN users u ON u.id = r.user_id WHERE u.name = $1",
	)
	.bind(name)
	.fetch_one(db)
	.await
	.unwrap()
}
