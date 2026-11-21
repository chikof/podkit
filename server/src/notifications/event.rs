//! The events podkit can tell people about, in one place: what they are called, how serious
//! they are by default and which ones come in up/down pairs.

use serde::{Deserialize, Serialize};

use crate::ids::{EnvironmentId, ProjectId, TeamId};

/// How serious an event is. A rule's minimum severity filters on it, and quiet hours let
/// only [`Severity::Critical`] through.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
	Info,
	Warning,
	Critical,
}

impl Severity {
	pub const ALL: [Self; 3] = [Self::Info, Self::Warning, Self::Critical];

	pub fn as_str(self) -> &'static str {
		match self {
			Self::Info => "info",
			Self::Warning => "warning",
			Self::Critical => "critical",
		}
	}

	pub fn label(self) -> &'static str {
		match self {
			Self::Info => "Info",
			Self::Warning => "Warning",
			Self::Critical => "Critical",
		}
	}

	pub fn parse(s: &str) -> Option<Self> {
		Self::ALL.into_iter().find(|v| v.as_str() == s)
	}

	/// The stored form, `0` to `2`.
	pub fn rank(self) -> i16 {
		match self {
			Self::Info => 0,
			Self::Warning => 1,
			Self::Critical => 2,
		}
	}

	pub fn from_rank(rank: i16) -> Self {
		match rank {
			..=0 => Self::Info,
			1 => Self::Warning,
			_ => Self::Critical,
		}
	}
}

/// What happened. Adding a kind here is all it takes for rules to offer it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
	DeploySucceeded,
	DeployFailed,
	Rollback,
	ServerUnreachable,
	ServerRecovered,
	BackupFailed,
	BackupSucceeded,
	/// A scheduled task failed on its last attempt.
	CronFailed,
	/// A scheduled task that was failing ran well again.
	CronRecovered,
	/// A new critical or "act now" finding.
	Vulnerability,
	DiskHigh,
	CertExpiring,
	ContainerOom,
	CrashLoop,
	ModelCrashed,
	/// Wired by the uptime card.
	UptimeDown,
	/// Wired by the uptime card.
	UptimeUp,
	/// A metric alert fired.
	MetricAlert,
	/// A metric alert's value is back under its threshold.
	MetricRecovered,
	/// A team passed 80% of one of its limits.
	QuotaWarning,
	/// A team reached a limit and something was refused.
	QuotaExceeded,
}

impl EventKind {
	pub const ALL: [Self; 21] = [
		Self::DeploySucceeded,
		Self::DeployFailed,
		Self::Rollback,
		Self::ServerUnreachable,
		Self::ServerRecovered,
		Self::BackupFailed,
		Self::BackupSucceeded,
		Self::CronFailed,
		Self::CronRecovered,
		Self::Vulnerability,
		Self::DiskHigh,
		Self::CertExpiring,
		Self::ContainerOom,
		Self::CrashLoop,
		Self::ModelCrashed,
		Self::UptimeDown,
		Self::UptimeUp,
		Self::MetricAlert,
		Self::MetricRecovered,
		Self::QuotaWarning,
		Self::QuotaExceeded,
	];

