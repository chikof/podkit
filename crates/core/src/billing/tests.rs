//! The billing hooks, against a provider that records what it is asked. Only built with the
//! `cloud` feature: `cargo test -p podkit-core --all-features -- billing`.

use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use podkit_billing::{
	BillingError, BillingProvider, BoxFuture, Decision, EntitlementRequest, Operation, QuotaLimit,
	TeamRef, UsageReport, WebhookOutcome, WebhookRequest,
};
use serde_json::json;
use sqlx::PgPool;
use time::{Duration, OffsetDateTime};
use tower::ServiceExt;

use super::Billing;
use crate::ids::TeamId;
use crate::quotas::service::{self, Need};
use crate::quotas::{QuotaKind, metering};
use crate::state::AppState;
use crate::tests::{call, count, db_test, seed_application_on, server_id, state, user_with_team};

/// Records every call; answers as told.
#[derive(Default)]
struct Mock {
	customers: Mutex<Vec<TeamRef>>,
	usage: Mutex<Vec<UsageReport>>,
	asked: Mutex<Vec<EntitlementRequest>>,
	hooks: Mutex<Vec<WebhookRequest>>,
	deny: Mutex<Option<String>>,
	fail_entitlement: Mutex<bool>,
	answer: Mutex<Option<Result<WebhookOutcome, String>>>,
}

impl BillingProvider for Mock {
	fn create_customer<'a>(
		&'a self,
		team: &'a TeamRef,
	) -> BoxFuture<'a, Result<Option<String>, BillingError>> {
		Box::pin(async move {
			self.customers.lock().unwrap().push(team.clone());
			Ok(Some(format!("cus_{}", team.slug)))
		})
	}

	fn report_usage<'a>(
		&'a self,
		report: &'a UsageReport,
	) -> BoxFuture<'a, Result<(), BillingError>> {
		Box::pin(async move {
			self.usage.lock().unwrap().push(report.clone());
			Ok(())
		})
	}

	fn check_entitlement<'a>(
		&'a self,
		request: &'a EntitlementRequest,
	) -> BoxFuture<'a, Result<Decision, BillingError>> {
		Box::pin(async move {
			self.asked.lock().unwrap().push(request.clone());
			if *self.fail_entitlement.lock().unwrap() {
				return Err(BillingError("the provider is down".to_owned()));
			}
			Ok(match self.deny.lock().unwrap().clone() {
				Some(reason) => Decision::Deny { reason },
				None => Decision::Allow,
			})
		})
	}

	fn handle_webhook<'a>(
		&'a self,
		request: &'a WebhookRequest,
	) -> BoxFuture<'a, Result<WebhookOutcome, BillingError>> {
		Box::pin(async move {
			self.hooks.lock().unwrap().push(request.clone());
			match self.answer.lock().unwrap().clone() {
				Some(Ok(outcome)) => Ok(outcome),
				Some(Err(e)) => Err(BillingError(e)),
				None => Ok(WebhookOutcome::Ignored),
			}
		})
	}
}

struct Fx {
	mock: Arc<Mock>,
	state: AppState,
	app: Router,
	db: PgPool,
	token: String,
	team: i64,
	project: i64,
	server: i64,
}

async fn fx(db: PgPool) -> Fx {
	let mock = Arc::new(Mock::default());
	let mut state = state(db.clone());
	state.billing = Billing::new(mock.clone());
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
	Fx {
		mock,
		state,
		app,
		token,
		team,
		project: project["id"].as_i64().unwrap(),
		server: server_id(&db, team).await,
		db,
	}
}

fn ask(op: Operation, resource: &str, team: i64) -> EntitlementRequest {
	EntitlementRequest {
		team,
		operation: op,
		resource: resource.to_owned(),
	}
}

