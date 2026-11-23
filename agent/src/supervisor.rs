//! Keeps desired containers running. The agent owns restarts; containers are
//! created with restart policy `no`.

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bollard::models::EventMessage;
use futures_util::StreamExt;
use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex;
use tracing::{info, warn};

use crate::podman::Podman;

/// This many deaths inside [`FAILURE_WINDOW`] make the supervisor give up.
pub const MAX_FAILURES: usize = 5;
/// Window in which deaths count towards [`MAX_FAILURES`].
pub const FAILURE_WINDOW: Duration = Duration::from_mins(10);

const BACKOFF_SECS: [u64; 5] = [1, 2, 4, 8, 16];
const RECONNECT_DELAY: Duration = Duration::from_secs(2);

/// Delay before restart number `attempt` (0-based): 1, 2, 4, 8, 16 s, then 16 s.
#[must_use]
pub fn backoff(attempt: usize) -> Duration {
	Duration::from_secs(BACKOFF_SECS[attempt.min(BACKOFF_SECS.len() - 1)])
}

/// What to do about a container that just died.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
	/// Restart after this delay.
	Retry(Duration),
	/// Too many recent deaths; stop restarting.
	GiveUp,
}

/// Records a death at `now` in `history` and decides. Deaths older than
/// [`FAILURE_WINDOW`] are forgotten.
pub fn judge(history: &mut VecDeque<Instant>, now: Instant) -> Verdict {
	while history
		.front()
		.is_some_and(|t| now.saturating_duration_since(*t) > FAILURE_WINDOW)
	{
		history.pop_front();
	}
	history.push_back(now);
	if history.len() >= MAX_FAILURES {
		Verdict::GiveUp
	} else {
		Verdict::Retry(backoff(history.len() - 1))
	}
}

#[derive(Default)]
struct State {
	desired: BTreeSet<String>,
	failures: HashMap<String, VecDeque<Instant>>,
	gave_up: HashSet<String>,
	/// Containers with a restart task already scheduled.
	pending: HashSet<String>,
}

struct Inner {
	path: PathBuf,
	podman: Podman,
	state: Mutex<State>,
}

/// Handle on the supervisor; cheap to clone.
#[derive(Clone)]
pub struct Supervisor(Arc<Inner>);

impl Supervisor {
	/// Loads `<data_dir>/desired.json` and spawns the reconcile and event loop.
	pub async fn start(podman: Podman, data_dir: &Path) -> Self {
		let path = data_dir.join("desired.json");
		let desired = load_desired(&path).await;
		info!(
			count = desired.len(),
			"supervisor loaded desired containers"
		);
		let supervisor = Self(Arc::new(Inner {
			path,
			podman,
			state: Mutex::new(State {
				desired,
				..State::default()
			}),
		}));
		tokio::spawn(supervisor.clone().run());
		supervisor
	}

	/// Marks `name` desired, resets its failure history, and persists.
	pub async fn add(&self, name: &str) {
		let mut st = self.0.state.lock().await;
		st.failures.remove(name);
		st.gave_up.remove(name);
		if st.desired.insert(name.to_string()) {
			self.persist(&st.desired).await;
		}
	}

	/// Stops supervising `name` and persists.
	pub async fn remove(&self, name: &str) {
		let mut st = self.0.state.lock().await;
		st.failures.remove(name);
		st.gave_up.remove(name);
		if st.desired.remove(name) {
			self.persist(&st.desired).await;
		}
	}

	/// True when the supervisor stopped restarting `name`.
	pub async fn is_gave_up(&self, name: &str) -> bool {
		self.0.state.lock().await.gave_up.contains(name)
	}

	/// Deaths of `name` inside the failure window: how often it was restarted lately.
	pub async fn recent_failures(&self, name: &str) -> u32 {
		let st = self.0.state.lock().await;
		let now = Instant::now();
		st.failures.get(name).map_or(0, |history| {
			let live = history
				.iter()
				.filter(|t| now.saturating_duration_since(**t) <= FAILURE_WINDOW)
				.count();
			u32::try_from(live).unwrap_or(u32::MAX)
		})
	}

	async fn persist(&self, desired: &BTreeSet<String>) {
		let result = match serde_json::to_vec(desired) {
			Ok(bytes) => write_atomic(&self.0.path, &bytes).await,
			Err(e) => Err(e.into()),
		};
		if let Err(e) = result {
			warn!(error = %e, path = %self.0.path.display(), "persisting desired containers failed");
		}
	}

	async fn run(self) {
		loop {
			self.reconcile().await;
			let mut events = std::pin::pin!(self.0.podman.die_events());
			while let Some(item) = events.next().await {
				match item {
					Ok(event) => self.on_die(event).await,
					Err(e) => {
						warn!(error = %e, "podman event stream failed");
						break;
					}
				}
			}
			warn!("podman event stream ended, reconnecting");
			tokio::time::sleep(RECONNECT_DELAY).await;
		}
	}

