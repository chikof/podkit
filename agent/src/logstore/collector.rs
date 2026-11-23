//! Follows the logs of podkit containers into the store.
//!
//! Every [`SCAN_EVERY`] the collector lists the containers that carry a `podkit.service`
//! label and starts one follower for each that has none: Podman's logs API with timestamps,
//! from the last line stored for that container. A follower ends with its container, and a
//! container that stopped before the agent looked is read once. A follower holds one
//! Podman stream and two line buffers capped at [`MAX_PENDING`]; a stream that breaks is
//! picked up again by the next scan, from its cursor.
//!
//! A line at the very millisecond of the cursor, not yet stored when the agent stopped, is
//! lost; one stored but not saved in the cursor file is skipped by the timestamp check, so
//! a restart never stores a line twice.

use std::collections::HashSet;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use bollard::container::LogOutput;
use bollard::models::ContainerSummaryStateEnum;
use futures_util::StreamExt;
use proto::{LogRecord, Stream};
use tracing::{debug, warn};

use super::LogStore;
use super::detect::{level_of, split_timestamp};
use super::files::{now, valid_service};
use crate::podman::{Podman, summary_name};
use crate::stats::resource_of;

/// How often the collector looks for containers to follow.
pub const SCAN_EVERY: Duration = Duration::from_secs(10);
/// Scans between retention sweeps (so once a minute).
const MAINTAIN_EVERY: u32 = 6;
/// A line without a newline is stored once it grows to this.
const MAX_PENDING: usize = 64 * 1024;

type Ids = Arc<Mutex<HashSet<String>>>;

/// Starts the collector. It runs until the agent exits.
pub fn spawn(podman: Podman, store: Arc<LogStore>) -> tokio::task::JoinHandle<()> {
	tokio::spawn(async move {
		let active = Ids::default();
		let drained = Ids::default();
		let mut scans = 0u32;
		loop {
			scan(&podman, &store, &active, &drained).await;
			scans = scans.wrapping_add(1);
			if scans.is_multiple_of(MAINTAIN_EVERY) {
				let store = Arc::clone(&store);
				let _ = tokio::task::spawn_blocking(move || store.maintain()).await;
			}
			tokio::time::sleep(SCAN_EVERY).await;
		}
	})
}

/// The deployment (or job) a container belongs to: the number after the service in its
/// name, `podkit-app-7-12` being deployment 12 of `app-7`.
#[must_use]
pub fn deployment_of(container: &str, svc: &str) -> Option<i64> {
	let rest = container
		.strip_prefix("podkit-")?
		.strip_prefix(svc)?
		.strip_prefix('-')?;
	(!rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit()))
		.then(|| rest.parse().ok())
		.flatten()
}

/// One scan: start followers for the containers that need one.
pub async fn scan(podman: &Podman, store: &Arc<LogStore>, active: &Ids, drained: &Ids) {
	let containers = match podman.list("podkit-").await {
		Ok(containers) => containers,
		Err(e) => {
			debug!(error = %e, "log collector: could not list containers");
			return;
		}
	};
	let mut names = Vec::new();
	for summary in &containers {
		let (Some(name), Some(id)) = (summary_name(summary), summary.id.as_deref()) else {
			continue;
		};
		names.push(name.to_owned());
		let Some(svc) = summary
			.labels
			.as_ref()
			.and_then(|l| l.get("podkit.service"))
			.filter(|s| valid_service(s) && resource_of(s).is_some())
		else {
			continue;
		};
		let running = summary.state == Some(ContainerSummaryStateEnum::RUNNING);
		let mut active_ids = active.lock().unwrap_or_else(PoisonError::into_inner);
		if active_ids.contains(id) {
			continue;
		}
		{
			let mut done = drained.lock().unwrap_or_else(PoisonError::into_inner);
			if running {
				done.remove(id);
			} else if done.contains(id) || summary.state == Some(ContainerSummaryStateEnum::CREATED)
			{
				continue;
			}
		}
		active_ids.insert(id.to_owned());
		let task = Follow {
			podman: podman.clone(),
			store: Arc::clone(store),
			name: name.to_owned(),
			id: id.to_owned(),
			svc: svc.clone(),
			active: Arc::clone(active),
			drained: Arc::clone(drained),
		};
		tokio::spawn(task.run());
	}
	store.keep_cursors(&names);
	store.set_following(active.lock().unwrap_or_else(PoisonError::into_inner).len());
}

struct Follow {
	podman: Podman,
	store: Arc<LogStore>,
	name: String,
	id: String,
	svc: String,
	active: Ids,
	drained: Ids,
}

impl Follow {
	async fn run(self) {
		let finished = self.follow().await;
		if finished {
			self.drained
				.lock()
				.unwrap_or_else(PoisonError::into_inner)
				.insert(self.id.clone());
		}
		self.active
			.lock()
			.unwrap_or_else(PoisonError::into_inner)
			.remove(&self.id);
	}

