//! The `ServerBootstrap` job: installs or upgrades `podkit-agent` on a remote
//! server over ssh and waits until it answers with this build's version.

use std::io::{Seek, Write};
use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};
use std::time::Duration;

use proto::Stream;
use tokio_util::sync::CancellationToken;

use super::{Server, host_key_fingerprint};
use crate::agents::tunnel::{self, SshTarget};
use crate::ids::ServerId;
use crate::jobs::{self, Job, JobFailure, JobKind};
use crate::logs::JobLog;
use crate::state::AppState;
use crate::validate;

const AGENT_VERSION: &str = env!("CARGO_PKG_VERSION");
const HEALTH_ATTEMPTS: u32 = 20;
const HEALTH_INTERVAL: Duration = Duration::from_millis(500);

const PROBE_CMD: &str = "uname -m; printf '%s\\n' \"$HOME\"; printf 'path=%s\\n' \"$PATH\"; command -v podman >/dev/null 2>&1 && echo podman=ok || echo podman=missing; command -v git >/dev/null 2>&1 && echo git=ok || echo git=missing";

const UPLOAD_CMD: &str = "mkdir -p \"$HOME/.podkit/bin\" && cat > \"$HOME/.podkit/bin/podkit-agent.new\" && chmod 755 \"$HOME/.podkit/bin/podkit-agent.new\" && mv -f \"$HOME/.podkit/bin/podkit-agent.new\" \"$HOME/.podkit/bin/podkit-agent\"";

// `systemctl --user` over a non-login ssh session has no XDG_RUNTIME_DIR unless
// pam_systemd set it, so fall back to the conventional path.
const INSTALL_USER_CMD: &str = "export XDG_RUNTIME_DIR=\"${XDG_RUNTIME_DIR:-/run/user/$(id -u)}\" && mkdir -p \"$HOME/.config/systemd/user\" && cat > \"$HOME/.config/systemd/user/podkit-agent.service\" && systemctl --user daemon-reload && systemctl --user enable --now podman.socket && systemctl --user enable podkit-agent.service && systemctl --user restart podkit-agent.service && (loginctl enable-linger \"$(id -un)\" || true)";

const INSTALL_ROOT_CMD: &str = "cat > /etc/systemd/system/podkit-agent.service && systemctl daemon-reload && systemctl enable --now podman.socket && systemctl enable podkit-agent.service && systemctl restart podkit-agent.service";

fn failed(message: impl Into<String>) -> JobFailure {
	JobFailure::Failed(message.into())
}

fn db_failed(e: &sqlx::Error) -> JobFailure {
	tracing::error!(error = %e, "server bootstrap database error");
	failed("database error")
}

/// Runs `fut` unless the job is cancelled first. Dropping the future kills any
/// ssh child it owns (`kill_on_drop`).
async fn cancellable<T>(
	cancel: &CancellationToken,
	fut: impl Future<Output = T>,
) -> Result<T, JobFailure> {
	tokio::select! {
		() = cancel.cancelled() => Err(JobFailure::Cancelled),
		out = fut => Ok(out),
	}
}

fn stderr_summary(output: &Output) -> String {
	let stderr = String::from_utf8_lossy(&output.stderr);
	let stderr = stderr.trim();
	if stderr.is_empty() {
		return output.status.to_string();
	}
	stderr.chars().take(300).collect()
}

async fn exec(
	target: &SshTarget,
	cancel: &CancellationToken,
	what: &str,
	cmd: &str,
	stdin: Option<Stdio>,
) -> Result<Output, JobFailure> {
	let output = cancellable(cancel, tunnel::ssh_exec(target, cmd, stdin))
		.await?
		.map_err(|e| failed(e.to_string()))?;
	if !output.status.success() {
		return Err(failed(format!(
			"{what} failed: {}",
			stderr_summary(&output)
		)));
	}
	Ok(output)
}

struct Probe {
	arch: String,
	home: String,
	path: String,
}

