//! Quotas and metering. `cargo test -p server --all-features -- quotas` and `-- metering`.
//!
//! - `quotas_*`: enforcement per kind, the soft limit, who may change a limit, tenancy, the
//!   API and the audit log;
//! - `metering_*`: the hourly records from fixture metrics.

use axum::Router;
use axum::http::StatusCode;
use serde_json::{Value, json};
use sqlx::PgPool;
use time::{Duration, OffsetDateTime};

use super::metering;
use super::service::{self, Grain, Need, reserved};
use super::{Level, QuotaKind};
use crate::error::ServerError;
use crate::ids::{ServerId, TeamId, UserId};
use crate::state::AppState;
use crate::tests::{
	call, count, db_test, scalar, seed_application_on, seed_model, server_id, state, user_id,
	user_with_team,
};

struct Fx {
	state: AppState,
	app: Router,
	db: PgPool,
	token: String,
	team: i64,
	user: UserId,
	project: i64,
	server: i64,
}

impl Fx {
	fn team(&self) -> TeamId {
		TeamId(self.team)
	}

	fn base(&self) -> String {
		format!("/api/v1/teams/{}", self.team)
	}

	/// Sets limits as alice, the instance administrator, straight through the service.
	async fn limit(&self, changes: &[(QuotaKind, Option<i64>)]) {
		service::set_limits(&self.state, self.user, self.team(), changes)
			.await
			.expect("alice is the instance administrator");
	}

	async fn create_app(&self, slug: &str, extra: Value) -> (StatusCode, Value) {
		let mut body = json!({
			"name": slug, "slug": slug, "server_id": self.server,
			"repo_url": "https://github.com/traefik/whoami", "git_ref": "master",
			"container_port": 80,
		});
		if let (Some(base), Some(more)) = (body.as_object_mut(), extra.as_object()) {
			base.extend(more.clone());
		}
		call(
			&self.app,
			"POST",
			&format!("{}/projects/{}/applications", self.base(), self.project),
			Some(&self.token),
			Some(body),
		)
		.await
	}
}

async fn fx(db: PgPool) -> Fx {
	let state = state(db.clone());
	let app = crate::routes::routes(state.clone()).await.unwrap();
	let (token, team) = user_with_team(&app, "alice").await;
	let (s, project) = call(
		&app,
		"POST",
		&format!("/api/v1/teams/{team}/projects"),
		Some(&token),
		Some(json!({ "name": "web", "slug": "web" })),
	)
	.await;
	assert!(s.is_success(), "{project}");
	let user = UserId(user_id(&db, "alice").await);
	Fx {
		state,
		app,
		token,
		team,
		user,
		project: project["id"].as_i64().unwrap(),
		server: server_id(&db, team).await,
		db,
	}
}

async fn events(db: &PgPool, kind: &str) -> i64 {
	sqlx::query_scalar("SELECT COUNT(*) FROM notification_events WHERE kind = $1")
		.bind(kind)
		.fetch_one(db)
		.await
		.unwrap()
}

fn refused(result: Result<(), ServerError>) -> String {
	match result {
		Err(ServerError::QuotaExceeded(message)) => message,
		other => panic!("expected a quota refusal, got {other:?}"),
	}
}

/// Registers `name` and returns their API token, without making them a team of their own.
async fn plain_user(app: &Router, name: &str) -> String {
	let creds = json!({
		"name": name, "email": format!("{name}@example.com"), "password": "correct horse battery",
	});
	let (s, _) = call(
		app,
		"POST",
		"/api/v1/auth/register",
		None,
		Some(creds.clone()),
	)
	.await;
	assert!(s.is_success(), "register {name}: {s}");
	let (s, login) = call(app, "POST", "/api/v1/auth/login", None, Some(creds)).await;
	assert_eq!(s, StatusCode::OK);
	login["token"].as_str().unwrap().to_owned()
}

// --- counted kinds, through the API ----------------------------------------------------------

#[db_test]
async fn quotas_refuse_the_application_over_the_limit_and_say_where_to_raise_it(db: PgPool) {
	let f = fx(db).await;
	f.limit(&[(QuotaKind::Applications, Some(1))]).await;

	let (s, body) = f.create_app("one", json!({})).await;
	assert_eq!(s, StatusCode::OK, "{body}");
	let (s, body) = f.create_app("two", json!({})).await;
	assert_eq!(s, StatusCode::PAYMENT_REQUIRED, "{body}");
	let message = body["error"].as_str().unwrap();
	assert!(message.contains("Applications limit reached"), "{message}");
	assert!(
		message.contains("1 applications in use, 1 applications allowed"),
		"{message}"
	);
	assert!(message.contains("Settings → Usage & limits"), "{message}");
	assert_eq!(count(&f.db, "applications").await, 1);

	// A raised limit lets the next one through.
	f.limit(&[(QuotaKind::Applications, Some(2))]).await;
	let (s, body) = f.create_app("two", json!({})).await;
	assert_eq!(s, StatusCode::OK, "{body}");
}

