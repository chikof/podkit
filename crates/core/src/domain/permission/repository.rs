use crate::domain::permission::entity::Permission;
use crate::domain::shared::errors::DomainResult;
use async_trait::async_trait;

#[async_trait]
pub trait PermissionRepository: Send + Sync {
	async fn list_all(&self) -> DomainResult<Vec<Permission>>;

	async fn find_by_action_resource(
		&self,
		action: &str,
		resource: &str,
	) -> DomainResult<Option<Permission>>;
}
