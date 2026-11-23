//! The health gate of a deploy: the check a container gets, the in-container probe that runs
//! it, and the wait that decides whether the new container may take over.
//!
//! Traefik's Docker provider leaves out a container whose Podman health is not `healthy`
//! ("Filtering unhealthy or starting container",
//! <https://github.com/traefik/traefik/blob/v3.6/pkg/provider/docker/config.go>), so giving the new
//! container a Podman healthcheck is what keeps traffic off it until it passes. An HTTP check
//! cannot rely on `curl` being in the image, so the check is this binary, bind-mounted
//! read-only and run as `podkit-agent --health-probe`.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::Path;
use std::time::{Duration, Instant};

use proto::{HealthCheck, HealthGate, HealthStatus, Healthcheck};
use tokio::sync::mpsc;

use crate::podman::{self, Podman};

/// Where the agent binary is mounted inside a gated container.
pub const PROBE_PATH: &str = "/.podkit/health";
/// The flag that turns the agent binary into a one-shot HTTP probe.
pub const PROBE_FLAG: &str = "--health-probe";
const POLL: Duration = Duration::from_millis(500);
const LOG_EVERY: Duration = Duration::from_secs(5);

/// The Podman healthcheck for `gate`.
#[must_use]
pub fn podman_check(gate: &HealthGate) -> Healthcheck {
	let test = match &gate.check {
		HealthCheck::Http { path, status, port } => vec![
			"CMD".to_owned(),
			PROBE_PATH.to_owned(),
			PROBE_FLAG.to_owned(),
			port.to_string(),
			path.clone(),
			status.to_string(),
			gate.timeout_ms.div_ceil(1000).max(1).to_string(),
		],
		HealthCheck::Command { command } => {
			vec!["CMD-SHELL".to_owned(), command.clone()]
		}
	};
	Healthcheck {
		test,
		interval_ms: Some(gate.interval_ms),
		timeout_ms: Some(gate.timeout_ms),
		retries: Some(gate.retries),
		start_period_ms: Some(gate.start_period_ms),
	}
}

/// Where the agent binary is when it is not this process: a host that runs the agent inside
/// another binary (the live tests do) names it once, before the first deploy.
static AGENT_EXE: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();

/// Names the binary a gated container's check runs. Only the first call counts.
pub fn set_agent_exe(path: std::path::PathBuf) {
	let _ = AGENT_EXE.set(path);
}

/// The agent binary as the host can bind-mount it: [`set_agent_exe`]'s, else this process's own.
///
/// # Errors
/// The binary cannot be found.
pub fn agent_exe() -> Result<std::path::PathBuf, String> {
	if let Some(path) = AGENT_EXE.get() {
		return Ok(path.clone());
	}
	let exe = std::env::current_exe()
		.map_err(|e| format!("cannot find the agent binary for the health check: {e}"))?;
	// A replaced binary is reported as "<path> (deleted)".
	Ok(std::path::PathBuf::from(
		exe.to_string_lossy().trim_end_matches(" (deleted)"),
	))
}

/// Host paths to bind into the container for `gate`'s check: the agent binary for an HTTP
/// check (plus the Nix store when a development build is linked against it).
#[must_use]
pub fn binds(gate: &HealthGate, exe: &Path) -> Vec<String> {
	if !matches!(gate.check, HealthCheck::Http { .. }) {
		return Vec::new();
	}
	let mut binds = vec![format!("{}:{PROBE_PATH}:ro", exe.display())];
	if crate::gateway::sidecar::needs_nix_store(exe) {
		binds.push("/nix/store:/nix/store:ro".to_owned());
	}
	binds
}

