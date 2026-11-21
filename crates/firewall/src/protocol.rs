//! The wire format: one JSON request line in, one JSON response line out, per connection.

use serde::{Deserialize, Serialize};

use crate::diff::DiffLine;
use crate::policy::Policy;

/// Longest request line the daemon reads.
pub const MAX_REQUEST_BYTES: usize = 256 * 1024;

/// What the agent asks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
	/// Is the firewall on, and is a change waiting for confirmation?
	Status,
	/// The script and the diff of `policy` against what is active, without applying.
	Preview {
		/// The wanted ruleset.
		policy: Policy,
	},
	/// Apply `policy`; unless confirmed within `confirm_secs` it is reverted.
	Apply {
		/// The wanted ruleset.
		policy: Policy,
		/// Seconds to confirm in; `None` is the default, `Some(0)` applies for good.
		#[serde(default)]
		confirm_secs: Option<u64>,
	},
	/// Keep the change with this token.
	Confirm {
		/// From [`Response::Applied`].
		token: u64,
	},
	/// Revert an unconfirmed change now.
	Rollback,
	/// Remove the table; the firewall is off.
	Disable,
}

/// A change waiting for confirmation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingInfo {
	/// To pass to [`Request::Confirm`].
	pub token: u64,
	/// Seconds left before the revert.
	pub expires_in_secs: u64,
}

/// What the helper reports about itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatusInfo {
	/// A confirmed ruleset is in force.
	pub enabled: bool,
	/// The confirmed policy.
	pub policy: Option<Policy>,
	/// An unconfirmed change.
	pub pending: Option<PendingInfo>,
}

/// A preview.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreviewInfo {
	/// The nft script that would be applied.
	pub script: String,
	/// Against the script now in force (empty when the firewall is off).
	pub diff: Vec<DiffLine>,
	/// Source addresses of SSH sessions open now, kept allowed.
	pub keep_sources: Vec<String>,
}

/// What the helper answers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Response {
	/// To [`Request::Status`].
	Status(StatusInfo),
	/// To [`Request::Preview`].
	Preview(PreviewInfo),
	/// A change is applied; confirm it or it reverts.
	Applied {
		/// To confirm with.
		token: u64,
		/// Seconds until the revert; 0 when applied for good.
		confirm_secs: u64,
	},
	/// Done.
	Done,
	/// Refused or failed.
	Error {
		/// What went wrong, safe to show.
		message: String,
	},
}