	pub fn as_str(self) -> &'static str {
		match self {
			Self::DeploySucceeded => "deploy_succeeded",
			Self::DeployFailed => "deploy_failed",
			Self::Rollback => "rollback",
			Self::ServerUnreachable => "server_unreachable",
			Self::ServerRecovered => "server_recovered",
			Self::BackupFailed => "backup_failed",
			Self::BackupSucceeded => "backup_succeeded",
			Self::CronFailed => "cron_failed",
			Self::CronRecovered => "cron_recovered",
			Self::Vulnerability => "vulnerability",
			Self::DiskHigh => "disk_high",
			Self::CertExpiring => "cert_expiring",
			Self::ContainerOom => "container_oom",
			Self::CrashLoop => "crash_loop",
			Self::ModelCrashed => "model_crashed",
			Self::UptimeDown => "uptime_down",
			Self::UptimeUp => "uptime_up",
			Self::MetricAlert => "metric_alert",
			Self::MetricRecovered => "metric_recovered",
			Self::QuotaWarning => "quota_warning",
			Self::QuotaExceeded => "quota_exceeded",
		}
	}

	pub fn parse(s: &str) -> Option<Self> {
		Self::ALL.into_iter().find(|k| k.as_str() == s)
	}

	pub fn label(self) -> &'static str {
		match self {
			Self::DeploySucceeded => "Deploy succeeded",
			Self::DeployFailed => "Deploy failed",
			Self::Rollback => "Rollback started",
			Self::ServerUnreachable => "Server unreachable",
			Self::ServerRecovered => "Server recovered",
			Self::BackupFailed => "Backup failed",
			Self::BackupSucceeded => "Backup succeeded",
			Self::CronFailed => "Cron job failed",
			Self::CronRecovered => "Cron job recovered",
			Self::Vulnerability => "New critical vulnerability",
			Self::DiskHigh => "Disk almost full",
			Self::CertExpiring => "Certificate expiring",
			Self::ContainerOom => "Container out of memory",
			Self::CrashLoop => "Container crashed",
			Self::ModelCrashed => "Model crashed",
			Self::UptimeDown => "Uptime check down",
			Self::UptimeUp => "Uptime check up",
			Self::MetricAlert => "Metric alert",
			Self::MetricRecovered => "Metric alert recovered",
			Self::QuotaWarning => "Quota almost used",
			Self::QuotaExceeded => "Quota reached",
		}
	}

	/// The heading its checkbox sits under.
	pub fn group(self) -> &'static str {
		match self {
			Self::DeploySucceeded | Self::DeployFailed | Self::Rollback => "Deploys",
			Self::ServerUnreachable | Self::ServerRecovered | Self::DiskHigh => "Servers",
			Self::BackupFailed | Self::BackupSucceeded | Self::CronFailed | Self::CronRecovered => {
				"Backups and jobs"
			}
			Self::Vulnerability | Self::CertExpiring => "Security",
			Self::ContainerOom | Self::CrashLoop | Self::ModelCrashed => "Containers and models",
			Self::UptimeDown | Self::UptimeUp => "Uptime",
			Self::MetricAlert | Self::MetricRecovered => "Metric alerts",
			Self::QuotaWarning | Self::QuotaExceeded => "Quotas",
		}
	}

	pub fn severity(self) -> Severity {
		match self {
			Self::DeploySucceeded
			| Self::BackupSucceeded
			| Self::ServerRecovered
			| Self::CronRecovered
			| Self::UptimeUp
			| Self::MetricRecovered => Severity::Info,
			Self::DeployFailed
			| Self::Rollback
			| Self::BackupFailed
			| Self::CronFailed
			| Self::DiskHigh
			| Self::CertExpiring
			| Self::ContainerOom
			| Self::ModelCrashed
			| Self::MetricAlert
			| Self::QuotaWarning
			| Self::QuotaExceeded => Severity::Warning,
			Self::ServerUnreachable | Self::Vulnerability | Self::CrashLoop | Self::UptimeDown => {
				Severity::Critical
			}
		}
	}

	/// For up/down kinds, the name the pair shares: flapping is counted across both.
	pub fn flap_pair(self) -> Option<&'static str> {
		match self {
			Self::ServerUnreachable | Self::ServerRecovered => Some("server"),
			Self::UptimeDown | Self::UptimeUp => Some("uptime"),
			Self::CronFailed | Self::CronRecovered => Some("cron"),
			_ => None,
		}
	}
}

/// Something that happened, ready to be routed. Build one with [`Event::new`] and hand it to
/// [`super::emit`].
#[derive(Debug, Clone)]
pub struct Event {
	pub team: TeamId,
	pub kind: EventKind,
	pub severity: Severity,
	pub project: Option<ProjectId>,
	pub environment: Option<EnvironmentId>,
	/// What it happened to (`app:12`); with the kind it is the dedup key.
	pub subject: String,
	pub title: String,
	pub body: String,
	/// A path inside podkit, such as `/teams/1/projects/2`.
	pub link: Option<String>,
}

impl Event {
	pub fn new(
		team: TeamId,
		kind: EventKind,
		subject: impl Into<String>,
		title: impl Into<String>,
	) -> Self {
		Self {
			team,
			kind,
			severity: kind.severity(),
			project: None,
			environment: None,
			subject: subject.into(),
			title: title.into(),
			body: String::new(),
			link: None,
		}
	}

	#[must_use]
	pub fn body(mut self, body: impl Into<String>) -> Self {
		self.body = body.into();
		self
	}

	#[must_use]
	pub fn scope(mut self, project: ProjectId, environment: Option<EnvironmentId>) -> Self {
		self.project = Some(project);
		self.environment = environment;
		self
	}

	#[must_use]
	pub fn link(mut self, path: impl Into<String>) -> Self {
		self.link = Some(path.into());
		self
	}

	#[must_use]
	pub fn severity(mut self, severity: Severity) -> Self {
		self.severity = severity;
		self
	}

	/// Events with the same key inside the dedup window collapse into one.
	pub fn key(&self) -> String {
		format!("{}:{}", self.kind.as_str(), self.subject)
	}

	/// The key up/down events share, so that flapping is counted across both directions.
	pub fn flap_key(&self) -> Option<String> {
		self.kind
			.flap_pair()
			.map(|pair| format!("{pair}:{}", self.subject))
	}
}

/// Cuts `text` to at most `max` characters, ending in an ellipsis when it was longer.
pub fn clip(text: &str, max: usize) -> String {
	if text.chars().count() <= max {
		return text.to_owned();
	}
	let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
	out.push('…');
	out
}
