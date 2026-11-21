//! The billing extension point.
//!
//! podkit itself never talks to a payment provider. The server holds one
//! [`BillingProvider`] and calls it at four places (see `docs/cloud-extension.md`):
//!
//! - a team is created: [`BillingProvider::create_customer`];
//! - usage was metered for an hour: [`BillingProvider::report_usage`];
//! - something is created or deployed: [`BillingProvider::check_entitlement`];
//! - the provider calls podkit back: [`BillingProvider::handle_webhook`].
//!
//! The default, [`NoopBilling`], allows everything and reports nothing. A private crate
//! (`podkit-cloud`) replaces it; this crate is what both sides build against, so the trait
//! is the whole contract. It has no dependency besides `thiserror`.

use std::future::Future;
use std::pin::Pin;

/// A boxed future: the trait is object safe without a macro crate.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// A provider failed. The message is logged and, for an entitlement check, never shown to the
/// user: a provider outage must not stop anyone from deploying, so callers treat an error as
/// "allowed".
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct BillingError(pub String);

/// The team a call is about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TeamRef {
	/// The team's id.
	pub id: i64,
	/// Display name.
	pub name: String,
	/// URL slug.
	pub slug: String,
}

/// One meter of an hour of usage. `meter` is one of [`METERS`].
#[derive(Debug, Clone, PartialEq)]
pub struct UsageLine {
	/// `cpu_seconds`, `memory_gb_hours`, ...
	pub meter: &'static str,
	/// How much of it.
	pub quantity: f64,
}

/// The meters a [`UsageReport`] can carry.
pub const METERS: [&str; 7] = [
	"cpu_seconds",
	"memory_gb_hours",
	"disk_gb_hours",
	"gpu_hours",
	"egress_bytes",
	"model_tokens",
	"build_minutes",
];

/// One team's usage for one hour. Reporting the same hour twice is possible (metering
/// recomputes the last day), so a provider must key on `(team, hour_start)`.
#[derive(Debug, Clone, PartialEq)]
pub struct UsageReport {
	/// The team.
	pub team: i64,
	/// Start of the hour, unix seconds.
	pub hour_start: i64,
	/// Every meter that is not zero.
	pub lines: Vec<UsageLine>,
}

/// What is about to happen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operation {
	/// A resource is being created.
	Create,
	/// A resource is being deployed, started or redeployed.
	Deploy,
}

/// The question "may this team do this?".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntitlementRequest {
	/// The team.
	pub team: i64,
	/// Create or deploy.
	pub operation: Operation,
	/// `application`, `database`, `model`, `stack` or `preview`.
	pub resource: String,
}

/// The answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
	/// Go ahead.
	Allow,
	/// Refuse; `reason` is shown to the user as written, so it is a sentence.
	Deny {
		/// Why.
		reason: String,
	},
}

/// A webhook the provider sent to `POST /webhooks/billing`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebhookRequest {
	/// Header names (lower case) and values.
	pub headers: Vec<(String, String)>,
	/// The raw body, so a signature can be checked.
	pub body: Vec<u8>,
}

/// One limit a webhook sets: `kind` is a quota key (`applications`, `memory_mb`, ...),
/// `limit` of `None` removes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuotaLimit {
	/// The quota's key.
	pub kind: String,
	/// The new hard limit, or none for unlimited.
	pub limit: Option<i64>,
}

/// What the server does after a webhook.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WebhookOutcome {
	/// Nothing to do (an event type podkit does not care about).
	Ignored,
	/// Set these limits for the team: a plan changed.
	SetQuotas {
		/// The team.
		team: i64,
		/// The limits to write.
		limits: Vec<QuotaLimit>,
	},
}

/// What the server asks of a billing provider. Every method has a default that does nothing, so
/// a provider overrides what it needs.
pub trait BillingProvider: Send + Sync {
	/// A team now exists. Returns the provider's customer id, stored next to the team.
	fn create_customer<'a>(
		&'a self,
		_team: &'a TeamRef,
	) -> BoxFuture<'a, Result<Option<String>, BillingError>> {
		Box::pin(async { Ok(None) })
	}

	/// An hour of usage was metered.
	fn report_usage<'a>(
		&'a self,
		_report: &'a UsageReport,
	) -> BoxFuture<'a, Result<(), BillingError>> {
		Box::pin(async { Ok(()) })
	}

	/// May the team create or deploy this? Asked after podkit's own quotas said yes.
	fn check_entitlement<'a>(
		&'a self,
		_request: &'a EntitlementRequest,
	) -> BoxFuture<'a, Result<Decision, BillingError>> {
		Box::pin(async { Ok(Decision::Allow) })
	}

	/// The provider called `POST /webhooks/billing`. The provider checks the signature.
	fn handle_webhook<'a>(
		&'a self,
		_request: &'a WebhookRequest,
	) -> BoxFuture<'a, Result<WebhookOutcome, BillingError>> {
		Box::pin(async { Ok(WebhookOutcome::Ignored) })
	}
}

/// The provider podkit ships: allows everything, reports nothing, ignores webhooks.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopBilling;

impl BillingProvider for NoopBilling {}

#[cfg(test)]
mod tests {
	use super::*;

	#[tokio::test]
	async fn the_default_provider_allows_everything_and_reports_nothing() {
		let noop = NoopBilling;
		let team = TeamRef {
			id: 1,
			name: "Home".into(),
			slug: "home".into(),
		};
		assert_eq!(noop.create_customer(&team).await.unwrap(), None);
		let report = UsageReport {
			team: 1,
			hour_start: 0,
			lines: vec![],
		};
		noop.report_usage(&report).await.unwrap();
		let ask = EntitlementRequest {
			team: 1,
			operation: Operation::Create,
			resource: "application".into(),
		};
		assert_eq!(noop.check_entitlement(&ask).await.unwrap(), Decision::Allow);
		let hook = WebhookRequest {
			headers: vec![],
			body: vec![],
		};
		assert_eq!(
			noop.handle_webhook(&hook).await.unwrap(),
			WebhookOutcome::Ignored
		);
	}
}
