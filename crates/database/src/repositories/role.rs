use async_trait::async_trait;
use podkit_core::domain::role::Role;
use podkit_core::domain::role::repository::RoleRepository;
use podkit_core::domain::shared::errors::{DomainError, DomainResult};
use podkit_core::domain::shared::ids::{RoleId, TeamId};

use crate::PgPool;
use crate::models::roles::{NewRole, RoleModel};

/// Postgres-backed `RoleRepository`.
pub struct PgRoleRepository(pub &'static PgPool);

fn to_infra(e: &crate::DatabaseError) -> DomainError {
	DomainError::Infrastructure(e.to_string())
}

fn map_role(row: RoleModel) -> Role {
	Role {
		id: RoleId(row.id),
		team_id: row.team_id.map(TeamId),
		name: row.name,
		is_default: row.is_default,
		created_at: row.created_at,
	}
}

#[async_trait]
impl RoleRepository for PgRoleRepository {
	async fn find_by_id(&self, id: RoleId) -> DomainResult<Option<Role>> {
		Ok(RoleModel::find_by_id(self.0, id.0)
			.await
			.map_err(|e| to_infra(&e))?
			.map(map_role))
	}

	async fn find_builtin_by_name(&self, name: &str) -> DomainResult<Option<Role>> {
		Ok(RoleModel::find_builtin_by_name(self.0, name)
			.await
			.map_err(|e| to_infra(&e))?
			.map(map_role))
	}

	async fn list_for_team(&self, team_id: TeamId) -> DomainResult<Vec<Role>> {
		Ok(RoleModel::list_for_team(self.0, team_id.0)
			.await
			.map_err(|e| to_infra(&e))?
			.into_iter()
			.map(map_role)
			.collect())
	}

	async fn save(&self, role: &Role) -> DomainResult<()> {
		RoleModel::create(
			self.0,
			NewRole {
				id: role.id.0,
				team_id: role.team_id.map(|t| t.0),
				name: role.name.clone(),
				is_default: role.is_default,
			},
		)
		.await
		.map_err(|e| to_infra(&e))?;
		Ok(())
	}

	async fn delete(&self, id: RoleId) -> DomainResult<()> {
		RoleModel::delete(self.0, id.0)
			.await
			.map_err(|e| to_infra(&e))
	}
}