#[db_test]
async fn billing_a_team_made_through_the_api_gets_a_customer(db: PgPool) {
	let f = fx(db).await;
	// alice's team was made by the API, which called the hook once.
	let customers = f.mock.customers.lock().unwrap().clone();
	assert_eq!(customers.len(), 1);
	assert_eq!(customers[0].id, f.team);
	assert_eq!(customers[0].slug, "alice");
	let stored: String =
		sqlx::query_scalar("SELECT external_id FROM billing_customers WHERE team_id = $1")
			.bind(f.team)
			.fetch_one(&f.db)
			.await
			.unwrap();
	assert_eq!(stored, "cus_alice");
}

#[db_test]
async fn billing_entitlement_is_asked_at_create_and_at_deploy_with_what_is_happening(db: PgPool) {
	let f = fx(db).await;
	let team = TeamId(f.team);
	service::check_create(
		&f.state,
		team,
		"application",
		&[Need::one(QuotaKind::Applications)],
	)
	.await
	.unwrap();
	service::check_deploy(&f.state, team, "stack")
		.await
		.unwrap();
	assert_eq!(
		*f.mock.asked.lock().unwrap(),
		vec![
			ask(Operation::Create, "application", f.team),
			ask(Operation::Deploy, "stack", f.team),
		]
	);
	// The customer exists already: the provider is not asked to create it again.
	assert_eq!(f.mock.customers.lock().unwrap().len(), 1);
}

#[db_test]
async fn billing_a_team_without_a_customer_gets_one_on_first_use(db: PgPool) {
	let f = fx(db).await;
	sqlx::query("DELETE FROM billing_customers")
		.execute(&f.db)
		.await
		.unwrap();
	service::check_deploy(&f.state, TeamId(f.team), "application")
		.await
		.unwrap();
	assert_eq!(f.mock.customers.lock().unwrap().len(), 2);
	assert_eq!(count(&f.db, "billing_customers").await, 1);
	service::check_deploy(&f.state, TeamId(f.team), "application")
		.await
		.unwrap();
	assert_eq!(f.mock.customers.lock().unwrap().len(), 2);
}

#[db_test]
async fn billing_a_refusal_from_the_provider_stops_the_create_with_its_sentence(db: PgPool) {
	let f = fx(db).await;
	*f.mock.deny.lock().unwrap() = Some("The trial has ended. Pick a plan to go on.".to_owned());
	let (s, body) = call(
		&f.app,
		"POST",
		&format!(
			"/api/v1/teams/{}/projects/{}/applications",
			f.team, f.project
		),
		Some(&f.token),
		Some(json!({
			"name": "web", "slug": "web", "server_id": f.server,
			"repo_url": "https://github.com/traefik/whoami", "git_ref": "master", "container_port": 80,
		})),
	)
	.await;
	assert_eq!(s, StatusCode::PAYMENT_REQUIRED, "{body}");
	assert_eq!(body["error"], "The trial has ended. Pick a plan to go on.");
	assert_eq!(count(&f.db, "applications").await, 0);
}

#[db_test]
async fn billing_a_provider_that_fails_does_not_stop_anyone(db: PgPool) {
	let f = fx(db).await;
	*f.mock.fail_entitlement.lock().unwrap() = true;
	service::check_deploy(&f.state, TeamId(f.team), "application")
		.await
		.unwrap();
	assert_eq!(f.mock.asked.lock().unwrap().len(), 1);
}

#[db_test]
async fn billing_our_own_quota_is_checked_before_the_provider_is_asked(db: PgPool) {
	let f = fx(db).await;
	let user = crate::ids::UserId(crate::tests::user_id(&f.db, "alice").await);
	service::set_limits(
		&f.state,
		user,
		TeamId(f.team),
		&[(QuotaKind::Applications, Some(0))],
	)
	.await
	.unwrap();
	let refused = service::check_create(
		&f.state,
		TeamId(f.team),
		"application",
		&[Need::one(QuotaKind::Applications)],
	)
	.await;
	assert!(refused.is_err());
	assert!(f.mock.asked.lock().unwrap().is_empty());
}

