//! Stored container logs: how long they are kept, where they are searched and where they are
//! forwarded.
//!
//! The agent of every server follows the logs of its podkit containers into rotating files on
//! its own disk (see `agent::logstore`), so a line outlives its container and a search spans
//! deploys. This slice is the control plane's half:
//!
//! * **Settings.** One row per team: retention in days, a cap in MB per server and, when the
//!   team wants it, a Loki or OTLP endpoint. [`sync`] hands them to every agent of the team on
//!   the sampler's tick (only when they changed) and tells the team when forwarding drops lines.
//!   The `Authorization` header of the endpoint is sealed with the server's key and never
//!   shown again.
//! * **Search.** [`service::search`] turns a query over the team's own resources into one
//!   question per server, asks the agents at the same time and [`merge`]s their oldest-first
//!   answers into one. A resource that is not the team's is a 404, and an empty list of
//!   resources never means "all of the agent's": it means all of the team's.
//! * **Live tail.** There is no second pipeline: the page asks again for what is newer than the
//!   last line it has, every couple of seconds, from the same files.

mod routes;
pub mod service;
pub mod sync;
#[cfg(test)]
mod tests;

use proto::{LogLevel, LogRecord, LogSearchResult};
use serde::Serialize;

pub use routes::router;

use crate::ids::ServerId;

/// What a team keeps until it says otherwise.
pub const DEFAULT_RETENTION_DAYS: i32 = 7;
/// ... and this many MB per server.
pub const DEFAULT_MAX_MB: i32 = 1024;
/// The least a team may cap a server at; a log file segment is 4 MiB.
pub const MIN_MAX_MB: i32 = 16;
/// The longest retention a team may ask for.
pub const MAX_RETENTION_DAYS: i32 = 3650;
/// Lines a search returns when it does not say.
pub const DEFAULT_LIMIT: u32 = 500;
/// The most lines a search returns (the agent's own cap).
pub const MAX_LIMIT: u32 = 2000;
/// The longest search text.
pub const MAX_TEXT_LEN: usize = 500;

/// The protocol logs are forwarded with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExportKind {
	/// Loki's push API.
	Loki,
	/// OTLP over HTTP, JSON encoded.
	Otlp,
}

impl ExportKind {
	pub fn as_str(self) -> &'static str {
		match self {
			Self::Loki => "loki",
			Self::Otlp => "otlp",
		}
	}

	pub fn label(self) -> &'static str {
		match self {
			Self::Loki => "Loki",
			Self::Otlp => "OpenTelemetry (OTLP/HTTP)",
		}
	}

	pub fn parse(s: &str) -> Option<Self> {
		match s {
			"loki" => Some(Self::Loki),
			"otlp" => Some(Self::Otlp),
			_ => None,
		}
	}
}

/// Where logs are forwarded, as a person may see it: the header's value is not in here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Export {
	pub kind: ExportKind,
	pub url: String,
	pub tenant: Option<String>,
	/// Whether an `Authorization` header is saved.
	pub has_authorization: bool,
}

/// A team's log settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
	pub retention_days: i32,
	pub max_mb: i32,
	pub export: Option<Export>,
}

impl Default for Settings {
	fn default() -> Self {
		Self {
			retention_days: DEFAULT_RETENTION_DAYS,
			max_mb: DEFAULT_MAX_MB,
			export: None,
		}
	}
}

/// One line of a merged answer: the line and the server whose agent had it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
	pub server: ServerId,
	pub record: LogRecord,
}

/// What [`merge`] makes of the answers.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Merged {
	pub lines: Vec<Line>,
	/// More lines matched than `limit`: the ones at the far end are not in `lines`.
	pub truncated: bool,
	/// An agent stopped reading early, so lines may be missing anywhere in the range.
	pub partial: bool,
}

/// Puts the answers of several agents, each oldest first and each at most `limit` long, into one
/// list ordered by time and cut to `limit` from the end the search asked for.
///
/// Lines of the same millisecond keep a fixed order (server, then resource, then the order they
/// came in), so a page that asks again sees the same list.
#[must_use]
pub fn merge(parts: Vec<(ServerId, LogSearchResult)>, limit: usize, newest: bool) -> Merged {
	let mut out = Merged::default();
	let mut all: Vec<Line> = Vec::new();
	for (server, result) in parts {
		out.partial |= result.partial;
		all.extend(
			result
				.records
				.into_iter()
				.map(|record| Line { server, record }),
		);
	}
	// A stable sort: lines of one agent that share every key stay in the agent's order.
	all.sort_by(|a, b| {
		(a.record.ts, a.server.0, &a.record.svc).cmp(&(b.record.ts, b.server.0, &b.record.svc))
	});
	if all.len() > limit {
		out.truncated = true;
		if newest {
			all.drain(..all.len() - limit);
		} else {
			all.truncate(limit);
		}
	}
	out.lines = all;
	out
}

/// The levels a search keeps for a choice of the page: `any`, `info` (info and above), `warn`
/// (warnings and errors) or `error`.
pub fn levels_for(choice: &str) -> Option<Vec<LogLevel>> {
	match choice {
		"" | "any" => Some(Vec::new()),
		"info" => Some(vec![LogLevel::Info, LogLevel::Warn, LogLevel::Error]),
		"warn" => Some(vec![LogLevel::Warn, LogLevel::Error]),
		"error" => Some(vec![LogLevel::Error]),
		_ => None,
	}
}
