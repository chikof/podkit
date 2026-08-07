use crate::domain::permission::entity::Permission;
use crate::domain::shared::errors::DomainResult;
use async_trait::async_trait;

/// Persistence contract for [`Permission`], implemented by the storage adapter.
#[async_trait]
pub trait PermissionRepository: Send + Sync {
	/// Lists every permission known to the system.
	async fn list_all(&self) -> DomainResult<Vec<Permission>>;

	/// Looks up a permission by its `(action, resource)` pair.
	async fn find_by_action_resource(
		&self,
		action: &str,
		resource: &str,
	) -> DomainResult<Option<Permission>>;
}
