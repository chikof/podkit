//! Servers: podman hosts a team can deploy to. The local one is created with the team.

pub mod bootstrap;
mod routes;
pub mod sync;

pub use routes::router;

use base64::Engine;
use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD};
use sha2::{Digest, Sha256};

use crate::ids::{ServerId, TeamId};

/// Connectivity state of a registered server.
#[derive(Debug, Clone, Copy, PartialEq, Eq, sqlx::Type, serde::Serialize, serde::Deserialize)]
#[sqlx(type_name = "varchar", rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum ServerStatus {
	/// Remote server registered, agent not yet installed.
	Pending,
	/// Agent reachable and ready to run containers.
	Active,
	/// Was active, most recent health check or bootstrap failed.
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
	/// Pinned `known_hosts` line(s), scanned by the first bootstrap.
	pub host_key: Option<String>,
	/// The agent's unix socket on the remote host. `None` until bootstrapped.
	pub agent_socket_path: Option<String>,
	/// Version the installed agent reported at the last bootstrap.
	pub agent_version: Option<String>,
	/// `uname -m` of the remote host, as found at the last bootstrap.
	pub arch: Option<String>,
	/// Why the server is not `active`, when known.
	pub status_message: Option<String>,
}

impl Server {
	/// Builds a remote server pending its first bootstrap.
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
			host_key: None,
			agent_socket_path: None,
			agent_version: None,
			arch: None,
			status_message: None,
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

/// `SHA256:<unpadded base64>` of the pinned key blob, as `ssh-keygen -lf` prints
/// it. Prefers the `ssh-ed25519` line, else the first key line. `None` when no
/// line holds a decodable key.
pub fn host_key_fingerprint(host_key: &str) -> Option<String> {
	let lines = || {
		host_key
			.lines()
			.map(str::trim)
			.filter(|l| !l.is_empty() && !l.starts_with('#'))
	};
	let line = lines()
		.find(|l| l.split_whitespace().any(|t| t == "ssh-ed25519"))
		.or_else(|| lines().next())?;
	let mut tokens = line.split_whitespace();
	tokens.find(|t| t.starts_with("ssh-") || t.starts_with("ecdsa-") || t.starts_with("sk-"))?;
	let blob = STANDARD.decode(tokens.next()?).ok()?;
	Some(format!(
		"SHA256:{}",
		STANDARD_NO_PAD.encode(Sha256::digest(blob))
	))
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

pub async fn find(
	db: impl sqlx::PgExecutor<'_>,
	id: ServerId,
) -> Result<Option<Server>, sqlx::Error> {
	sqlx::query_as!(
		Server,
		r#"
			SELECT id AS "id: ServerId", team_id AS "team_id: TeamId", name, hostname, ssh_port,
				ssh_user, ssh_private_key, podman_socket_path, is_local,
				status AS "status: ServerStatus", host_key, agent_socket_path, agent_version,
				arch, status_message
			FROM servers
			WHERE id = $1
		"#,
		id as ServerId
	)
	.fetch_optional(db)
	.await
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
				status AS "status: ServerStatus", host_key, agent_socket_path, agent_version,
				arch, status_message
			FROM servers
			WHERE team_id = $1
		"#,
		team as TeamId
	)
	.fetch_all(db)
	.await
}

/// Servers the status sync checks: every active one plus all local ones.
pub async fn list_for_sync(db: impl sqlx::PgExecutor<'_>) -> Result<Vec<Server>, sqlx::Error> {
	sqlx::query_as!(
		Server,
		r#"
			SELECT id AS "id: ServerId", team_id AS "team_id: TeamId", name, hostname, ssh_port,
				ssh_user, ssh_private_key, podman_socket_path, is_local,
				status AS "status: ServerStatus", host_key, agent_socket_path, agent_version,
				arch, status_message
			FROM servers
			WHERE status = 'active' OR is_local OR (status = 'unreachable' AND agent_version IS NOT NULL)
		"#
	)
	.fetch_all(db)
	.await
}

pub async fn set_host_key(
	db: impl sqlx::PgExecutor<'_>,
	id: ServerId,
	host_key: Option<&str>,
) -> Result<(), sqlx::Error> {
	sqlx::query!(
		"UPDATE servers SET host_key = $2 WHERE id = $1",
		id as ServerId,
		host_key
	)
	.execute(db)
	.await?;
	Ok(())
}

pub async fn set_agent_socket_path(
	db: impl sqlx::PgExecutor<'_>,
	id: ServerId,
	path: &str,
) -> Result<(), sqlx::Error> {
	sqlx::query!(
		"UPDATE servers SET agent_socket_path = $2 WHERE id = $1",
		id as ServerId,
		path
	)
	.execute(db)
	.await?;
	Ok(())
}

/// Records a finished bootstrap: the agent answered with the expected version.
pub async fn mark_bootstrapped(
	db: impl sqlx::PgExecutor<'_>,
	id: ServerId,
	agent_version: &str,
	arch: &str,
) -> Result<(), sqlx::Error> {
	sqlx::query!(
		"UPDATE servers SET status = 'active', agent_version = $2, arch = $3, status_message = NULL WHERE id = $1",
		id as ServerId,
		agent_version,
		arch
	)
	.execute(db)
	.await?;
	Ok(())
}

/// Writes only when something changes, so the 30 s sync does not churn rows.
pub async fn mark_unreachable(
	db: impl sqlx::PgExecutor<'_>,
	id: ServerId,
	message: &str,
) -> Result<(), sqlx::Error> {
	sqlx::query!(
		r#"
			UPDATE servers SET status = 'unreachable', status_message = $2
			WHERE id = $1 AND (status <> 'unreachable' OR status_message IS DISTINCT FROM $2)
		"#,
		id as ServerId,
		message
	)
	.execute(db)
	.await?;
	Ok(())
}

pub async fn mark_active(db: impl sqlx::PgExecutor<'_>, id: ServerId) -> Result<(), sqlx::Error> {
	sqlx::query!(
		r#"
			UPDATE servers SET status = 'active', status_message = NULL
			WHERE id = $1 AND (status <> 'active' OR status_message IS NOT NULL)
		"#,
		id as ServerId
	)
	.execute(db)
	.await?;
	Ok(())
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

#[cfg(test)]
mod tests {
	use super::*;

	const ED25519: &str = "AAAAC3NzaC1lZDI1NTE5AAAAIJchJVs7z1tmicd4SpMN5UdQGOeEF7c+3zkUwGLv44BI";
	const ED25519_FP: &str = "SHA256:ywBw9jYLna1cd5Li260999IYfpxB/BrS/G6GhT1Xkwg";
	const ECDSA: &str = "AAAAE2VjZHNhLXNoYTItbmlzdHAyNTYAAAAIbmlzdHAyNTYAAABBBC0zD4CZuHNU8aoHB/3WyovTEJm6iAtrq5ekzPOGCImXBltH3T+gigNYNhKvdeuC4OKk0eY2ttw9/S86rp3vSbg=";
	const ECDSA_FP: &str = "SHA256:FqUVfbXpKVRSXQGtouPKGojmEQVz6t/VNKnlHfvejQ0";

	#[test]
	fn fingerprint_matches_ssh_keygen() {
		let line = format!("10.0.0.5 ssh-ed25519 {ED25519}");
		assert_eq!(host_key_fingerprint(&line).as_deref(), Some(ED25519_FP));
	}

	#[test]
	fn fingerprint_prefers_ed25519_over_earlier_lines() {
		let keys = format!(
			"# 10.0.0.5:22 SSH-2.0-OpenSSH_9.6\n10.0.0.5 ecdsa-sha2-nistp256 {ECDSA}\n10.0.0.5 ssh-ed25519 {ED25519}"
		);
		assert_eq!(host_key_fingerprint(&keys).as_deref(), Some(ED25519_FP));
	}

	#[test]
	fn fingerprint_falls_back_to_first_key_line() {
		let keys = format!("# banner\n[10.0.0.5]:2222 ecdsa-sha2-nistp256 {ECDSA}");
		assert_eq!(host_key_fingerprint(&keys).as_deref(), Some(ECDSA_FP));
	}

	#[test]
	fn fingerprint_rejects_garbage() {
		assert_eq!(host_key_fingerprint(""), None);
		assert_eq!(host_key_fingerprint("host ssh-ed25519 !!!notbase64"), None);
		assert_eq!(host_key_fingerprint("host"), None);
	}
}
