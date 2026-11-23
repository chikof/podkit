//! Quotas, usage metering and where a billing provider plugs in.
//!
//! A team may have a hard limit on each [`QuotaKind`]. A team with no limit set has none, so
//! nothing changes for a homelab that never sets one. Limits are checked where something is
//! created or deployed ([`service::check_create`], [`service::check_deploy`]); a refusal says
//! which limit, how much is in use and where to raise it. Passing 80% of a limit sends one
//! notification (`quota_warning`), reaching it one more (`quota_exceeded`), each once per level
//! (and per month for the kinds that reset).
//!
//! What a kind counts:
//!
//! - **Resources** (applications, databases, models, stacks) and **previews** (environments
//!   made by a clone) are counted rows.
//! - **CPU, memory and GPUs** are what the team has *reserved*: the CPU and memory limits set
//!   on its applications, databases and models, and one GPU per model that asks for one. A
//!   resource without a limit reserves nothing, which is why a team with a memory quota should
//!   set a memory limit on what it runs.
//! - **Disk** is what the agents last measured on the team's volumes. Images are shared by all
//!   teams on a server and are not counted.
//! - **Model tokens** and **build minutes** are used up each calendar month (UTC) and start
//!   again from zero on the 1st.
//!
//! [`metering`] turns the metrics samples into hourly `usage_records` and hands each new hour
//! to the billing provider (`cloud` feature only). The provider and its hooks are in
//! [`crate::billing`].

pub mod metering;
mod routes;
pub mod service;

#[cfg(test)]
mod tests;

pub use routes::router;

/// Where a limit is raised, for an error message to point at.
pub const WHERE_TO_RAISE: &str =
	"An instance administrator can raise it in Settings → Usage & limits.";

/// The share of a limit at which the soft warning goes out.
pub const SOFT_PERCENT: i64 = 80;

/// Something a team can be limited on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum QuotaKind {
	/// Applications the team has.
	Applications,
	/// Databases the team has.
	Databases,
	/// Models the team has.
	Models,
	/// Stacks the team has.
	Stacks,
	/// Environments made by cloning another one.
	Previews,
	/// CPU reserved, in thousandths of a core.
	CpuMillis,
	/// Memory reserved, in MB.
	MemoryMb,
	/// Volume data, in MB.
	DiskMb,
	/// GPUs held by models.
	Gpus,
	/// Tokens the gateway served this month.
	ModelTokens,
	/// Minutes of builds this month.
	BuildMinutes,
}

/// The heading a kind sits under on the usage page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Group {
	/// How many of each thing.
	Resources,
	/// What they hold on the servers.
	Capacity,
	/// Used up and reset every month.
	Monthly,
}

impl Group {
	/// The heading.
	#[must_use]
	pub const fn label(self) -> &'static str {
		match self {
			Self::Resources => "Resources",
			Self::Capacity => "Capacity",
			Self::Monthly => "This month",
		}
	}
}

impl QuotaKind {
	/// Every kind, in the order the page lists them.
	pub const ALL: [Self; 11] = [
		Self::Applications,
		Self::Databases,
		Self::Models,
		Self::Stacks,
		Self::Previews,
		Self::CpuMillis,
		Self::MemoryMb,
		Self::DiskMb,
		Self::Gpus,
		Self::ModelTokens,
		Self::BuildMinutes,
	];

