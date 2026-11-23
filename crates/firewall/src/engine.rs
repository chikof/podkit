//! Apply with confirm. The engine is plain synchronous logic over a [`Host`]; time is passed
//! in, so the auto-revert is tested without sleeping.

use std::net::IpAddr;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::diff::diff;
use crate::policy::Policy;
use crate::protocol::{PendingInfo, PreviewInfo, Request, Response, StatusInfo};
use crate::render::{delete_script, render};

/// Seconds a change waits for confirmation when the caller does not say.
pub const DEFAULT_CONFIRM_SECS: u64 = 60;
/// Longest wait a caller may ask for.
pub const MAX_CONFIRM_SECS: u64 = 600;
/// Shortest wait; below this a human cannot confirm.
pub const MIN_CONFIRM_SECS: u64 = 5;
/// SSH session sources kept at most.
const MAX_KEEP: usize = 16;

/// Everything outside the process: nft, /proc and the state file.
pub trait Host: Send + Sync {
	/// Runs `script` through `nft -f -`.
	///
	/// # Errors
	/// nft's message when it refuses.
	fn nft_apply(&self, script: &str) -> Result<(), String>;
	/// Source addresses of open SSH sessions to `port`.
	fn ssh_peers(&self, port: u16) -> Vec<IpAddr>;
	/// The confirmed state saved by [`Host::save`].
	fn load(&self) -> Option<Active>;
	/// Saves the confirmed state; `None` forgets it.
	///
	/// # Errors
	/// A message when it cannot be written.
	fn save(&self, active: Option<&Active>) -> Result<(), String>;
}

/// A ruleset in force: what was asked and the script that made it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Active {
	/// The request.
	pub policy: Policy,
	/// The exact script applied.
	pub script: String,
}

struct Pending {
	token: u64,
	deadline: Instant,
	/// What to go back to.
	previous: Option<Active>,
	active: Active,
}

/// The state machine behind the socket.
pub struct Engine<H: Host> {
	host: H,
	committed: Option<Active>,
	pending: Option<Pending>,
	next_token: u64,
}

fn clamp_confirm(secs: Option<u64>) -> u64 {
	match secs {
		None => DEFAULT_CONFIRM_SECS,
		Some(0) => 0,
		Some(s) => s.clamp(MIN_CONFIRM_SECS, MAX_CONFIRM_SECS),
	}
}

impl<H: Host> Engine<H> {
	/// An engine with the saved state loaded but not yet applied; call [`Engine::restore`].
	pub fn new(host: H) -> Self {
		let committed = host.load();
		Self {
			host,
			committed,
			pending: None,
			next_token: 1,
		}
	}

	/// Puts the machine in the saved state: the confirmed ruleset again, or no table. Run at
	/// start, so a crash with a change waiting never leaves unconfirmed rules behind.
	///
	/// # Errors
	/// nft's message.
	pub fn restore(&self) -> Result<(), String> {
		match &self.committed {
			Some(a) => self.host.nft_apply(&a.script),
			None => self.host.nft_apply(delete_script()),
		}
	}

	/// The ruleset in force, confirmed or not.
	fn current(&self) -> Option<&Active> {
		self.pending
			.as_ref()
			.map(|p| &p.active)
			.or(self.committed.as_ref())
	}

	fn keep(&self, policy: &Policy) -> Vec<IpAddr> {
		let mut peers = self.host.ssh_peers(policy.ssh_port);
		peers.sort();
		peers.dedup();
		peers.truncate(MAX_KEEP);
		peers
	}

	fn status(&self, now: Instant) -> StatusInfo {
		StatusInfo {
			enabled: self.committed.is_some(),
			policy: self.committed.as_ref().map(|a| a.policy.clone()),
			pending: self.pending.as_ref().map(|p| PendingInfo {
				token: p.token,
				expires_in_secs: p.deadline.saturating_duration_since(now).as_secs(),
			}),
		}
	}

	/// Handles one request.
	pub fn handle(&mut self, req: Request, now: Instant) -> Response {
		let result = match req {
			Request::Status => Ok(Response::Status(self.status(now))),
			Request::Preview { policy } => self.preview(&policy),
			Request::Apply {
				policy,
				confirm_secs,
			} => self.apply(&policy, clamp_confirm(confirm_secs), now),
			Request::Confirm { token } => self.confirm(token).map(|()| Response::Done),
			Request::Rollback => self.rollback().map(|()| Response::Done),
			Request::Disable => self.disable().map(|()| Response::Done),
		};
		result.unwrap_or_else(|message| Response::Error { message })
	}

