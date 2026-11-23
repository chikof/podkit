//! `podkit-agent` binary entrypoint.

use std::fs::Permissions;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use agent::podman::{self, Podman};
use agent::supervisor::Supervisor;
use agent::{Ctx, routes};
use tracing::{info, warn};

const BACKUP_MAX_AGE: Duration = Duration::from_hours(24);
const USAGE: &str = "usage: podkit-agent [--data-dir <dir>] [--podman-socket <path>] [--socket <path>] [--version]\n       podkit-agent --gateway [--gateway-dir <dir>] [--listen <addr>]\n       podkit-agent --mcp-bridge [--listen <addr>] [--path <path>] -- <command...>";

struct Args {
	data_dir: PathBuf,
	podman_socket: String,
	socket: PathBuf,
}

/// `None` after `--version`.
fn parse_args() -> Result<Option<Args>, String> {
	let (mut data_dir, mut podman_socket, mut socket) = (None, None, None);
	let mut args = std::env::args().skip(1);
	while let Some(arg) = args.next() {
		let (flag, inline) = match arg.split_once('=') {
			Some((flag, value)) => (flag.to_string(), Some(value.to_string())),
			None => (arg, None),
		};
		if flag == "--version" {
			println!("{}", env!("CARGO_PKG_VERSION"));
			return Ok(None);
		}
		let slot = match flag.as_str() {
			"--data-dir" => &mut data_dir,
			"--podman-socket" => &mut podman_socket,
			"--socket" => &mut socket,
			_ => return Err(format!("unknown argument {flag}\n{USAGE}")),
		};
		*slot = Some(
			inline
				.or_else(|| args.next())
				.ok_or_else(|| format!("{flag} needs a value\n{USAGE}"))?,
		);
	}

	let data_dir = match data_dir {
		Some(dir) => PathBuf::from(dir),
		None => std::env::var_os("HOME")
			.map(|home| PathBuf::from(home).join(".podkit"))
			.ok_or("HOME is not set; pass --data-dir")?,
	};
	Ok(Some(Args {
		podman_socket: podman_socket.unwrap_or_else(podman::local_socket_path),
		socket: socket.map_or_else(|| data_dir.join("agent.sock"), PathBuf::from),
		data_dir,
	}))
}

fn create_private_dir(path: &Path) -> std::io::Result<()> {
	std::fs::DirBuilder::new()
		.recursive(true)
		.mode(0o700)
		.create(path)?;
	std::fs::set_permissions(path, Permissions::from_mode(0o700))
}

/// Dumps are uploaded right after they are written; anything older was orphaned.
fn sweep_backups(dir: &Path) {
	let Ok(entries) = std::fs::read_dir(dir) else {
		return;
	};
	let Some(cutoff) = SystemTime::now().checked_sub(BACKUP_MAX_AGE) else {
		return;
	};
	for entry in entries.flatten() {
		let stale = entry
			.metadata()
			.and_then(|m| m.modified())
			.is_ok_and(|modified| modified < cutoff);
		if !stale {
			continue;
		}
		let path = entry.path();
		let removed = if path.is_dir() {
			std::fs::remove_dir_all(&path)
		} else {
			std::fs::remove_file(&path)
		};
		match removed {
			Ok(()) => info!(path = %path.display(), "removed stale backup file"),
			Err(e) => {
				warn!(path = %path.display(), error = %e, "removing stale backup file failed");
			}
		}
	}
}

async fn shutdown_signal() {
	use tokio::signal::unix::{SignalKind, signal};

	let mut term = signal(SignalKind::terminate()).expect("install SIGTERM handler");
	tokio::select! {
		_ = term.recv() => {}
		_ = tokio::signal::ctrl_c() => {}
	}
}

/// `--gateway [--gateway-dir <dir>] [--listen <addr>]`: the LLM gateway sidecar,
/// which is this same binary run in a container. `None` for a normal start.
fn gateway_args() -> Option<Result<(PathBuf, String), String>> {
	let mut args = std::env::args().skip(1).peekable();
	args.peek()?;
	let all: Vec<String> = args.collect();
	if !all.iter().any(|a| a == "--gateway") {
		return None;
	}
	let (mut dir, mut listen) = (None, None);
	let mut rest = all.into_iter();
	while let Some(arg) = rest.next() {
		let (flag, inline) = match arg.split_once('=') {
			Some((flag, value)) => (flag.to_owned(), Some(value.to_owned())),
			None => (arg, None),
		};
		let slot = match flag.as_str() {
			"--gateway" => continue,
			"--gateway-dir" => &mut dir,
			"--listen" => &mut listen,
			_ => return Some(Err(format!("unknown argument {flag}\n{USAGE}"))),
		};
		match inline.or_else(|| rest.next()) {
			Some(value) => *slot = Some(value),
			None => return Some(Err(format!("{flag} needs a value\n{USAGE}"))),
		}
	}
	Some(Ok((
		dir.map_or_else(|| PathBuf::from("/gw"), PathBuf::from),
		listen.unwrap_or_else(|| format!("0.0.0.0:{}", proto::GATEWAY_PORT)),
	)))
}

