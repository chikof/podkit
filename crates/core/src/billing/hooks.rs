//! The hooks, built against a [`BillingProvider`]. Only compiled with the `cloud` feature.

use std::sync::Arc;

use axum::Router;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use podkit_billing::{
	BillingProvider, Decision, EntitlementRequest, Operation, TeamRef, UsageLine, UsageReport,
	WebhookOutcome, WebhookRequest,
};

use super::Op;
use crate::error::{AppResult, ServerError};
use crate::ids::TeamId;
use crate::quotas::QuotaKind;
use crate::quotas::metering::Record;
use crate::state::AppState;
use crate::teams::Team;

/// The provider this build talks to. Cheap to clone; it is shared.
#[derive(Clone)]
pub struct Billing(Arc<dyn BillingProvider>);

impl Billing {
	/// A build's provider, from `podkit-cloud`.
	#[must_use]
	pub fn cloud() -> Self {
		Self(podkit_cloud::provider())
	}

	/// Any provider (tests plug in a recording one).
	#[cfg(test)]
	#[must_use]
	pub fn new(provider: Arc<dyn BillingProvider>) -> Self {
		Self(provider)
	}
}

impl std::fmt::Debug for Billing {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.write_str("Billing")
	}
}

/// A team exists: the provider may create its customer, which is stored next to it.
pub async fn team_created(s: &AppState, team: &Team) {
	ensure_customer(s, team.id, Some(team)).await;
}

/// Makes sure the team has a customer at the provider. A failure is logged; the next
/// entitlement check tries again.
async fn ensure_customer(s: &AppState, team: TeamId, known: Option<&Team>) {
	let have =
		sqlx::query_scalar::<_, i64>("SELECT team_id FROM billing_customers WHERE team_id = $1")
			.bind(team.0)
			.fetch_optional(&s.db)
			.await;
	match have {
		Ok(Some(_)) => return,
		Ok(None) => {}
		Err(e) => {
			tracing::warn!(%team, "billing customer could not be looked up: {e}");
			return;
		}
	}
	let reference = if let Some(t) = known {
		TeamRef {
			id: t.id.0,
			name: t.name.clone(),
			slug: t.slug.clone(),
		}
	} else {
		let row: Result<Option<(String, String)>, _> =
			sqlx::query_as("SELECT name, slug FROM teams WHERE id = $1")
				.bind(team.0)
				.fetch_optional(&s.db)
				.await;
		let Ok(Some((name, slug))) = row else {
			return;
		};
		TeamRef {
			id: team.0,
			name,
			slug,
		}
	};
	match s.billing.0.create_customer(&reference).await {
		Ok(Some(external)) => {
			let saved = sqlx::query(
				"INSERT INTO billing_customers (team_id, external_id) VALUES ($1, $2)
				ON CONFLICT (team_id) DO NOTHING",
			)
			.bind(team.0)
			.bind(external)
			.execute(&s.db)
			.await;
			if let Err(e) = saved {
				tracing::warn!(%team, "billing customer could not be saved: {e}");
			}
		}
		Ok(None) => {}
		Err(e) => tracing::warn!(%team, "billing provider could not create a customer: {e}"),
	}
}

/// May the team create or deploy this? A refusal carries the provider's sentence; a provider
/// that fails is treated as allowing it.
///
/// # Errors
/// [`ServerError::QuotaExceeded`] with the provider's reason.
pub async fn entitlement(s: &AppState, team: TeamId, op: Op, resource: &str) -> AppResult<()> {
	ensure_customer(s, team, None).await;
	let request = EntitlementRequest {
		team: team.0,
		operation: match op {
			Op::Create => Operation::Create,
			Op::Deploy => Operation::Deploy,
		},
		resource: resource.to_owned(),
	};
	match s.billing.0.check_entitlement(&request).await {
		Ok(Decision::Allow) => Ok(()),
		Ok(Decision::Deny { reason }) => Err(ServerError::QuotaExceeded(reason)),
		Err(e) => {
			tracing::warn!(%team, "billing entitlement check failed, allowing: {e}");
			Ok(())
		}
	}
}

/// The metering pass wrote these records: each goes to the provider once.
pub async fn usage_metered(s: &AppState, records: &[Record]) {
	for r in records {
		let lines: Vec<UsageLine> = [
			("cpu_seconds", r.cpu_seconds),
			("memory_gb_hours", r.memory_gb_hours),
			("disk_gb_hours", r.disk_gb_hours),
			("gpu_hours", r.gpu_hours),
			("egress_bytes", r.egress_bytes as f64),
			("model_tokens", r.model_tokens as f64),
			("build_minutes", r.build_minutes),
		]
		.into_iter()
		.filter(|(_, q)| *q > 0.0)
		.map(|(meter, quantity)| UsageLine { meter, quantity })
		.collect();
		let report = UsageReport {
			team: r.team_id,
			hour_start: r.hour.unix_timestamp(),
			lines,
		};
		if let Err(e) = s.billing.0.report_usage(&report).await {
			tracing::warn!(
				team = r.team_id,
				"billing provider did not take the usage: {e}"
			);
		}
	}
}

/// `POST /webhooks/billing`.
pub fn router() -> Router<AppState> {
	Router::new().route("/webhooks/billing", post(webhook))
}

async fn webhook(State(s): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
	let request = WebhookRequest {
		headers: headers
			.iter()
			.filter_map(|(k, v)| Some((k.as_str().to_owned(), v.to_str().ok()?.to_owned())))
			.collect(),
		body: body.to_vec(),
	};
	match s.billing.0.handle_webhook(&request).await {
		Ok(WebhookOutcome::Ignored) => StatusCode::NO_CONTENT.into_response(),
		Ok(WebhookOutcome::SetQuotas { team, limits }) => {
			let mut changes = Vec::new();
			for l in limits {
				let Some(kind) = QuotaKind::parse(&l.kind) else {
					return (StatusCode::BAD_REQUEST, "unknown quota").into_response();
				};
				if l.limit.is_some_and(|v| v < 0) {
					return (StatusCode::BAD_REQUEST, "negative quota").into_response();
				}
				changes.push((kind, l.limit));
			}
			match crate::quotas::service::apply_from_provider(&s, TeamId(team), &changes).await {
				Ok(()) => StatusCode::NO_CONTENT.into_response(),
				Err(e) => e.into_response(),
			}
		}
		Err(e) => {
			// The provider's message may name its own account details; keep it in the log.
			tracing::warn!("billing webhook refused: {e}");
			StatusCode::BAD_REQUEST.into_response()
		}
	}
}
