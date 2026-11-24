//! Restore drills: a dump is restored into a throwaway container of the database's engine
//! and version, and its tables are counted, to prove the backup can come back.
//!
//! The copy is private to the drill and never near the real database: it runs with no
//! network at all (loopback only, `--network none`), no published port, no named volume
//! (the image's own data directory is an anonymous volume removed with the container),
//! memory and CPU limits, and the name `podkit-drill-{id}`, which no other podkit container
//! uses. A guard removes it when the call ends, fails or is dropped because the client went
//! away; a container left by an agent that crashed mid-drill is removed by the next drill
//! on the same server once it is older than [`STALE_AFTER_SECS`].
//!
//! The restore itself is the same code as a real restore ([`crate::backup::restore_inner`]),
//! pointed at the copy instead of the database.
//!
//! Each entry point sends exactly one terminal event on `tx`.

use std::sync::Arc;
use std::time::{Duration, Instant};

use proto::{CountRequest, DrillRequest, Event, RestoreRequest, Stream, parse_table_count};
use tokio::sync::mpsc;

use crate::Ctx;
use crate::model_download::{ensure_image, valid_ref};
use crate::podman::{ContainerSpec, Podman, summary_name};

/// Every drill container's name starts with this.
pub const NAME_PREFIX: &str = "podkit-drill-";
/// Marks a drill container, with the drill id as the value.
const LABEL: &str = "podkit.drill";
/// A drill container older than this belongs to no running drill: the server gives up on a
/// drill long before.
pub const STALE_AFTER_SECS: i64 = 6 * 3600;
const READY_POLL: Duration = Duration::from_secs(2);
/// Answers in a row before the engine counts as up, so a server that answers once while
/// it is still being set up does not get the dump.
const READY_STREAK: u32 = 2;
const MIN_MEMORY_BYTES: i64 = 128 << 20;
const MAX_READY_TIMEOUT_S: u32 = 1800;

/// Restores `req.url` into a throwaway copy and counts its tables.
pub async fn drill(ctx: Arc<Ctx>, req: DrillRequest, tx: mpsc::Sender<Event>) {
	let event = match drill_inner(&ctx, &req, &tx).await {
		Ok(tables) => Event::TablesCounted { tables },
		Err(message) => Event::Failed { message },
	};
	let _ = tx.send(event).await;
}

/// Counts the user tables of a running database, read-only.
pub async fn count(ctx: Arc<Ctx>, req: CountRequest, tx: mpsc::Sender<Event>) {
	let event = match count_inner(&ctx, &req).await {
		Ok(tables) => Event::TablesCounted { tables },
		Err(message) => Event::Failed { message },
	};
	let _ = tx.send(event).await;
}

/// `podkit-drill-{id}`.
#[must_use]
pub fn container_name(drill_id: i64) -> String {
	format!("{NAME_PREFIX}{drill_id}")
}

/// The drill containers to remove before a new drill starts: those named like one, other
/// than `current`, created more than [`STALE_AFTER_SECS`] before `now` (unix seconds). A
/// container whose age is unknown is left alone, since it may be another drill's.
#[must_use]
pub fn stale_drills(containers: &[(String, Option<i64>)], now: i64, current: &str) -> Vec<String> {
	containers
		.iter()
		.filter(|(name, created)| {
			name.starts_with(NAME_PREFIX)
				&& name != current
				&& created.is_some_and(|c| now.saturating_sub(c) > STALE_AFTER_SECS)
		})
		.map(|(name, _)| name.clone())
		.collect()
}

/// Counts consecutive successful readiness checks.
#[derive(Debug, Default)]
pub struct Readiness {
	streak: u32,
}

impl Readiness {
	/// Records one check; true once [`READY_STREAK`] passed in a row.
	pub fn observe(&mut self, ok: bool) -> bool {
		self.streak = if ok { self.streak + 1 } else { 0 };
		self.streak >= READY_STREAK
	}
}

/// An image reference as the server sends it: no spaces, controls or option-like start.
fn valid_image(image: &str) -> bool {
	!image.is_empty()
		&& image.len() <= 255
		&& !image.starts_with('-')
		&& image.bytes().all(|b| {
			b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_' | b'/' | b':' | b'@')
		})
}

