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
const USAGE: &str =
	"usage: podkit-agent [--data-dir <dir>] [--podman-socket <path>] [--socket <path>] [--version]";

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

async fn run() -> Result<(), Box<dyn std::error::Error>> {
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
	for sub in ["work", "backups", "traefik-acme"] {
		create_private_dir(&args.data_dir.join(sub))?;
	}
	sweep_backups(&args.data_dir.join("backups"));

	let podman = Podman::connect(&args.podman_socket)?;
	let supervisor = Supervisor::start(podman.clone(), &args.data_dir).await;
	let ctx = Arc::new(Ctx {
		docker: podman.docker().clone(),
		data_dir: args.data_dir,
		podman_socket: args.podman_socket,
		supervisor,
		http: reqwest::Client::builder()
			.connect_timeout(Duration::from_secs(30))
			.build()?,
	});

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

#[tokio::main]
async fn main() -> ExitCode {
	match run().await {
		Ok(()) => ExitCode::SUCCESS,
		Err(e) => {
			eprintln!("podkit-agent: {e}");
			ExitCode::FAILURE
		}
	}
}