#[db_test]
async fn billing_each_new_usage_record_is_reported_once(db: PgPool) {
	let f = fx(db).await;
	seed_application_on(&f.db, f.project, f.server).await;
	let now = OffsetDateTime::now_utc().unix_timestamp();
	let hour = OffsetDateTime::from_unix_timestamp(now - now.rem_euclid(3600) - 7200).unwrap();
	sqlx::query(
		"INSERT INTO resource_metrics (kind, resource_id, at, cpu, mem) VALUES ('application', 5002, $1, 100, 1073741824)",
	)
	.bind(hour + Duration::minutes(2))
	.execute(&f.db)
	.await
	.unwrap();
	metering::run(&f.state).await;
	{
		let usage = f.mock.usage.lock().unwrap();
		assert_eq!(usage.len(), 1);
		assert_eq!(usage[0].team, f.team);
		assert_eq!(usage[0].hour_start, hour.unix_timestamp());
		let meters: Vec<&str> = usage[0].lines.iter().map(|l| l.meter).collect();
		assert_eq!(meters, ["cpu_seconds", "memory_gb_hours"]);
	}
	// The same hour is not reported again.
	metering::run(&f.state).await;
	assert_eq!(f.mock.usage.lock().unwrap().len(), 1);
}

async fn post_webhook(app: &Router, body: &'static str) -> StatusCode {
	let req = Request::builder()
		.method("POST")
		.uri("/webhooks/billing")
		.header(header::CONTENT_TYPE, "application/json")
		.header("x-provider-signature", "t=1,v1=abc")
		.body(Body::from(body))
		.unwrap();
	app.clone().oneshot(req).await.unwrap().status()
}

#[db_test]
async fn billing_a_webhook_reaches_the_provider_with_its_headers_and_raw_body(db: PgPool) {
	let f = fx(db).await;
	assert_eq!(
		post_webhook(&f.app, "{\"type\":\"ping\"}").await,
		StatusCode::NO_CONTENT
	);
	let hooks = f.mock.hooks.lock().unwrap();
	assert_eq!(hooks.len(), 1);
	assert_eq!(hooks[0].body, b"{\"type\":\"ping\"}");
	assert!(
		hooks[0]
			.headers
			.contains(&("x-provider-signature".to_owned(), "t=1,v1=abc".to_owned()))
	);
}

#[db_test]
async fn billing_a_webhook_can_set_a_teams_limits_and_is_audited_as_podkit(db: PgPool) {
	let f = fx(db).await;
	*f.mock.answer.lock().unwrap() = Some(Ok(WebhookOutcome::SetQuotas {
		team: f.team,
		limits: vec![
			QuotaLimit {
				kind: "applications".to_owned(),
				limit: Some(12),
			},
			QuotaLimit {
				kind: "memory_mb".to_owned(),
				limit: None,
			},
		],
	}));
	assert_eq!(post_webhook(&f.app, "{}").await, StatusCode::NO_CONTENT);
	let limits = service::limits(&f.db, TeamId(f.team)).await.unwrap();
	assert_eq!(limits.get(&QuotaKind::Applications), Some(&12));
	assert_eq!(limits.get(&QuotaKind::MemoryMb), None);
	let (kind, source): (String, serde_json::Value) = sqlx::query_as(
		"SELECT actor_kind, diff -> 'source' FROM audit_events WHERE action = 'quota.update'",
	)
	.fetch_one(&f.db)
	.await
	.unwrap();
	assert_eq!(kind, "system");
	assert_eq!(source, json!("billing"));
}

#[db_test]
async fn billing_a_webhook_with_an_unknown_quota_or_a_refusing_provider_changes_nothing(
	db: PgPool,
) {
	let f = fx(db).await;
	*f.mock.answer.lock().unwrap() = Some(Ok(WebhookOutcome::SetQuotas {
		team: f.team,
		limits: vec![QuotaLimit {
			kind: "bandwidth".to_owned(),
			limit: Some(1),
		}],
	}));
	assert_eq!(post_webhook(&f.app, "{}").await, StatusCode::BAD_REQUEST);
	*f.mock.answer.lock().unwrap() = Some(Err("bad signature for acct_123".to_owned()));
	assert_eq!(post_webhook(&f.app, "{}").await, StatusCode::BAD_REQUEST);
	assert_eq!(count(&f.db, "team_quotas").await, 0);
}
