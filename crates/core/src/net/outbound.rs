//! The instance's outbound policy: what the control plane may reach when a person gives it an
//! address (SEC-02, SEC-03). The mechanism is the `outbound` crate; this is the part that knows
//! about podkit: where the setting comes from, who may change it, and the one place that builds
//! the client.
//!
//! Private networks (RFC 1918, `fc00::/7`, CGNAT) are reachable when [`allowed`] says so, in
//! this order: `OUTBOUND_PRIVATE` if set, else the instance administrator's setting, else
//! the default of the registration policy: `allow` for `first-user` and `closed` (one owner's
//! homelab, where an ntfy or Gotify on the LAN is the normal case), `deny` for `open`.
//! Loopback, link-local (the cloud metadata address included) and podkit's own listen address
//! are never reachable.

use std::net::IpAddr;

use sqlx::PgPool;

use crate::audit::{self, Diff, Target};
use crate::error::{AppResult, ServerError};
use crate::ids::{TeamId, UserId};
use crate::quotas::service::is_instance_admin;
use crate::state::{AppState, Registration};

pub use ::outbound::{ConnectError, Denied, Outbound, read_capped};

const KEY: &str = "outbound_private";

/// The one answer to an address podkit will not send to, whatever the reason: a forbidden range
/// and a name that does not resolve read the same, so it tells nobody what is on the network.
#[must_use]
pub fn refused() -> ServerError {
	ServerError::Validation(
		"podkit does not send requests to that address. Loopback and link-local addresses are never allowed, and private network addresses only when the instance allows them (Settings → Registration)."
			.to_owned(),
	)
}

/// The default for `registration`.
#[must_use]
pub const fn default_private(registration: Registration) -> bool {
	!matches!(registration, Registration::Open)
}

/// Where the answer of [`status`] came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
	/// `OUTBOUND_PRIVATE`.
	Env,
	/// The administrator's setting.
	Setting,
	/// Nothing was set: the registration policy decides.
	Default,
}

/// The effective policy and where it comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Status {
	/// Whether private addresses are reachable.
	pub allow_private: bool,
	/// What decided.
	pub source: Source,
}

/// Parses `allow` or `deny`.
///
/// # Errors
/// The text says how to write it.
pub fn parse(value: &str) -> Result<bool, String> {
	match value.trim().to_ascii_lowercase().as_str() {
		"allow" | "true" | "yes" | "on" | "1" => Ok(true),
		"deny" | "false" | "no" | "off" | "0" => Ok(false),
		other => Err(format!(
			"invalid OUTBOUND_PRIVATE {other:?}; use allow or deny"
		)),
	}
}

async fn stored(db: &PgPool) -> Result<Option<bool>, sqlx::Error> {
	let value: Option<String> =
		sqlx::query_scalar("SELECT value FROM instance_settings WHERE key = $1")
			.bind(KEY)
			.fetch_optional(db)
			.await?;
	Ok(value.and_then(|v| parse(&v).ok()))
}

/// What the policy is now, for `registration` and `pinned` (`OUTBOUND_PRIVATE`).
///
/// # Errors
/// Database failures only.
pub async fn resolve(
	db: &PgPool,
	registration: Registration,
	pinned: Option<bool>,
) -> Result<Status, sqlx::Error> {
	if let Some(allow_private) = pinned {
		return Ok(Status {
			allow_private,
			source: Source::Env,
		});
	}
	Ok(match stored(db).await? {
		Some(allow_private) => Status {
			allow_private,
			source: Source::Setting,
		},
		None => Status {
			allow_private: default_private(registration),
			source: Source::Default,
		},
	})
}

/// The effective policy of this instance.
///
/// # Errors
/// Database failures only.
pub async fn status(s: &AppState) -> Result<Status, sqlx::Error> {
	resolve(&s.db, s.config.registration, s.config.outbound_private).await
}

/// The client every request to a typed address goes through. `listen` is where podkit itself
/// listens, `host` and `port` of the configuration.
///
/// # Errors
/// The HTTP client cannot be built.
pub fn build(
	allow_private: bool,
	host: &str,
	port: u16,
	extra: &[u16],
) -> Result<Outbound, reqwest::Error> {
	// A name that is not an address (`localhost`) stands for every address of the machine.
	let ip: IpAddr = host
		.trim()
		.trim_start_matches('[')
		.trim_end_matches(']')
		.parse()
		.unwrap_or(IpAddr::from([0, 0, 0, 0]));
	let own = std::iter::once(port)
		.chain(extra.iter().copied())
		.map(|p| (ip, p))
		.collect();
	Outbound::new(allow_private, own)
}

/// Changes the instance setting, for the instance administrator. Takes effect at once.
///
/// # Errors
/// `Forbidden` for anyone but the administrator; `Validation` when `OUTBOUND_PRIVATE` pins
/// the value, because then the page's setting would change nothing.
pub async fn set_private(s: &AppState, user: UserId, team: TeamId, allow: bool) -> AppResult<()> {
	if !is_instance_admin(&s.db, user).await? {
		return Err(ServerError::Forbidden);
	}
	if s.config.outbound_private.is_some() {
		return Err(ServerError::Validation(
			"OUTBOUND_PRIVATE is set for this instance, so this setting cannot change it. Change the variable and restart podkit."
				.to_owned(),
		));
	}
	let value = if allow { "allow" } else { "deny" };
	let before = status(s).await?;
	sqlx::query(
		"INSERT INTO instance_settings (key, value) VALUES ($1, $2)
		ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value, updated_at = NOW()",
	)
	.bind(KEY)
	.bind(value)
	.execute(&s.db)
	.await?;
	s.outbound.set_allow_private(allow);
	audit::log(
		&s.db,
		user,
		team,
		"outbound.update",
		Target::none("instance"),
		Diff::new().change(
			"outbound_private",
			if before.allow_private {
				"allow"
			} else {
				"deny"
			},
			value,
		),
	)
	.await;
	Ok(())
}

#[cfg(test)]
mod tests;
