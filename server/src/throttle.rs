//! Throttling of sign-in guesses (SEC-01): passwords, second-factor codes, recovery codes,
//! passkey assertions, the password that links a provider account, and wrong API tokens.
//!
//! One wrong guess is one row in `auth_failures` (migration 0072), kept in Postgres so a
//! restart or a second replica sees the same counters. Rows of the last [`WINDOW`] are
//! counted per *subject*:
//!
//! - **the account** (`u:<id>`): [`ACCOUNT_LIMIT`] failures lock it. A known account and an
//!   email nobody has (`e:<hash>`) behave the same, so the answer never says which exist.
//! - **the client address** (`ip:<addr>`): [`IP_LIMIT`] failures across any accounts stop
//!   spraying one password over many emails.
//! - **the client address for tokens** (`tip:<addr>`): wrong API tokens, kept apart so a
//!   script with a revoked token cannot lock its owner out of the sign-in page.
//!
//! At the limit a wait of one second is asked for, then two, four, … up to [`MAX_WAIT`]
//! (counted from the latest failure). While waiting, a request is refused without looking at
//! the password or code, so even the right one fails. A full sign-in clears the account's
//! rows (never the address's: signing in as yourself must not reset a spray).
//!
//! The wrong guess is booked *before* it is checked ([`begin`]) under an advisory lock, so a
//! flood of parallel requests cannot all slip in ahead of the first failure. A guess that
//! turns out right gives its row back with [`Attempt::ok`].

use std::collections::HashMap;
use std::fmt::Write as _;

use sha2::{Digest, Sha256};
use sqlx::PgPool;
use time::{Duration, OffsetDateTime};

use crate::audit::{self, Diff, Target};
use crate::error::{AppResult, ServerError};
use crate::ids::UserId;
use crate::notifications::{self, Event, EventKind};

/// How far back a failure counts.
pub const WINDOW: Duration = Duration::minutes(15);
/// Failures per account in the window before a wait is asked for.
pub const ACCOUNT_LIMIT: i64 = 5;
/// Failures per client address in the window, over all accounts.
pub const IP_LIMIT: i64 = 30;
/// Wrong API tokens per client address in the window.
pub const TOKEN_LIMIT: i64 = 30;
/// The longest wait, in seconds.
pub const MAX_WAIT: i64 = 15 * 60;

/// Where the request came from when nobody can tell (in-process calls).
const UNKNOWN_IP: &str = "unknown";

/// The wait after `count` failures against `limit`: none below the limit, then 1 s, 2 s,
/// 4 s … capped at [`MAX_WAIT`].
fn wait_after(count: i64, limit: i64) -> i64 {
	if count < limit {
		return 0;
	}
	let exponent = u32::try_from((count - limit).min(20)).unwrap_or(20);
	(1_i64 << exponent).min(MAX_WAIT)
}

/// "N seconds" or "N minutes", for the message a person reads.
pub fn describe(secs: u64) -> String {
	if secs < 60 {
		let n = secs.max(1);
		format!("{n} second{}", if n == 1 { "" } else { "s" })
	} else {
		let n = secs.div_ceil(60);
		format!("{n} minute{}", if n == 1 { "" } else { "s" })
	}
}

/// Whose guess it is, as far as the account goes.
#[derive(Debug, Clone)]
pub enum Who {
	/// An account that exists.
	User(UserId),
	/// An email that matched no account.
	Email(String),
}

impl Who {
	/// For an email typed at sign-in: the account when there is one, else the email itself.
	pub fn of_email(user: Option<UserId>, email: &str) -> Self {
		user.map_or_else(|| Self::Email(email.trim().to_lowercase()), Self::User)
	}

	fn subject(&self) -> String {
		match self {
			Self::User(id) => format!("u:{}", id.0),
			Self::Email(email) => {
				let digest = Sha256::digest(email.as_bytes());
				let mut subject = String::from("e:");
				for byte in digest.iter().take(16) {
					let _ = write!(subject, "{byte:02x}");
				}
				subject
			}
		}
	}

	const fn user(&self) -> Option<UserId> {
		match self {
			Self::User(id) => Some(*id),
			Self::Email(_) => None,
		}
	}
}

/// The address of whoever is making the request being served (the shared helper in `audit`).
fn client() -> String {
	audit::origin().0.unwrap_or_else(|| UNKNOWN_IP.to_owned())
}

fn too_many(wait: i64) -> ServerError {
	ServerError::TooManyAttempts {
		retry_after: u64::try_from(wait).unwrap_or(1).max(1),
	}
}