	/// The stored and API name.
	#[must_use]
	pub const fn key(self) -> &'static str {
		match self {
			Self::Applications => "applications",
			Self::Databases => "databases",
			Self::Models => "models",
			Self::Stacks => "stacks",
			Self::Previews => "previews",
			Self::CpuMillis => "cpu_millis",
			Self::MemoryMb => "memory_mb",
			Self::DiskMb => "disk_mb",
			Self::Gpus => "gpus",
			Self::ModelTokens => "model_tokens",
			Self::BuildMinutes => "build_minutes",
		}
	}

	/// The kind named `key`.
	#[must_use]
	pub fn parse(key: &str) -> Option<Self> {
		Self::ALL.into_iter().find(|k| k.key() == key)
	}

	/// What people call it.
	#[must_use]
	pub const fn label(self) -> &'static str {
		match self {
			Self::Applications => "Applications",
			Self::Databases => "Databases",
			Self::Models => "Models",
			Self::Stacks => "Stacks",
			Self::Previews => "Previews",
			Self::CpuMillis => "CPU",
			Self::MemoryMb => "Memory",
			Self::DiskMb => "Disk",
			Self::Gpus => "GPUs",
			Self::ModelTokens => "Model tokens",
			Self::BuildMinutes => "Build minutes",
		}
	}

	/// The lower-case noun a sentence uses: "5 applications".
	#[must_use]
	pub const fn noun(self) -> &'static str {
		match self {
			Self::Applications => "applications",
			Self::Databases => "databases",
			Self::Models => "models",
			Self::Stacks => "stacks",
			Self::Previews => "previews",
			Self::CpuMillis => "CPU",
			Self::MemoryMb => "memory",
			Self::DiskMb => "disk",
			Self::Gpus => "GPUs",
			Self::ModelTokens => "model tokens",
			Self::BuildMinutes => "build minutes",
		}
	}

	/// The heading it sits under.
	#[must_use]
	pub const fn group(self) -> Group {
		match self {
			Self::Applications | Self::Databases | Self::Models | Self::Stacks | Self::Previews => {
				Group::Resources
			}
			Self::CpuMillis | Self::MemoryMb | Self::DiskMb | Self::Gpus => Group::Capacity,
			Self::ModelTokens | Self::BuildMinutes => Group::Monthly,
		}
	}

	/// Used up over a month and reset on the 1st; reaching the limit means nothing is left.
	#[must_use]
	pub const fn monthly(self) -> bool {
		matches!(self, Self::ModelTokens | Self::BuildMinutes)
	}

	/// What the limit field is called and counted in.
	#[must_use]
	pub const fn input_unit(self) -> &'static str {
		match self {
			Self::CpuMillis => "cores",
			Self::MemoryMb | Self::DiskMb => "GB",
			Self::ModelTokens => "tokens",
			Self::BuildMinutes => "minutes",
			_ => "",
		}
	}

	/// A stored amount as people read it: `2.5 cores`, `1.5 GB`, `3.2M`.
	#[must_use]
	pub fn show(self, value: i64) -> String {
		match self {
			Self::CpuMillis => {
				let cores = trim(value as f64 / 1000.0);
				if cores == "1" {
					"1 core".to_owned()
				} else {
					format!("{cores} cores")
				}
			}
			Self::MemoryMb | Self::DiskMb => {
				if value >= 1024 {
					format!("{} GB", trim(value as f64 / 1024.0))
				} else {
					format!("{value} MB")
				}
			}
			Self::ModelTokens => compact(value),
			Self::BuildMinutes => format!("{value} min"),
			_ => value.to_string(),
		}
	}

	/// A stored amount as the number in the limit field: cores and GB as decimals.
	#[must_use]
	pub fn input_value(self, value: i64) -> String {
		match self {
			Self::CpuMillis => trim(value as f64 / 1000.0),
			Self::MemoryMb | Self::DiskMb => trim(value as f64 / 1024.0),
			_ => value.to_string(),
		}
	}

	/// The stored amount for what was typed in the limit field. Blank is no limit.
	///
	/// # Errors
	/// A sentence for something that is not a non-negative number.
	pub fn parse_input(self, raw: &str) -> Result<Option<i64>, String> {
		let raw = raw.trim();
		if raw.is_empty() {
			return Ok(None);
		}
		let bad = || {
			format!(
				"{} must be a number of {} or empty for no limit.",
				self.label(),
				if self.input_unit().is_empty() {
					self.noun()
				} else {
					self.input_unit()
				}
			)
		};
		let number: f64 = raw.replace(',', ".").parse().map_err(|_| bad())?;
		if !number.is_finite() || number < 0.0 || number > 1e15 {
			return Err(bad());
		}
		let scale = match self {
			Self::CpuMillis => 1000.0,
			Self::MemoryMb | Self::DiskMb => 1024.0,
			_ => 1.0,
		};
		let whole = !matches!(self, Self::CpuMillis | Self::MemoryMb | Self::DiskMb);
		if whole && number.fract() != 0.0 {
			return Err(bad());
		}
		#[allow(clippy::cast_possible_truncation, reason = "bounded above")]
		Ok(Some((number * scale).round() as i64))
	}
}