fn valid_env_key(key: &str) -> bool {
	let mut bytes = key.bytes();
	bytes
		.next()
		.is_some_and(|b| b.is_ascii_alphabetic() || b == b'_')
		&& bytes.all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// Refuses a request that could not describe a private, limited copy.
///
/// # Errors
/// What is wrong with the request.
pub fn validate(req: &DrillRequest) -> Result<(), String> {
	if req.drill_id <= 0 {
		return Err("invalid drill id".to_owned());
	}
	if !valid_image(&req.image) {
		return Err(format!("invalid image {:?}", req.image));
	}
	if let Some((key, _)) = req.env.iter().find(|(k, _)| !valid_env_key(k)) {
		return Err(format!("invalid environment variable {key:?}"));
	}
	if req.memory_bytes < MIN_MEMORY_BYTES {
		return Err(format!(
			"a drill needs a memory limit of at least {} MiB",
			MIN_MEMORY_BYTES >> 20
		));
	}
	if !(req.cpu_cores > 0.0 && req.cpu_cores <= 64.0) {
		return Err("a drill needs a CPU limit between 0 and 64 cores".to_owned());
	}
	if req.ready_timeout_s == 0 || req.ready_timeout_s > MAX_READY_TIMEOUT_S {
		return Err(format!(
			"the ready timeout must be 1 to {MAX_READY_TIMEOUT_S} seconds"
		));
	}
	Ok(())
}

/// The copy: no network, no ports, no named volumes, limits, removed on stop.
#[must_use]
pub fn container_spec(req: &DrillRequest, name: &str) -> ContainerSpec {
	ContainerSpec {
		name: name.to_owned(),
		image: req.image.clone(),
		command: req.command.clone(),
		env: req.env.clone(),
		labels: vec![(LABEL.to_owned(), req.drill_id.to_string())],
		memory_bytes: Some(req.memory_bytes),
		cpu_cores: Some(req.cpu_cores),
		stop_timeout_s: Some(0),
		no_network: true,
		..Default::default()
	}
}

/// Force-removes the drill container, with its anonymous volumes, when the drill ends.
struct Cleanup {
	podman: Podman,
	name: String,
}

impl Drop for Cleanup {
	fn drop(&mut self) {
		let (podman, name) = (self.podman.clone(), std::mem::take(&mut self.name));
		tokio::spawn(async move {
			match podman.remove(&name).await {
				Ok(()) => tracing::debug!(container = %name, "drill container removed"),
				Err(e) => {
					tracing::warn!(container = %name, error = %e, "removing drill container failed");
				}
			}
		});
	}
}

async fn log(tx: &mpsc::Sender<Event>, line: String) -> Result<(), String> {
	tx.send(Event::Log {
		stream: Stream::System,
		line,
	})
	.await
	.map_err(|_| "client disconnected".to_owned())
}

/// Best effort: a container that cannot be listed or removed now is tried again next time.
async fn sweep_stale(podman: &Podman, current: &str, tx: &mpsc::Sender<Event>) {
	let containers = match podman.list(NAME_PREFIX).await {
		Ok(list) => list,
		Err(e) => {
			tracing::warn!(error = %e, "cannot list drill containers");
			return;
		}
	};
	let named: Vec<(String, Option<i64>)> = containers
		.iter()
		.filter_map(|c| summary_name(c).map(|n| (n.to_owned(), c.created)))
		.collect();
	let now = std::time::SystemTime::now()
		.duration_since(std::time::UNIX_EPOCH)
		.map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX));
	for name in stale_drills(&named, now, current) {
		match podman.remove(&name).await {
			Ok(()) => {
				let _ = log(tx, format!("removed the leftover drill container {name}")).await;
			}
			Err(e) => {
				tracing::warn!(container = %name, error = %e, "removing a stale drill failed");
			}
		}
	}
}

async fn run_sh(podman: &Podman, container: &str, script: &str) -> Result<(i64, String), String> {
	podman
		.exec_output(container, &["sh", "-c", script])
		.await
		.map_err(|e| format!("exec in {container}: {e}"))
}

