use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};
use std::sync::Mutex;
use std::time::Duration;

use crypto::age::SecretBox;
use tempfile::TempDir;
use tokio::io::AsyncReadExt;
use tokio::process::{Child, Command};
use zeroize::Zeroizing;

use crate::servers::Server;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Messages are safe to store in a server's `status_message`: they carry ssh
/// stderr at most, never key material.
#[derive(thiserror::Error, Debug)]
pub enum TunnelError {
	#[error("server is missing ssh credentials or a pinned host key")]
	MissingCredentials,

	#[error("could not decrypt the server's ssh key")]
	Decrypt,

	#[error("io error: {0}")]
	Io(#[from] std::io::Error),

	#[error("ssh failed: {0}")]
	Ssh(String),

	#[error("timed out waiting for the ssh tunnel to come up")]
	Timeout,

	#[error("ssh-keyscan returned no host key")]
	NoHostKey,
}

/// Everything needed to reach a remote server over ssh, with its host key pinned.
pub struct SshTarget {
	pub host: String,
	pub port: u16,
	pub user: String,
	/// Decrypted OpenSSH-format private key.
	pub private_key: Zeroizing<String>,
	/// `known_hosts` line(s) as produced by [`keyscan`].
	pub host_key: String,
}

impl SshTarget {
	/// `sealed_key` is the server's vault key, as stored (age-encrypted).
	///
	/// # Errors
	/// Fails if the server lacks an ssh user or pinned host key, or the key cannot be
	/// decrypted.
	pub fn for_server(
		server: &Server,
		sealed_key: &[u8],
		secrets: &SecretBox,
	) -> Result<Self, TunnelError> {
		let (Some(user), Some(host_key)) = (server.ssh_user.as_ref(), server.host_key.as_ref())
		else {
			return Err(TunnelError::MissingCredentials);
		};
		let private_key = secrets.decrypt(sealed_key).map_err(|e| {
			tracing::warn!(server_id = %server.id, error = %e, "ssh key decryption failed");
			TunnelError::Decrypt
		})?;
		let port = u16::try_from(server.ssh_port).map_err(|_| TunnelError::MissingCredentials)?;
		Ok(Self {
			host: server.hostname.clone(),
			port,
			user: user.clone(),
			private_key: Zeroizing::new(private_key),
			host_key: host_key.clone(),
		})
	}

	/// The same host and user, logged in with another key.
	#[must_use]
	pub fn with_private_key(&self, private_key: Zeroizing<String>) -> Self {
		Self {
			host: self.host.clone(),
			port: self.port,
			user: self.user.clone(),
			private_key,
			host_key: self.host_key.clone(),
		}
	}

	/// Writes the key and `known_hosts` into a fresh temp dir. The dir must
	/// outlive every ssh process using these files.
	fn write_files(&self) -> Result<SshFiles, TunnelError> {
		let dir = tempfile::tempdir()?;
		let key = dir.path().join("id_key");
		write_private_key(&key, &self.private_key)?;
		let known_hosts = dir.path().join("known_hosts");
		let mut line = self.host_key.trim_end().to_string();
		line.push('\n');
		std::fs::write(&known_hosts, line)?;
		Ok(SshFiles {
			dir,
			key,
			known_hosts,
		})
	}

	fn user_at_host(&self) -> String {
		format!("{}@{}", self.user, self.host)
	}
}

struct SshFiles {
	dir: TempDir,
	key: PathBuf,
	known_hosts: PathBuf,
}

impl SshFiles {
	fn command(&self, target: &SshTarget) -> Command {
		let mut cmd = Command::new("ssh");
		cmd.arg("-o")
			.arg("BatchMode=yes")
			.arg("-o")
			.arg("StrictHostKeyChecking=yes")
			.arg("-o")
			.arg(format!("UserKnownHostsFile={}", self.known_hosts.display()))
			.arg("-o")
			.arg("IdentitiesOnly=yes")
			.arg("-o")
			.arg("ConnectTimeout=10")
			.arg("-i")
			.arg(&self.key)
			.arg("-p")
			.arg(target.port.to_string());
		cmd
	}
}

/// A live SSH tunnel forwarding a local unix socket to a remote unix socket
/// (the remote agent's), via OpenSSH's unix-domain local forwarding:
/// `ssh -L local_sock:remote_sock host`.
///
/// Shells out to the system `ssh` binary: streamlocal forwarding is exactly
/// this use case, and OpenSSH's implementation is far better audited than a
/// hand-rolled client would be.
///
/// Dropping this kills the `ssh` process and removes the temp dir holding the
/// private key and local socket.
pub struct SshTunnel {
	child: Mutex<Child>,
	_dir: TempDir,
	local_socket_path: PathBuf,
}

impl SshTunnel {
	/// Opens a tunnel and waits for it to come up. That wait doubles as the
	/// connectivity, auth and host-key probe.
	///
	/// # Errors
	/// Fails if `ssh` cannot be spawned, exits early (auth failure, host key
	/// mismatch, unreachable host, bad remote socket path; its stderr is
	/// included) or the socket does not appear within 10 s.
	pub async fn open(target: &SshTarget, remote_socket: &str) -> Result<Self, TunnelError> {
		let files = target.write_files()?;
		let local_socket_path = files.dir.path().join("agent.sock");

		let mut child = files
			.command(target)
			.arg("-N")
			.arg("-o")
			.arg("ExitOnForwardFailure=yes")
			.arg("-o")
			.arg("ServerAliveInterval=15")
			.arg("-o")
			.arg("ServerAliveCountMax=3")
			.arg("-L")
			.arg(format!("{}:{remote_socket}", local_socket_path.display()))
			.arg("--")
			.arg(target.user_at_host())
			.stdin(Stdio::null())
			.stdout(Stdio::null())
			.stderr(Stdio::piped())
			.kill_on_drop(true)
			.spawn()?;

		wait_for_socket_or_exit(&mut child, &local_socket_path).await?;

		Ok(Self {
			child: Mutex::new(child),
			_dir: files.dir,
			local_socket_path,
		})
	}