/// Seconds still to wait for `subject`, if it is locked at `now`.
async fn remaining(
	db: impl sqlx::PgExecutor<'_>,
	subject: &str,
	limit: i64,
	now: OffsetDateTime,
) -> Result<Option<i64>, sqlx::Error> {
	let (count, last) = sqlx::query_as::<_, (i64, Option<OffsetDateTime>)>(
		"SELECT COUNT(*), MAX(at) FROM auth_failures WHERE subject = $1 AND at > $2",
	)
	.bind(subject)
	.bind(now - WINDOW)
	.fetch_one(db)
	.await?;
	let wait = wait_after(count, limit);
	let Some(last) = last.filter(|_| wait > 0) else {
		return Ok(None);
	};
	let left = (last + Duration::seconds(wait) - now)
		.as_seconds_f64()
		.ceil();
	#[allow(clippy::cast_possible_truncation)]
	let left = left as i64;
	Ok((left > 0).then_some(left))
}

/// A guess that has been booked and not yet decided.
#[must_use = "say whether the guess was right with ok() or fail()"]
pub struct Attempt {
	db: PgPool,
	who: Who,
	ip: String,
	rows: Vec<i64>,
}

/// Books a guess for `who` from the current request's address, or refuses it with
/// [`ServerError::TooManyAttempts`] when the account or the address is waiting.
pub async fn begin(db: &PgPool, who: Who) -> AppResult<Attempt> {
	begin_at(db, who, client(), OffsetDateTime::now_utc()).await
}

async fn begin_at(db: &PgPool, who: Who, ip: String, now: OffsetDateTime) -> AppResult<Attempt> {
	let subjects = [
		(who.subject(), ACCOUNT_LIMIT),
		(format!("ip:{ip}"), IP_LIMIT),
	];
	let mut order = [0, 1];
	if subjects[0].0 > subjects[1].0 {
		order = [1, 0];
	}
	let mut tx = db.begin().await?;
	// Always in the same order, so two requests cannot wait on each other.
	for i in order {
		sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
			.bind(&subjects[i].0)
			.execute(&mut *tx)
			.await?;
	}
	let mut wait = 0;
	for (subject, limit) in &subjects {
		sqlx::query("DELETE FROM auth_failures WHERE subject = $1 AND at <= $2")
			.bind(subject)
			.bind(now - WINDOW)
			.execute(&mut *tx)
			.await?;
		wait = wait.max(
			remaining(&mut *tx, subject, *limit, now)
				.await?
				.unwrap_or(0),
		);
	}
	if wait > 0 {
		tx.rollback().await?;
		return Err(too_many(wait));
	}
	let mut rows = Vec::new();
	for (subject, _) in &subjects {
		let id = sqlx::query_scalar::<_, i64>(
			"INSERT INTO auth_failures (subject, at) VALUES ($1, $2) RETURNING id",
		)
		.bind(subject)
		.bind(now)
		.fetch_one(&mut *tx)
		.await?;
		rows.push(id);
	}
	tx.commit().await?;
	Ok(Attempt {
		db: db.clone(),
		who,
		ip,
		rows,
	})
}

impl Attempt {
	/// The guess was right: its row is given back.
	pub async fn ok(self) {
		let gone = sqlx::query("DELETE FROM auth_failures WHERE id = ANY($1)")
			.bind(&self.rows)
			.execute(&self.db)
			.await;
		if let Err(e) = gone {
			tracing::warn!("cannot release a sign-in attempt: {e}");
		}
	}

	/// The guess was right or wrong, as `ok` says.
	pub async fn finish(self, ok: bool) {
		if ok {
			self.ok().await;
		} else {
			self.fail().await;
		}
	}

	/// The guess was wrong: its row stays. When it is the one that locks the account, the
	/// audit log and the account's teams hear of it.
	pub async fn fail(self) {
		self.fail_at(OffsetDateTime::now_utc()).await;
	}