	fn preview(&self, policy: &Policy) -> Result<Response, String> {
		policy.validate()?;
		let keep = self.keep(policy);
		let script = render(policy, &keep);
		let old = self.current().map_or("", |a| a.script.as_str());
		Ok(Response::Preview(PreviewInfo {
			diff: diff(old, &script),
			script,
			keep_sources: keep.iter().map(ToString::to_string).collect(),
		}))
	}

	fn apply(&mut self, policy: &Policy, confirm: u64, now: Instant) -> Result<Response, String> {
		policy.validate()?;
		if self.pending.is_some() {
			return Err("a change is waiting for confirmation; confirm or roll it back".to_owned());
		}
		let active = Active {
			policy: policy.clone(),
			script: render(policy, &self.keep(policy)),
		};
		self.host.nft_apply(&active.script)?;
		if confirm == 0 {
			self.host.save(Some(&active))?;
			self.committed = Some(active);
			return Ok(Response::Applied {
				token: 0,
				confirm_secs: 0,
			});
		}
		let token = self.next_token;
		self.next_token += 1;
		self.pending = Some(Pending {
			token,
			deadline: now + Duration::from_secs(confirm),
			previous: self.committed.clone(),
			active,
		});
		Ok(Response::Applied {
			token,
			confirm_secs: confirm,
		})
	}

	fn confirm(&mut self, token: u64) -> Result<(), String> {
		let pending = match self.pending.take() {
			Some(p) if p.token == token => p,
			other => {
				self.pending = other;
				return Err(
					"nothing to confirm with that token (it may have been reverted)".to_owned(),
				);
			}
		};
		self.host.save(Some(&pending.active))?;
		self.committed = Some(pending.active);
		Ok(())
	}

	fn revert(&self, pending: &Pending) -> Result<(), String> {
		match &pending.previous {
			Some(a) => self.host.nft_apply(&a.script),
			None => self.host.nft_apply(delete_script()),
		}
	}

	fn rollback(&mut self) -> Result<(), String> {
		let pending = self
			.pending
			.take()
			.ok_or_else(|| "no change is waiting for confirmation".to_owned())?;
		self.revert(&pending)
	}

	fn disable(&mut self) -> Result<(), String> {
		self.pending = None;
		self.host.nft_apply(delete_script())?;
		self.host.save(None)?;
		self.committed = None;
		Ok(())
	}

	/// Reverts an unconfirmed change whose time is up. Returns whether it did.
	pub fn tick(&mut self, now: Instant) -> bool {
		if self.pending.as_ref().is_none_or(|p| now < p.deadline) {
			return false;
		}
		let Some(pending) = self.pending.take() else {
			return false;
		};
		if let Err(e) = self.revert(&pending) {
			tracing::error!(error = %e, "reverting the unconfirmed firewall change failed");
		} else {
			tracing::warn!("firewall change was not confirmed in time and was reverted");
		}
		true
	}
}

#[cfg(test)]
mod tests {
	use std::sync::{Arc, Mutex};

	use super::*;
	use crate::policy::{Allow, Proto, Source};

	#[derive(Default)]
	struct State {
		/// Scripts passed to nft, in order.
		applied: Vec<String>,
		saved: Option<Active>,
		peers: Vec<IpAddr>,
	}

	#[derive(Clone, Default)]
	struct Fake(Arc<Mutex<State>>);

	impl Host for Fake {
		fn nft_apply(&self, script: &str) -> Result<(), String> {
			self.0.lock().unwrap().applied.push(script.to_owned());
			Ok(())
		}
		fn ssh_peers(&self, _: u16) -> Vec<IpAddr> {
			self.0.lock().unwrap().peers.clone()
		}
		fn load(&self) -> Option<Active> {
			self.0.lock().unwrap().saved.clone()
		}
		fn save(&self, active: Option<&Active>) -> Result<(), String> {
			self.0.lock().unwrap().saved = active.cloned();
			Ok(())
		}
	}

	fn policy_with(port: u16) -> Policy {
		Policy {
			allows: vec![Allow {
				proto: Proto::Tcp,
				port,
				source: None,
			}],
			..Policy::default()
		}
	}

	fn applied(host: &Fake) -> Vec<String> {
		host.0.lock().unwrap().applied.clone()
	}

	fn apply(policy: Policy, secs: Option<u64>) -> Request {
		Request::Apply {
			policy,
			confirm_secs: secs,
		}
	}

	fn token(r: &Response) -> u64 {
		match r {
			Response::Applied { token, .. } => *token,
			other => panic!("not applied: {other:?}"),
		}
	}