/// Rejects a gate the agent would not run: a path that is not a path, no port, a check that
/// can never finish in time.
///
/// # Errors
/// A message naming the bad field.
pub fn validate(gate: &HealthGate) -> Result<(), String> {
	match &gate.check {
		HealthCheck::Http { path, status, port } => {
			if !path.starts_with('/') || path.chars().any(|c| c.is_control() || c == ' ') {
				return Err(format!("invalid health check path {path:?}"));
			}
			if !(100..=599).contains(status) {
				return Err(format!("invalid health check status {status}"));
			}
			if *port == 0 {
				return Err("invalid health check port 0".to_owned());
			}
		}
		HealthCheck::Command { command } => {
			if command.trim().is_empty() || command.contains('\0') {
				return Err("the health check command is empty".to_owned());
			}
		}
	}
	if gate.interval_ms == 0 || gate.timeout_ms == 0 || gate.retries == 0 {
		return Err("health check interval, timeout and retries must be above zero".to_owned());
	}
	if gate.gate_timeout_s == 0 {
		return Err("the health gate timeout must be above zero".to_owned());
	}
	Ok(())
}

// --- the probe, run inside the container --------------------------------------------------

/// One `GET` over plain HTTP/1.0 to `127.0.0.1:port`, returning the status. Std only: the
/// binary runs in whatever image the application brought.
///
/// # Errors
/// The connection or the answer failed.
pub fn probe_http(port: u16, path: &str, timeout: Duration) -> Result<u16, String> {
	let addrs = [
		SocketAddr::from(([127, 0, 0, 1], port)),
		SocketAddr::from(([0, 0, 0, 0, 0, 0, 0, 1], port)),
	];
	let mut last = String::from("no address");
	for addr in addrs {
		match TcpStream::connect_timeout(&addr, timeout) {
			Ok(stream) => return request(stream, path, timeout),
			Err(e) => last = format!("connect {addr}: {e}"),
		}
	}
	Err(last)
}

fn request(mut stream: TcpStream, path: &str, timeout: Duration) -> Result<u16, String> {
	stream
		.set_read_timeout(Some(timeout))
		.and_then(|()| stream.set_write_timeout(Some(timeout)))
		.map_err(|e| e.to_string())?;
	let request = format!(
		"GET {path} HTTP/1.0\r\nHost: localhost\r\nUser-Agent: podkit-health\r\nConnection: close\r\n\r\n"
	);
	stream
		.write_all(request.as_bytes())
		.map_err(|e| format!("write: {e}"))?;
	let mut head = [0u8; 64];
	let mut filled = 0;
	while filled < head.len() && !head[..filled].contains(&b'\n') {
		match stream.read(&mut head[filled..]) {
			Ok(0) => break,
			Ok(n) => filled += n,
			Err(e) => return Err(format!("read: {e}")),
		}
	}
	parse_status(&head[..filled])
}

/// The status of `HTTP/1.x NNN ...`.
///
/// # Errors
/// The bytes are not a status line.
pub fn parse_status(line: &[u8]) -> Result<u16, String> {
	let text = String::from_utf8_lossy(line);
	let mut parts = text.split_ascii_whitespace();
	let version = parts.next().unwrap_or_default();
	if !version.starts_with("HTTP/") {
		return Err(format!("not an HTTP answer: {:?}", text.trim()));
	}
	parts
		.next()
		.and_then(|code| code.parse::<u16>().ok())
		.ok_or_else(|| format!("no status in {:?}", text.trim()))
}

/// `podkit-agent --health-probe PORT PATH STATUS TIMEOUT_S`: the exit code of the check, or
/// `None` when `args` (the process arguments after the program name) are not a probe.
#[must_use]
pub fn probe_cli(args: &[String]) -> Option<i32> {
	if args.first().map(String::as_str) != Some(PROBE_FLAG) {
		return None;
	}
	let parsed = (|| {
		let port = args.get(1)?.parse::<u16>().ok()?;
		let path = args.get(2)?.clone();
		let want = args.get(3)?.parse::<u16>().ok()?;
		let timeout = args.get(4)?.parse::<u64>().ok()?;
		Some((port, path, want, timeout))
	})();
	let Some((port, path, want, timeout)) = parsed else {
		eprintln!("usage: podkit-agent {PROBE_FLAG} PORT PATH STATUS TIMEOUT_S");
		return Some(2);
	};
	match probe_http(port, &path, Duration::from_secs(timeout.max(1))) {
		Ok(status) if status == want => Some(0),
		Ok(status) => {
			eprintln!("got status {status}, want {want}");
			Some(1)
		}
		Err(e) => {
			eprintln!("{e}");
			Some(1)
		}
	}
}