#[db_test]
async fn quotas_refuse_memory_and_cpu_that_would_pass_the_limit(db: PgPool) {
	let f = fx(db).await;
	f.limit(&[
		(QuotaKind::MemoryMb, Some(1024)),
		(QuotaKind::CpuMillis, Some(1500)),
	])
	.await;

	let (s, body) = f
		.create_app("a", json!({ "memory_limit_mb": 768, "cpu_limit": 1.0 }))
		.await;
	assert_eq!(s, StatusCode::OK, "{body}");
	let (s, body) = f.create_app("b", json!({ "memory_limit_mb": 512 })).await;
	assert_eq!(s, StatusCode::PAYMENT_REQUIRED, "{body}");
	let message = body["error"].as_str().unwrap();
	assert!(message.contains("Memory limit reached"), "{message}");
	assert!(
		message.contains("768 MB in use, 1 GB allowed, and this needs 512 MB more"),
		"{message}"
	);
	let (s, body) = f.create_app("c", json!({ "cpu_limit": 1.0 })).await;
	assert_eq!(s, StatusCode::PAYMENT_REQUIRED, "{body}");
	assert!(
		body["error"]
			.as_str()
			.unwrap()
			.contains("CPU limit reached"),
		"{body}"
	);
	// A resource that sets neither limit reserves nothing, so it still fits.
	let (s, body) = f.create_app("d", json!({})).await;
	assert_eq!(s, StatusCode::OK, "{body}");
}

#[db_test]
async fn quotas_refuse_the_database_over_the_limit(db: PgPool) {
	let f = fx(db).await;
	f.limit(&[(QuotaKind::Databases, Some(0))]).await;
	let (s, body) = call(
		&f.app,
		"POST",
		&format!("{}/projects/{}/databases", f.base(), f.project),
		Some(&f.token),
		Some(json!({ "name": "orders", "engine": "postgres", "server_id": f.server })),
	)
	.await;
	assert_eq!(s, StatusCode::PAYMENT_REQUIRED, "{body}");
	assert!(
		body["error"]
			.as_str()
			.unwrap()
			.contains("Databases limit reached"),
		"{body}"
	);
	assert_eq!(count(&f.db, "databases").await, 0);
}

#[db_test]
async fn quotas_refuse_the_clone_that_would_be_one_preview_too_many(db: PgPool) {
	let f = fx(db).await;
	seed_application_on(&f.db, f.project, f.server).await;
	let environment: i64 = sqlx::query_scalar("SELECT id FROM environments WHERE project_id = $1")
		.bind(f.project)
		.fetch_one(&f.db)
		.await
		.unwrap();
	f.limit(&[(QuotaKind::Previews, Some(1))]).await;
	let path = format!(
		"{}/projects/{}/environments/{environment}/clone",
		f.base(),
		f.project
	);
	let clone = |name: &'static str| {
		call(
			&f.app,
			"POST",
			&path,
			Some(&f.token),
			Some(json!({ "name": name, "slug": name })),
		)
	};
	let (s, body) = clone("pr-1").await;
	assert!(s.is_success(), "{s} {body}");
	let (s, body) = clone("pr-2").await;
	assert_eq!(s, StatusCode::PAYMENT_REQUIRED, "{body}");
	assert!(
		body["error"]
			.as_str()
			.unwrap()
			.contains("Previews limit reached"),
		"{body}"
	);
}

// --- every kind at the service ---------------------------------------------------------------