fn trim(value: f64) -> String {
	let s = format!("{value:.2}");
	s.trim_end_matches('0').trim_end_matches('.').to_owned()
}

fn compact(value: i64) -> String {
	match value {
		v if v >= 1_000_000_000 => format!("{}B", trim(v as f64 / 1e9)),
		v if v >= 1_000_000 => format!("{}M", trim(v as f64 / 1e6)),
		v if v >= 10_000 => format!("{}k", trim(v as f64 / 1e3)),
		v => v.to_string(),
	}
}

/// How far a limit is used.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
	/// Under the soft limit.
	Ok = 0,
	/// At or over the soft limit (80%).
	Warn = 1,
	/// At the limit.
	Full = 2,
}

impl Level {
	/// The stored form.
	#[must_use]
	pub const fn rank(self) -> i16 {
		self as i16
	}

	/// From the stored form.
	#[must_use]
	pub const fn from_rank(rank: i16) -> Self {
		match rank {
			..=0 => Self::Ok,
			1 => Self::Warn,
			_ => Self::Full,
		}
	}

	/// The level of `used` against `limit`.
	#[must_use]
	pub fn of(used: i64, limit: i64) -> Self {
		if limit <= 0 {
			return if used > 0 || limit == 0 {
				Self::Full
			} else {
				Self::Ok
			};
		}
		if used >= limit {
			Self::Full
		} else if i128::from(used) * 100 >= i128::from(limit) * i128::from(SOFT_PERCENT) {
			Self::Warn
		} else {
			Self::Ok
		}
	}
}

/// The calendar month (UTC) of `at` as `YYYY-MM`.
#[must_use]
pub fn month_of(at: time::OffsetDateTime) -> String {
	format!("{:04}-{:02}", at.year(), u8::from(at.month()))
}

#[cfg(test)]
mod kind_tests {
	use super::*;

	#[test]
	fn every_kind_round_trips_through_its_key() {
		for kind in QuotaKind::ALL {
			assert_eq!(QuotaKind::parse(kind.key()), Some(kind));
		}
		assert_eq!(QuotaKind::parse("nope"), None);
	}

	#[test]
	fn limits_are_typed_in_their_own_units() {
		assert_eq!(QuotaKind::CpuMillis.parse_input("2.5"), Ok(Some(2500)));
		assert_eq!(QuotaKind::MemoryMb.parse_input("1,5"), Ok(Some(1536)));
		assert_eq!(QuotaKind::Applications.parse_input(" 7 "), Ok(Some(7)));
		assert_eq!(QuotaKind::Applications.parse_input(""), Ok(None));
		assert!(QuotaKind::Applications.parse_input("2.5").is_err());
		assert!(QuotaKind::Applications.parse_input("-1").is_err());
		assert!(QuotaKind::DiskMb.parse_input("lots").is_err());
	}

	#[test]
	fn amounts_read_as_people_say_them() {
		assert_eq!(QuotaKind::CpuMillis.show(1000), "1 core");
		assert_eq!(QuotaKind::CpuMillis.show(2500), "2.5 cores");
		assert_eq!(QuotaKind::MemoryMb.show(512), "512 MB");
		assert_eq!(QuotaKind::MemoryMb.show(3072), "3 GB");
		assert_eq!(QuotaKind::ModelTokens.show(3_200_000), "3.2M");
		assert_eq!(QuotaKind::ModelTokens.show(900), "900");
		assert_eq!(QuotaKind::MemoryMb.input_value(1536), "1.5");
	}

	#[test]
	fn the_soft_limit_is_eighty_percent() {
		assert_eq!(Level::of(79, 100), Level::Ok);
		assert_eq!(Level::of(80, 100), Level::Warn);
		assert_eq!(Level::of(99, 100), Level::Warn);
		assert_eq!(Level::of(100, 100), Level::Full);
		assert_eq!(Level::of(4, 5), Level::Warn);
		assert_eq!(Level::of(3, 5), Level::Ok);
		// A limit of zero means nothing is allowed.
		assert_eq!(Level::of(0, 0), Level::Full);
	}
}
