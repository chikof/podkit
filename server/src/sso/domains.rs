//! Email domains of a team's identity provider.
//!
//! A provider a team administrator set up may create accounts (just in time) only for email
//! addresses on a domain **an instance administrator approved** for it. Without that, a team
//! administrator who runs their own identity provider could sign in as any email nobody has
//! registered yet and so lock the real person out (SEC-07).
//!
//! The smallest safe check: the team administrator lists the domains; each waits for the
//! approval of the instance administrator (the oldest account, who on a homelab is usually
//! the same person). A domain the instance administrator lists themselves is approved on the
//! spot. Instance providers are the administrator's own and have no list. Linking an
//! existing account is unaffected: it always needs that account's password.

use sqlx::PgPool;

use super::providers::{Scope, is_instance_admin};
use crate::audit::{Diff, Target};
use crate::auth::record_everywhere;
use crate::error::{AppResult, ServerError};
use crate::ids::{AuthProviderId, TeamId, UserId};
use crate::state::AppState;

const MAX_DOMAINS: usize = 20;
const MAX_DOMAIN_LEN: usize = 253;

/// One domain of a provider and whether it was approved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Domain {
	pub name: String,
	pub approved: bool,
}

/// A domain waiting for the instance administrator.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Pending {
	pub provider_id: i64,
	pub provider: String,
	pub team: String,
	pub domain: String,
}

fn bad(message: impl Into<String>) -> ServerError {
	ServerError::Validation(message.into())
}

/// The domain of an email address, lower-cased.
#[must_use]
pub fn of_email(email: &str) -> Option<String> {
	let (_, domain) = email.trim().rsplit_once('@')?;
	let domain = domain.trim().trim_end_matches('.').to_ascii_lowercase();
	(!domain.is_empty()).then_some(domain)
}

fn valid(domain: &str) -> bool {
	domain.len() <= MAX_DOMAIN_LEN
		&& domain.contains('.')
		&& domain.split('.').all(|label| {
			!label.is_empty()
				&& label.len() <= 63
				&& !label.starts_with('-')
				&& !label.ends_with('-')
				&& label
					.bytes()
					.all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
		})
}

/// `example.com, corp.example` (or one per line) as a clean list.
///
/// # Errors
/// `Validation` naming the entry that is not a domain, or too many.
pub fn parse(raw: &str) -> AppResult<Vec<String>> {
	let mut out: Vec<String> = Vec::new();
	for part in raw.split(|c: char| c == ',' || c.is_whitespace()) {
		let domain = part.trim().trim_start_matches('@').to_ascii_lowercase();
		if domain.is_empty() {
			continue;
		}
		if !valid(&domain) {
			return Err(bad(format!(
				"{domain:?} is not an email domain like example.com."
			)));
		}
		if !out.contains(&domain) {
			out.push(domain);
		}
	}
	if out.len() > MAX_DOMAINS {
		return Err(bad(format!(
			"A provider has at most {MAX_DOMAINS} domains."
		)));
	}
	Ok(out)
}

/// The domains of `provider`, approved or not.
///
/// # Errors
/// The database failed.
pub async fn list(db: &PgPool, provider: AuthProviderId) -> Result<Vec<Domain>, sqlx::Error> {
	let rows: Vec<(String, bool)> = sqlx::query_as(
		"SELECT domain, approved_at IS NOT NULL FROM auth_provider_domains
		WHERE provider_id = $1 ORDER BY domain",
	)
	.bind(provider.0)
	.fetch_all(db)
	.await?;
	Ok(rows
		.into_iter()
		.map(|(name, approved)| Domain { name, approved })
		.collect())
}

/// The domains as the form shows them, one per line.
#[must_use]
pub fn text(domains: &[Domain]) -> String {
	domains
		.iter()
		.map(|d| d.name.as_str())
		.collect::<Vec<_>>()
		.join("\n")
}