#[db_test]
async fn quotas_enforce_each_resource_kind_at_create(db: PgPool) {
	let f = fx(db).await;
	let s = &f.state;
	let team = f.team();

	// A limit of zero refuses the first of every counted kind.
	for (kind, resource, label) in [
		(QuotaKind::Applications, "application", "Applications"),
		(QuotaKind::Databases, "database", "Databases"),
		(QuotaKind::Models, "model", "Models"),
		(QuotaKind::Stacks, "stack", "Stacks"),
		(QuotaKind::Previews, "preview", "Previews"),
	] {
		f.limit(&[(kind, Some(0))]).await;
		let message = refused(service::check_create(s, team, resource, &[Need::one(kind)]).await);
		assert!(
			message.starts_with(&format!("{label} limit reached")),
			"{message}"
		);
		f.limit(&[(kind, None)]).await;
		service::check_create(s, team, resource, &[Need::one(kind)])
			.await
			.unwrap();
	}

	// Reserved capacity: the request is added to what is reserved.
	f.limit(&[(QuotaKind::MemoryMb, Some(2048))]).await;
	let model = |mb| reserved(QuotaKind::Models, Some(mb), None, false);
	service::check_create(s, team, "model", &model(2048))
		.await
		.unwrap();
	refused(service::check_create(s, team, "model", &model(2049)).await);

	// GPUs: one per model that takes one.
	f.limit(&[(QuotaKind::Gpus, Some(0))]).await;
	let gpu = reserved(QuotaKind::Models, None, None, true);
	let message = refused(service::check_create(s, team, "model", &gpu).await);
	assert!(message.starts_with("GPUs limit reached"), "{message}");
	let cpu_only = reserved(QuotaKind::Models, None, None, false);
	service::check_create(s, team, "model", &cpu_only)
		.await
		.unwrap();
}

#[db_test]
async fn quotas_gpus_in_use_count_the_models_that_hold_one(db: PgPool) {
	let f = fx(db).await;
	let (_, model) = seed_model(&f.db, f.team, f.project, "llm", "ready", None).await;
	sqlx::query("UPDATE models SET gpu = TRUE WHERE id = $1")
		.bind(model)
		.execute(&f.db)
		.await
		.unwrap();
	let now = OffsetDateTime::now_utc();
	assert_eq!(
		service::used(&f.db, f.team(), QuotaKind::Gpus, now)
			.await
			.unwrap(),
		1
	);
	f.limit(&[(QuotaKind::Gpus, Some(1))]).await;
	let gpu = reserved(QuotaKind::Models, None, None, true);
	refused(service::check_create(&f.state, f.team(), "model", &gpu).await);
}

#[db_test]
async fn quotas_enforce_disk_and_build_minutes_at_deploy(db: PgPool) {
	let f = fx(db).await;
	let s = &f.state;
	let team = f.team();
	seed_application_on(&f.db, f.project, f.server).await;

	// Disk: the volume bytes the agents measured, 3 GB here.
	sqlx::query(
		"INSERT INTO resource_extra_metrics (kind, resource_id, at, volume_bytes)
		VALUES ('application', 5002, NOW(), 3221225472)",
	)
	.execute(&f.db)
	.await
	.unwrap();
	f.limit(&[(QuotaKind::DiskMb, Some(4096))]).await;
	service::check_deploy(s, team, "application").await.unwrap();
	f.limit(&[(QuotaKind::DiskMb, Some(2048))]).await;
	let message = refused(service::check_deploy(s, team, "application").await);
	assert!(
		message.starts_with("Disk limit reached: 3 GB in use, 2 GB allowed"),
		"{message}"
	);
	f.limit(&[(QuotaKind::DiskMb, None)]).await;

	// Build minutes: 30 minutes of a git build this month, none left at a limit of 30.
	sqlx::query(
		"INSERT INTO deployments (id, application_id, status, started_at, finished_at)
		VALUES (7001, 5002, 'running', NOW() - INTERVAL '30 minutes', NOW())",
	)
	.execute(&f.db)
	.await
	.unwrap();
	f.limit(&[(QuotaKind::BuildMinutes, Some(31))]).await;
	service::check_deploy(s, team, "application").await.unwrap();
	f.limit(&[(QuotaKind::BuildMinutes, Some(30))]).await;
	let message = refused(service::check_deploy(s, team, "application").await);
	assert!(message.contains("Build minutes limit reached"), "{message}");
	assert!(message.contains("starts again on the 1st"), "{message}");
}

#[db_test]
async fn quotas_a_deploy_through_the_api_is_refused_when_a_limit_was_lowered_under_use(db: PgPool) {
	let f = fx(db).await;
	let (s, app) = f.create_app("web", json!({ "memory_limit_mb": 512 })).await;
	assert_eq!(s, StatusCode::OK, "{app}");
	let id = app["id"].as_i64().unwrap();
	f.limit(&[(QuotaKind::MemoryMb, Some(256))]).await;
	let (s, body) = call(
		&f.app,
		"POST",
		&format!(
			"{}/projects/{}/applications/{id}/deployments",
			f.base(),
			f.project
		),
		Some(&f.token),
		None,
	)
	.await;
	assert_eq!(s, StatusCode::PAYMENT_REQUIRED, "{body}");
	assert!(
		body["error"]
			.as_str()
			.unwrap()
			.contains("Memory limit reached"),
		"{body}"
	);
	assert_eq!(count(&f.db, "deployments").await, 0);
}

