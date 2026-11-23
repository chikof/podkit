//! Short-lived state between two requests of one sign-in: the OIDC round trip, the wait for
//! a second factor, the wait for a password that links an account, a `WebAuthn` ceremony.
//!
//! The browser holds a random token (in a cookie); only its SHA-256 digest is stored, like a
//! session's. A flow is single use: [`take`] deletes it as it reads it.

use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx::PgExecutor;
use time::{Duration, OffsetDateTime};

use crate::ids::{AuthProviderId, UserId};

/// What a flow is waiting for. A flow of one kind never answers a lookup for another, so a
/// token from one step cannot be played in a different one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
	/// Sent to an identity provider; waiting for its callback.
	Oidc,
	/// The provider knows an email an account has; waiting for that account's password.
	Link,
	/// The password was right; waiting for the second factor.
	SecondFactor,
	/// A passkey sign-in challenge is out.
	PasskeyLogin,
	/// A passkey registration challenge is out.
	PasskeyEnroll,
}

impl Kind {
	const fn as_str(self) -> &'static str {
		match self {
			Self::Oidc => "oidc",
			Self::Link => "link",
			Self::SecondFactor => "second_factor",
			Self::PasskeyLogin => "passkey_login",
			Self::PasskeyEnroll => "passkey_enroll",
		}
	}
}

/// Wrong guesses a second-factor or link flow tolerates before it is dropped.
pub const MAX_ATTEMPTS: i32 = 5;

/// How long a person has to finish the step.
pub const TTL: Duration = Duration::minutes(10);

#[derive(Debug, Clone)]
pub struct Flow {
	pub user: Option<UserId>,
	pub provider: Option<AuthProviderId>,
	pub data: Value,
}

fn digest(token: &str) -> Vec<u8> {
	Sha256::digest(token.as_bytes()).to_vec()
}

/// Opens a flow and returns its token, to be handed to the browser.
pub async fn start(
	db: impl PgExecutor<'_>,
	kind: Kind,
	user: Option<UserId>,
	provider: Option<AuthProviderId>,
	data: &Value,
) -> Result<String, sqlx::Error> {
	let token = crypto::generate_token(32);
	sqlx::query!(
		"INSERT INTO auth_flows (token_hash, kind, user_id, provider_id, data, expires_at)
		VALUES ($1, $2, $3, $4, $5, $6)",
		digest(&token),
		kind.as_str(),
		user.map(|u| u.0),
		provider.map(|p| p.0),
		data,
		OffsetDateTime::now_utc() + TTL
	)
	.execute(db)
	.await?;
	Ok(token)
}

struct Row {
	user_id: Option<i64>,
	provider_id: Option<i64>,
	data: Value,
}

impl From<Row> for Flow {
	fn from(r: Row) -> Self {
		Self {
			user: r.user_id.map(UserId),
			provider: r.provider_id.map(AuthProviderId),
			data: r.data,
		}
	}
}

/// Reads and deletes the flow in one step: a token works once.
pub async fn take(
	db: impl PgExecutor<'_>,
	kind: Kind,
	token: &str,
) -> Result<Option<Flow>, sqlx::Error> {
	Ok(sqlx::query_as!(
		Row,
		"DELETE FROM auth_flows WHERE token_hash = $1 AND kind = $2 AND expires_at > NOW()
		RETURNING user_id, provider_id, data",
		digest(token),
		kind.as_str()
	)
	.fetch_optional(db)
	.await?
	.map(Flow::from))
}

/// Reads the flow and leaves it for a later step.
pub async fn peek(
	db: impl PgExecutor<'_>,
	kind: Kind,
	token: &str,
) -> Result<Option<Flow>, sqlx::Error> {
	Ok(sqlx::query_as!(
		Row,
		"SELECT user_id, provider_id, data FROM auth_flows
		WHERE token_hash = $1 AND kind = $2 AND expires_at > NOW()",
		digest(token),
		kind.as_str()
	)
	.fetch_optional(db)
	.await?
	.map(Flow::from))
}

/// Counts one wrong guess. Past [`MAX_ATTEMPTS`] the flow is deleted, so the person has to
/// start again with the password. Returns whether the flow is still open.
pub async fn miss(db: impl PgExecutor<'_> + Copy, token: &str) -> Result<bool, sqlx::Error> {
	let attempts = sqlx::query_scalar!(
		"UPDATE auth_flows SET attempts = attempts + 1 WHERE token_hash = $1 RETURNING attempts",
		digest(token)
	)
	.fetch_optional(db)
	.await?;
	if attempts.is_some_and(|n| n >= MAX_ATTEMPTS) {
		forget(db, token).await?;
		return Ok(false);
	}
	Ok(attempts.is_some())
}

pub async fn forget(db: impl PgExecutor<'_>, token: &str) -> Result<(), sqlx::Error> {
	sqlx::query!(
		"DELETE FROM auth_flows WHERE token_hash = $1",
		digest(token)
	)
	.execute(db)
	.await?;
	Ok(())
}

/// Deletes flows nobody finished and returns how many.
pub async fn purge_expired(db: impl PgExecutor<'_>) -> Result<u64, sqlx::Error> {
	Ok(
		sqlx::query!("DELETE FROM auth_flows WHERE expires_at <= NOW()")
			.execute(db)
			.await?
			.rows_affected(),
	)
}
