use time::OffsetDateTime;

use crate::domain::shared::ids::{ApplicationId, EnvVarId};

/// A single environment variable injected into an application's container
/// at deploy time. Values are always age-encrypted at rest and never
/// round-tripped in plaintext through the API after creation: redacted on
/// read, matching how a webhook/deploy-key secret works.
#[derive(Debug, Clone)]
pub struct EnvVar {
	/// Unique id of this env var.
	pub id: EnvVarId,
	/// The application it's injected into.
	pub application_id: ApplicationId,
	/// Variable name.
	pub key: String,
	/// age-encrypted value.
	pub value: Vec<u8>,
	/// When the env var was created.
	pub created_at: OffsetDateTime,
	/// When the env var was last updated.
	pub updated_at: OffsetDateTime,
}

impl EnvVar {
	/// Creates a new env var, stamping both timestamps to now.
	#[must_use]
	pub fn new(id: EnvVarId, application_id: ApplicationId, key: String, value: Vec<u8>) -> Self {
		let now = OffsetDateTime::now_utc();
		Self {
			id,
			application_id,
			key,
			value,
			created_at: now,
			updated_at: now,
		}
	}
}