// --- the wait, run by the agent ---------------------------------------------------------------

/// What one look at the new container showed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Sample {
	/// It is not running (exited, or gone): the message says how.
	Gone(String),
	/// It was started again behind the deploy's back.
	Restarted,
	/// Running, with this Podman health (`None` before the first verdict).
	Running(Option<HealthStatus>),
}

/// What the gate makes of a sample.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
	/// Take over: the container is healthy.
	Healthy,
	/// Keep the old one: the container will not become healthy.
	Unhealthy(String),
	/// Not yet.
	Waiting,
}

/// The decision for one sample, `elapsed` after the container started.
#[must_use]
pub fn judge(gate: &HealthGate, sample: &Sample, elapsed: Duration) -> Verdict {
	match sample {
		Sample::Gone(message) => Verdict::Unhealthy(message.clone()),
		Sample::Restarted => {
			Verdict::Unhealthy("the container restarted before it became healthy".to_owned())
		}
		Sample::Running(Some(HealthStatus::Healthy)) => Verdict::Healthy,
		Sample::Running(Some(HealthStatus::Unhealthy)) => Verdict::Unhealthy(format!(
			"the health check failed {} times in a row",
			gate.retries
		)),
		Sample::Running(Some(HealthStatus::Starting) | None) => {
			if elapsed >= Duration::from_secs(u64::from(gate.gate_timeout_s)) {
				Verdict::Unhealthy(format!(
					"the container was not healthy after {}s",
					gate.gate_timeout_s
				))
			} else {
				Verdict::Waiting
			}
		}
	}
}

/// What the last failed check printed, for the message that says why a container was not
/// healthy.
fn last_failure(health: Option<&bollard::models::Health>) -> Option<String> {
	let entry = health?
		.log
		.as_ref()?
		.iter()
		.rev()
		.find(|r| r.exit_code != Some(0))?;
	let output = entry.output.as_deref().unwrap_or_default().trim();
	let output: String = output.chars().take(300).collect();
	Some(if output.is_empty() {
		format!("exit code {}", entry.exit_code.unwrap_or(-1))
	} else {
		format!("exit code {}: {output}", entry.exit_code.unwrap_or(-1))
	})
}

/// One look at `name`, and what its last failed check printed.
#[allow(
	clippy::option_option,
	reason = "the container's start time is itself optional; the outer one is whether it was seen yet"
)]
async fn sample(
	podman: &Podman,
	name: &str,
	first_start: &mut Option<Option<String>>,
) -> Result<(Sample, Option<String>), String> {
	let info = podman
		.inspect(name)
		.await
		.map_err(|e| format!("inspect failed while waiting for health: {e}"))?;
	let Some(info) = info else {
		return Ok((Sample::Gone("the container disappeared".to_owned()), None));
	};
	let health = podman::health_of(&info);
	let state = info.state.unwrap_or_default();
	let failure = last_failure(state.health.as_ref());
	if !state.running.unwrap_or(false) {
		let status = state
			.status
			.map_or_else(|| "unknown".to_owned(), |s| s.to_string());
		return Ok((
			Sample::Gone(format!(
				"the container is {status} (exit code {})",
				state.exit_code.unwrap_or(-1)
			)),
			failure,
		));
	}
	match first_start {
		None => *first_start = Some(state.started_at),
		Some(started) if *started != state.started_at => return Ok((Sample::Restarted, failure)),
		Some(_) => {}
	}
	Ok((Sample::Running(health), failure))
}

