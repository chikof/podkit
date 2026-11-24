//! What the agent says about each replica, as rows for the header and Monitoring, and the one
//! verdict over all of them. No I/O: [`super::service::status`] fetches, this decides.

use std::collections::HashMap;

use proto::{ContainerHealth, ContainerState, ContainerStats, HealthStatus};

/// How one replica is doing, in the words the UI shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplicaState {
	/// Running, and its health check passes.
	Healthy,
	/// Running, with no health check to ask.
	Running,
	/// Just started: the health check has not passed yet.
	Starting,
	/// Running, and its health check fails. Traefik sends it no traffic.
	Unhealthy,
	/// Died; the agent's supervisor is bringing it back.
	Restarting,
	/// Died and the supervisor stopped trying.
	Crashed,
	/// Stopped or paused on purpose.
	Stopped,
}

impl ReplicaState {
	/// The word shown next to the dot.
	#[must_use]
	pub fn label(self) -> &'static str {
		match self {
			Self::Healthy => "Healthy",
			Self::Running => "Running",
			Self::Starting => "Starting",
			Self::Unhealthy => "Unhealthy",
			Self::Restarting => "Restarting",
			Self::Crashed => "Crashed",
			Self::Stopped => "Stopped",
		}
	}

	/// Whether the replica can take requests.
	#[must_use]
	pub fn is_up(self) -> bool {
		matches!(self, Self::Healthy | Self::Running)
	}
}

/// The state of one container from its health report.
#[must_use]
pub fn state_of(report: &ContainerHealth) -> ReplicaState {
	if report.gave_up {
		return ReplicaState::Crashed;
	}
	match (report.state, report.health) {
		(ContainerState::Running, Some(HealthStatus::Healthy)) => ReplicaState::Healthy,
		(ContainerState::Running, None) => ReplicaState::Running,
		(ContainerState::Running, Some(HealthStatus::Starting)) | (ContainerState::Created, _) => {
			ReplicaState::Starting
		}
		(ContainerState::Running, Some(HealthStatus::Unhealthy)) => ReplicaState::Unhealthy,
		(ContainerState::Restarting | ContainerState::Exited | ContainerState::Dead, _) => {
			ReplicaState::Restarting
		}
		(ContainerState::Paused | ContainerState::Unknown, _) => ReplicaState::Stopped,
	}
}

/// One replica's row.
#[derive(Debug, Clone, PartialEq)]
pub struct ReplicaRow {
	/// 1-based position: the first container is replica 1, then `-r2`, `-r3`.
	pub number: usize,
	/// The container's name.
	pub name: String,
	pub state: ReplicaState,
	/// Times the agent restarted it recently.
	pub restarts: u32,
	/// Share of the host's CPU, in percent; `None` until the agent has two samples.
	pub cpu: Option<f64>,
	/// Memory in bytes without page cache; `None` when it is not running.
	pub mem: Option<u64>,
}

/// The replica number of a container name of deployment `deployment` of application `app`:
/// 1 for `podkit-app-{app}-{deployment}`, `n` for `…-r{n}`. `None` for any other container.
#[must_use]
pub fn number_of(name: &str, app: i64, deployment: i64) -> Option<usize> {
	let base = format!("podkit-app-{app}-{deployment}");
	let rest = name.strip_prefix(&base)?;
	if rest.is_empty() {
		return Some(1);
	}
	let n = rest.strip_prefix("-r")?;
	if n.is_empty() || !n.bytes().all(|b| b.is_ascii_digit()) {
		return None;
	}
	n.parse().ok()
}

/// The rows of one deployment's replicas, in order. `reports` are the health reports of the
/// containers the agent lists, `stats` its CPU and memory readings (both may hold containers
/// of other deployments, which are left out).
#[must_use]
pub fn rows(
	app: i64,
	deployment: i64,
	reports: &[ContainerHealth],
	stats: &[ContainerStats],
) -> Vec<ReplicaRow> {
	let by_name: HashMap<&str, &ContainerStats> = stats
		.iter()
		.map(|s| (s.name.trim_start_matches('/'), s))
		.collect();
	let mut rows: Vec<ReplicaRow> = reports
		.iter()
		.filter_map(|report| {
			let number = number_of(&report.name, app, deployment)?;
			let health = state_of(report);
			let reading = by_name.get(report.name.as_str());
			Some(ReplicaRow {
				number,
				name: report.name.clone(),
				state: health,
				restarts: report.restarts,
				cpu: reading
					.and_then(|s| s.cpu_percent)
					.filter(|_| health.is_up()),
				mem: reading.map(|s| s.mem_used).filter(|_| health.is_up()),
			})
		})
		.collect();
	rows.sort_by_key(|r| r.number);
	rows
}

/// One verdict over the replicas of an application.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rollup {
	/// Replicas the application is set to run.
	pub wanted: usize,
	/// Replicas that can take requests.
	pub up: usize,
	/// Replicas the agent reports at all.
	pub found: usize,
	/// Restarts over every replica.
	pub restarts: u32,
}

impl Rollup {
	/// All wanted replicas are up.
	#[must_use]
	pub fn complete(&self) -> bool {
		self.wanted > 0 && self.up >= self.wanted
	}

	/// `3 of 3 up`.
	#[must_use]
	pub fn label(&self) -> String {
		format!("{} of {} up", self.up.min(self.wanted), self.wanted)
	}
}

/// The verdict over `rows` for an application set to run `wanted` replicas. A replica that
/// is missing counts against it, the same as one that is down.
#[must_use]
pub fn rollup(wanted: usize, rows: &[ReplicaRow]) -> Rollup {
	Rollup {
		wanted,
		up: rows.iter().filter(|r| r.state.is_up()).count(),
		found: rows.len(),
		restarts: rows.iter().map(|r| r.restarts).sum(),
	}
}
