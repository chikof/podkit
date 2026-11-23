//! Access rules: what Traefik does to the requests of an application before they reach it
//! (IP allow and deny lists, a rate limit, basic auth, a body size limit, the security
//! headers) and the "internal only" switch that takes the application off the public ingress.
//!
//! The rules are one row per application (`access_rules`) and its basic-auth users
//! (`access_users`). They are not applied live: the next deploy turns them into Traefik
//! labels (the agent writes the middlewares, see `agent/src/ingress.rs`), like every other
//! setting of an application.
//!
//! - [`Rules`] is the row; [`Rules::validate`] is what the table cannot check.
//! - [`service`] is what the web UI calls: authorize, scope, validate, store, audit.
//! - [`policy`] reads the other way: which resources a network connects, and which of them
//!   can be reached from outside the server.
//!
//! Basic-auth passwords are write-only. Only the bcrypt hash is stored, it goes into the
//! container's labels, and no page or response shows it.

pub mod policy;
pub mod service;

#[cfg(test)]
mod tests;

use proto::cidr::{self, Cidr};
use sqlx::PgExecutor;

use crate::error::ServerError;
use crate::ids::ApplicationId;

/// Most entries in one list: the allow list Traefik gets is the lists' difference, which
/// grows with every denied range.
pub const MAX_ENTRIES: usize = 50;
/// Most ranges the allow list sent to Traefik may be split into.
const MAX_RANGES: usize = 512;
/// Most basic-auth users of one application: they all travel in one container label.
pub const MAX_USERS: i64 = 20;

/// The access settings of one application.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct Rules {
	pub allow_cidrs: Vec<String>,
	pub deny_cidrs: Vec<String>,
	pub rate_average: Option<i32>,
	pub rate_burst: Option<i32>,
	pub max_body_mb: Option<i32>,
	pub security_headers: bool,
	/// No public router: reachable only on the application's networks.
	pub internal_only: bool,
}

impl Default for Rules {
	/// What an application without a row has: open, the headers on, routed.
	fn default() -> Self {
		Self {
			allow_cidrs: Vec::new(),
			deny_cidrs: Vec::new(),
			rate_average: None,
			rate_burst: None,
			max_body_mb: None,
			security_headers: true,
			internal_only: false,
		}
	}
}

impl Rules {
	/// Checks what the table cannot: every range parses (and is stored in its normal form),
	/// the lists are short, nobody is locked out entirely, a burst has a rate to go with.
	///
	/// # Errors
	/// [`ServerError::Validation`] with a message the form can show.
	pub fn validate(&mut self) -> Result<(), ServerError> {
		let bad = |m: String| Err(ServerError::Validation(m));
		for (list, what) in [
			(&mut self.allow_cidrs, "allow"),
			(&mut self.deny_cidrs, "deny"),
		] {
			if list.len() > MAX_ENTRIES {
				return bad(format!("The {what} list can hold {MAX_ENTRIES} entries"));
			}
			for entry in list.iter_mut() {
				*entry = Cidr::parse(entry)
					.map_err(ServerError::Validation)?
					.to_string();
			}
			list.sort();
			list.dedup();
		}
		if !self.allow_cidrs.is_empty() || !self.deny_cidrs.is_empty() {
			let ranges = cidr::effective_allow(&self.allow_cidrs, &self.deny_cidrs)
				.map_err(ServerError::Validation)?;
			if ranges.is_empty() {
				return bad(
					"These lists block every address, nobody could reach the application"
						.to_owned(),
				);
			}
			if ranges.len() > MAX_RANGES {
				return bad("The deny list cuts the allowed addresses into too many pieces; use fewer, wider ranges".to_owned());
			}
		}
		let within = |v: Option<i32>, high: i32, what: &str| match v {
			Some(n) if !(1..=high).contains(&n) => {
				bad(format!("{what} must be between 1 and {high}"))
			}
			_ => Ok(()),
		};
		within(self.rate_average, 100_000, "The rate limit")?;
		within(self.rate_burst, 100_000, "The burst")?;
		within(self.max_body_mb, 10_240, "The body size")?;
		if self.rate_burst.is_some() && self.rate_average.is_none() {
			return bad("A burst needs a rate limit; set the requests per second too".to_owned());
		}
		Ok(())
	}

	/// What the agent gets, or `None` when no middleware would be written. `users` are the
	/// `user:hash` entries of the basic-auth users.
	#[must_use]
	pub fn to_wire(&self, users: Vec<String>) -> Option<proto::AccessRules> {
		let count = |n: Option<i32>| n.and_then(|n| u32::try_from(n).ok());
		let wire = proto::AccessRules {
			allow: self.allow_cidrs.clone(),
			deny: self.deny_cidrs.clone(),
			rate_average: count(self.rate_average),
			rate_burst: count(self.rate_burst),
			basic_auth: users,
			security_headers: self.security_headers,
			max_body_bytes: self
				.max_body_mb
				.and_then(|n| u64::try_from(n).ok())
				.map(|mb| mb * 1024 * 1024),
		};
		(!wire.is_empty()).then_some(wire)
	}
}

