use async_trait::async_trait;

use crate::domain::server::entity::Server;
use crate::domain::shared::errors::DomainResult;
use crate::domain::shared::ids::{ServerId, TeamId};

/// Persistence contract for [`Server`], implemented by the storage adapter.
#[async_trait]
pub trait ServerRepository: Send + Sync {
	/// Looks up a server by id.
	async fn find_by_id(&self, id: ServerId) -> DomainResult<Option<Server>>;

	/// Lists all servers registered to a team.
	async fn list_by_team(&self, team_id: TeamId) -> DomainResult<Vec<Server>>;

	/// Inserts or updates a server.
	async fn save(&self, server: &Server) -> DomainResult<()>;

	/// Deletes a server by id.
	async fn delete(&self, id: ServerId) -> DomainResult<()>;
}