	/// [`Attempt::fail`] at a given time. The wait counts from here, not from when the guess
	/// was booked: checking a password takes a while and must not eat into it.
	async fn fail_at(self, now: OffsetDateTime) {
		let stamped = sqlx::query("UPDATE auth_failures SET at = $2 WHERE id = ANY($1)")
			.bind(&self.rows)
			.bind(now)
			.execute(&self.db)
			.await;
		if let Err(e) = stamped {
			tracing::warn!("cannot stamp a wrong sign-in guess: {e}");
		}
		let counted = |subject: String| {
			let db = self.db.clone();
			async move {
				sqlx::query_scalar::<_, i64>(
					"SELECT COUNT(*) FROM auth_failures WHERE subject = $1 AND at > $2",
				)
				.bind(subject)
				.bind(now - WINDOW)
				.fetch_one(&db)
				.await
			}
		};
		if let Ok(n) = counted(format!("ip:{}", self.ip)).await
			&& n == IP_LIMIT
		{
			tracing::warn!(
				ip = %self.ip,
				"{IP_LIMIT} wrong sign-in guesses from one address; it must wait now"
			);
		}
		let Some(user) = self.who.user() else {
			return;
		};
		if let Ok(n) = counted(self.who.subject()).await
			&& n == ACCOUNT_LIMIT
		{
			locked(&self.db, user, &self.ip).await;
		}
	}
}

/// Tells the audit log and the teams of `user` that the account is locked.
async fn locked(db: &PgPool, user: UserId, ip: &str) {
	let wait = describe(u64::try_from(wait_after(ACCOUNT_LIMIT, ACCOUNT_LIMIT)).unwrap_or(1));
	crate::auth::record_everywhere(
		db,
		user,
		"session.locked",
		Target::none("session").named("(too many attempts)"),
		Diff::new().set("failures", ACCOUNT_LIMIT),
	)
	.await;
	let email = sqlx::query_scalar::<_, String>("SELECT email FROM users WHERE id = $1")
		.bind(user.0)
		.fetch_optional(db)
		.await
		.ok()
		.flatten()
		.unwrap_or_default();
	let teams = match crate::teams::list_for_user(db, user).await {
		Ok(teams) => teams,
		Err(e) => {
			tracing::warn!("cannot list teams for a lockout notice: {e}");
			return;
		}
	};
	for team in teams {
		let event = Event::new(
			team.id,
			EventKind::SignInLocked,
			format!("user:{}", user.0),
			"Sign-in locked after repeated wrong attempts",
		)
		.body(format!(
			"The account {email} had {ACCOUNT_LIMIT} wrong sign-in attempts in {} minutes (latest from {ip}) and now has to wait, starting at {wait} and growing. A team admin can clear it under Members.",
			WINDOW.whole_minutes()
		))
		.link(format!("/teams/{}/members", team.id));
		notifications::emit_db(db, event).await;
	}
}

/// A full sign-in: the account's counters start over. The address's do not.
pub async fn clear_account(db: &PgPool, user: UserId) {
	let gone = sqlx::query("DELETE FROM auth_failures WHERE subject = $1")
		.bind(Who::User(user).subject())
		.execute(db)
		.await;
	if let Err(e) = gone {
		tracing::warn!("cannot clear sign-in failures: {e}");
	}
}

/// How long each of `users` still has to wait; accounts that are free are left out.
pub async fn locked_users(
	db: &PgPool,
	users: &[UserId],
) -> Result<HashMap<UserId, u64>, sqlx::Error> {
	let now = OffsetDateTime::now_utc();
	let mut out = HashMap::new();
	for user in users {
		if let Some(left) = remaining(db, &Who::User(*user).subject(), ACCOUNT_LIMIT, now).await? {
			out.insert(*user, u64::try_from(left).unwrap_or(1));
		}
	}
	Ok(out)
}

/// Refuses a token lookup from an address that has sent too many wrong tokens.
pub async fn check_token(db: &PgPool) -> AppResult<()> {
	let subject = format!("tip:{}", client());
	match remaining(db, &subject, TOKEN_LIMIT, OffsetDateTime::now_utc()).await? {
		Some(wait) => Err(too_many(wait)),
		None => Ok(()),
	}
}

/// Books a wrong API token against the current request's address.
pub async fn token_failed(db: &PgPool) {
	let booked = sqlx::query("INSERT INTO auth_failures (subject, at) VALUES ($1, NOW())")
		.bind(format!("tip:{}", client()))
		.execute(db)
		.await;
	if let Err(e) = booked {
		tracing::warn!("cannot book a wrong API token: {e}");
	}
}

/// Drops rows past the window; hourly, from the maintenance loop.
pub async fn purge_expired(db: &PgPool) -> Result<u64, sqlx::Error> {
	Ok(sqlx::query("DELETE FROM auth_failures WHERE at <= $1")
		.bind(OffsetDateTime::now_utc() - WINDOW)
		.execute(db)
		.await?
		.rows_affected())
}

#[cfg(test)]
pub(crate) mod tests;
