//! Per-team noise control and the SMTP server of the team's email channels.

use sqlx::PgExecutor;
use time::OffsetDateTime;

use crate::ids::TeamId;

pub const DEFAULT_DEDUP_MINUTES: i32 = 10;
pub const MAX_DEDUP_MINUTES: i32 = 24 * 60;

/// A team's dedup window and quiet hours. A team that never opened the page has the defaults.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct Settings {
	pub dedup_minutes: i32,
	pub quiet_enabled: bool,
	/// Minutes after local midnight.
	pub quiet_start_min: i32,
	pub quiet_end_min: i32,
	/// The team's offset from UTC, in minutes.
	pub utc_offset_min: i32,
}

impl Default for Settings {
	fn default() -> Self {
		Self {
			dedup_minutes: DEFAULT_DEDUP_MINUTES,
			quiet_enabled: false,
			quiet_start_min: 22 * 60,
			quiet_end_min: 7 * 60,
			utc_offset_min: 0,
		}
	}
}

impl Settings {
	/// Whether `now` falls in the quiet hours. A window that ends before it starts runs
	/// through midnight.
	pub fn is_quiet(&self, now: OffsetDateTime) -> bool {
		if !self.quiet_enabled || self.quiet_start_min == self.quiet_end_min {
			return false;
		}
		let utc = i32::from(now.hour()) * 60 + i32::from(now.minute());
		let local = (utc + self.utc_offset_min).rem_euclid(24 * 60);
		if self.quiet_start_min < self.quiet_end_min {
			(self.quiet_start_min..self.quiet_end_min).contains(&local)
		} else {
			local >= self.quiet_start_min || local < self.quiet_end_min
		}
	}
}

pub async fn load(db: impl PgExecutor<'_>, team: TeamId) -> Result<Settings, sqlx::Error> {
	Ok(sqlx::query_as::<_, Settings>(
		"SELECT dedup_minutes, quiet_enabled, quiet_start_min, quiet_end_min, utc_offset_min
		FROM notification_settings WHERE team_id = $1",
	)
	.bind(team)
	.fetch_optional(db)
	.await?
	.unwrap_or_default())
}

pub async fn save(
	db: impl PgExecutor<'_>,
	team: TeamId,
	settings: &Settings,
) -> Result<(), sqlx::Error> {
	sqlx::query(
		"INSERT INTO notification_settings
			(team_id, dedup_minutes, quiet_enabled, quiet_start_min, quiet_end_min, utc_offset_min)
		VALUES ($1, $2, $3, $4, $5, $6)
		ON CONFLICT (team_id) DO UPDATE SET dedup_minutes = $2, quiet_enabled = $3,
			quiet_start_min = $4, quiet_end_min = $5, utc_offset_min = $6",
	)
	.bind(team)
	.bind(settings.dedup_minutes)
	.bind(settings.quiet_enabled)
	.bind(settings.quiet_start_min)
	.bind(settings.quiet_end_min)
	.bind(settings.utc_offset_min)
	.execute(db)
	.await?;
	Ok(())
}

/// How a mail connection is secured.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Security {
	/// Plain, then upgraded with STARTTLS (usually port 587).
	StartTls,
	/// TLS from the first byte (usually port 465).
	Tls,
	/// No encryption; only for a relay on a trusted network.
	None,
}

impl Security {
	pub fn as_str(self) -> &'static str {
		match self {
			Self::StartTls => "starttls",
			Self::Tls => "tls",
			Self::None => "none",
		}
	}

	pub fn parse(s: &str) -> Option<Self> {
		[Self::StartTls, Self::Tls, Self::None]
			.into_iter()
			.find(|v| v.as_str() == s)
	}
}

/// A row of `notification_smtp`. The password is still sealed.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Smtp {
	pub host: String,
	pub port: i32,
	pub security: String,
	pub username: Option<String>,
	pub password: Option<Vec<u8>>,
	pub from_addr: String,
}

pub async fn load_smtp(db: impl PgExecutor<'_>, team: TeamId) -> Result<Option<Smtp>, sqlx::Error> {
	sqlx::query_as::<_, Smtp>(
		"SELECT host, port, security, username, password, from_addr
		FROM notification_smtp WHERE team_id = $1",
	)
	.bind(team)
	.fetch_optional(db)
	.await
}

#[cfg(test)]
mod tests {
	use time::macros::datetime;

	use super::*;

	fn quiet(start: i32, end: i32, offset: i32) -> Settings {
		Settings {
			quiet_enabled: true,
			quiet_start_min: start,
			quiet_end_min: end,
			utc_offset_min: offset,
			..Settings::default()
		}
	}

	#[test]
	fn a_window_through_midnight_wraps() {
		let s = quiet(22 * 60, 7 * 60, 0);
		assert!(s.is_quiet(datetime!(2026-01-01 23:30 UTC)));
		assert!(s.is_quiet(datetime!(2026-01-02 06:59 UTC)));
		assert!(!s.is_quiet(datetime!(2026-01-02 07:00 UTC)));
		assert!(!s.is_quiet(datetime!(2026-01-01 12:00 UTC)));
	}

	#[test]
	fn the_offset_moves_the_window() {
		// 23:30 UTC is 01:30 at +02:00, still inside 22:00 to 07:00; 20:30 UTC is 22:30 there.
		let s = quiet(22 * 60, 7 * 60, 120);
		assert!(s.is_quiet(datetime!(2026-01-01 20:30 UTC)));
		assert!(!s.is_quiet(datetime!(2026-01-01 19:30 UTC)));
		assert!(s.is_quiet(datetime!(2026-01-02 04:59 UTC)));
		assert!(!s.is_quiet(datetime!(2026-01-02 05:00 UTC)));
	}

	#[test]
	fn a_daytime_window_and_an_empty_one() {
		let s = quiet(9 * 60, 17 * 60, 0);
		assert!(s.is_quiet(datetime!(2026-01-01 12:00 UTC)));
		assert!(!s.is_quiet(datetime!(2026-01-01 17:00 UTC)));
		assert!(!quiet(60, 60, 0).is_quiet(datetime!(2026-01-01 01:00 UTC)));
		assert!(!Settings::default().is_quiet(datetime!(2026-01-01 23:00 UTC)));
	}
}
