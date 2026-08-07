use time::OffsetDateTime;

use crate::domain::shared::ids::{ApplicationId, CustomDomainId};

/// A user-provided hostname routed to an application, distinct from its
/// generated sslip.io subdomain. Getting traffic here requires the user's
/// own DNS to point at the target server; podkit doesn't verify that
/// (self-evident to the user when it doesn't work).
#[derive(Debug, Clone)]
pub struct CustomDomain {
	/// Unique id of this custom domain.
	pub id: CustomDomainId,
	/// The application it routes to.
	pub application_id: ApplicationId,
	/// Unique across the whole install, since a hostname can only point at
	/// one place at a time.
	pub hostname: String,
	/// When the custom domain was created.
	pub created_at: OffsetDateTime,
	/// When the custom domain was last updated.
	pub updated_at: OffsetDateTime,
}

impl CustomDomain {
	/// Creates a new custom domain mapping, stamping both timestamps to now.
	#[must_use]
	pub fn new(id: CustomDomainId, application_id: ApplicationId, hostname: String) -> Self {
		let now = OffsetDateTime::now_utc();
		Self {
			id,
			application_id,
			hostname,
			created_at: now,
			updated_at: now,
		}
	}
}