/// Waits until `name` is healthy or the gate says it will not be, logging progress on `tx`.
///
/// # Errors
/// Why the container did not become healthy, or that the client went away.
pub async fn wait_healthy(
	podman: &Podman,
	name: &str,
	gate: &HealthGate,
	tx: &mpsc::Sender<proto::Event>,
) -> Result<(), String> {
	let started = Instant::now();
	let mut first_start = None;
	let mut last_log = Instant::now();
	let log = |line: String| {
		tx.send(proto::Event::Log {
			stream: proto::Stream::System,
			line,
		})
	};
	log(format!(
		"waiting for the new container to be healthy (up to {}s)",
		gate.gate_timeout_s
	))
	.await
	.map_err(|_| "client disconnected".to_owned())?;
	loop {
		tokio::time::sleep(POLL).await;
		let (seen, failure) = sample(podman, name, &mut first_start).await?;
		match judge(gate, &seen, started.elapsed()) {
			Verdict::Healthy => {
				log(format!(
					"healthy after {:.1}s",
					started.elapsed().as_secs_f64()
				))
				.await
				.map_err(|_| "client disconnected".to_owned())?;
				return Ok(());
			}
			Verdict::Unhealthy(message) => {
				return Err(match failure {
					Some(last) => format!("{message} (last check: {last})"),
					None => message,
				});
			}
			Verdict::Waiting => {}
		}
		if last_log.elapsed() >= LOG_EVERY {
			last_log = Instant::now();
			log(format!(
				"still waiting for health ({}s of {}s)",
				started.elapsed().as_secs(),
				gate.gate_timeout_s
			))
			.await
			.map_err(|_| "client disconnected".to_owned())?;
		}
	}
}

#[cfg(test)]
mod tests {
	use std::io::{Read, Write};
	use std::net::TcpListener;

	use super::*;

	fn gate(check: HealthCheck) -> HealthGate {
		HealthGate {
			check,
			interval_ms: 5000,
			timeout_ms: 2500,
			retries: 3,
			start_period_ms: 10_000,
			gate_timeout_s: 60,
			drain_s: 5,
		}
	}

	fn http() -> HealthGate {
		gate(HealthCheck::Http {
			path: "/up".to_owned(),
			status: 200,
			port: 8080,
		})
	}

	#[test]
	fn health_http_check_runs_the_agent_binary_in_the_container() {
		let check = podman_check(&http());
		assert_eq!(
			check.test,
			["CMD", PROBE_PATH, PROBE_FLAG, "8080", "/up", "200", "3"]
		);
		assert_eq!(check.interval_ms, Some(5000));
		assert_eq!(check.timeout_ms, Some(2500));
		assert_eq!(check.retries, Some(3));
		assert_eq!(check.start_period_ms, Some(10_000));
		let binds = binds(&http(), Path::new("/usr/lib/podkit/podkit-agent"));
		assert_eq!(binds[0], "/usr/lib/podkit/podkit-agent:/.podkit/health:ro");
	}

	#[test]
	fn health_command_check_is_a_shell_check_and_mounts_nothing() {
		let g = gate(HealthCheck::Command {
			command: "pg_isready -q".to_owned(),
		});
		assert_eq!(podman_check(&g).test, ["CMD-SHELL", "pg_isready -q"]);
		assert!(binds(&g, Path::new("/x")).is_empty());
	}

	#[test]
	fn health_gate_validation_refuses_what_cannot_run() {
		assert!(validate(&http()).is_ok());
		let bad = |edit: fn(&mut HealthGate)| {
			let mut g = http();
			edit(&mut g);
			validate(&g).is_err()
		};
		assert!(bad(|g| g.gate_timeout_s = 0));
		assert!(bad(|g| g.retries = 0));
		assert!(bad(|g| g.interval_ms = 0));
		assert!(bad(|g| {
			g.check = HealthCheck::Http {
				path: "up".to_owned(),
				status: 200,
				port: 80,
			};
		}));
		assert!(bad(|g| {
			g.check = HealthCheck::Http {
				path: "/a b".to_owned(),
				status: 200,
				port: 80,
			};
		}));
		assert!(bad(|g| {
			g.check = HealthCheck::Http {
				path: "/".to_owned(),
				status: 99,
				port: 80,
			};
		}));
		assert!(bad(|g| {
			g.check = HealthCheck::Command {
				command: "  ".to_owned(),
			};
		}));
	}

	#[test]
	fn health_judge_waits_then_passes() {
		let g = http();
		let t = Duration::from_secs;
		assert_eq!(
			judge(&g, &Sample::Running(Some(HealthStatus::Starting)), t(3)),
			Verdict::Waiting
		);
		assert_eq!(judge(&g, &Sample::Running(None), t(3)), Verdict::Waiting);
		assert_eq!(
			judge(&g, &Sample::Running(Some(HealthStatus::Healthy)), t(4)),
			Verdict::Healthy
		);
	}