/// Parses the output of [`PROBE_CMD`]. `arch` and `home` end up in file paths
/// and a stored socket path, and `path` is written verbatim into the systemd
/// unit, so all three are restricted to a safe alphabet.
fn parse_probe(stdout: &str) -> Result<Probe, String> {
	let mut lines = stdout.lines().map(str::trim);
	let arch = lines.next().unwrap_or_default();
	let home = lines.next().unwrap_or_default();
	let rest: Vec<&str> = lines.collect();

	if !rest.contains(&"podman=ok") {
		return Err("podman not found on server".to_string());
	}
	if !rest.contains(&"git=ok") {
		return Err("git not found on server".to_string());
	}
	if arch.is_empty() || !arch.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_') {
		return Err(format!("unsupported architecture {arch:?}"));
	}
	if !validate::abs_path(home) {
		return Err(format!("unsupported home directory {home:?}"));
	}
	let path = rest
		.iter()
		.find_map(|line| line.strip_prefix("path="))
		.unwrap_or_default();
	if path.is_empty() {
		return Err("remote PATH is empty".to_string());
	}
	if !path.bytes().all(is_path_byte) {
		return Err("remote PATH contains unsupported characters".to_string());
	}
	Ok(Probe {
		arch: arch.to_string(),
		home: home.to_string(),
		path: path.to_string(),
	})
}

fn is_path_byte(c: u8) -> bool {
	c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'/' | b':' | b'+' | b'-')
}

fn binary_candidates(dir: &Path, arch: &str) -> Vec<PathBuf> {
	let mut candidates = vec![dir.join(format!("podkit-agent-{arch}"))];
	if arch == std::env::consts::ARCH {
		candidates.push(dir.join("podkit-agent"));
	}
	candidates
}

async fn pick_binary(dir: &Path, arch: &str) -> Result<PathBuf, JobFailure> {
	for candidate in binary_candidates(dir, arch) {
		if tokio::fs::try_exists(&candidate).await.unwrap_or(false) {
			return Ok(candidate);
		}
	}
	Err(failed(format!(
		"no podkit-agent binary for {arch} in AGENT_BINARIES_DIR"
	)))
}

fn unit_file(podman_socket: &str, root: bool, path: &str) -> String {
	let wanted_by = if root {
		"multi-user.target"
	} else {
		"default.target"
	};
	format!(
		"[Unit]\n\
		Description=podkit agent\n\
		Wants=podman.socket\n\
		After=podman.socket\n\
		\n\
		[Service]\n\
		Environment=PATH={path}\n\
		ExecStart=%h/.podkit/bin/podkit-agent --data-dir %h/.podkit --podman-socket {podman_socket}\n\
		Restart=always\n\
		RestartSec=2\n\
		\n\
		[Install]\n\
		WantedBy={wanted_by}\n"
	)
}

fn stdin_from_bytes(bytes: &[u8]) -> std::io::Result<Stdio> {
	let mut file = tempfile::tempfile()?;
	file.write_all(bytes)?;
	file.rewind()?;
	Ok(Stdio::from(file))
}

async fn note(log: &mut JobLog, line: &str) {
	log.push(Stream::System, line).await;
}

