//! The server firewall: an opt-in nftables ruleset that the `podkit-firewall` helper (a tiny
//! root process, see `crates/firewall`) keeps in its own table, `inet podkit`, on one server.
//!
//! podkit holds what the team *wants* (a few explicit allows and, optionally, the ranges SSH
//! is limited to); the helper holds what is *in force*. Nothing is applied by saving: the
//! Firewall page previews the change as a diff and applies it with a confirm window. The
//! helper reverts the change after 60 s unless podkit confirms it, and podkit confirms only
//! after it has reached the server again over a fresh connection, so a ruleset that cuts podkit
//! off takes itself back.
//!
//! - [`service`] is what the web UI calls: authorize, scope, validate, store, talk to the
//!   helper through the agent, audit.
//! - [`policy()`] turns the stored rows into the helper's [`Policy`].

pub mod service;

#[cfg(test)]
mod tests;

use podkit_firewall::policy::{Allow, Policy, Proto, Source};
use proto::cidr::Cidr;
use sqlx::PgExecutor;

use crate::error::ServerError;
use crate::ids::{FirewallRuleId, ServerId};

/// Most explicit allows of one server.
pub const MAX_RULES: i64 = 100;
/// Longest note on a rule.
pub const MAX_NOTE: usize = 80;

const UNIQUE: &str = "server_firewall_rules_server_id_proto_port_source_key";

/// What a team wants of a server's firewall.
#[derive(Debug, Clone, PartialEq, Eq, Default, sqlx::FromRow)]
pub struct Settings {
	/// Applied (and confirmed) at least once and not turned off since.
	pub enabled: bool,
	/// SSH is limited to these ranges when the list is not empty.
	pub ssh_sources: Vec<String>,
	/// The next bootstrap installs the root helper.
	pub install_helper: bool,
}

/// One explicit allow.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct Rule {
	pub id: FirewallRuleId,
	pub proto: String,
	pub port: i32,
	/// A range, or `None` for anyone.
	pub source: Option<String>,
	pub note: String,
}

impl Rule {
	/// `tcp 5432 from 10.0.0.0/24`, as one short phrase.
	#[must_use]
	pub fn summary(&self) -> String {
		match &self.source {
			Some(s) => format!("{} {} from {s}", self.proto, self.port),
			None => format!("{} {} from anywhere", self.proto, self.port),
		}
	}
}

/// What a new allow is asked with, as the form sends it.
#[derive(Debug, Clone, Default)]
pub struct RuleInput {
	pub proto: String,
	pub port: String,
	pub source: String,
	pub note: String,
}

/// A checked allow, in the form it is stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewRule {
	pub proto: Proto,
	pub port: u16,
	pub source: Option<String>,
	pub note: String,
}

impl RuleInput {
	/// Checks what the table cannot and puts the range in its normal form.
	///
	/// # Errors
	/// [`ServerError::Validation`] with a message the form can show.
	pub fn parse(&self) -> Result<NewRule, ServerError> {
		let bad = |m: &str| Err(ServerError::Validation(m.to_owned()));
		let proto = match self.proto.trim() {
			"tcp" | "" => Proto::Tcp,
			"udp" => Proto::Udp,
			_ => return bad("The protocol is TCP or UDP."),
		};
		let port = match self.port.trim().parse::<u16>() {
			Ok(p) if p > 0 => p,
			_ => return bad("The port is a number from 1 to 65535."),
		};
		let source = match self.source.trim() {
			"" => None,
			raw => match Cidr::parse(raw) {
				Ok(c) => Some(c.to_string()),
				Err(_) => return bad("The source is an address or a range such as 10.0.0.0/24."),
			},
		};
		let note = self.note.trim();
		if note.chars().count() > MAX_NOTE {
			return bad("The note can hold 80 characters.");
		}
		if note.chars().any(char::is_control) {
			return bad("The note cannot hold control characters.");
		}
		Ok(NewRule {
			proto,
			port,
			source,
			note: note.to_owned(),
		})
	}
}

/// Checks and normalizes the ranges SSH is limited to (one per line, or separated by commas or
/// spaces).
///
/// # Errors
/// [`ServerError::Validation`] naming the first range that does not parse.
pub fn parse_ssh_sources(raw: &str) -> Result<Vec<String>, ServerError> {
	let mut out: Vec<String> = Vec::new();
	for part in raw
		.split(|c: char| c == ',' || c.is_whitespace())
		.filter(|s| !s.is_empty())
	{
		let cidr = Cidr::parse(part)
			.map_err(|_| ServerError::Validation(format!("{part} is not an address or a range")))?;
		let cidr = cidr.to_string();
		if !out.contains(&cidr) {
			out.push(cidr);
		}
	}
	if out.len() > podkit_firewall::policy::MAX_SSH_SOURCES {
		return Err(ServerError::Validation(format!(
			"SSH can be limited to {} ranges",
			podkit_firewall::policy::MAX_SSH_SOURCES
		)));
	}
	Ok(out)
}

