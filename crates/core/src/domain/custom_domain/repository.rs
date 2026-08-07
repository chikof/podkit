use async_trait::async_trait;

use crate::domain::custom_domain::entity::CustomDomain;
use crate::domain::shared::errors::DomainResult;
use crate::domain::shared::ids::{ApplicationId, CustomDomainId};

/// Persistence contract for [`CustomDomain`], implemented by the storage adapter.
#[async_trait]
pub trait CustomDomainRepository: Send + Sync {
	/// Looks up a custom domain by id.
	async fn find_by_id(&self, id: CustomDomainId) -> DomainResult<Option<CustomDomain>>;

	/// Lists all custom domains routed to an application.
	async fn list_by_application(
		&self,
		application_id: ApplicationId,
	) -> DomainResult<Vec<CustomDomain>>;

	/// # Errors
	/// Returns [`crate::domain::shared::errors::DomainError::AlreadyExists`]
	/// if `hostname` is already routed to another application.
	async fn save(&self, domain: &CustomDomain) -> DomainResult<()>;

	/// Deletes a custom domain by id.
	async fn delete(&self, id: CustomDomainId) -> DomainResult<()>;
}