pub async fn run(
	state: &AppState,
	job: &Job,
	cancel: &CancellationToken,
	log: &mut JobLog,
) -> Result<(), JobFailure> {
	let mut server = super::find(&state.db, job.server_id)
		.await
		.map_err(|e| db_failed(&e))?
		.ok_or_else(|| failed("server no longer exists"))?;
	if server.is_local {
		return Err(failed("local server agent is managed by the operator"));
	}

	pin_host_key(state, cancel, log, &mut server).await?;
	let target =
		SshTarget::for_server(&server, &state.secrets).map_err(|e| failed(e.to_string()))?;

	note(log, "probing server").await;
	let output = exec(&target, cancel, "probe", PROBE_CMD, None).await?;
	let probe = parse_probe(&String::from_utf8_lossy(&output.stdout)).map_err(failed)?;
	note(log, &format!("arch {}, home {}", probe.arch, probe.home)).await;

	let binary = pick_binary(&state.config.agent_binaries_dir, &probe.arch).await?;
	note(log, &format!("uploading {}", binary.display())).await;
	let file = std::fs::File::open(&binary)
		.map_err(|e| failed(format!("cannot read {}: {e}", binary.display())))?;
	exec(
		&target,
		cancel,
		"agent upload",
		UPLOAD_CMD,
		Some(Stdio::from(file)),
	)
	.await?;

	let root = server.ssh_user.as_deref() == Some("root");
	note(log, "installing systemd unit").await;
	let unit =
		stdin_from_bytes(unit_file(&server.podman_socket_path, root, &probe.path).as_bytes())
			.map_err(|e| failed(format!("io error: {e}")))?;
	let install_cmd = if root {
		INSTALL_ROOT_CMD
	} else {
		INSTALL_USER_CMD
	};
	exec(&target, cancel, "unit install", install_cmd, Some(unit)).await?;

	let socket = format!("{}/.podkit/agent.sock", probe.home);
	super::set_agent_socket_path(&state.db, server.id, &socket)
		.await
		.map_err(|e| db_failed(&e))?;
	server.agent_socket_path = Some(socket);
	// A cached tunnel still points at the previous socket path or process.
	state.agents.forget(server.id);

	note(log, "waiting for the agent").await;
	let version = await_agent(state, cancel, &server).await?;
	if version != AGENT_VERSION {
		return Err(failed(format!(
			"agent reports version {version}, expected {AGENT_VERSION}"
		)));
	}

	super::mark_bootstrapped(&state.db, server.id, &version, &probe.arch)
		.await
		.map_err(|e| db_failed(&e))?;
	note(log, &format!("agent {version} is running")).await;
	Ok(())
}

async fn pin_host_key(
	state: &AppState,
	cancel: &CancellationToken,
	log: &mut JobLog,
	server: &mut Server,
) -> Result<(), JobFailure> {
	if server.host_key.is_none() {
		note(log, "scanning host key").await;
		let port = u16::try_from(server.ssh_port).map_err(|_| failed("invalid ssh port"))?;
		let key = cancellable(cancel, tunnel::keyscan(&server.hostname, port))
			.await?
			.map_err(|e| failed(e.to_string()))?;
		super::set_host_key(&state.db, server.id, Some(&key))
			.await
			.map_err(|e| db_failed(&e))?;
		server.host_key = Some(key);
	}
	if let Some(fingerprint) = server.host_key.as_deref().and_then(host_key_fingerprint) {
		note(log, &format!("host key {fingerprint}")).await;
	}
	Ok(())
}

/// Polls the agent's health until it answers and returns its version.
async fn await_agent(
	state: &AppState,
	cancel: &CancellationToken,
	server: &Server,
) -> Result<String, JobFailure> {
	let mut last_error = String::new();
	for attempt in 0..HEALTH_ATTEMPTS {
		if attempt > 0 {
			cancellable(cancel, tokio::time::sleep(HEALTH_INTERVAL)).await?;
		}
		let health = async {
			let client = state.agents.client(state, server).await?;
			client.health().await
		};
		match cancellable(cancel, health).await? {
			Ok(health) => return Ok(health.version),
			Err(e) => last_error = e.to_string(),
		}
	}
	Err(failed(format!(
		"agent did not become healthy: {last_error}"
	)))
}