#[db_test]
async fn quotas_model_tokens_count_per_month_and_cut_the_gateway_keys(db: PgPool) {
	let f = fx(db).await;
	let (_, model) = seed_model(&f.db, f.team, f.project, "llm", "ready", None).await;
	seed_application_on(&f.db, f.project, f.server).await;
	sqlx::query(
		"INSERT INTO gateway_keys (application_id, key_hash, key) VALUES (5002, 'hash-a', decode('00', 'hex'))",
	)
	.execute(&f.db)
	.await
	.unwrap();
	sqlx::query("INSERT INTO application_models (application_id, model_id) VALUES (5002, $1)")
		.bind(model)
		.execute(&f.db)
		.await
		.unwrap();
	// 600 tokens this month; the 10,000 of the hour before the 1st do not count.
	sqlx::query(
		"INSERT INTO model_usage (key_hash, model_id, hour, application_id, team_id, requests, input_tokens, output_tokens)
		VALUES ('hash-a', $1, date_trunc('month', NOW()) - INTERVAL '2 hours', 5002, $2, 1, 9000, 1000),
			('hash-a', $1, date_trunc('month', NOW()), 5002, $2, 1, 400, 200)",
	)
	.bind(model)
	.bind(f.team)
	.execute(&f.db)
	.await
	.unwrap();
	let now = OffsetDateTime::now_utc();
	assert_eq!(
		service::used(&f.db, f.team(), QuotaKind::ModelTokens, now)
			.await
			.unwrap(),
		600
	);

	let server = ServerId(f.server);
	f.limit(&[(QuotaKind::ModelTokens, Some(1000))]).await;
	assert!(
		service::keys_out_of_tokens(&f.db, server, now)
			.await
			.unwrap()
			.is_empty()
	);
	let table = crate::links::build_config(&f.db, server).await.unwrap();
	let key = table.keys.iter().find(|k| k.key_hash == "hash-a").unwrap();
	assert_eq!(key.models.len(), 1);

	f.limit(&[(QuotaKind::ModelTokens, Some(600))]).await;
	assert_eq!(
		service::keys_out_of_tokens(&f.db, server, now)
			.await
			.unwrap(),
		vec!["hash-a".to_owned()]
	);
	let table = crate::links::build_config(&f.db, server).await.unwrap();
	// The key stays known (an unknown key would be a 401) but reaches no model.
	let key = table.keys.iter().find(|k| k.key_hash == "hash-a").unwrap();
	assert!(key.models.is_empty());
}

#[db_test]
async fn quotas_a_team_with_no_limits_is_never_refused(db: PgPool) {
	let f = fx(db).await;
	for i in 0..3 {
		let (s, body) = f
			.create_app(
				&format!("app-{i}"),
				json!({ "memory_limit_mb": 4096, "cpu_limit": 8.0 }),
			)
			.await;
		assert_eq!(s, StatusCode::OK, "{body}");
	}
	service::check_deploy(&f.state, f.team(), "application")
		.await
		.unwrap();
	assert_eq!(events(&f.db, "quota_warning").await, 0);
}

// --- the soft limit ----------------------------------------------------------------------------

