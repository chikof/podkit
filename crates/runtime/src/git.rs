use std::path::Path;
use std::process::Stdio;

use podkit_core::domain::application::BuildStrategy;
use tokio::process::Command;

use crate::RuntimeError;
use crate::nixpacks;

/// A cloned repo, packaged as a build context ready for
/// `ContainerRuntime::build_image`.
#[derive(Debug)]
pub struct ClonedSource {
	/// The build context, tarred up and ready for `ContainerRuntime::build_image`.
	pub context_tar: Vec<u8>,
	/// The commit sha that was actually checked out.
	pub commit_sha: String,
	/// The path to hand `BuildSpec::dockerfile_path`: either the caller's
	/// `dockerfile_path` verbatim (`BuildStrategy::Dockerfile`) or
	/// [`nixpacks::GENERATED_DOCKERFILE_PATH`] (`BuildStrategy::Nixpacks`,
	/// written into the tree by this function before tarring).
	pub dockerfile_path: String,
}

/// Clones `repo_url` at `git_ref` (branch or tag, see note below). For
/// `BuildStrategy::Nixpacks`, also runs `nixpacks build` in place to
/// generate a Dockerfile before packing the working tree (minus `.git`)
/// into a tar build context. This has been proven end to end against a real
/// podman.sock in `crates/runtime/tests/nixpacks_spike.rs`: nixpacks'
/// generated `COPY . /app` assumes the build context is the app directory
/// itself, so it composes with the existing tar-the-whole-clone approach
/// unchanged, just with a different `dockerfile_path`.
///
/// Shells out to the system `git` binary for the same reason [`crate::SshTunnel`]
/// shells out to `ssh`: battle-tested auth handling (both ssh keys and https
/// tokens) beats reimplementing the git protocol.
///
/// `deploy_key_pem` selects the auth method by `repo_url`'s scheme:
/// - `git@host:...` / `ssh://...` -> used as an ssh private key via a
///   temporary `GIT_SSH_COMMAND`.
/// - `https://...` -> injected as a bearer token in the URL
///   (`https://<token>@host/...`).
/// - `None` -> no auth (public repo, or a local path / `file://` url).
///
/// `git_ref` is passed to `--branch`, so it must name a branch or tag, not
/// an arbitrary commit sha. Full-history clones for arbitrary sha refs would
/// be needed to support that, and aren't implemented yet.
///
/// For `BuildStrategy::Dockerfile`, `dockerfile_path` must exist in the
/// cloned tree. For `BuildStrategy::Nixpacks`, `dockerfile_path` is unused
/// since nixpacks always writes to a fixed path, and the `nixpacks` CLI must
/// be on `PATH`.
///
/// # Errors
/// Returns an error if the clone fails (bad ref, auth failure, unreachable
/// host), the Dockerfile strategy's `dockerfile_path` is missing from the
/// tree, nixpacks fails to detect/build a plan, or the tree can't be tarred.
pub async fn clone_to_tar(
	repo_url: &str,
	git_ref: &str,
	deploy_key_pem: Option<&str>,
	build_strategy: BuildStrategy,
	dockerfile_path: &str,
) -> Result<ClonedSource, RuntimeError> {
	let dir = tempfile::tempdir()?;
	let dest = dir.path().join("src");

	let (clone_url, ssh_key_path) = match deploy_key_pem {
		Some(pem) if is_ssh_url(repo_url) => {
			let key_path = dir.path().join("deploy_key");
			write_private_key(&key_path, pem)?;
			(repo_url.to_string(), Some(key_path))
		}
		Some(token) if repo_url.starts_with("https://") => (inject_token(repo_url, token), None),
		_ => (repo_url.to_string(), None),
	};

	let mut cmd = Command::new("git");
	cmd.arg("clone")
		.arg("--depth")
		.arg("1")
		.arg("--branch")
		.arg(git_ref)
		.arg(&clone_url)
		.arg(&dest)
		.stdin(Stdio::null())
		.stdout(Stdio::piped())
		.stderr(Stdio::piped());

	if let Some(key_path) = &ssh_key_path {
		let known_hosts = dir.path().join("known_hosts");
		cmd.env(
			"GIT_SSH_COMMAND",
			format!(
				"ssh -i {} -o StrictHostKeyChecking=accept-new -o UserKnownHostsFile={} -o BatchMode=yes",
				key_path.display(),
				known_hosts.display()
			),
		);
	}

	let output = cmd.output().await?;
	if !output.status.success() {
		return Err(RuntimeError::Git(format!(
			"git clone failed: {}",
			String::from_utf8_lossy(&output.stderr).trim()
		)));
	}

	let effective_dockerfile_path = match build_strategy {
		BuildStrategy::Dockerfile => {
			if !dest.join(dockerfile_path).is_file() {
				return Err(RuntimeError::Git(format!(
					"{dockerfile_path} not found in cloned repo at {git_ref}"
				)));
			}
			dockerfile_path.to_string()
		}
		BuildStrategy::Nixpacks => {
			nixpacks::generate_plan(&dest).await?;
			if !dest.join(nixpacks::GENERATED_DOCKERFILE_PATH).is_file() {
				return Err(RuntimeError::Nixpacks(
					"nixpacks reported success but wrote no Dockerfile".to_string(),
				));
			}
			nixpacks::GENERATED_DOCKERFILE_PATH.to_string()
		}
	};

	let commit_sha = resolve_head(&dest).await?;
	std::fs::remove_dir_all(dest.join(".git"))?;
	let context_tar = tar_directory(&dest)?;

	Ok(ClonedSource {
		context_tar,
		commit_sha,
		dockerfile_path: effective_dockerfile_path,
	})
}

fn is_ssh_url(repo_url: &str) -> bool {
	repo_url.starts_with("ssh://")
		|| (repo_url.contains('@') && repo_url.contains(':') && !repo_url.starts_with("http"))
}

fn inject_token(https_url: &str, token: &str) -> String {
	https_url.replacen("https://", &format!("https://{token}@"), 1)
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

async fn resolve_head(repo_dir: &Path) -> Result<String, RuntimeError> {
	let output = Command::new("git")
		.arg("-C")
		.arg(repo_dir)
		.arg("rev-parse")
		.arg("HEAD")
		.output()
		.await?;

	if !output.status.success() {
		return Err(RuntimeError::Git(format!(
			"git rev-parse HEAD failed: {}",
			String::from_utf8_lossy(&output.stderr).trim()
		)));
	}

	Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn tar_directory(dir: &Path) -> Result<Vec<u8>, RuntimeError> {
	let mut builder = tar::Builder::new(Vec::new());
	builder
		.append_dir_all(".", dir)
		.map_err(|e| RuntimeError::Git(format!("failed to tar build context: {e}")))?;
	builder
		.into_inner()
		.map_err(|e| RuntimeError::Git(format!("failed to finalize build context tar: {e}")))
}
