use time::OffsetDateTime;

use crate::domain::shared::ids::{ServerId, TeamId};

/// Connectivity state of a registered server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServerStatus {
	/// Remote server registered, connectivity/podman-version probe not yet confirmed.
	Pending,
	/// Reachable and ready to run containers.
	Active,
	/// Was active, most recent probe/op failed.
	Unreachable,
}

impl ServerStatus {
	/// Returns the lowercase string form used for storage and display.
	#[must_use]
	pub fn as_str(self) -> &'static str {
		match self {
			Self::Pending => "pending",
			Self::Active => "active",
			Self::Unreachable => "unreachable",
		}
	}

	/// Parses the storage string form back into a status. Unrecognized
	/// input falls back to `Pending` rather than failing.
	#[must_use]
	pub fn parse(s: &str) -> Self {
		match s {
			"active" => Self::Active,
			"unreachable" => Self::Unreachable,
			_ => Self::Pending,
		}
	}
}

/// A podman host a team can deploy containers to. Team-scoped, same tenancy
/// boundary as `Project`.
#[derive(Debug, Clone)]
pub struct Server {
	/// Unique id of this server.
	pub id: ServerId,
	/// The team that owns this server.
	pub team_id: TeamId,
	/// Human-readable name.
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
	/// True for the single podkit-managed local server; false for remote,
	/// SSH-reached hosts.
	pub is_local: bool,
	/// Current connectivity state.
	pub status: ServerStatus,
	/// When the server was registered.
	pub created_at: OffsetDateTime,
	/// When the server was last updated.
	pub updated_at: OffsetDateTime,
}

impl Server {
	/// Builds the podkit-managed local server for a team. No ssh credentials,
	/// active immediately (no probe needed, it's the podkit host itself).
	#[must_use]
	pub fn new_local(
		id: ServerId,
		team_id: TeamId,
		name: String,
		hostname: String,
		podman_socket_path: String,
	) -> Self {
		let now = OffsetDateTime::now_utc();
		Self {
			id,
			team_id,
			name,
			hostname,
			ssh_port: 22,
			ssh_user: None,
			ssh_private_key: None,
			podman_socket_path,
			is_local: true,
			status: ServerStatus::Active,
			created_at: now,
			updated_at: now,
		}
	}

	/// Builds a remote server pending its first connectivity probe.
	#[must_use]
	#[allow(clippy::too_many_arguments)]
	pub fn new_remote(
		id: ServerId,
		team_id: TeamId,
		name: String,
		hostname: String,
		ssh_port: i32,
		ssh_user: String,
		ssh_private_key: Vec<u8>,
		podman_socket_path: String,
	) -> Self {
		let now = OffsetDateTime::now_utc();
		Self {
			id,
			team_id,
			name,
			hostname,
			ssh_port,
			ssh_user: Some(ssh_user),
			ssh_private_key: Some(ssh_private_key),
			podman_socket_path,
			is_local: false,
			status: ServerStatus::Pending,
			created_at: now,
			updated_at: now,
		}
	}
}