#[db_test]
async fn quotas_warn_once_at_eighty_percent_and_once_more_at_the_limit(db: PgPool) {
	let f = fx(db).await;
	let team = f.team();
	f.limit(&[(QuotaKind::Applications, Some(10))]).await;
	for i in 0..7 {
		let (st, body) = f.create_app(&format!("a{i}"), json!({})).await;
		assert_eq!(st, StatusCode::OK, "{body}");
	}
	// 7 of 10 is 70%: quiet.
	assert_eq!(events(&f.db, "quota_warning").await, 0);

	// The 8th makes 80%: one warning, with the link to the page.
	let (st, _) = f.create_app("a7", json!({})).await;
	assert_eq!(st, StatusCode::OK);
	assert_eq!(events(&f.db, "quota_warning").await, 1);
	let (title, link): (String, Option<String>) =
		sqlx::query_as("SELECT title, link FROM notification_events WHERE kind = 'quota_warning'")
			.fetch_one(&f.db)
			.await
			.unwrap();
	assert_eq!(title, "Applications at 80% of the limit");
	assert_eq!(link, Some(format!("/teams/{team}/settings/usage")));

	// The 9th stays at the same level: nothing more.
	let (st, _) = f.create_app("a8", json!({})).await;
	assert_eq!(st, StatusCode::OK);
	assert_eq!(events(&f.db, "quota_warning").await, 1);
	assert_eq!(events(&f.db, "quota_exceeded").await, 0);

	// The 10th reaches the limit: one "limit reached", and the 11th is refused without a
	// second one.
	let (st, _) = f.create_app("a9", json!({})).await;
	assert_eq!(st, StatusCode::OK);
	assert_eq!(events(&f.db, "quota_exceeded").await, 1);
	let (st, _) = f.create_app("a10", json!({})).await;
	assert_eq!(st, StatusCode::PAYMENT_REQUIRED);
	assert_eq!(events(&f.db, "quota_exceeded").await, 1);

	// Free some room: the hourly pass sees the level fall, so crossing again will speak again.
	sqlx::query("DELETE FROM applications WHERE slug IN ('a5', 'a6', 'a7', 'a8', 'a9')")
		.execute(&f.db)
		.await
		.unwrap();
	service::evaluate_team(&f.state, team).await;
	let level: i16 =
		sqlx::query_scalar("SELECT level FROM quota_levels WHERE kind = 'applications'")
			.fetch_one(&f.db)
			.await
			.unwrap();
	assert_eq!(level, Level::Ok.rank());
}

#[db_test]
async fn quotas_monthly_levels_start_again_in_a_new_month(db: PgPool) {
	let f = fx(db).await;
	f.limit(&[(QuotaKind::ModelTokens, Some(100))]).await;
	// The level was announced last month.
	sqlx::query(
		"INSERT INTO quota_levels (team_id, kind, period, level) VALUES ($1, 'model_tokens', '2020-01', 2)",
	)
	.bind(f.team)
	.execute(&f.db)
	.await
	.unwrap();
	let (_, model) = seed_model(&f.db, f.team, f.project, "llm", "ready", None).await;
	sqlx::query(
		"INSERT INTO model_usage (key_hash, model_id, hour, team_id, requests, input_tokens, output_tokens)
		VALUES ('k', $1, date_trunc('month', NOW()), $2, 1, 90, 0)",
	)
	.bind(model)
	.bind(f.team)
	.execute(&f.db)
	.await
	.unwrap();
	service::evaluate_team(&f.state, f.team()).await;
	assert_eq!(events(&f.db, "quota_warning").await, 1);
}

// --- who may change a limit --------------------------------------------------------------------

#[db_test]
async fn quotas_only_the_instance_administrator_changes_limits(db: PgPool) {
	let f = fx(db).await;
	// bob joins alice's team as an owner: powerful in the team, but not the administrator.
	let bob = plain_user(&f.app, "bob").await;
	let (s, _) = call(
		&f.app,
		"POST",
		&format!("{}/members", f.base()),
		Some(&f.token),
		Some(json!({ "email": "bob@example.com", "role_id": 1 })),
	)
	.await;
	assert!(s.is_success(), "{s}");

	let put = json!({ "limits": { "applications": 3 } });
	let path = format!("{}/usage/limits", f.base());
	let (s, _) = call(&f.app, "PUT", &path, Some(&bob), Some(put.clone())).await;
	assert_eq!(s, StatusCode::FORBIDDEN);
	assert_eq!(count(&f.db, "team_quotas").await, 0);

	let (s, body) = call(&f.app, "PUT", &path, Some(&f.token), Some(put)).await;
	assert_eq!(s, StatusCode::OK, "{body}");
	let apps = body["quotas"]
		.as_array()
		.unwrap()
		.iter()
		.find(|q| q["kind"] == "applications")
		.unwrap();
	assert_eq!(apps["limit"], 3);
	assert_eq!(body["can_edit"], true);

	// bob sees the usage, and that he cannot edit it.
	let (s, body) = call(
		&f.app,
		"GET",
		&format!("{}/usage", f.base()),
		Some(&bob),
		None,
	)
	.await;
	assert_eq!(s, StatusCode::OK);
	assert_eq!(body["can_edit"], false);
}

#[db_test]
async fn quotas_a_limit_change_is_audited_with_what_changed(db: PgPool) {
	let f = fx(db).await;
	f.limit(&[
		(QuotaKind::Applications, Some(5)),
		(QuotaKind::MemoryMb, Some(2048)),
	])
	.await;
	f.limit(&[
		(QuotaKind::Applications, Some(8)),
		(QuotaKind::MemoryMb, Some(2048)),
	])
	.await;
	let diff: Value = sqlx::query_scalar(
		"SELECT diff FROM audit_events WHERE action = 'quota.update' ORDER BY created_at DESC, id DESC LIMIT 1",
	)
	.fetch_one(&f.db)
	.await
	.unwrap();
	assert_eq!(diff, json!({ "applications": { "from": 5, "to": 8 } }));
}