async fn wait_ready(
	podman: &Podman,
	name: &str,
	cmd: &str,
	timeout: Duration,
	tx: &mpsc::Sender<Event>,
) -> Result<(), String> {
	log(
		tx,
		"waiting for the engine to accept connections".to_owned(),
	)
	.await?;
	let deadline = Instant::now() + timeout;
	let mut readiness = Readiness::default();
	loop {
		let info = podman
			.inspect(name)
			.await
			.map_err(|e| format!("inspect {name}: {e}"))?
			.ok_or("the drill container disappeared while starting")?;
		let state = info.state.unwrap_or_default();
		if state.running != Some(true) {
			let code = state
				.exit_code
				.map_or_else(|| "unknown".to_owned(), |c| c.to_string());
			return Err(format!(
				"the drill container stopped while starting (exit code {code})"
			));
		}
		// A failed exec only means "not yet": the engine may still be starting.
		let ok = matches!(run_sh(podman, name, cmd).await, Ok((0, _)));
		if readiness.observe(ok) {
			return Ok(());
		}
		if Instant::now() >= deadline {
			return Err(format!(
				"the engine did not accept connections within {} s",
				timeout.as_secs()
			));
		}
		tokio::time::sleep(READY_POLL).await;
	}
}

async fn count_tables(podman: &Podman, container: &str, cmd: &str) -> Result<u64, String> {
	let (code, stdout) = run_sh(podman, container, cmd).await?;
	if code != 0 {
		return Err(format!("the table count exited with code {code}"));
	}
	parse_table_count(&stdout)
}

async fn drill_inner(
	ctx: &Ctx,
	req: &DrillRequest,
	tx: &mpsc::Sender<Event>,
) -> Result<u64, String> {
	validate(req)?;
	let unsupported = || format!("{:?} databases cannot be drilled", req.engine);
	let ready = req.engine.ready_cmd().ok_or_else(unsupported)?;
	let count = req.engine.count_cmd().ok_or_else(unsupported)?;
	req.engine.restore_cmd().ok_or_else(unsupported)?;

	let podman = ctx.podman();
	let name = container_name(req.drill_id);
	sweep_stale(&podman, &name, tx).await;
	ensure_image(&podman, &req.image, tx).await?;
	// A drill of the same id left by a crashed agent would hold the name.
	podman
		.remove(&name)
		.await
		.map_err(|e| format!("removing a leftover drill container failed: {e}"))?;

	log(
		tx,
		format!("starting {name} from {} with no network", req.image),
	)
	.await?;
	podman
		.create_container(container_spec(req, &name))
		.await
		.map_err(|e| format!("drill container create failed: {e}"))?;
	let _cleanup = Cleanup {
		podman: podman.clone(),
		name: name.clone(),
	};
	podman
		.start(&name)
		.await
		.map_err(|e| format!("drill container start failed: {e}"))?;
	wait_ready(
		&podman,
		&name,
		ready,
		Duration::from_secs(u64::from(req.ready_timeout_s)),
		tx,
	)
	.await?;

	log(tx, "restoring the backup into the copy".to_owned()).await?;
	let restore = RestoreRequest {
		container: name.clone(),
		engine: req.engine,
		url: req.url.clone(),
	};
	crate::backup::restore_inner(ctx, &restore, tx)
		.await
		.map_err(|e| format!("restore failed: {e}"))?;

	log(tx, "counting the restored tables".to_owned()).await?;
	let tables = count_tables(&podman, &name, count)
		.await
		.map_err(|e| format!("check failed: {e}"))?;
	log(tx, format!("found {tables} tables; removing {name}")).await?;
	Ok(tables)
}

async fn count_inner(ctx: &Ctx, req: &CountRequest) -> Result<u64, String> {
	if !valid_ref(&req.container, false) {
		return Err(format!("invalid container {:?}", req.container));
	}
	let cmd = req
		.engine
		.count_cmd()
		.ok_or_else(|| format!("{:?} tables cannot be counted", req.engine))?;
	count_tables(&ctx.podman(), &req.container, cmd).await
}

