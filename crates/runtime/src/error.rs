use thiserror::Error as ThisError;

/// Everything that can go wrong talking to a container runtime, whether
/// that's podman itself, the ssh tunnel to a remote server, or one of the
/// build strategies (git clone, nixpacks).
#[derive(ThisError, Debug)]
pub enum RuntimeError {
	/// Bubbled up straight from the podman/docker API client.
	#[error("{0}")]
	Podman(#[from] bollard::errors::Error),

	/// Any filesystem or process I/O failure (reading a socket, spawning a
	/// subprocess, and so on).
	#[error("{0}")]
	Io(#[from] std::io::Error),

	/// Building a tar archive for the build context failed.
	#[error("{0}")]
	Tar(String),

	/// Couldn't figure out which uid owns the local podman socket, so we
	/// have nothing to connect to.
	#[error("no runtime uid could be resolved for the default local podman socket")]
	NoLocalSocket,

	/// Opening or maintaining the ssh tunnel to a remote server failed.
	#[error("ssh tunnel failed: {0}")]
	Tunnel(String),

	/// A remote server has no ssh user/key on file, so we can't tunnel to it.
	#[error("remote server is missing ssh credentials")]
	MissingSshCredentials,

	/// Cloning or otherwise interacting with a git repository failed.
	#[error("git: {0}")]
	Git(String),

	/// Running `nixpacks` to generate a build failed.
	#[error("nixpacks: {0}")]
	Nixpacks(String),
}
