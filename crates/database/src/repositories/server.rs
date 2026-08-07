use async_trait::async_trait;
use podkit_core::domain::server::entity::{Server, ServerStatus};
use podkit_core::domain::server::repository::ServerRepository;
use podkit_core::domain::shared::errors::{DomainError, DomainResult};
use podkit_core::domain::shared::ids::{ServerId, TeamId};

use crate::PgPool;
use crate::models::server::{NewServer, ServerModel};

/// Postgres-backed `ServerRepository`.
pub struct PgServerRepository(pub &'static PgPool);

fn to_infra(e: &crate::DatabaseError) -> DomainError {
	DomainError::Infrastructure(e.to_string())
}

fn map_server(row: ServerModel) -> Server {
	Server {
		id: ServerId(row.id),
		team_id: TeamId(row.team_id),
		name: row.name,
		hostname: row.hostname,
		ssh_port: row.ssh_port,
		ssh_user: row.ssh_user,
		ssh_private_key: row.ssh_private_key,
		podman_socket_path: row.podman_socket_path,
		is_local: row.is_local,
		status: ServerStatus::parse(&row.status),
		created_at: row.created_at,
		updated_at: row.updated_at,
	}
}

#[async_trait]
impl ServerRepository for PgServerRepository {
	async fn find_by_id(&self, id: ServerId) -> DomainResult<Option<Server>> {
		Ok(ServerModel::find_by_id(self.0, id.0)
			.await
			.map_err(|e| to_infra(&e))?
			.map(map_server))
	}

	async fn list_by_team(&self, team_id: TeamId) -> DomainResult<Vec<Server>> {
		Ok(ServerModel::list_by_team(self.0, team_id.0)
			.await
			.map_err(|e| to_infra(&e))?
			.into_iter()
			.map(map_server)
			.collect())
	}

	async fn save(&self, server: &Server) -> DomainResult<()> {
		ServerModel::create(
			self.0,
			NewServer {
				id: server.id.0,
				team_id: server.team_id.0,
				name: server.name.clone(),
				hostname: server.hostname.clone(),
				ssh_port: server.ssh_port,
				ssh_user: server.ssh_user.clone(),
				ssh_private_key: server.ssh_private_key.clone(),
				podman_socket_path: server.podman_socket_path.clone(),
				is_local: server.is_local,
				status: server.status.as_str().to_string(),
			},
		)
		.await
		.map_err(|e| to_infra(&e))?;
		Ok(())
	}

	async fn delete(&self, id: ServerId) -> DomainResult<()> {
		ServerModel::delete(self.0, id.0)
			.await
			.map_err(|e| to_infra(&e))
	}
}