/// What a deploy of the application needs from its access rules.
#[derive(Debug, Clone, Default)]
pub struct ForDeploy {
	/// Leave the public ingress out.
	pub internal_only: bool,
	/// The middlewares of the public routers.
	pub rules: Option<proto::AccessRules>,
}

/// The row of the application, if it has one.
pub async fn rules(
	db: impl PgExecutor<'_>,
	app: ApplicationId,
) -> Result<Option<Rules>, sqlx::Error> {
	sqlx::query_as::<_, Rules>(
		"SELECT allow_cidrs, deny_cidrs, rate_average, rate_burst, max_body_mb, security_headers, \
		 internal_only FROM access_rules WHERE application_id = $1",
	)
	.bind(app.0)
	.fetch_optional(db)
	.await
}

/// Stores the rules, keeping the internal-only switch (it has its own action).
pub async fn save_rules(
	db: impl PgExecutor<'_>,
	app: ApplicationId,
	r: &Rules,
) -> Result<(), sqlx::Error> {
	sqlx::query(
		"INSERT INTO access_rules (application_id, allow_cidrs, deny_cidrs, rate_average, rate_burst, \
		 max_body_mb, security_headers) VALUES ($1, $2, $3, $4, $5, $6, $7) \
		 ON CONFLICT (application_id) DO UPDATE SET allow_cidrs = $2, deny_cidrs = $3, \
		 rate_average = $4, rate_burst = $5, max_body_mb = $6, security_headers = $7, updated_at = NOW()",
	)
	.bind(app.0)
	.bind(&r.allow_cidrs)
	.bind(&r.deny_cidrs)
	.bind(r.rate_average)
	.bind(r.rate_burst)
	.bind(r.max_body_mb)
	.bind(r.security_headers)
	.execute(db)
	.await?;
	Ok(())
}

/// Flips the internal-only switch, keeping everything else.
pub async fn set_internal(
	db: impl PgExecutor<'_>,
	app: ApplicationId,
	internal: bool,
) -> Result<(), sqlx::Error> {
	sqlx::query(
		"INSERT INTO access_rules (application_id, internal_only) VALUES ($1, $2) \
		 ON CONFLICT (application_id) DO UPDATE SET internal_only = $2, updated_at = NOW()",
	)
	.bind(app.0)
	.bind(internal)
	.execute(db)
	.await?;
	Ok(())
}

/// The usernames of the application's basic-auth users, never their hashes.
pub async fn usernames(
	db: impl PgExecutor<'_>,
	app: ApplicationId,
) -> Result<Vec<String>, sqlx::Error> {
	sqlx::query_scalar(
		"SELECT username FROM access_users WHERE application_id = $1 ORDER BY username",
	)
	.bind(app.0)
	.fetch_all(db)
	.await
}

/// How many basic-auth users the application has.
pub async fn user_count(db: impl PgExecutor<'_>, app: ApplicationId) -> Result<i64, sqlx::Error> {
	sqlx::query_scalar("SELECT COUNT(*) FROM access_users WHERE application_id = $1")
		.bind(app.0)
		.fetch_one(db)
		.await
}

/// Adds a user, or replaces the hash of one with that name.
pub async fn upsert_user(
	db: impl PgExecutor<'_>,
	app: ApplicationId,
	username: &str,
	hash: &str,
) -> Result<(), sqlx::Error> {
	sqlx::query(
		"INSERT INTO access_users (application_id, username, password_hash) VALUES ($1, $2, $3) \
		 ON CONFLICT (application_id, username) DO UPDATE SET password_hash = $3",
	)
	.bind(app.0)
	.bind(username)
	.bind(hash)
	.execute(db)
	.await?;
	Ok(())
}

/// Removes a user; `false` when there was none of that name.
pub async fn delete_user(
	db: impl PgExecutor<'_>,
	app: ApplicationId,
	username: &str,
) -> Result<bool, sqlx::Error> {
	let done = sqlx::query("DELETE FROM access_users WHERE application_id = $1 AND username = $2")
		.bind(app.0)
		.bind(username)
		.execute(db)
		.await?;
	Ok(done.rows_affected() > 0)
}

/// The `user:hash` entries Traefik's basic-auth middleware takes.
async fn user_entries(
	db: impl PgExecutor<'_>,
	app: ApplicationId,
) -> Result<Vec<String>, sqlx::Error> {
	let rows: Vec<(String, String)> = sqlx::query_as(
		"SELECT username, password_hash FROM access_users WHERE application_id = $1 ORDER BY username",
	)
	.bind(app.0)
	.fetch_all(db)
	.await?;
	Ok(rows.into_iter().map(|(u, h)| format!("{u}:{h}")).collect())
}

/// What a deploy of `app` writes: whether it is on the ingress and the middlewares of its
/// routers. An application without a row gets the security headers only.
pub async fn for_deploy(db: &sqlx::PgPool, app: ApplicationId) -> Result<ForDeploy, sqlx::Error> {
	let rules = rules(db, app).await?.unwrap_or_default();
	let users = user_entries(db, app).await?;
	Ok(ForDeploy {
		internal_only: rules.internal_only,
		rules: rules.to_wire(users),
	})
}