	/// Drops desired entries whose container is gone and starts the stopped ones.
	async fn reconcile(&self) {
		let names: Vec<String> = self.0.state.lock().await.desired.iter().cloned().collect();
		for name in names {
			if self.is_gave_up(&name).await {
				continue;
			}
			match self.0.podman.inspect(&name).await {
				Ok(None) => {
					info!(container = %name, "desired container no longer exists");
					self.remove(&name).await;
				}
				Ok(Some(info)) => {
					if !info.state.and_then(|s| s.running).unwrap_or(false)
						&& let Err(e) = self.0.podman.start(&name).await
					{
						warn!(container = %name, error = %e, "reconcile start failed");
					}
				}
				Err(e) => warn!(container = %name, error = %e, "reconcile inspect failed"),
			}
		}
	}

	async fn on_die(&self, event: EventMessage) {
		let Some(actor) = event.actor else { return };
		let name = if let Some(name) = actor.attributes.and_then(|mut a| a.remove("name")) {
			name
		} else {
			let Some(id) = actor.id else { return };
			match self.0.podman.inspect(&id).await {
				Ok(Some(info)) => match info.name {
					Some(n) => n.trim_start_matches('/').to_string(),
					None => return,
				},
				_ => return,
			}
		};

		{
			let mut st = self.0.state.lock().await;
			if !st.desired.contains(&name)
				|| st.gave_up.contains(&name)
				|| !st.pending.insert(name.clone())
			{
				return;
			}
		}
		let this = self.clone();
		tokio::spawn(async move { this.restart(&name).await });
	}

	async fn restart(&self, name: &str) {
		loop {
			let delay = {
				let mut st = self.0.state.lock().await;
				if !st.desired.contains(name) {
					st.pending.remove(name);
					return;
				}
				let history = st.failures.entry(name.to_string()).or_default();
				match judge(history, Instant::now()) {
					Verdict::GiveUp => {
						st.gave_up.insert(name.to_string());
						st.pending.remove(name);
						warn!(container = %name, "container keeps dying, giving up");
						return;
					}
					Verdict::Retry(delay) => delay,
				}
			};
			tokio::time::sleep(delay).await;

			{
				let mut st = self.0.state.lock().await;
				let proceed = st.desired.contains(name) && !st.gave_up.contains(name);
				// Cleared before the start so a death right after it schedules again.
				st.pending.remove(name);
				if !proceed {
					return;
				}
			}
			match self.0.podman.start(name).await {
				Ok(()) => return,
				Err(e) if e.is_not_found() => {
					self.remove(name).await;
					return;
				}
				Err(e) => {
					warn!(container = %name, error = %e, "restart failed");
					self.0.state.lock().await.pending.insert(name.to_string());
				}
			}
		}
	}
}

async fn load_desired(path: &Path) -> BTreeSet<String> {
	match tokio::fs::read(path).await {
		Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|e| {
			warn!(error = %e, path = %path.display(), "desired.json is corrupt, starting empty");
			BTreeSet::new()
		}),
		Err(e) if e.kind() == std::io::ErrorKind::NotFound => BTreeSet::new(),
		Err(e) => {
			warn!(error = %e, path = %path.display(), "reading desired.json failed, starting empty");
			BTreeSet::new()
		}
	}
}

/// Write-temp, fsync, rename: readers see the old or the new file, never a torn one.
async fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
	let tmp = path.with_extension("json.tmp");
	let mut file = tokio::fs::File::create(&tmp).await?;
	file.write_all(bytes).await?;
	file.sync_all().await?;
	drop(file);
	tokio::fs::rename(&tmp, path).await
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn backoff_doubles_then_clamps() {
		let secs: Vec<u64> = (0..7).map(|n| backoff(n).as_secs()).collect();
		assert_eq!(secs, [1, 2, 4, 8, 16, 16, 16]);
	}

	#[test]
	fn gives_up_on_the_fifth_death_inside_the_window() {
		let t0 = Instant::now();
		let mut history = VecDeque::new();
		let verdicts: Vec<Verdict> = (0..5)
			.map(|i| judge(&mut history, t0 + Duration::from_secs(i * 30)))
			.collect();
		assert_eq!(
			verdicts,
			[
				Verdict::Retry(Duration::from_secs(1)),
				Verdict::Retry(Duration::from_secs(2)),
				Verdict::Retry(Duration::from_secs(4)),
				Verdict::Retry(Duration::from_secs(8)),
				Verdict::GiveUp,
			]
		);
	}

	#[test]
	fn old_deaths_leave_the_window() {
		let t0 = Instant::now();
		let mut history = VecDeque::new();
		for i in 0..4 {
			judge(&mut history, t0 + Duration::from_secs(i));
		}
		// Eleven minutes later the earlier four no longer count.
		let verdict = judge(&mut history, t0 + Duration::from_mins(11));
		assert_eq!(verdict, Verdict::Retry(Duration::from_secs(1)));
		assert_eq!(history.len(), 1);
	}
}
