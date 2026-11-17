//! Servers: podman hosts a team can deploy to. The local one is created with the team.

mod routes;

pub use routes::router;

use crypto::age::SecretBox;
use runtime::{ConnectTarget, ContainerRuntime, ServerConnection};

use crate::ids::{ServerId, TeamId};

/// Connectivity state of a registered server.
#[derive(Debug, Clone, Copy, PartialEq, Eq, sqlx::Type, serde::Serialize, serde::Deserialize)]
#[sqlx(type_name = "varchar", rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum ServerStatus {
	/// Remote server registered, connectivity probe not yet confirmed.
	Pending,
	/// Reachable and ready to run containers.
	Active,
	/// Was active, most recent probe/op failed.
	Unreachable,
}

impl ServerStatus {
	/// Returns the lowercase string form used for storage and display.
	pub fn as_str(self) -> &'static str {
		match self {
			Self::Pending => "pending",
			Self::Active => "active",
			Self::Unreachable => "unreachable",
		}
	}
}

/// A podman host a team can deploy containers to.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Server {
	pub id: ServerId,
	pub team_id: TeamId,
	pub name: String,
	/// Hostname or IP podkit connects to.
	pub hostname: String,
	/// SSH port, unused for the local server.
	pub ssh_port: i32,
	/// SSH username. `None` iff `is_local`.
	pub ssh_user: Option<String>,
	/// age-encrypted private key bytes. `None` iff `is_local`.
	pub ssh_private_key: Option<Vec<u8>>,
	/// Path to the podman socket on the target host.
	pub podman_socket_path: String,
	/// True for the podkit host itself; false for SSH-reached hosts.
	pub is_local: bool,
	pub status: ServerStatus,
}

impl Server {
	/// The connection parameters `runtime` needs to reach this server.
	pub fn connect_target(&self) -> ConnectTarget<'_> {
		ConnectTarget {
			is_local: self.is_local,
			hostname: &self.hostname,
			ssh_port: self.ssh_port,
			ssh_user: self.ssh_user.as_deref(),
			ssh_private_key: self.ssh_private_key.as_deref(),
			podman_socket_path: &self.podman_socket_path,
		}
	}

	/// Builds a remote server pending its first connectivity probe.
	#[allow(clippy::too_many_arguments)]
	pub fn new_remote(
		team_id: TeamId,
		name: String,
		hostname: String,
		ssh_port: i32,
		ssh_user: String,
		ssh_private_key: Vec<u8>,
		podman_socket_path: String,
	) -> Self {
		Self {
			id: ServerId::generate(),
			team_id,
			name,
			hostname,
			ssh_port,
			ssh_user: Some(ssh_user),
			ssh_private_key: Some(ssh_private_key),
			podman_socket_path,
			is_local: false,
			status: ServerStatus::Pending,
		}
	}
}

/// Resolves the current user's default rootless podman socket path
/// (`$XDG_RUNTIME_DIR/podman/podman.sock`, falling back to
/// `/run/user/<uid>/podman/podman.sock`). Does not verify the socket exists.
pub fn local_podman_socket() -> String {
	match std::env::var("XDG_RUNTIME_DIR").ok() {
		Some(dir) => format!("{dir}/podman/podman.sock"),
		None => format!("/run/user/{}/podman/podman.sock", getuid()),
	}
}

fn getuid() -> u32 {
	unsafe extern "C" {
		fn getuid() -> u32;
	}
	unsafe { getuid() }
}

/// Probes ssh and podman API reachability of `server`.
pub async fn probe(server: &Server, secrets: &SecretBox) -> bool {
	match ServerConnection::connect(&server.connect_target(), secrets).await {
		Ok(connection) => connection.runtime.ping().await.is_ok(),
		Err(_) => false,
	}
}

pub async fn insert(db: impl sqlx::PgExecutor<'_>, server: &Server) -> Result<(), sqlx::Error> {
	sqlx::query!(
		r#"
			INSERT INTO servers
				(id, team_id, name, hostname, ssh_port, ssh_user, ssh_private_key, podman_socket_path, is_local, status)
			VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
		"#,
		server.id as ServerId,
		server.team_id as TeamId,
		server.name,
		server.hostname,
		server.ssh_port,
		server.ssh_user,
		server.ssh_private_key,
		server.podman_socket_path,
		server.is_local,
		server.status.as_str()
	)
	.execute(db)
	.await?;
	Ok(())
}

pub async fn list_by_team(
	db: impl sqlx::PgExecutor<'_>,
	team: TeamId,
) -> Result<Vec<Server>, sqlx::Error> {
	sqlx::query_as!(
		Server,
		r#"
			SELECT id AS "id: ServerId", team_id AS "team_id: TeamId", name, hostname, ssh_port,
				ssh_user, ssh_private_key, podman_socket_path, is_local,
				status AS "status: ServerStatus"
			FROM servers
			WHERE team_id = $1
		"#,
		team as TeamId
	)
	.fetch_all(db)
	.await
}

pub async fn delete(
	db: impl sqlx::PgExecutor<'_>,
	team: TeamId,
	id: ServerId,
) -> Result<(), sqlx::Error> {
	sqlx::query!(
		"DELETE FROM servers WHERE id = $1 AND team_id = $2",
		id as ServerId,
		team as TeamId
	)
	.execute(db)
	.await?;
	Ok(())
}
