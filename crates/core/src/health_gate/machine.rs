//! The health of one deployment as a state machine, with no I/O: the deploy run and the
//! periodic watch feed it what they saw, it says what to store and whether to roll back.
//!
//! ```text
//!  deploy starts the new container
//!        |  Phase::HealthCheck
//!        v
//!    Waiting --- Failed (timeout, exit, unhealthy) ---> Unhealthy   (old container kept; deploy failed)
//!        |  Deployed (the new container took over)
//!        v
//!    Healthy --- failed checks >= N, or a crash, inside the window ---> RolledBack (+ Rollback effect)
//!        |  window over
//!        v
//!     settled (stays Healthy, never rolled back any more)
//! ```

use std::time::Duration;

use super::State;

/// What happened that the machine cares about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Signal {
	/// The agent started the new container and is waiting for its first healthy verdict.
	Started,
	/// The agent reports the deploy done: the new container is healthy and took over.
	Deployed,
	/// The agent reports the deploy failed (any reason).
	DeployFailed,
	/// The periodic watch found the container passing its check.
	CheckPassed,
	/// The watch found it failing (unhealthy, or exited and being restarted).
	CheckFailed,
	/// The watch found it dead for good (the agent gave up restarting it, or it is gone).
	Crashed,
}

/// What the caller must do besides storing the new state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
	/// Nothing more.
	None,
	/// Put the previous image back; the string says why, for the timeline and the alert.
	Rollback(String),
}

/// One deployment's health.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Machine {
	/// Where it stands; `None` before the gate has seen it.
	pub state: Option<State>,
	/// Consecutive failed checks since the deploy took over.
	pub failures: i32,
	/// Failed checks in a row that make a release bad.
	pub rollback_after: i32,
	/// How long after the deploy a bad release is rolled back; zero turns it off.
	pub window: Duration,
	/// A deployment started by a rollback is never rolled back again: two bad releases in a
	/// row would flip between them forever.
	pub is_rollback: bool,
	/// There is an earlier image to go back to.
	pub has_previous: bool,
}

impl Machine {
	/// A deployment the gate has not seen yet.
	#[must_use]
	pub fn new(
		rollback_after: i32,
		window: Duration,
		is_rollback: bool,
		has_previous: bool,
	) -> Self {
		Self {
			state: None,
			failures: 0,
			rollback_after,
			window,
			is_rollback,
			has_previous,
		}
	}

	/// Whether a bad release may still be rolled back, `since_deploy` after it took over.
	#[must_use]
	pub fn watching(&self, since_deploy: Duration) -> bool {
		self.state == Some(State::Healthy)
			&& !self.is_rollback
			&& !self.window.is_zero()
			&& since_deploy < self.window
	}

	/// Applies `signal`, `since_deploy` after the deploy took over (zero before that).
	pub fn on(&mut self, signal: Signal, since_deploy: Duration) -> Effect {
		match (self.state, signal) {
			(None | Some(State::Waiting), Signal::Started) => {
				self.state = Some(State::Waiting);
				self.failures = 0;
				Effect::None
			}
			(None | Some(State::Waiting), Signal::Deployed) => {
				self.state = Some(State::Healthy);
				self.failures = 0;
				Effect::None
			}
			(None | Some(State::Waiting), Signal::DeployFailed) => {
				// Only a deploy that was waiting on its check counts as "never became healthy";
				// a failure before the container started says nothing about health.
				if self.state == Some(State::Waiting) {
					self.state = Some(State::Unhealthy);
				}
				Effect::None
			}
			(Some(State::Healthy), Signal::CheckPassed) => {
				self.failures = 0;
				Effect::None
			}
			(Some(State::Healthy), Signal::CheckFailed) => {
				self.failures += 1;
				if self.failures >= self.rollback_after {
					self.roll_back(since_deploy, "failed its health check")
				} else {
					Effect::None
				}
			}
			(Some(State::Healthy), Signal::Crashed) => self.roll_back(since_deploy, "crashed"),
			_ => Effect::None,
		}
	}

	fn roll_back(&mut self, since_deploy: Duration, why: &str) -> Effect {
		if !self.watching(since_deploy) || !self.has_previous {
			return Effect::None;
		}
		self.state = Some(State::RolledBack);
		Effect::Rollback(format!(
			"the release {why} {}s after it went live",
			since_deploy.as_secs()
		))
	}
}
