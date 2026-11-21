//! `podkit-firewall --uid <agent uid> [--socket PATH] [--state-dir DIR] [--nft PATH]`
//!
//! Runs as root under systemd (see `docs/firewall.md`). It binds the socket, gives it to the
//! agent's user, puts the machine in the last confirmed state and serves.

use std::os::unix::fs::{PermissionsExt as _, chown};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::{Arc, Mutex};

use podkit_firewall::daemon::{run_timer, serve};
use podkit_firewall::engine::Engine;
use podkit_firewall::host::SystemHost;
use tokio::net::UnixListener;

struct Args {
	uid: u32,
	socket: PathBuf,
	state_dir: PathBuf,
	nft: PathBuf,
}

fn parse_args() -> Result<Args, String> {
	let mut uid = None;
	let mut socket = PathBuf::from("/run/podkit-firewall/firewall.sock");
	let mut state_dir = PathBuf::from("/var/lib/podkit-firewall");
	let mut nft = PathBuf::from("nft");
	let mut args = std::env::args().skip(1);
	while let Some(flag) = args.next() {
		let mut value = || args.next().ok_or(format!("{flag} needs a value"));
		match flag.as_str() {
			"--uid" => uid = Some(value()?.parse().map_err(|_| "--uid is a number")?),
			"--socket" => socket = value()?.into(),
			"--state-dir" => state_dir = value()?.into(),
			"--nft" => nft = value()?.into(),
			other => return Err(format!("unknown argument {other}")),
		}
	}
	Ok(Args {
		uid: uid.ok_or("--uid <the agent's user id> is required")?,
		socket,
		state_dir,
		nft,
	})
}

fn bind(args: &Args) -> std::io::Result<UnixListener> {
	if let Some(dir) = args.socket.parent() {
		std::fs::create_dir_all(dir)?;
	}
	std::fs::create_dir_all(&args.state_dir)?;
	std::fs::set_permissions(&args.state_dir, std::fs::Permissions::from_mode(0o700))?;
	match std::fs::remove_file(&args.socket) {
		Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e),
		_ => {}
	}
	let listener = UnixListener::bind(&args.socket)?;
	chown(&args.socket, Some(args.uid), None)?;
	std::fs::set_permissions(&args.socket, std::fs::Permissions::from_mode(0o600))?;
	Ok(listener)
}

/// `podkit-firewall ctl <socket> '<request json>'`: one request to a running helper, the
/// answer on stdout. For checking a host by hand and for the container test.
fn ctl(socket: &str, request: &str) -> ExitCode {
	use std::io::{Read as _, Write as _};
	let run = || -> std::io::Result<String> {
		let mut s = std::os::unix::net::UnixStream::connect(socket)?;
		s.write_all(request.as_bytes())?;
		s.write_all(b"\n")?;
		let mut out = String::new();
		s.read_to_string(&mut out)?;
		Ok(out)
	};
	match run() {
		Ok(out) => {
			print!("{out}");
			ExitCode::SUCCESS
		}
		Err(e) => {
			eprintln!("podkit-firewall ctl: {e}");
			ExitCode::FAILURE
		}
	}
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
	let cli: Vec<String> = std::env::args().collect();
	if let [_, cmd, socket, request] = cli.as_slice()
		&& cmd == "ctl"
	{
		return ctl(socket, request);
	}
	better_tracing::fmt().init();
	let args = match parse_args() {
		Ok(a) => a,
		Err(e) => {
			eprintln!("podkit-firewall: {e}");
			return ExitCode::from(2);
		}
	};
	let listener = match bind(&args) {
		Ok(l) => l,
		Err(e) => {
			eprintln!(
				"podkit-firewall: cannot set up {}: {e}",
				args.socket.display()
			);
			return ExitCode::FAILURE;
		}
	};
	let host = SystemHost::new(args.nft.clone(), args.state_dir.join("active.json"));
	let engine = Engine::new(host);
	if let Err(e) = engine.restore() {
		tracing::error!(error = %e, "could not restore the firewall state");
	}
	let engine = Arc::new(Mutex::new(engine));
	tracing::info!(socket = %args.socket.display(), uid = args.uid, "podkit-firewall ready");
	tokio::select! {
		() = serve(listener, Arc::clone(&engine), args.uid) => {}
		() = run_timer(engine) => {}
		_ = tokio::signal::ctrl_c() => {}
	}
	ExitCode::SUCCESS
}
