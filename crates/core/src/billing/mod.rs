//! Where a billing provider plugs in. podkit itself has no payment code: the `cloud` feature
//! builds the hooks below against [`podkit_billing::BillingProvider`], and the provider in the
//! build comes from the `podkit-cloud` crate (this repository carries a stand-in that does
//! nothing; the hosted offering's private overlay replaces it). `docs/cloud-extension.md`
//! describes the contract.
//!
//! Without the feature every hook here is empty and the whole crate surface is a few lines,
//! so a self-hosted podkit pays nothing for the extension point.
//!
//! The hooks:
//!
//! - [`team_created`]: a team exists, so the provider may create its customer. Teams made by
//!   sign-up are picked up by [`entitlement`] the first time they create or deploy anything.
//! - [`entitlement`]: asked after podkit's own quotas said yes, at every create and deploy.
//!   An answer of "no" is a refusal with the provider's sentence; a provider that fails is
//!   treated as "yes", because a billing outage must not stop anyone's deploys.
//! - [`usage_metered`]: the hourly metering pass found new records; each is reported once.
//! - [`webhook`] (`POST /webhooks/billing`): the provider calls back; the provider checks the
//!   signature and may answer with new limits for a team.

/// What is about to happen, for the entitlement question.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
	/// A resource is being created.
	Create,
	/// A resource is being deployed, started or redeployed.
	Deploy,
}

#[cfg(feature = "cloud")]
mod hooks;
#[cfg(feature = "cloud")]
pub use hooks::{Billing, entitlement, router, team_created, usage_metered};

#[cfg(not(feature = "cloud"))]
mod off {
	use super::Op;
	use crate::error::AppResult;
	use crate::ids::TeamId;
	use crate::quotas::metering::Record;
	use crate::state::AppState;
	use crate::teams::Team;

	/// No provider: everything is allowed.
	#[allow(clippy::unused_async, reason = "same signature as the `cloud` build")]
	pub async fn entitlement(
		_s: &AppState,
		_team: TeamId,
		_op: Op,
		_resource: &str,
	) -> AppResult<()> {
		Ok(())
	}

	/// No provider: nothing to create.
	#[allow(clippy::unused_async, reason = "same signature as the `cloud` build")]
	pub async fn team_created(_s: &AppState, _team: &Team) {}

	/// No provider: nothing to report.
	#[allow(clippy::unused_async, reason = "same signature as the `cloud` build")]
	pub async fn usage_metered(_s: &AppState, _records: &[Record]) {}
}
#[cfg(not(feature = "cloud"))]
pub use off::{entitlement, team_created, usage_metered};

#[cfg(all(test, feature = "cloud"))]
mod tests;
