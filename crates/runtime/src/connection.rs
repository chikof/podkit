use crypto::age::SecretBox;
use podkit_core::domain::server::Server;

use crate::RuntimeError;
use crate::podman::PodmanRuntime;
use crate::tunnel::{SshTarget, SshTunnel};

/// A `ContainerRuntime`-capable connection to a team's server. For remote
/// servers this also owns the live ssh tunnel backing it, so dropping this
/// tears the tunnel down.
pub struct ServerConnection {
	/// The connected runtime, ready to run container operations against.
	pub runtime: PodmanRuntime,
	_tunnel: Option<SshTunnel>,
}

impl ServerConnection {
	/// Connects to `server`, directly if it's the team's local server,
	/// otherwise by opening an ssh tunnel to its `podman_socket_path` first.
	///
	/// # Errors
	/// Returns an error if a remote server is missing ssh credentials (this
	/// shouldn't happen for a properly provisioned server), the stored key
	/// can't be decrypted, the tunnel can't be established, or the podman
	/// socket can't be reached.
	pub async fn connect(server: &Server, secrets: &SecretBox) -> Result<Self, RuntimeError> {
		if server.is_local {
			let runtime = PodmanRuntime::connect(&server.podman_socket_path)?;
			return Ok(Self {
				runtime,
				_tunnel: None,
			});
		}

		let ssh_user = server
			.ssh_user
			.as_deref()
			.ok_or(RuntimeError::MissingSshCredentials)?;
		let encrypted_key = server
			.ssh_private_key
			.as_deref()
			.ok_or(RuntimeError::MissingSshCredentials)?;
		let private_key_pem = secrets
			.decrypt(encrypted_key)
			.map_err(|e| RuntimeError::Tunnel(e.to_string()))?;

		let tunnel = SshTunnel::open(SshTarget {
			host: &server.hostname,
			port: server.ssh_port,
			user: ssh_user,
			private_key_pem: &private_key_pem,
			remote_socket_path: &server.podman_socket_path,
		})
		.await?;

		let socket_path = tunnel.local_socket_path().to_string_lossy().into_owned();
		let runtime = PodmanRuntime::connect(&socket_path)?;

		Ok(Self {
			runtime,
			_tunnel: Some(tunnel),
		})
	}
}
