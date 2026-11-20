//! Image vulnerability scanning: every image podkit deploys is scanned with
//! Syft and Grype on its own server (`docs/spikes/scanning.md`), the result is
//! shown as a badge and a Security tab, and nothing is ever blocked by it.
//!
//! - A deploy queues a scan of what it deployed ([`queue`]) and carries on; the
//!   scan runs as a job of its own (`jobs::JobKind::ImageScan`).
//! - A scan of the same image id on the same server is reused for 24 hours.
//! - Once a day each server rescans the SBOMs it stored (`ImageRescan`), so a
//!   vulnerability published tomorrow shows up without touching the image.
//! - A finding is **act now** when it is KEV-listed, or when its EPSS is at
//!   least 0.10 and its CVSS at least 7 ([`is_act_now`]).
//! - A team can ignore a finding with a reason until a date.

pub mod queue;
pub mod routes;
pub mod run;
pub mod service;
pub mod store;
#[cfg(test)]
pub(crate) mod tests;

pub use queue::{Target, queue, queue_daily_rescans, queue_fresh};

use std::fmt::Write as _;
use std::io::{Read, Write};

use proto::ScanFinding;
use serde::{Deserialize, Serialize};
use time::Duration;

use crate::ids::{ApplicationId, DatabaseId, ModelId, StackId};

/// How long a finished scan of an image is reused instead of scanning again.
pub const CACHE_FOR: Duration = Duration::hours(24);
/// A server's daily rescan runs when the last one is older than this.
pub const RESCAN_EVERY: Duration = Duration::hours(23);
/// The longest an ignore may last.
pub const MAX_IGNORE_DAYS: i64 = 365;
/// EPSS at or above this (with a high CVSS) is "act now".
pub const ACT_NOW_EPSS: f64 = 0.10;
/// CVSS at or above this (with a high EPSS) is "act now".
pub const ACT_NOW_CVSS: f64 = 7.0;

/// The kinds of resource an image belongs to. A template deploys as one of
/// these (an app, or a stack).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ResourceKind {
	Application,
	StackService,
	Database,
	Model,
}

impl ResourceKind {
	/// The value stored in `scan_links.resource_kind`.
	#[must_use]
	pub const fn as_str(self) -> &'static str {
		match self {
			Self::Application => "application",
			Self::StackService => "stack_service",
			Self::Database => "database",
			Self::Model => "model",
		}
	}
}

/// What a page asks about: a resource, or a whole stack (its services together).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Subject {
	Application(ApplicationId),
	Stack(StackId),
	Database(DatabaseId),
	Model(ModelId),
}

/// How bad a finding is; the order is the order of the badge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
	Unknown,
	Low,
	Medium,
	High,
	Critical,
}

impl Severity {
	#[must_use]
	pub const fn as_str(self) -> &'static str {
		match self {
			Self::Unknown => "unknown",
			Self::Low => "low",
			Self::Medium => "medium",
			Self::High => "high",
			Self::Critical => "critical",
		}
	}

	#[must_use]
	pub fn parse(s: &str) -> Option<Self> {
		match s {
			"unknown" => Some(Self::Unknown),
			"low" => Some(Self::Low),
			"medium" => Some(Self::Medium),
			"high" => Some(Self::High),
			"critical" => Some(Self::Critical),
			_ => None,
		}
	}

	#[must_use]
	pub const fn label(self) -> &'static str {
		match self {
			Self::Unknown => "Unknown",
			Self::Low => "Low",
			Self::Medium => "Medium",
			Self::High => "High",
			Self::Critical => "Critical",
		}
	}
}

/// Where a scan is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, sqlx::Type, Serialize, Deserialize)]
#[sqlx(type_name = "varchar", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum ScanStatus {
	Queued,
	Scanning,
	Succeeded,
	Failed,
}

impl ScanStatus {
	/// Queued or running: a result is on its way.
	#[must_use]
	pub const fn in_progress(self) -> bool {
		matches!(self, Self::Queued | Self::Scanning)
	}
}

/// Whether a finding needs attention first: listed as actively exploited, or
/// likely to be (EPSS) and severe (CVSS).
#[must_use]
pub fn is_act_now(kev: bool, epss: Option<f64>, cvss: Option<f64>) -> bool {
	kev || (epss.is_some_and(|e| e >= ACT_NOW_EPSS) && cvss.is_some_and(|c| c >= ACT_NOW_CVSS))
}

/// Findings per severity, and how many of them are act-now.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct Counts {
	pub critical: i64,
	pub high: i64,
	pub medium: i64,
	pub low: i64,
	pub unknown: i64,
	pub act_now: i64,
}

impl Counts {
	/// Counts `findings` as they would be shown.
	#[must_use]
	pub fn of(findings: &[ScanFinding]) -> Self {
		let mut counts = Self::default();
		for f in findings {
			counts.add(
				Severity::parse(&f.severity).unwrap_or(Severity::Unknown),
				is_act_now(f.kev, f.epss, f.cvss),
			);
		}
		counts
	}