/// Enqueues a bootstrap for every remote server whose agent differs from this
/// build, which is how agents are upgraded.
pub async fn enqueue_outdated(state: &AppState) -> Result<(), sqlx::Error> {
	let ids = sqlx::query_scalar!(
		r#"
			SELECT id AS "id: ServerId" FROM servers
			WHERE NOT is_local AND agent_version IS DISTINCT FROM $1
		"#,
		AGENT_VERSION
	)
	.fetch_all(&state.db)
	.await?;

	for id in ids {
		if jobs::active_exists(&state.db, JobKind::ServerBootstrap, id.0).await? {
			continue;
		}
		jobs::enqueue(
			state,
			JobKind::ServerBootstrap,
			id,
			id.0,
			format!("server:{id}"),
		)
		.await?;
	}
	Ok(())
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn probe_parses_a_complete_host() {
		let p = parse_probe(
			"x86_64\n/home/deploy\npath=/run/wrappers/bin:/home/deploy/.nix-profile/bin\npodman=ok\ngit=ok\n",
		)
		.unwrap();
		assert_eq!(p.arch, "x86_64");
		assert_eq!(p.home, "/home/deploy");
		assert_eq!(p.path, "/run/wrappers/bin:/home/deploy/.nix-profile/bin");
	}

	#[test]
	fn probe_reports_missing_tools_before_anything_else() {
		assert_eq!(
			parse_probe("aarch64\n/root\npath=/usr/bin\npodman=missing\ngit=ok\n")
				.err()
				.as_deref(),
			Some("podman not found on server")
		);
		assert_eq!(
			parse_probe("aarch64\n/root\npath=/usr/bin\npodman=ok\ngit=missing\n")
				.err()
				.as_deref(),
			Some("git not found on server")
		);
	}

	#[test]
	fn probe_rejects_unsafe_arch_and_home() {
		assert!(parse_probe("x86 64\n/root\npath=/usr/bin\npodman=ok\ngit=ok\n").is_err());
		assert!(parse_probe("x86_64\n/home/my user\npath=/usr/bin\npodman=ok\ngit=ok\n").is_err());
		assert!(parse_probe("x86_64\n\npath=/usr/bin\npodman=ok\ngit=ok\n").is_err());
	}

	#[test]
	fn probe_rejects_empty_or_unsafe_path() {
		assert!(parse_probe("x86_64\n/root\npath=\npodman=ok\ngit=ok\n").is_err());
		assert!(parse_probe("x86_64\n/root\npodman=ok\ngit=ok\n").is_err());
		assert_eq!(
			parse_probe("x86_64\n/root\npath=/usr/bin:/my dir\npodman=ok\ngit=ok\n")
				.err()
				.as_deref(),
			Some("remote PATH contains unsupported characters")
		);
		assert!(parse_probe("x86_64\n/root\npath=/usr/bin$X\npodman=ok\ngit=ok\n").is_err());
	}

	#[test]
	fn binary_falls_back_to_the_unsuffixed_name_only_for_the_host_arch() {
		let dir = Path::new("/opt/podkit");
		let host = std::env::consts::ARCH;
		assert_eq!(
			binary_candidates(dir, host),
			vec![
				dir.join(format!("podkit-agent-{host}")),
				dir.join("podkit-agent")
			]
		);
		assert_eq!(
			binary_candidates(dir, "riscv64gc"),
			vec![dir.join("podkit-agent-riscv64gc")]
		);
	}

	#[test]
	fn unit_targets_match_the_install_scope() {
		let user = unit_file("/run/user/1000/podman/podman.sock", false, "/usr/bin:/bin");
		assert!(user.contains("WantedBy=default.target\n"));
		assert!(user.contains(
			"ExecStart=%h/.podkit/bin/podkit-agent --data-dir %h/.podkit --podman-socket /run/user/1000/podman/podman.sock\n"
		));
		let root = unit_file("/run/podman/podman.sock", true, "/usr/bin:/bin");
		assert!(root.contains("WantedBy=multi-user.target\n"));
		assert!(!root.contains("default.target"));
	}

	#[test]
	fn unit_sets_the_remote_path_before_exec_start() {
		let unit = unit_file(
			"/run/podman/podman.sock",
			false,
			"/usr/bin:/home/u/.nix-profile/bin",
		);
		let env = unit
			.find("Environment=PATH=/usr/bin:/home/u/.nix-profile/bin\n")
			.unwrap();
		assert!(unit.find("[Service]\n").unwrap() < env);
		assert!(env < unit.find("ExecStart=").unwrap());
	}

	#[test]
	fn default_socket_renders_systemd_specifier() {
		let unit = unit_file("%t/podman/podman.sock", false, "/usr/bin");
		assert!(unit.contains("--podman-socket %t/podman/podman.sock\n"));
	}
}
