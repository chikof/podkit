use async_trait::async_trait;
use podkit_core::domain::permission::Permission;
use podkit_core::domain::permission::repository::PermissionRepository;
use podkit_core::domain::shared::errors::{DomainError, DomainResult};
use podkit_core::domain::shared::ids::PermissionId;

use crate::PgPool;
use crate::models::permissions::PermissionModel;

/// Postgres-backed `PermissionRepository`.
pub struct PgPermissionRepository(pub &'static PgPool);

fn to_infra(e: &crate::DatabaseError) -> DomainError {
	DomainError::Infrastructure(e.to_string())
}

fn map_permission(row: PermissionModel) -> Permission {
	Permission {
		id: PermissionId(row.id),
		action: row.action,
		resource: row.resource,
	}
}

#[async_trait]
impl PermissionRepository for PgPermissionRepository {
	async fn list_all(&self) -> DomainResult<Vec<Permission>> {
		Ok(PermissionModel::list_all(self.0)
			.await
			.map_err(|e| to_infra(&e))?
			.into_iter()
			.map(map_permission)
			.collect())
	}

	async fn find_by_action_resource(
		&self,
		action: &str,
		resource: &str,
	) -> DomainResult<Option<Permission>> {
		Ok(
			PermissionModel::find_by_action_resource(self.0, action, resource)
				.await
				.map_err(|e| to_infra(&e))?
				.map(map_permission),
		)
	}
}