	/// Path to the local unix socket the remote one is forwarded to.
	#[must_use]
	pub fn local_socket_path(&self) -> &Path {
		&self.local_socket_path
	}

	/// `true` while the underlying `ssh` process has not exited.
	pub fn is_alive(&self) -> bool {
		let mut child = self
			.child
			.lock()
			.unwrap_or_else(std::sync::PoisonError::into_inner);
		matches!(child.try_wait(), Ok(None))
	}
}

/// ssh hands the command string to the user's login shell, which may not be
/// POSIX (fish). Base64 plus a single-quoted `sh -c` contains no characters any
/// common shell treats specially, so `sh` receives the command verbatim.
fn posix_wrap(cmd: &str) -> String {
	use base64::Engine;
	let b64 = base64::engine::general_purpose::STANDARD.encode(cmd);
	format!("sh -c 'eval \"$(printf %s {b64} | base64 -d)\"'")
}

/// Runs `remote_cmd` on the target with the pinned host key. A non-zero exit
/// status is returned in the `Output`, not as an error.
///
/// # Errors
/// Fails if the key files cannot be written or `ssh` cannot be spawned.
pub async fn ssh_exec(
	target: &SshTarget,
	remote_cmd: &str,
	stdin: Option<Stdio>,
) -> Result<Output, TunnelError> {
	let files = target.write_files()?;
	let output = files
		.command(target)
		.arg("--")
		.arg(target.user_at_host())
		.arg(posix_wrap(remote_cmd))
		.stdin(stdin.unwrap_or_else(Stdio::null))
		.stdout(Stdio::piped())
		.stderr(Stdio::piped())
		.kill_on_drop(true)
		.output()
		.await?;
	Ok(output)
}

/// Scans the host's public keys with `ssh-keyscan` and returns `known_hosts`
/// lines to pin.
///
/// # Errors
/// Fails if `ssh-keyscan` cannot run or finds no key.
pub async fn keyscan(host: &str, port: u16) -> Result<String, TunnelError> {
	let output = Command::new("ssh-keyscan")
		.arg("-T")
		.arg("10")
		.arg("-p")
		.arg(port.to_string())
		.arg("--")
		.arg(host)
		.stdin(Stdio::null())
		.kill_on_drop(true)
		.output()
		.await?;
	let stdout = String::from_utf8_lossy(&output.stdout);
	let keys = stdout.trim();
	if keys.is_empty() {
		return Err(TunnelError::NoHostKey);
	}
	Ok(keys.to_string())
}

fn write_private_key(path: &Path, pem: &str) -> Result<(), std::io::Error> {
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

async fn wait_for_socket_or_exit(child: &mut Child, socket_path: &Path) -> Result<(), TunnelError> {
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
			return Err(TunnelError::Ssh(format!(
				"ssh exited with {status}: {}",
				stderr.trim()
			)));
		}

		if tokio::time::Instant::now() >= deadline {
			let _ = child.start_kill();
			return Err(TunnelError::Timeout);
		}

		tokio::time::sleep(Duration::from_millis(100)).await;
	}
}

#[cfg(test)]
mod tests {
	use base64::Engine;

	use super::posix_wrap;

	#[test]
	fn posix_wrap_is_shell_neutral_and_roundtrips() {
		let cmd = "d=${XDG_RUNTIME_DIR:-/tmp}/$(id -un) || echo 'x' \\ \"y\"";
		let wrapped = posix_wrap(cmd);
		assert!(!wrapped.contains('\\'));
		assert_eq!(wrapped.matches('\'').count(), 2);
		let b64 = wrapped
			.split("printf %s ")
			.nth(1)
			.and_then(|s| s.split(' ').next())
			.unwrap();
		let decoded = base64::engine::general_purpose::STANDARD
			.decode(b64)
			.unwrap();
		assert_eq!(decoded, cmd.as_bytes());
	}
}