	/// Reads until the stream ends. `true` when it ended by itself (the container stopped).
	async fn follow(&self) -> bool {
		let resume = self.store.cursor(&self.name);
		let since = resume.map_or(0, |ms| ms.div_euclid(1000));
		let dep = deployment_of(&self.name, &self.svc);
		let mut stream = std::pin::pin!(self.podman.logs_stamped(&self.name, since, true));
		let mut pending = Pending::default();
		while let Some(item) = stream.next().await {
			let output = match item {
				Ok(output) => output,
				Err(e) => {
					debug!(container = %self.name, error = %e, "log collector: stream broke");
					self.store_batch(pending.take_all(dep), resume);
					return false;
				}
			};
			let (stream_kind, bytes) = match &output {
				LogOutput::StdErr { message } => (Stream::Stderr, message),
				LogOutput::StdOut { message } | LogOutput::Console { message } => {
					(Stream::Stdout, message)
				}
				LogOutput::StdIn { .. } => continue,
			};
			let batch = pending.push(stream_kind, &String::from_utf8_lossy(bytes), dep);
			self.store_batch(batch, resume);
		}
		self.store_batch(pending.take_all(dep), resume);
		true
	}

	fn store_batch(&self, records: Vec<LogRecord>, resume: Option<i64>) {
		let records: Vec<LogRecord> = records
			.into_iter()
			.filter(|r| resume.is_none_or(|c| r.ts > c))
			.collect();
		let Some(newest) = records.iter().map(|r| r.ts).max() else {
			return;
		};
		match self.store.append(&self.svc, &records) {
			Ok(()) => self.store.set_cursor(&self.name, newest),
			Err(e) => warn!(container = %self.name, error = %e, "storing container logs failed"),
		}
	}
}

/// The bytes of each stream that have not reached a newline yet.
#[derive(Default)]
struct Pending {
	out: String,
	err: String,
}

impl Pending {
	/// Adds a chunk and returns the lines it completed.
	fn push(&mut self, stream: Stream, chunk: &str, dep: Option<i64>) -> Vec<LogRecord> {
		let carry = match stream {
			Stream::Stderr => &mut self.err,
			_ => &mut self.out,
		};
		carry.push_str(chunk);
		let mut lines = Vec::new();
		while let Some(i) = carry.find('\n') {
			lines.push(carry[..i].trim_end_matches('\r').to_owned());
			carry.drain(..=i);
		}
		if carry.len() > MAX_PENDING {
			lines.push(std::mem::take(carry));
		}
		lines
			.iter()
			.filter_map(|raw| record(stream, raw, dep))
			.collect()
	}

	/// What is left when a stream ends: a last line without its newline.
	fn take_all(&mut self, dep: Option<i64>) -> Vec<LogRecord> {
		let mut records = Vec::new();
		for (stream, carry) in [
			(Stream::Stdout, std::mem::take(&mut self.out)),
			(Stream::Stderr, std::mem::take(&mut self.err)),
		] {
			if let Some(r) = record(stream, &carry, dep) {
				records.push(r);
			}
		}
		records
	}
}

/// One stamped line from Podman as a record. A blank line is dropped.
fn record(stream: Stream, raw: &str, dep: Option<i64>) -> Option<LogRecord> {
	let (ts, text) = split_timestamp(raw);
	if text.trim().is_empty() {
		return None;
	}
	Some(LogRecord {
		ts: ts.unwrap_or_else(now),
		stream,
		dep,
		level: level_of(text),
		line: text.to_owned(),
		svc: String::new(),
	})
}

#[cfg(test)]
mod tests {
	use proto::LogLevel;

	use super::*;

	#[test]
	fn logstore_the_deployment_is_the_number_after_the_service_in_the_name() {
		assert_eq!(deployment_of("podkit-app-7-12", "app-7"), Some(12));
		assert_eq!(
			deployment_of("podkit-stack-3-web-4", "stack-3-web"),
			Some(4)
		);
		assert_eq!(deployment_of("podkit-app-7", "app-7"), None);
		assert_eq!(deployment_of("podkit-app-70-12", "app-7"), None);
		assert_eq!(deployment_of("other-app-7-12", "app-7"), None);
		assert_eq!(deployment_of("podkit-app-7-x", "app-7"), None);
	}

