//! The public stand-in for the private `podkit-cloud` crate.
//!
//! The server's `cloud` feature calls [`provider`] once at start-up and keeps the result. This
//! stand-in returns the provider that does nothing. The cloud overlay replaces this crate (same
//! name, same path, same one function) with its own, which holds the payment integration; see
//! `docs/cloud-extension.md`. No payment code belongs in this repository.

use std::sync::Arc;

use podkit_billing::{BillingProvider, NoopBilling};

/// The billing provider of this build.
#[must_use]
pub fn provider() -> Arc<dyn BillingProvider> {
	Arc::new(NoopBilling)
}
