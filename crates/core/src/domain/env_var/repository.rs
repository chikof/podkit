use async_trait::async_trait;

use crate::domain::env_var::entity::EnvVar;
use crate::domain::shared::errors::DomainResult;
use crate::domain::shared::ids::{ApplicationId, EnvVarId};

/// Persistence contract for [`EnvVar`], implemented by the storage adapter.
#[async_trait]
pub trait EnvVarRepository: Send + Sync {
	/// Lists all env vars for an application.
	async fn list_by_application(&self, application_id: ApplicationId)
	-> DomainResult<Vec<EnvVar>>;

	/// Looks up an env var by id.
	async fn find_by_id(&self, id: EnvVarId) -> DomainResult<Option<EnvVar>>;

	/// Inserts or replaces the value for `(application_id, key)`. Setting
	/// an env var twice updates it rather than erroring, matching how every
	/// `PaaS` env-var UI behaves.
	async fn upsert(&self, env_var: &EnvVar) -> DomainResult<EnvVar>;

	/// Deletes an env var by id.
	async fn delete(&self, id: EnvVarId) -> DomainResult<()>;
}