/// Whether `provider` may create an account for `email`: instance providers always; a team's
/// only for an approved domain.
///
/// # Errors
/// The database failed.
pub async fn allows_new_account(
	db: &PgPool,
	provider: AuthProviderId,
	team: Option<TeamId>,
	email: &str,
) -> Result<bool, sqlx::Error> {
	if team.is_none() {
		return Ok(true);
	}
	let Some(domain) = of_email(email) else {
		return Ok(false);
	};
	sqlx::query_scalar(
		"SELECT EXISTS (SELECT 1 FROM auth_provider_domains
			WHERE provider_id = $1 AND domain = $2 AND approved_at IS NOT NULL)",
	)
	.bind(provider.0)
	.bind(domain)
	.fetch_one(db)
	.await
}

/// Replaces the domains of a team provider with `raw`. A domain that was approved stays so;
/// a new one is approved at once when `user` is the instance administrator, else it waits.
/// Does nothing for an instance provider. The caller has checked `user` may manage the
/// provider.
///
/// # Errors
/// `Validation` for a bad domain; the database failed.
pub async fn set(
	s: &AppState,
	user: UserId,
	scope: Scope,
	provider: AuthProviderId,
	raw: &str,
) -> AppResult<Vec<Domain>> {
	if !matches!(scope, Scope::Team(_)) {
		return Ok(Vec::new());
	}
	let wanted = parse(raw)?;
	let admin = is_instance_admin(&s.db, user).await?;
	let mut tx = s.db.begin().await?;
	sqlx::query("DELETE FROM auth_provider_domains WHERE provider_id = $1 AND domain <> ALL($2)")
		.bind(provider.0)
		.bind(&wanted)
		.execute(&mut *tx)
		.await?;
	for domain in &wanted {
		sqlx::query(
			"INSERT INTO auth_provider_domains (provider_id, domain, approved_at, approved_by)
			VALUES ($1, $2, CASE WHEN $3 THEN NOW() END, CASE WHEN $3 THEN $4 END)
			ON CONFLICT (provider_id, domain) DO UPDATE SET
				approved_at = COALESCE(auth_provider_domains.approved_at,
					CASE WHEN $3 THEN NOW() END),
				approved_by = COALESCE(auth_provider_domains.approved_by,
					CASE WHEN $3 THEN $4 END)",
		)
		.bind(provider.0)
		.bind(domain)
		.bind(admin)
		.bind(user.0)
		.execute(&mut *tx)
		.await?;
	}
	tx.commit().await?;
	Ok(list(&s.db, provider).await?)
}

/// The domains of team providers waiting for approval: the instance administrator's list.
///
/// # Errors
/// `Forbidden` for anyone else.
pub async fn pending(db: &PgPool, user: UserId) -> AppResult<Vec<Pending>> {
	if !is_instance_admin(db, user).await? {
		return Err(ServerError::Forbidden);
	}
	Ok(sqlx::query_as(
		"SELECT d.provider_id, p.name AS provider, t.name AS team, d.domain
		FROM auth_provider_domains d
		JOIN auth_providers p ON p.id = d.provider_id
		JOIN teams t ON t.id = p.team_id
		WHERE d.approved_at IS NULL ORDER BY t.name, p.name, d.domain",
	)
	.fetch_all(db)
	.await?)
}

/// The instance administrator approves one domain of a team's provider.
///
/// # Errors
/// `Forbidden` for anyone else; `NotFound` for a domain that is not waiting.
pub async fn approve(
	s: &AppState,
	user: UserId,
	provider: AuthProviderId,
	domain: &str,
) -> AppResult<()> {
	if !is_instance_admin(&s.db, user).await? {
		return Err(ServerError::Forbidden);
	}
	let name: Option<(String,)> = sqlx::query_as(
		"UPDATE auth_provider_domains d SET approved_at = NOW(), approved_by = $3
		FROM auth_providers p
		WHERE d.provider_id = $1 AND d.domain = $2 AND d.approved_at IS NULL
			AND p.id = d.provider_id RETURNING p.name",
	)
	.bind(provider.0)
	.bind(domain.trim().to_ascii_lowercase())
	.bind(user.0)
	.fetch_optional(&s.db)
	.await?;
	let (name,) = name.ok_or(ServerError::NotFound)?;
	record_everywhere(
		&s.db,
		user,
		"auth_provider.domain_approve",
		Target::new("auth_provider", provider.0).named(name),
		Diff::new().set("domain", domain.trim().to_ascii_lowercase()),
	)
	.await;
	Ok(())
}