	pub fn add(&mut self, severity: Severity, act_now: bool) {
		*self.slot(severity) += 1;
		self.act_now += i64::from(act_now);
	}

	fn slot(&mut self, severity: Severity) -> &mut i64 {
		match severity {
			Severity::Critical => &mut self.critical,
			Severity::High => &mut self.high,
			Severity::Medium => &mut self.medium,
			Severity::Low => &mut self.low,
			Severity::Unknown => &mut self.unknown,
		}
	}

	#[must_use]
	pub fn of_severity(&self, severity: Severity) -> i64 {
		match severity {
			Severity::Critical => self.critical,
			Severity::High => self.high,
			Severity::Medium => self.medium,
			Severity::Low => self.low,
			Severity::Unknown => self.unknown,
		}
	}

	#[must_use]
	pub const fn total(&self) -> i64 {
		self.critical + self.high + self.medium + self.low + self.unknown
	}

	/// The worst severity present; `None` when there are no findings.
	#[must_use]
	pub fn worst(&self) -> Option<Severity> {
		[
			Severity::Critical,
			Severity::High,
			Severity::Medium,
			Severity::Low,
			Severity::Unknown,
		]
		.into_iter()
		.find(|s| self.of_severity(*s) > 0)
	}

	/// Sums two sets, for a stack's services.
	#[must_use]
	pub const fn plus(self, other: Self) -> Self {
		Self {
			critical: self.critical + other.critical,
			high: self.high + other.high,
			medium: self.medium + other.medium,
			low: self.low + other.low,
			unknown: self.unknown + other.unknown,
			act_now: self.act_now + other.act_now,
		}
	}

	/// What a person reads: "2 high, 1 medium (1 act now)", or "no known vulnerabilities".
	#[must_use]
	pub fn summary(&self) -> String {
		let parts: Vec<String> = [
			Severity::Critical,
			Severity::High,
			Severity::Medium,
			Severity::Low,
			Severity::Unknown,
		]
		.into_iter()
		.filter(|s| self.of_severity(*s) > 0)
		.map(|s| format!("{} {}", self.of_severity(s), s.as_str()))
		.collect();
		if parts.is_empty() {
			return "no known vulnerabilities".to_owned();
		}
		let mut text = parts.join(", ");
		if self.act_now > 0 {
			let _ = write!(text, " ({} act now)", self.act_now);
		}
		text
	}
}

/// What a badge says.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BadgeState {
	/// Never scanned, or scanning is off.
	None,
	/// A scan is queued or running.
	Scanning,
	/// The last scan failed.
	Failed,
	/// Scanned, nothing found.
	Clean,
	/// The worst severity found.
	Found(Severity),
}

/// The state of a resource's security, as one badge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Badge {
	pub state: BadgeState,
	pub counts: Counts,
}

impl Badge {
	pub const NONE: Self = Self {
		state: BadgeState::None,
		counts: Counts {
			critical: 0,
			high: 0,
			medium: 0,
			low: 0,
			unknown: 0,
			act_now: 0,
		},
	};

	/// A badge for a finished scan with these (effective) counts.
	#[must_use]
	pub fn found(counts: Counts) -> Self {
		Self {
			state: counts.worst().map_or(BadgeState::Clean, BadgeState::Found),
			counts,
		}
	}

	/// Orders resources by risk: act-now first, then by severity, then by count.
	#[must_use]
	pub fn risk(&self) -> (i64, i64, i64, i64, i64) {
		(
			self.counts.act_now,
			self.counts.critical,
			self.counts.high,
			self.counts.medium,
			self.counts.low,
		)
	}
}

/// Compresses an SBOM for storage.
///
/// # Errors
/// Only on an I/O failure of the in-memory writer.
pub fn compress(sbom: &str) -> std::io::Result<Vec<u8>> {
	let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
	encoder.write_all(sbom.as_bytes())?;
	encoder.finish()
}

/// Inverse of [`compress`].
///
/// # Errors
/// The bytes are not gzip, or not UTF-8 inside.
pub fn decompress(bytes: &[u8]) -> std::io::Result<String> {
	let mut text = String::new();
	flate2::read::GzDecoder::new(bytes).read_to_string(&mut text)?;
	Ok(text)
}

#[cfg(test)]
mod unit {
	use super::*;

	#[test]
	fn the_sbom_survives_storage() {
		let sbom = include_str!("../../tests/fixtures/sbom.cdx.json");
		let packed = compress(sbom).unwrap();
		assert!(packed.len() < sbom.len());
		assert_eq!(decompress(&packed).unwrap(), sbom);
		assert!(decompress(b"plain text").is_err());
	}
}
