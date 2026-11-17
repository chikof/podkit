use crypto::age::SecretBox;

use crate::RuntimeError;
use crate::podman::PodmanRuntime;
use crate::tunnel::{SshTarget, SshTunnel};

/// Everything needed to reach a team's server, borrowed from wherever the
/// caller stores it. `ssh_private_key` is the *encrypted* key as stored; it's
/// decrypted with the `SecretBox` passed to [`ServerConnection::connect`].
#[derive(Debug, Clone, Copy)]
pub struct ConnectTarget<'a> {
	/// Whether the server is the team's local one (no ssh tunnel needed).
	pub is_local: bool,
	/// Host to ssh to for a remote server.
	pub hostname: &'a str,
	/// ssh port for a remote server.
	pub ssh_port: i32,
	/// ssh user for a remote server; required when `is_local` is false.
	pub ssh_user: Option<&'a str>,
	/// Encrypted ssh private key for a remote server; required when
	/// `is_local` is false.
	pub ssh_private_key: Option<&'a [u8]>,
	/// Path of the podman socket on the server itself.
	pub podman_socket_path: &'a str,
}

/// A `ContainerRuntime`-capable connection to a team's server. For remote
/// servers this also owns the live ssh tunnel backing it, so dropping this
/// tears the tunnel down.
pub struct ServerConnection {
	/// The connected runtime, ready to run container operations against.
	pub runtime: PodmanRuntime,
	_tunnel: Option<SshTunnel>,
}

impl ServerConnection {
	/// Connects to `target`, directly if it's the team's local server,
	/// otherwise by opening an ssh tunnel to its `podman_socket_path` first.
	///
	/// # Errors
	/// Returns an error if a remote server is missing ssh credentials (this
	/// shouldn't happen for a properly provisioned server), the stored key
	/// can't be decrypted, the tunnel can't be established, or the podman
	/// socket can't be reached.
	pub async fn connect(
		target: &ConnectTarget<'_>,
		secrets: &SecretBox,
	) -> Result<Self, RuntimeError> {
		if target.is_local {
			let runtime = PodmanRuntime::connect(target.podman_socket_path)?;
			return Ok(Self {
				runtime,
				_tunnel: None,
			});
		}

		let ssh_user = target.ssh_user.ok_or(RuntimeError::MissingSshCredentials)?;
		let encrypted_key = target
			.ssh_private_key
			.ok_or(RuntimeError::MissingSshCredentials)?;
		let private_key_pem = secrets
			.decrypt(encrypted_key)
			.map_err(|e| RuntimeError::Tunnel(e.to_string()))?;

		let tunnel = SshTunnel::open(SshTarget {
			host: target.hostname,
			port: target.ssh_port,
			user: ssh_user,
			private_key_pem: &private_key_pem,
			remote_socket_path: target.podman_socket_path,
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