#[db_test]
async fn quotas_the_limits_api_refuses_what_is_not_a_quota(db: PgPool) {
	let f = fx(db).await;
	let path = format!("{}/usage/limits", f.base());
	for body in [
		json!({ "limits": { "bandwidth": 1 } }),
		json!({ "limits": { "applications": -1 } }),
	] {
		let (s, _) = call(&f.app, "PUT", &path, Some(&f.token), Some(body)).await;
		assert_eq!(s, StatusCode::BAD_REQUEST);
	}
	assert_eq!(count(&f.db, "team_quotas").await, 0);
}

// --- tenancy -----------------------------------------------------------------------------------

#[db_test]
async fn quotas_another_teams_usage_is_not_found(db: PgPool) {
	let f = fx(db).await;
	let (bob, bob_team) = user_with_team(&f.app, "bob").await;
	f.limit(&[(QuotaKind::Applications, Some(3))]).await;
	for (method, path, body) in [
		("GET", format!("{}/usage", f.base()), None),
		("GET", format!("{}/usage/history", f.base()), None),
		("GET", format!("{}/usage/export", f.base()), None),
		(
			"PUT",
			format!("{}/usage/limits", f.base()),
			Some(json!({ "limits": { "applications": 99 } })),
		),
	] {
		let (s, _) = call(&f.app, method, &path, Some(&bob), body).await;
		assert_eq!(s, StatusCode::NOT_FOUND, "{method} {path}");
	}
	let limit: i64 =
		sqlx::query_scalar("SELECT hard_limit FROM team_quotas WHERE kind = 'applications'")
			.fetch_one(&f.db)
			.await
			.unwrap();
	assert_eq!(limit, 3);

	// The limits of one team never count for another.
	service::check_create(
		&f.state,
		TeamId(bob_team),
		"application",
		&[Need::one(QuotaKind::Applications)],
	)
	.await
	.unwrap();
}

#[db_test]
async fn quotas_usage_counts_only_the_teams_own_resources(db: PgPool) {
	let f = fx(db).await;
	seed_application_on(&f.db, f.project, f.server).await;
	let (_, bob_team) = user_with_team(&f.app, "bob").await;
	let now = OffsetDateTime::now_utc();
	let apps = |team| service::used(&f.db, TeamId(team), QuotaKind::Applications, now);
	assert_eq!(apps(f.team).await.unwrap(), 1);
	assert_eq!(apps(bob_team).await.unwrap(), 0);
}

// --- the API -----------------------------------------------------------------------------------

#[db_test]
async fn quotas_the_api_reports_every_kind_with_its_level(db: PgPool) {
	let f = fx(db).await;
	f.limit(&[(QuotaKind::Applications, Some(5))]).await;
	for i in 0..4 {
		f.create_app(&format!("x{i}"), json!({})).await;
	}
	let (s, body) = call(
		&f.app,
		"GET",
		&format!("{}/usage", f.base()),
		Some(&f.token),
		None,
	)
	.await;
	assert_eq!(s, StatusCode::OK);
	let quotas = body["quotas"].as_array().unwrap();
	assert_eq!(quotas.len(), QuotaKind::ALL.len());
	let apps = quotas.iter().find(|q| q["kind"] == "applications").unwrap();
	assert_eq!(apps["used"], 4);
	assert_eq!(apps["limit"], 5);
	assert_eq!(apps["percent"], 80);
	assert_eq!(apps["level"], "warning");
	let dbs = quotas.iter().find(|q| q["kind"] == "databases").unwrap();
	assert_eq!(dbs["limit"], Value::Null);
	assert_eq!(dbs["level"], "ok");
}

