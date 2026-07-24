use crate::domain::role::entity::Role;
use crate::domain::shared::errors::DomainResult;
use crate::domain::shared::ids::{RoleId, TeamId};
use async_trait::async_trait;

#[async_trait]
pub trait RoleRepository: Send + Sync {
	async fn find_by_id(&self, id: RoleId) -> DomainResult<Option<Role>>;

	async fn find_builtin_by_name(&self, name: &str) -> DomainResult<Option<Role>>;

	async fn list_for_team(&self, team_id: TeamId) -> DomainResult<Vec<Role>>;

	async fn save(&self, role: &Role) -> DomainResult<()>;

	async fn delete(&self, id: RoleId) -> DomainResult<()>;
}
