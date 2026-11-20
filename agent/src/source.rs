//! Turns a git repository into a build context tar.

use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::Stdio;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use proto::{Build, GitAuth};
use tokio::process::Command;

use crate::nixpacks;

/// Largest build context the agent accepts.
pub const MAX_CONTEXT_BYTES: u64 = 1 << 30;

/// Failure while fetching or packaging source.
#[derive(Debug, thiserror::Error)]
pub enum SourceError {
	/// Local file access failed.
	#[error("{0}")]
	Io(#[from] io::Error),
	/// Git, nixpacks or validation failed; the message is safe to show users.
	#[error("{0}")]
	Failed(String),
}

/// A cloned repo packaged for `Podman::build_image`.
#[derive(Debug)]
pub struct Prepared {
	/// The build context tar, inside the work dir.
	pub context_tar: PathBuf,
	/// The commit that was checked out.
	pub commit_sha: String,
	/// Dockerfile path relative to the context.
	pub dockerfile: String,
}

/// Clones `repo_url` at `git_ref` (a branch or tag) into `<work>/src`, resolves
/// the build file, strips `.git` and tars the tree to `<work>/context.tar`.
///
/// `data_dir/known_hosts` persists the SSH host keys of git servers
/// (trust on first use).
///
/// # Errors
/// The clone fails, the Dockerfile is missing or its path is unsafe, nixpacks
/// fails, or the context exceeds [`MAX_CONTEXT_BYTES`].
pub async fn prepare(
	work: &Path,
	data_dir: &Path,
	repo_url: &str,
	git_ref: &str,
	auth: &GitAuth,
	build: &Build,
) -> Result<Prepared, SourceError> {
	let src = work.join("src");
	clone(work, data_dir, repo_url, git_ref, auth, &src).await?;
	let commit_sha = resolve_head(&src).await?;

	let dockerfile = match build {
		Build::Dockerfile { path } => {
			if !rel_path(path) {
				return Err(SourceError::Failed(format!(
					"invalid Dockerfile path {path:?}"
				)));
			}
			if !src.join(path).is_file() {
				return Err(SourceError::Failed(format!(
					"{path} not found in cloned repo at {git_ref}"
				)));
			}
			path.clone()
		}
		Build::Nixpacks => {
			nixpacks::generate_plan(&src)
				.await
				.map_err(SourceError::Failed)?;
			if !src.join(nixpacks::GENERATED_DOCKERFILE_PATH).is_file() {
				return Err(SourceError::Failed(
					"nixpacks reported success but wrote no Dockerfile".to_string(),
				));
			}
			nixpacks::GENERATED_DOCKERFILE_PATH.to_string()
		}
	};

	tokio::fs::remove_dir_all(src.join(".git")).await?;

	let context_tar = work.join("context.tar");
	let (tar_src, tar_out) = (src, context_tar.clone());
	tokio::task::spawn_blocking(move || tar_directory(&tar_src, &tar_out, MAX_CONTEXT_BYTES))
		.await
		.map_err(|e| SourceError::Failed(format!("tar task failed: {e}")))?
		.map_err(|e| SourceError::Failed(format!("failed to tar build context: {e}")))?;

	Ok(Prepared {
		context_tar,
		commit_sha,
		dockerfile,
	})
}

async fn clone(
	work: &Path,
	data_dir: &Path,
	repo_url: &str,
	git_ref: &str,
	auth: &GitAuth,
	dest: &Path,
) -> Result<(), SourceError> {
	let mut cmd = Command::new("git");
	cmd.arg("clone")
		.arg("--depth")
		.arg("1")
		.arg("--branch")
		.arg(git_ref)
		.arg("--")
		.arg(repo_url)
		.arg(dest)
		.env("GIT_TERMINAL_PROMPT", "0")
		.stdin(Stdio::null())
		.stdout(Stdio::null())
		.stderr(Stdio::piped())
		.kill_on_drop(true);

	let mut secrets: Vec<String> = Vec::new();
	match auth {
		GitAuth::None => {}
		GitAuth::SshKey { private_key } => {
			let key_path = work.join("deploy_key");
			write_private_key(&key_path, private_key)?;
			let known_hosts = data_dir.join("known_hosts");
			cmd.env(
				"GIT_SSH_COMMAND",
				format!(
					"ssh -i {} -o IdentitiesOnly=yes -o BatchMode=yes -o StrictHostKeyChecking=accept-new -o UserKnownHostsFile={}",
					sh_quote(&key_path.to_string_lossy()),
					sh_quote(&known_hosts.to_string_lossy()),
				),
			);
		}
		GitAuth::HttpsToken { token, username } => {
			let basic = STANDARD.encode(format!(
				"{}:{token}",
				username.as_deref().unwrap_or("x-access-token")
			));
			cmd.env("GIT_CONFIG_COUNT", "1")
				.env("GIT_CONFIG_KEY_0", "http.extraHeader")
				.env(
					"GIT_CONFIG_VALUE_0",
					format!("Authorization: Basic {basic}"),
				);
			secrets.push(token.clone());
			secrets.push(basic);
		}
	}

	let output = cmd.output().await.map_err(|e| {
		SourceError::Failed(format!(
			"failed to run git: {}",
			redact(&e.to_string(), &secrets)
		))
	})?;
	if !output.status.success() {
		let stderr = redact(String::from_utf8_lossy(&output.stderr).trim(), &secrets);
		return Err(SourceError::Failed(format!("git clone failed: {stderr}")));
	}
	Ok(())
}

async fn resolve_head(repo_dir: &Path) -> Result<String, SourceError> {
	let output = Command::new("git")
		.arg("-C")
		.arg(repo_dir)
		.arg("rev-parse")
		.arg("HEAD")
		.stdin(Stdio::null())
		.kill_on_drop(true)
		.output()
		.await
		.map_err(|e| SourceError::Failed(format!("failed to run git: {e}")))?;

	if !output.status.success() {
		return Err(SourceError::Failed(format!(
			"git rev-parse HEAD failed: {}",
			String::from_utf8_lossy(&output.stderr).trim()
		)));
	}
	Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn write_private_key(path: &Path, pem: &str) -> io::Result<()> {
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

/// Non-empty relative path with no `..` component.
fn rel_path(s: &str) -> bool {
	!s.is_empty() && !s.starts_with('/') && s.split('/').all(|part| part != "..")
}

/// Quotes `s` for `sh` (git runs `GIT_SSH_COMMAND` through a shell).
fn sh_quote(s: &str) -> String {
	format!("'{}'", s.replace('\'', r"'\''"))
}

fn redact<S: AsRef<str>>(text: &str, secrets: &[S]) -> String {
	secrets
		.iter()
		.map(|s| -> &str { s.as_ref() })
		.filter(|s| !s.is_empty())
		.fold(text.to_string(), |acc, s| acc.replace(s, "***"))
}

/// Errors once more than `limit` bytes would be written.
struct LimitWriter<W> {
	inner: W,
	written: u64,
	limit: u64,
}

impl<W: Write> Write for LimitWriter<W> {
	fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
		if self.written + buf.len() as u64 > self.limit {
			return Err(io::Error::other(format!(
				"context exceeds the {} MiB limit",
				self.limit >> 20
			)));
		}
		let n = self.inner.write(buf)?;
		self.written += n as u64;
		Ok(n)
	}

	fn flush(&mut self) -> io::Result<()> {
		self.inner.flush()
	}
}

/// Symlinks are archived as links, never followed: a repo must not be able to
/// pull host files into its own build context.
fn tar_directory(dir: &Path, out: &Path, limit: u64) -> io::Result<()> {
	let file = std::fs::File::create(out)?;
	let mut builder = tar::Builder::new(LimitWriter {
		inner: BufWriter::new(file),
		written: 0,
		limit,
	});
	builder.follow_symlinks(false);
	builder.append_dir_all(".", dir)?;
	builder.into_inner()?.flush()
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn rel_path_cases() {
		for (input, want) in [
			("Dockerfile", true),
			("docker/Dockerfile", true),
			("", false),
			("/etc/passwd", false),
			("../Dockerfile", false),
			("a/../../b", false),
		] {
			assert_eq!(rel_path(input), want, "{input:?}");
		}
	}

	#[test]
	fn redact_hides_every_secret() {
		let out = redact("fatal: bad tok123 and Zm9v", &["tok123", "Zm9v", ""]);
		assert_eq!(out, "fatal: bad *** and ***");
	}

	#[test]
	fn sh_quote_escapes_single_quotes() {
		assert_eq!(sh_quote("a b"), "'a b'");
		assert_eq!(sh_quote("it's"), r"'it'\''s'");
	}

	#[test]
	fn tar_over_limit_fails() {
		let dir = tempfile::tempdir().unwrap();
		std::fs::write(dir.path().join("big"), vec![0u8; 4096]).unwrap();
		let out = dir.path().join("out.tar");
		let err = tar_directory(dir.path(), &out, 1024).unwrap_err();
		assert!(err.to_string().contains("limit"), "{err}");
	}
}