#[db_test]
async fn quotas_usage_exports_as_csv_and_the_export_is_audited(db: PgPool) {
	let f = fx(db).await;
	let hour = OffsetDateTime::now_utc() - Duration::hours(3);
	sqlx::query(
		"INSERT INTO usage_records (team_id, hour, cpu_seconds, model_tokens)
		VALUES ($1, date_trunc('hour', $2::timestamptz), 120.5, 42)",
	)
	.bind(f.team)
	.bind(hour)
	.execute(&f.db)
	.await
	.unwrap();
	let csv = service::export(&f.state, f.user, f.team(), Grain::Hour, 7)
		.await
		.unwrap();
	let mut lines = csv.lines();
	assert_eq!(
		lines.next().unwrap(),
		"time,cpu_seconds,memory_gb_hours,disk_gb_hours,gpu_hours,egress_bytes,model_tokens,build_minutes"
	);
	let row = lines.next().unwrap();
	assert!(
		row.ends_with(",120.5,0.0000,0.0000,0.0000,0,42,0.00"),
		"{row}"
	);
	assert!(lines.next().is_none());
	assert_eq!(
		scalar(
			&f.db,
			"SELECT COUNT(*) FROM audit_events WHERE action = 'usage.export'",
			None
		)
		.await,
		1
	);
	let (s, body) = call(
		&f.app,
		"GET",
		&format!("{}/usage/history?grain=day&days=2", f.base()),
		Some(&f.token),
		None,
	)
	.await;
	assert_eq!(s, StatusCode::OK);
	assert_eq!(body.as_array().unwrap().len(), 1);
	assert_eq!(body[0]["model_tokens"], 42);
	let (s, _) = call(
		&f.app,
		"GET",
		&format!("{}/usage/history?grain=week", f.base()),
		Some(&f.token),
		None,
	)
	.await;
	assert_eq!(s, StatusCode::BAD_REQUEST);
}

#[test]
fn quotas_cloud_is_not_a_default_feature() {
	let manifest = include_str!("../../Cargo.toml");
	let features = manifest.split("[features]").nth(1).unwrap();
	let features = features.split("\n[").next().unwrap();
	assert!(
		features.contains("cloud = [\"dep:podkit-cloud\"]"),
		"{features}"
	);
	assert!(
		!features
			.lines()
			.any(|l| l.trim_start().starts_with("default")),
		"{features}"
	);
}

// --- metering ----------------------------------------------------------------------------------

/// The start of the hour two hours ago, which is complete and inside the lookback.
fn hour_two_ago() -> OffsetDateTime {
	let now = OffsetDateTime::now_utc().unix_timestamp();
	OffsetDateTime::from_unix_timestamp(now - now.rem_euclid(3600) - 7200).unwrap()
}

async fn set_threads(db: &PgPool, server: i64, threads: i32) {
	sqlx::query(
		"UPDATE servers SET hardware = jsonb_build_object('cpu', jsonb_build_object('threads', $2::int))
		WHERE id = $1",
	)
	.bind(server)
	.bind(threads)
	.execute(db)
	.await
	.unwrap();
}

async fn sample(db: &PgPool, at: OffsetDateTime, cpu: f64, mem: i64) {
	sqlx::query(
		"INSERT INTO resource_metrics (kind, resource_id, at, cpu, mem) VALUES ('application', 5002, $1, $2, $3)",
	)
	.bind(at)
	.bind(cpu)
	.bind(mem)
	.execute(db)
	.await
	.unwrap();
}

#[db_test]
async fn metering_sums_the_samples_of_an_hour_into_one_record(db: PgPool) {
	let f = fx(db).await;
	seed_application_on(&f.db, f.project, f.server).await;
	set_threads(&f.db, f.server, 4).await;
	let hour = hour_two_ago();

	// Twelve 5-minute slots, two raw samples each: 50% of a 4-thread host and 2 GiB of memory,
	// 10 GiB of volumes, 1 kB/s out and GPU memory held for the whole hour.
	for slot in 0..12 {
		for offset in [0, 30] {
			let at = hour + Duration::minutes(slot * 5) + Duration::seconds(offset);
			sample(&f.db, at, 50.0, 2_147_483_648).await;
			sqlx::query(
				"INSERT INTO resource_extra_metrics (kind, resource_id, at, volume_bytes, vram_bytes, bytes_out_rate)
				VALUES ('application', 5002, $1, 10737418240, 1048576, 1000)",
			)
			.bind(at)
			.execute(&f.db)
			.await
			.unwrap();
		}
	}
	// A model's tokens in the hour, and a 10-minute git build that started in it.
	let (_, model) = seed_model(&f.db, f.team, f.project, "llm", "ready", None).await;
	sqlx::query(
		"INSERT INTO model_usage (key_hash, model_id, hour, team_id, requests, input_tokens, output_tokens)
		VALUES ('k', $1, $2, $3, 2, 700, 300)",
	)
	.bind(model)
	.bind(hour)
	.bind(f.team)
	.execute(&f.db)
	.await
	.unwrap();
	sqlx::query(
		"INSERT INTO deployments (id, application_id, status, started_at, finished_at)
		VALUES (7001, 5002, 'running', $1, $1 + INTERVAL '10 minutes')",
	)
	.bind(hour + Duration::minutes(20))
	.execute(&f.db)
	.await
	.unwrap();

	let records = metering::compute(&f.db, OffsetDateTime::now_utc())
		.await
		.unwrap();
	assert_eq!(records.len(), 1, "{records:?}");
	let r = &records[0];
	assert_eq!(r.team_id, f.team);
	assert_eq!(r.hour, hour);
	// 50% of 4 threads for 3600 s.
	assert!((r.cpu_seconds - 7200.0).abs() < 0.01, "{}", r.cpu_seconds);
	assert!(
		(r.memory_gb_hours - 2.0).abs() < 0.001,
		"{}",
		r.memory_gb_hours
	);
	assert!(
		(r.disk_gb_hours - 10.0).abs() < 0.001,
		"{}",
		r.disk_gb_hours
	);
	assert!((r.gpu_hours - 1.0).abs() < 0.001, "{}", r.gpu_hours);
	assert_eq!(r.egress_bytes, 3_600_000);
	assert_eq!(r.model_tokens, 1000);
	assert!(
		(r.build_minutes - 10.0).abs() < 0.001,
		"{}",
		r.build_minutes
	);

	// What was written is final: a second pass finds nothing new and changes nothing.
	assert!(
		metering::compute(&f.db, OffsetDateTime::now_utc())
			.await
			.unwrap()
			.is_empty()
	);
	assert_eq!(count(&f.db, "usage_records").await, 1);
}