/// The helper's policy for a server: its SSH port, the SSH ranges and the allows.
///
/// # Errors
/// [`ServerError::Validation`] when a stored value does not fit the helper's types (the table
/// and [`RuleInput::parse`] keep that from happening).
pub fn policy(ssh_port: i32, settings: &Settings, rules: &[Rule]) -> Result<Policy, ServerError> {
	let bad = |what: &str| ServerError::Validation(format!("invalid stored firewall {what}"));
	let source = |s: &str| Source::try_from(s.to_owned()).map_err(|_| bad("range"));
	let mut allows = Vec::with_capacity(rules.len());
	for r in rules {
		allows.push(Allow {
			proto: if r.proto == "udp" {
				Proto::Udp
			} else {
				Proto::Tcp
			},
			port: u16::try_from(r.port).map_err(|_| bad("port"))?,
			source: r.source.as_deref().map(source).transpose()?,
		});
	}
	Ok(Policy {
		ssh_port: u16::try_from(ssh_port).map_err(|_| bad("SSH port"))?,
		ssh_sources: settings
			.ssh_sources
			.iter()
			.map(|s| source(s))
			.collect::<Result<_, _>>()?,
		allows,
	})
}

/// The server's settings; the defaults when it has no row (the firewall off).
pub async fn settings(db: impl PgExecutor<'_>, server: ServerId) -> Result<Settings, sqlx::Error> {
	Ok(sqlx::query_as::<_, Settings>(
		"SELECT enabled, ssh_sources, install_helper FROM server_firewalls WHERE server_id = $1",
	)
	.bind(server.0)
	.fetch_optional(db)
	.await?
	.unwrap_or_default())
}

/// The server's explicit allows, in the order they were added.
pub async fn rules(db: impl PgExecutor<'_>, server: ServerId) -> Result<Vec<Rule>, sqlx::Error> {
	sqlx::query_as::<_, Rule>(
		"SELECT id, proto, port, source, note FROM server_firewall_rules \
		 WHERE server_id = $1 ORDER BY created_at, id",
	)
	.bind(server.0)
	.fetch_all(db)
	.await
}

/// How many allows the server has.
pub async fn rule_count(db: impl PgExecutor<'_>, server: ServerId) -> Result<i64, sqlx::Error> {
	sqlx::query_scalar("SELECT COUNT(*) FROM server_firewall_rules WHERE server_id = $1")
		.bind(server.0)
		.fetch_one(db)
		.await
}

/// Adds an allow; a second one of the same protocol, port and source is a conflict.
pub async fn add_rule(
	db: impl PgExecutor<'_>,
	server: ServerId,
	rule: &NewRule,
) -> Result<FirewallRuleId, ServerError> {
	let id = FirewallRuleId::generate();
	sqlx::query(
		"INSERT INTO server_firewall_rules (id, server_id, proto, port, source, note) \
		 VALUES ($1, $2, $3, $4, $5, $6)",
	)
	.bind(id.0)
	.bind(server.0)
	.bind(rule.proto.as_str())
	.bind(i32::from(rule.port))
	.bind(&rule.source)
	.bind(&rule.note)
	.execute(db)
	.await
	.map_err(|e| {
		if crate::db::is_unique_violation(&e, UNIQUE) {
			ServerError::Conflict("That allow is already in the list.".to_owned())
		} else {
			e.into()
		}
	})?;
	Ok(id)
}

/// Removes an allow of the server; `None` when there was none with that id.
pub async fn delete_rule(
	db: impl PgExecutor<'_>,
	server: ServerId,
	id: FirewallRuleId,
) -> Result<Option<Rule>, sqlx::Error> {
	sqlx::query_as::<_, Rule>(
		"DELETE FROM server_firewall_rules WHERE id = $1 AND server_id = $2 \
		 RETURNING id, proto, port, source, note",
	)
	.bind(id.0)
	.bind(server.0)
	.fetch_optional(db)
	.await
}

/// Stores the SSH ranges, keeping the enabled flag.
pub async fn set_ssh_sources(
	db: impl PgExecutor<'_>,
	server: ServerId,
	sources: &[String],
) -> Result<(), sqlx::Error> {
	sqlx::query(
		"INSERT INTO server_firewalls (server_id, ssh_sources) VALUES ($1, $2) \
		 ON CONFLICT (server_id) DO UPDATE SET ssh_sources = $2, updated_at = NOW()",
	)
	.bind(server.0)
	.bind(sources)
	.execute(db)
	.await?;
	Ok(())
}

/// Asks the next bootstrap to install the helper (or not).
pub async fn set_install_helper(
	db: impl PgExecutor<'_>,
	server: ServerId,
	wanted: bool,
) -> Result<(), sqlx::Error> {
	sqlx::query(
		"INSERT INTO server_firewalls (server_id, install_helper) VALUES ($1, $2) \
		 ON CONFLICT (server_id) DO UPDATE SET install_helper = $2, updated_at = NOW()",
	)
	.bind(server.0)
	.bind(wanted)
	.execute(db)
	.await?;
	Ok(())
}

/// Flips the enabled flag, keeping the ranges.
pub async fn set_enabled(
	db: impl PgExecutor<'_>,
	server: ServerId,
	enabled: bool,
) -> Result<(), sqlx::Error> {
	sqlx::query(
		"INSERT INTO server_firewalls (server_id, enabled) VALUES ($1, $2) \
		 ON CONFLICT (server_id) DO UPDATE SET enabled = $2, updated_at = NOW()",
	)
	.bind(server.0)
	.bind(enabled)
	.execute(db)
	.await?;
	Ok(())
}