	#[test]
	fn health_judge_fails_on_timeout_unhealthy_exit_and_restart() {
		let g = http();
		let t = Duration::from_secs;
		let Verdict::Unhealthy(message) =
			judge(&g, &Sample::Running(Some(HealthStatus::Starting)), t(60))
		else {
			panic!("expected a timeout");
		};
		assert!(message.contains("60s"), "{message}");
		assert!(matches!(
			judge(&g, &Sample::Running(Some(HealthStatus::Unhealthy)), t(1)),
			Verdict::Unhealthy(m) if m.contains("3 times")
		));
		assert!(matches!(
			judge(&g, &Sample::Gone("exited (exit code 1)".to_owned()), t(1)),
			Verdict::Unhealthy(m) if m.contains("exit code 1")
		));
		assert!(matches!(
			judge(&g, &Sample::Restarted, t(1)),
			Verdict::Unhealthy(_)
		));
		// A healthy verdict at the very end of the window still counts.
		assert_eq!(
			judge(&g, &Sample::Running(Some(HealthStatus::Healthy)), t(60)),
			Verdict::Healthy
		);
	}

	#[test]
	fn health_status_lines_parse() {
		assert_eq!(parse_status(b"HTTP/1.1 204 No Content\r\n"), Ok(204));
		assert_eq!(
			parse_status(b"HTTP/1.0 503 Service Unavailable\r\n"),
			Ok(503)
		);
		assert!(parse_status(b"SSH-2.0-OpenSSH\r\n").is_err());
		assert!(parse_status(b"HTTP/1.1 abc\r\n").is_err());
		assert!(parse_status(b"").is_err());
	}

	/// Serves one canned answer per connection on a free port.
	fn serve(answer: &'static str, connections: usize) -> u16 {
		let listener = TcpListener::bind("127.0.0.1:0").unwrap();
		let port = listener.local_addr().unwrap().port();
		std::thread::spawn(move || {
			for _ in 0..connections {
				let Ok((mut stream, _)) = listener.accept() else {
					return;
				};
				let mut request = String::new();
				let mut buf = [0u8; 512];
				while !request.contains("\r\n\r\n") {
					let n = stream.read(&mut buf).unwrap_or(0);
					if n == 0 {
						break;
					}
					request.push_str(&String::from_utf8_lossy(&buf[..n]));
				}
				assert!(request.starts_with("GET /up HTTP/1.0\r\n"), "{request}");
				let _ = stream.write_all(answer.as_bytes());
			}
		});
		port
	}

	#[test]
	fn health_probe_reads_the_status_of_a_real_server() {
		let port = serve("HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n", 1);
		assert_eq!(probe_http(port, "/up", Duration::from_secs(2)), Ok(200));
		let port = serve("HTTP/1.1 500 Oops\r\n\r\n", 1);
		assert_eq!(probe_http(port, "/up", Duration::from_secs(2)), Ok(500));
	}

	#[test]
	fn health_probe_cli_exit_codes() {
		let args = |port: u16, want: u16| {
			[
				PROBE_FLAG.to_owned(),
				port.to_string(),
				"/up".to_owned(),
				want.to_string(),
				"2".to_owned(),
			]
		};
		let port = serve("HTTP/1.1 200 OK\r\n\r\n", 2);
		assert_eq!(probe_cli(&args(port, 200)), Some(0));
		assert_eq!(probe_cli(&args(port, 204)), Some(1));
		// Nothing listens here: a refused connection is a failed check, not a crash.
		let closed = TcpListener::bind("127.0.0.1:0").unwrap();
		let port = closed.local_addr().unwrap().port();
		drop(closed);
		assert_eq!(probe_cli(&args(port, 200)), Some(1));
		assert_eq!(probe_cli(&["--other".to_owned()]), None);
		assert_eq!(probe_cli(&[PROBE_FLAG.to_owned()]), Some(2));
	}
}
