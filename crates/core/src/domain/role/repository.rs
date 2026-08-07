use crate::domain::role::entity::Role;
use crate::domain::shared::errors::DomainResult;
use crate::domain::shared::ids::{RoleId, TeamId};
use async_trait::async_trait;

/// Persistence contract for [`Role`], implemented by the storage adapter.
#[async_trait]
pub trait RoleRepository: Send + Sync {
	/// Looks up a role by id.
	async fn find_by_id(&self, id: RoleId) -> DomainResult<Option<Role>>;

	/// Looks up a built-in (global) role by name.
	async fn find_builtin_by_name(&self, name: &str) -> DomainResult<Option<Role>>;

	/// Lists all roles available to a team, built-in and team-specific alike.
	async fn list_for_team(&self, team_id: TeamId) -> DomainResult<Vec<Role>>;

	/// Inserts or updates a role.
	async fn save(&self, role: &Role) -> DomainResult<()>;

	/// Deletes a role by id.
	async fn delete(&self, id: RoleId) -> DomainResult<()>;
}
