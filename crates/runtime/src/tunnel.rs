use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use tempfile::TempDir;
use tokio::io::AsyncReadExt;
use tokio::process::{Child, Command};

use crate::RuntimeError;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Target for an ssh unix-socket forward.
pub struct SshTarget<'a> {
	/// Hostname or IP of the remote server.
	pub host: &'a str,
	/// SSH port on the remote server.
	pub port: i32,
	/// SSH user to connect as.
	pub user: &'a str,
	/// Decrypted OpenSSH-format private key PEM.
	pub private_key_pem: &'a str,
	/// Path to the unix socket on the remote host to forward, typically
	/// its `podman.sock`.
	pub remote_socket_path: &'a str,
}

/// A live SSH tunnel forwarding a local unix socket to a remote unix socket
/// (typically the remote host's `podman.sock`), via OpenSSH's unix-domain
/// local forwarding: `ssh -L local_sock:remote_sock host`.
///
/// Shells out to the system `ssh` binary rather than a hand-rolled client.
/// Streamlocal forwarding is exactly this use case, and OpenSSH's
/// implementation is far better audited than reimplementing the protocol
/// would be.
///
/// Dropping this kills the underlying `ssh` process and removes the temp
/// dir holding the private key and local socket.
pub struct SshTunnel {
	child: Child,
	_dir: TempDir,
	local_socket_path: PathBuf,
}

impl SshTunnel {
	/// Opens a tunnel and waits for it to come up. That wait doubles as the
	/// connectivity and auth probe we need before a remote server can be
	/// marked active.
	///
	/// # Errors
	/// Returns an error if the key is malformed, the `ssh` process can't be
	/// spawned, or the tunnel doesn't come up within ~10s (auth failure,
	/// unreachable host, bad remote socket path, etc, the underlying
	/// `ssh` stderr is included).
	pub async fn open(target: SshTarget<'_>) -> Result<Self, RuntimeError> {
		let dir = tempfile::tempdir()?;

		let key_path = dir.path().join("id_key");
		write_private_key(&key_path, target.private_key_pem)?;

		let known_hosts_path = dir.path().join("known_hosts");
		let local_socket_path = dir.path().join("podman.sock");

		let mut child = Command::new("ssh")
			.arg("-N")
			.arg("-o")
			.arg("BatchMode=yes")
			.arg("-o")
			.arg("ExitOnForwardFailure=yes")
			.arg("-o")
			.arg("StrictHostKeyChecking=accept-new")
			.arg("-o")
			.arg(format!("UserKnownHostsFile={}", known_hosts_path.display()))
			.arg("-o")
			.arg("ConnectTimeout=10")
			.arg("-i")
			.arg(&key_path)
			.arg("-p")
			.arg(target.port.to_string())
			.arg("-L")
			.arg(format!(
				"{}:{}",
				local_socket_path.display(),
				target.remote_socket_path
			))
			.arg(format!("{}@{}", target.user, target.host))
			.stdin(Stdio::null())
			.stdout(Stdio::piped())
			.stderr(Stdio::piped())
			.kill_on_drop(true)
			.spawn()?;

		wait_for_socket_or_exit(&mut child, &local_socket_path).await?;

		Ok(Self {
			child,
			_dir: dir,
			local_socket_path,
		})
	}

	/// Path to the local unix socket the remote one is forwarded to.
	#[must_use]
	pub fn local_socket_path(&self) -> &Path {
		&self.local_socket_path
	}

	/// `true` if the underlying `ssh` process is still alive.
	pub fn is_alive(&mut self) -> bool {
		matches!(self.child.try_wait(), Ok(None))
	}
}

fn write_private_key(path: &Path, pem: &str) -> Result<(), RuntimeError> {
	use std::io::Write;
	use std::os::unix::fs::OpenOptionsExt;

	let mut file = std::fs::OpenOptions::new()
		.write(true)
		.create_new(true)
		.mode(0o600)
		.open(path)?;
	file.write_all(pem.as_bytes())?;
	if !pem.ends_with('\n') {
		file.write_all(b"\n")?;
	}
	Ok(())
}

async fn wait_for_socket_or_exit(
	child: &mut Child,
	socket_path: &Path,
) -> Result<(), RuntimeError> {
	let deadline = tokio::time::Instant::now() + CONNECT_TIMEOUT;

	loop {
		if socket_path.exists() {
			return Ok(());
		}

		if let Some(status) = child.try_wait()? {
			let mut stderr = String::new();
			if let Some(mut s) = child.stderr.take() {
				let _ = s.read_to_string(&mut stderr).await;
			}
			return Err(RuntimeError::Tunnel(format!(
				"ssh exited with {status}: {}",
				stderr.trim()
			)));
		}

		if tokio::time::Instant::now() >= deadline {
			let _ = child.start_kill();
			return Err(RuntimeError::Tunnel(
				"timed out waiting for ssh tunnel to come up".to_string(),
			));
		}

		tokio::time::sleep(Duration::from_millis(100)).await;
	}
}