#[cfg(test)]
mod tests {
	use super::*;

	fn request() -> DrillRequest {
		DrillRequest {
			drill_id: 42,
			engine: proto::Engine::Postgres,
			image: "docker.io/library/postgres:18-alpine".to_owned(),
			env: vec![("POSTGRES_USER".to_owned(), "podkit".to_owned())],
			command: None,
			memory_bytes: 1 << 30,
			cpu_cores: 1.0,
			ready_timeout_s: 300,
			url: "https://s3.example.com/b/1.sql.gz?X-Amz-Signature=x".to_owned(),
		}
	}

	#[test]
	fn names_are_private_to_drills() {
		assert_eq!(container_name(42), "podkit-drill-42");
		// A database's own container is `podkit-db-{id}-{instance}`: never a drill name.
		assert!(!"podkit-db-42-7".starts_with(NAME_PREFIX));
	}

	#[test]
	fn the_copy_has_no_network_ports_or_volumes_and_is_limited() {
		let spec = container_spec(&request(), "podkit-drill-42");
		assert!(spec.no_network);
		assert!(spec.network.is_empty());
		assert!(spec.extra_networks.is_empty());
		assert!(spec.aliases.is_empty());
		assert!(spec.published_ports.is_empty());
		assert!(spec.binds.is_empty());
		assert_eq!(spec.memory_bytes, Some(1 << 30));
		assert_eq!(spec.cpu_cores, Some(1.0));
		assert_eq!(spec.stop_timeout_s, Some(0));
		assert_eq!(spec.labels, [("podkit.drill".to_owned(), "42".to_owned())]);
		assert_eq!(spec.image, "docker.io/library/postgres:18-alpine");
	}

	#[test]
	fn only_old_drill_containers_are_swept() {
		let now = 1_000_000;
		let old = now - STALE_AFTER_SECS - 1;
		let containers = vec![
			("podkit-drill-1".to_owned(), Some(old)),
			("podkit-drill-2".to_owned(), Some(now - 60)),
			("podkit-drill-3".to_owned(), None),
			("podkit-drill-9".to_owned(), Some(old)),
			("podkit-db-5-1".to_owned(), Some(old)),
			("podkit-drill-edge".to_owned(), Some(now - STALE_AFTER_SECS)),
		];
		assert_eq!(
			stale_drills(&containers, now, "podkit-drill-9"),
			["podkit-drill-1"]
		);
	}

	#[test]
	fn readiness_needs_answers_in_a_row() {
		let mut r = Readiness::default();
		assert!(!r.observe(true));
		assert!(!r.observe(false));
		assert!(!r.observe(true));
		assert!(r.observe(true));
	}

	#[test]
	fn requests_are_validated() {
		assert_eq!(validate(&request()), Ok(()));
		let bad = |f: fn(&mut DrillRequest)| {
			let mut r = request();
			f(&mut r);
			validate(&r).unwrap_err()
		};
		assert!(bad(|r| r.drill_id = 0).contains("drill id"));
		assert!(bad(|r| r.image = "--privileged".to_owned()).contains("image"));
		assert!(bad(|r| r.image = "postgres:18 ; rm -rf /".to_owned()).contains("image"));
		assert!(bad(|r| r.env.push(("A=B".to_owned(), "x".to_owned()))).contains("environment"));
		assert!(bad(|r| r.env.push(("1X".to_owned(), "x".to_owned()))).contains("environment"));
		assert!(bad(|r| r.memory_bytes = 1 << 20).contains("memory"));
		assert!(bad(|r| r.cpu_cores = 0.0).contains("CPU"));
		assert!(bad(|r| r.cpu_cores = f64::NAN).contains("CPU"));
		assert!(bad(|r| r.ready_timeout_s = 0).contains("timeout"));
		assert!(bad(|r| r.ready_timeout_s = 99_999).contains("timeout"));
	}

	#[test]
	fn digests_and_registry_ports_are_valid_images() {
		assert!(valid_image("registry.local:5000/pg/postgres:17"));
		assert!(valid_image("docker.io/library/mongo@sha256:abc123"));
		assert!(!valid_image(""));
	}
}