	#[test]
	fn logstore_lines_split_across_chunks_are_put_back_together() {
		let mut pending = Pending::default();
		let first = pending.push(Stream::Stdout, "2026-11-21T08:00:00.001Z hel", Some(3));
		assert!(first.is_empty());
		let second = pending.push(
			Stream::Stdout,
			"lo\n2026-11-21T08:00:00.002Z [ERROR] boom\r\n2026-11-21T08:00:00.003Z par",
			Some(3),
		);
		assert_eq!(second.len(), 2);
		assert_eq!(second[0].line, "hello");
		assert_eq!(second[0].ts, 1_795_248_000_001);
		assert_eq!(second[0].dep, Some(3));
		assert_eq!(second[1].line, "[ERROR] boom");
		assert_eq!(second[1].level, Some(LogLevel::Error));
		let rest = pending.take_all(Some(3));
		assert_eq!(rest.len(), 1);
		assert_eq!(rest[0].line, "par");
	}

	#[test]
	fn logstore_stderr_and_stdout_keep_their_own_partial_lines() {
		let mut pending = Pending::default();
		assert!(
			pending
				.push(Stream::Stdout, "2026-11-21T08:00:00Z out-", None)
				.is_empty()
		);
		let err = pending.push(Stream::Stderr, "2026-11-21T08:00:00Z err\n", None);
		assert_eq!(err.len(), 1);
		assert_eq!(err[0].stream, Stream::Stderr);
		let out = pending.push(Stream::Stdout, "half\n", None);
		assert_eq!(out.len(), 1);
		assert_eq!(out[0].line, "out-half");
		assert_eq!(out[0].stream, Stream::Stdout);
	}

	#[test]
	fn logstore_blank_lines_are_dropped_and_a_giant_line_is_flushed() {
		let mut pending = Pending::default();
		let blank = pending.push(Stream::Stdout, "2026-11-21T08:00:00Z \n\n", None);
		assert!(blank.is_empty());
		let giant = pending.push(Stream::Stdout, &"x".repeat(MAX_PENDING + 1), None);
		assert_eq!(giant.len(), 1);
		assert!(pending.take_all(None).is_empty());
	}

	/// Needs rootless Podman (and the network for a small Alpine image): an application that
	/// logs in two deployments, each container left stopped as a redeploy leaves it. Search
	/// then finds the lines of both, in time order, tagged with their deployment.
	/// Containers are named `podkit-app-27162-<n>` and removed by that name at the end.
	#[tokio::test]
	#[ignore = "needs Podman and the network"]
	async fn logstore_live_an_app_logging_across_two_deploys_is_searchable_in_order() {
		use std::process::Command;

		const IMAGE: &str = "docker.io/library/alpine:3.20";
		const SVC: &str = "app-27162";
		let podman = Podman::connect(&crate::podman::local_socket_path()).unwrap();
		let cli = |args: &[&str]| {
			let out = Command::new("podman").args(args).output().unwrap();
			assert!(
				out.status.success(),
				"{args:?}: {}",
				String::from_utf8_lossy(&out.stderr)
			);
		};
		let cleanup = || {
			for dep in [1, 2] {
				let _ = Command::new("podman")
					.args(["rm", "-f", &format!("podkit-{SVC}-{dep}")])
					.output();
			}
		};
		cleanup();
		cli(&["pull", "-q", IMAGE]);
		let run = |dep: i64, script: &str| {
			cli(&[
				"run",
				"-d",
				"--name",
				&format!("podkit-{SVC}-{dep}"),
				"--label",
				&format!("podkit.service={SVC}"),
				IMAGE,
				"sh",
				"-c",
				script,
			]);
		};
		run(1, "echo one-start; echo 'ERROR one-crashed' >&2");
		tokio::time::sleep(Duration::from_millis(1500)).await;
		run(2, "echo two-start; echo 'warn: two-slow'");
		tokio::time::sleep(Duration::from_secs(2)).await;

		let dir = tempfile::tempdir().unwrap();
		let store = Arc::new(LogStore::open(&dir.path().join("logs")));
		let (active, drained) = (Ids::default(), Ids::default());
		scan(&podman, &store, &active, &drained).await;
		for _ in 0..50 {
			if active.lock().unwrap().is_empty() {
				break;
			}
			tokio::time::sleep(Duration::from_millis(200)).await;
		}
		cleanup();

		let found = store
			.search(proto::LogSearchRequest {
				services: vec![SVC.to_owned()],
				since: None,
				until: None,
				text: None,
				regex: false,
				levels: Vec::new(),
				deployment: None,
				limit: 100,
				newest: true,
			})
			.await
			.unwrap();
		let seen: Vec<(Option<i64>, &str)> = found
			.records
			.iter()
			.map(|r| (r.dep, r.line.as_str()))
			.collect();
		assert_eq!(
			seen,
			[
				(Some(1), "one-start"),
				(Some(1), "ERROR one-crashed"),
				(Some(2), "two-start"),
				(Some(2), "warn: two-slow"),
			]
		);
		assert!(found.records.windows(2).all(|w| w[0].ts <= w[1].ts));
		assert_eq!(found.records[1].level, Some(LogLevel::Error));
	}
}