	#[test]
	fn unconfirmed_change_is_reverted_to_no_table() {
		let host = Fake::default();
		let mut e = Engine::new(host.clone());
		let t0 = Instant::now();
		let r = e.handle(apply(policy_with(5432), Some(60)), t0);
		assert!(matches!(
			r,
			Response::Applied {
				confirm_secs: 60,
				..
			}
		));
		assert!(!e.tick(t0 + Duration::from_secs(59)));
		assert_eq!(applied(&host).len(), 1);
		assert!(e.tick(t0 + Duration::from_secs(60)));
		assert_eq!(applied(&host).last().unwrap(), delete_script());
		assert!(host.0.lock().unwrap().saved.is_none());
	}

	#[test]
	fn unconfirmed_change_goes_back_to_the_confirmed_ruleset() {
		let host = Fake::default();
		let mut e = Engine::new(host.clone());
		let t0 = Instant::now();
		e.handle(apply(policy_with(1000), Some(0)), t0);
		let first = applied(&host)[0].clone();
		e.handle(apply(policy_with(2000), Some(30)), t0);
		assert!(e.tick(t0 + Duration::from_secs(31)));
		assert_eq!(applied(&host).last().unwrap(), &first);
		let Response::Status(s) = e.handle(Request::Status, t0) else {
			panic!("status");
		};
		assert_eq!(s.policy, Some(policy_with(1000)));
		assert!(s.pending.is_none());
	}

	#[test]
	fn confirm_keeps_the_change_and_saves_it() {
		let host = Fake::default();
		let mut e = Engine::new(host.clone());
		let t0 = Instant::now();
		let t = token(&e.handle(apply(policy_with(5432), None), t0));
		assert_eq!(e.handle(Request::Confirm { token: t }, t0), Response::Done);
		assert!(!e.tick(t0 + Duration::from_secs(3600)));
		assert_eq!(
			host.0.lock().unwrap().saved.as_ref().unwrap().policy,
			policy_with(5432)
		);
	}

	#[test]
	fn wrong_token_does_not_confirm() {
		let mut e = Engine::new(Fake::default());
		let t0 = Instant::now();
		let t = token(&e.handle(apply(policy_with(1), None), t0));
		assert!(matches!(
			e.handle(Request::Confirm { token: t + 1 }, t0),
			Response::Error { .. }
		));
		// The right one still works afterwards.
		assert_eq!(e.handle(Request::Confirm { token: t }, t0), Response::Done);
	}

	#[test]
	fn a_second_apply_waits_for_the_first() {
		let mut e = Engine::new(Fake::default());
		let t0 = Instant::now();
		e.handle(apply(policy_with(1), None), t0);
		assert!(matches!(
			e.handle(apply(policy_with(1), None), t0),
			Response::Error { .. }
		));
	}

	#[test]
	fn rollback_reverts_now() {
		let host = Fake::default();
		let mut e = Engine::new(host.clone());
		let t0 = Instant::now();
		e.handle(apply(policy_with(1), None), t0);
		assert_eq!(e.handle(Request::Rollback, t0), Response::Done);
		assert_eq!(applied(&host).last().unwrap(), delete_script());
		assert!(matches!(
			e.handle(Request::Rollback, t0),
			Response::Error { .. }
		));
	}

	#[test]
	fn restore_reapplies_the_saved_ruleset_or_clears_the_table() {
		let host = Fake::default();
		Engine::new(host.clone()).restore().unwrap();
		assert_eq!(applied(&host), vec![delete_script().to_owned()]);

		let mut e = Engine::new(host.clone());
		e.handle(apply(policy_with(7), Some(0)), Instant::now());
		let script = applied(&host).last().unwrap().clone();
		Engine::new(host.clone()).restore().unwrap();
		assert_eq!(applied(&host).last().unwrap(), &script);
	}

	#[test]
	fn open_ssh_sessions_stay_allowed_when_ssh_is_limited() {
		let host = Fake::default();
		host.0.lock().unwrap().peers = vec!["203.0.113.9".parse().unwrap()];
		let mut e = Engine::new(host);
		let policy = Policy {
			ssh_sources: vec![Source::try_from("10.0.0.0/8".to_owned()).unwrap()],
			..Policy::default()
		};
		let Response::Preview(p) = e.handle(Request::Preview { policy }, Instant::now()) else {
			panic!("preview");
		};
		assert_eq!(p.keep_sources, vec!["203.0.113.9"]);
		assert!(
			p.script
				.contains("ip saddr 203.0.113.9/32 tcp dport 22 accept")
		);
	}

	#[test]
	fn confirm_window_is_clamped() {
		assert_eq!(clamp_confirm(None), 60);
		assert_eq!(clamp_confirm(Some(0)), 0);
		assert_eq!(clamp_confirm(Some(1)), MIN_CONFIRM_SECS);
		assert_eq!(clamp_confirm(Some(99_999)), MAX_CONFIRM_SECS);
	}
}