#[db_test]
async fn metering_counts_only_the_slots_that_have_a_sample(db: PgPool) {
	let f = fx(db).await;
	seed_application_on(&f.db, f.project, f.server).await;
	set_threads(&f.db, f.server, 2).await;
	let hour = hour_two_ago();
	// The server answered in the first 30 minutes only: six slots at 100% of 2 threads.
	for slot in 0..6 {
		sample(
			&f.db,
			hour + Duration::minutes(slot * 5 + 1),
			100.0,
			1_073_741_824,
		)
		.await;
	}
	let records = metering::compute(&f.db, OffsetDateTime::now_utc())
		.await
		.unwrap();
	let r = &records[0];
	assert!((r.cpu_seconds - 3600.0).abs() < 0.01, "{}", r.cpu_seconds);
	assert!(
		(r.memory_gb_hours - 0.5).abs() < 0.001,
		"{}",
		r.memory_gb_hours
	);
}

#[db_test]
async fn metering_keeps_teams_apart_and_drops_samples_nobody_owns(db: PgPool) {
	let f = fx(db).await;
	seed_application_on(&f.db, f.project, f.server).await;
	let (_, bob_team) = user_with_team(&f.app, "bob").await;
	let hour = hour_two_ago();
	sample(&f.db, hour + Duration::minutes(1), 10.0, 1_073_741_824).await;
	// Samples of a resource that no longer exists belong to nobody.
	sqlx::query(
		"INSERT INTO resource_metrics (kind, resource_id, at, cpu, mem) VALUES ('application', 99999, $1, 10, 1073741824)",
	)
	.bind(hour + Duration::minutes(1))
	.execute(&f.db)
	.await
	.unwrap();
	let records = metering::compute(&f.db, OffsetDateTime::now_utc())
		.await
		.unwrap();
	assert_eq!(records.len(), 1);
	assert_eq!(records[0].team_id, f.team);
	let bob: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM usage_records WHERE team_id = $1")
		.bind(bob_team)
		.fetch_one(&f.db)
		.await
		.unwrap();
	assert_eq!(bob, 0);
}

#[db_test]
async fn metering_ignores_the_hour_in_progress_and_what_is_older_than_a_day(db: PgPool) {
	let f = fx(db).await;
	seed_application_on(&f.db, f.project, f.server).await;
	let now = OffsetDateTime::now_utc();
	for at in [now, now - Duration::hours(30)] {
		sample(&f.db, at, 10.0, 1_073_741_824).await;
	}
	assert!(metering::compute(&f.db, now).await.unwrap().is_empty());
}

#[db_test]
async fn metering_the_hourly_pass_also_announces_the_limits_it_finds_passed(db: PgPool) {
	let f = fx(db).await;
	f.limit(&[(QuotaKind::Applications, Some(1))]).await;
	seed_application_on(&f.db, f.project, f.server).await;
	metering::run(&f.state).await;
	assert_eq!(events(&f.db, "quota_exceeded").await, 1);
}