/// `--mcp-bridge [--listen <addr>] [--path <path>] -- <command...>`: a stdio MCP
/// server behind streamable HTTP, run in the server's own container. It is
/// recognised by being the first argument, so a command that itself contains
/// `--gateway` is not mistaken for the sidecar.
#[allow(clippy::type_complexity)]
fn mcp_bridge_args() -> Option<Result<(String, String, Vec<String>), String>> {
	let mut args = std::env::args().skip(1);
	if args.next()? != "--mcp-bridge" {
		return None;
	}
	let (mut listen, mut path) = (None, None);
	while let Some(arg) = args.next() {
		if arg == "--" {
			let command: Vec<String> = args.collect();
			if command.is_empty() {
				return Some(Err(format!(
					"--mcp-bridge needs a command after --\n{USAGE}"
				)));
			}
			return Some(Ok((
				listen.unwrap_or_else(|| "0.0.0.0:8080".to_owned()),
				path.unwrap_or_else(|| "/mcp".to_owned()),
				command,
			)));
		}
		let (flag, inline) = match arg.split_once('=') {
			Some((flag, value)) => (flag.to_owned(), Some(value.to_owned())),
			None => (arg, None),
		};
		let slot = match flag.as_str() {
			"--listen" => &mut listen,
			"--path" => &mut path,
			_ => return Some(Err(format!("unknown argument {flag}\n{USAGE}"))),
		};
		match inline.or_else(|| args.next()) {
			Some(value) => *slot = Some(value),
			None => return Some(Err(format!("{flag} needs a value\n{USAGE}"))),
		}
	}
	Some(Err(format!("--mcp-bridge needs -- and a command\n{USAGE}")))
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
	// A container's health check is this binary: one request, then exit.
	let args: Vec<String> = std::env::args().skip(1).collect();
	if let Some(code) = agent::health::probe_cli(&args) {
		std::process::exit(code);
	}
	if let Some(bridge) = mcp_bridge_args() {
		let (listen, path, command) = bridge?;
		better_tracing::fmt().init();
		return Ok(agent::mcp_bridge::serve(&listen, &path, &command).await?);
	}
	if let Some(gateway) = gateway_args() {
		let (dir, listen) = gateway?;
		better_tracing::fmt().init();
		// The gateway never speaks TLS, but its HTTP client needs a provider to be built.
		let _ = rustls::crypto::ring::default_provider().install_default();
		return agent::gateway::sidecar::serve(dir, &listen).await;
	}
	let Some(args) = parse_args()? else {
		return Ok(());
	};

	better_tracing::fmt().init();
	if rustls::crypto::ring::default_provider()
		.install_default()
		.is_err()
	{
		warn!("a rustls crypto provider was already installed");
	}

	create_private_dir(&args.data_dir)?;
	for sub in ["work", "backups", "traefik-acme", "gateway"] {
		create_private_dir(&args.data_dir.join(sub))?;
	}
	sweep_backups(&args.data_dir.join("backups"));

	let podman = Podman::connect(&args.podman_socket)?;
	agent::scan::sweep(&podman, &args.data_dir).await;
	let supervisor = Supervisor::start(podman.clone(), &args.data_dir).await;
	let gateway_dir = agent::gateway::sidecar::dir(&args.data_dir);
	let ctx = Arc::new(Ctx {
		docker: podman.docker().clone(),
		data_dir: args.data_dir,
		podman_socket: args.podman_socket,
		supervisor,
		http: reqwest::Client::builder()
			.connect_timeout(Duration::from_secs(30))
			.build()?,
	});

	// Follows the containers' logs into the log store (retention, search, export).
	agent::logstore::collector::spawn(ctx.podman(), agent::logstore::for_dir(&ctx.data_dir));

	// A sidecar from before a restart or an upgrade is replaced by one of this binary.
	if let Some(config) = agent::gateway::sidecar::read_config(&gateway_dir)
		.await
		.filter(|c| !c.keys.is_empty() || !c.mcp_keys.is_empty())
	{
		let podman = ctx.podman();
		if let Err(e) =
			agent::gateway::sidecar::ensure_running(&podman, &ctx.supervisor, &ctx.data_dir).await
		{
			warn!(error = %e, "starting the gateway sidecar failed");
		} else if let Err(e) =
			agent::gateway::sidecar::sync_networks(&podman, &config.networks).await
		{
			warn!(error = %e, "joining the gateway sidecar to its networks failed");
		}
	}

	match std::fs::remove_file(&args.socket) {
		Ok(()) => {}
		Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
		Err(e) => return Err(e.into()),
	}
	let listener = tokio::net::UnixListener::bind(&args.socket)?;
	std::fs::set_permissions(&args.socket, Permissions::from_mode(0o600))?;
	info!(socket = %args.socket.display(), version = env!("CARGO_PKG_VERSION"), "agent listening");

	axum::serve(listener, routes::router(ctx))
		.with_graceful_shutdown(shutdown_signal())
		.await?;

	if let Err(e) = std::fs::remove_file(&args.socket) {
		warn!(error = %e, "removing the socket failed");
	}
	Ok(())
}

/// The agent waits on Podman and the network almost all of the time: two
/// workers are plenty and keep its idle footprint small.
fn main() -> ExitCode {
	let mut builder = tokio::runtime::Builder::new_multi_thread();
	builder.enable_all();
	if std::env::var_os("TOKIO_WORKER_THREADS").is_none() {
		builder.worker_threads(2);
	}
	let runtime = match builder.build() {
		Ok(runtime) => runtime,
		Err(e) => {
			eprintln!("podkit-agent: {e}");
			return ExitCode::FAILURE;
		}
	};
	match runtime.block_on(run()) {
		Ok(()) => ExitCode::SUCCESS,
		Err(e) => {
			eprintln!("podkit-agent: {e}");
			ExitCode::FAILURE
		}
	}
}
