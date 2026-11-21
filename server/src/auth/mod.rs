//! Auth slice: opaque sessions, permission checks, users, and the
//! register/login/logout routes.

use axum::{RequestPartsExt, extract::FromRequestParts, http::request::Parts};
use axum_extra::{
	TypedHeader,
	headers::{Authorization, authorization::Bearer},
};
use tower_cookies::Cookies;

use crate::error::ServerError;
use crate::ids::UserId;
use crate::state::AppState;

mod events;
pub mod flow;
pub mod origin;
pub mod permission;
mod routes;
pub mod service;
pub mod session;
pub mod tokens;

pub use events::record_everywhere;
pub use routes::{
	Credentials, log_in, log_out, open_session, reached_over_https, router, session_cookie,
	verify_password,
};

/// The authenticated caller: a live session, or an API token acting as the user who made
/// it. Either way `id` is the user, and RBAC is asked about them as usual.
pub struct AuthUser {
	pub id: UserId,
	/// Set when the caller used an API token: its scopes and limits.
	pub grant: Option<tokens::Grant>,
}

impl AuthUser {
	/// Whether secrets (database passwords) may be included in what this caller sees. A
	/// session always may, as far as RBAC allows; a token needs `read:sensitive`.
	pub fn can_reveal(&self) -> bool {
		self.grant
			.as_ref()
			.is_none_or(|g| g.allows(tokens::Scope::ReadSensitive))
	}
}

/// The raw session token the request presented (bearer header first, then the
/// `session` cookie). Does not check that the token is valid.
pub struct SessionToken(pub String);

impl FromRequestParts<AppState> for SessionToken {
	type Rejection = ServerError;

	async fn from_request_parts(
		parts: &mut Parts,
		_state: &AppState,
	) -> Result<Self, Self::Rejection> {
		if let Ok(TypedHeader(Authorization(bearer))) =
			parts.extract::<TypedHeader<Authorization<Bearer>>>().await
		{
			return Ok(Self(bearer.token().to_string()));
		}

		if let Ok(cookies) = parts.extract::<Cookies>().await
			&& let Some(cookie) = cookies.get("session")
		{
			return Ok(Self(cookie.value().to_string()));
		}

		Err(ServerError::MissingToken)
	}
}

impl FromRequestParts<AppState> for AuthUser {
	type Rejection = ServerError;

	async fn from_request_parts(
		parts: &mut Parts,
		state: &AppState,
	) -> Result<Self, Self::Rejection> {
		let SessionToken(token) = SessionToken::from_request_parts(parts, state).await?;

		// An API token was already checked, scope and limits included, by the gate in
		// front of the JSON API. Anywhere else (the web UI) it is simply not a session.
		if token.starts_with(tokens::PREFIX) {
			let grant = parts
				.extensions
				.get::<tokens::Grant>()
				.cloned()
				.ok_or(ServerError::InvalidToken)?;
			return Ok(Self {
				id: grant.user,
				grant: Some(grant),
			});
		}

		let id = session::resolve(&state.db, &token)
			.await?
			.ok_or(ServerError::InvalidToken)?;

		Ok(Self { id, grant: None })
	}
}

/// Row type and queries for the `users` table.
pub mod users {
	use sqlx::PgExecutor;

	use crate::ids::UserId;

	/// Constraint violated when an email is registered twice.
	pub const EMAIL_UNIQUE: &str = "users_email_key";

	#[derive(Debug, Clone, sqlx::FromRow)]
	pub struct User {
		pub id: UserId,
		/// Argon2id PHC string, never the plaintext.
		pub password_hash: String,
	}

	pub async fn insert(
		db: impl PgExecutor<'_>,
		name: &str,
		email: &str,
		password_hash: &str,
	) -> Result<User, sqlx::Error> {
		sqlx::query_as!(
			User,
			r#"
				INSERT INTO users (id, name, email, password_hash)
				VALUES ($1, $2, $3, $4)
				RETURNING id AS "id: UserId", password_hash
			"#,
			UserId::generate().0,
			name,
			email,
			password_hash
		)
		.fetch_one(db)
		.await
	}

	/// Advisory-lock key serializing "is this the first user?" decisions.
	const FIRST_USER_LOCK: i64 = 0x706f_646b_6974;

	/// True when at least one account exists.
	pub async fn any(db: impl PgExecutor<'_>) -> Result<bool, sqlx::Error> {
		sqlx::query_scalar!(r#"SELECT EXISTS (SELECT 1 FROM users) AS "exists!""#)
			.fetch_one(db)
			.await
	}

	/// Inserts the user only while the table is empty; `None` when it isn't.
	/// Concurrent callers queue on an advisory lock, so the later ones see the
	/// first one's row (a bare `INSERT .. WHERE NOT EXISTS` would let two
	/// transactions both see an empty table under READ COMMITTED).
	pub async fn insert_if_no_users(
		db: &sqlx::PgPool,
		name: &str,
		email: &str,
		password_hash: &str,
	) -> Result<Option<User>, sqlx::Error> {
		let mut tx = db.begin().await?;
		let user = insert_if_no_users_in(&mut tx, name, email, password_hash).await?;
		tx.commit().await?;
		Ok(user)
	}

	/// The body of [`insert_if_no_users`], inside the caller's transaction:
	/// takes the lock, which the transaction holds until it ends.
	pub async fn insert_if_no_users_in(
		tx: &mut sqlx::PgConnection,
		name: &str,
		email: &str,
		password_hash: &str,
	) -> Result<Option<User>, sqlx::Error> {
		sqlx::query!("SELECT pg_advisory_xact_lock($1)", FIRST_USER_LOCK)
			.execute(&mut *tx)
			.await?;
		sqlx::query_as!(
			User,
			r#"
				INSERT INTO users (id, name, email, password_hash)
				SELECT $1, $2, $3, $4
				WHERE NOT EXISTS (SELECT 1 FROM users)
				RETURNING id AS "id: UserId", password_hash
			"#,
			UserId::generate().0,
			name,
			email,
			password_hash
		)
		.fetch_optional(&mut *tx)
		.await
	}

	/// The display name shown in the web UI header.
	pub async fn name_of(
		db: impl PgExecutor<'_>,
		id: UserId,
	) -> Result<Option<String>, sqlx::Error> {
		sqlx::query_scalar!("SELECT name FROM users WHERE id = $1", id as UserId)
			.fetch_optional(db)
			.await
	}

	/// What the web UI shows of the signed-in person: name and picture hash.
	#[derive(Debug, Clone)]
	pub struct Header {
		pub name: String,
		pub avatar: Option<String>,
	}

	pub async fn header_of(
		db: impl PgExecutor<'_>,
		id: UserId,
	) -> Result<Option<Header>, sqlx::Error> {
		sqlx::query_as!(
			Header,
			r#"
				SELECT name, (SELECT hash FROM user_avatars WHERE user_id = users.id) AS avatar
				FROM users WHERE id = $1
			"#,
			id as UserId
		)
		.fetch_optional(db)
		.await
	}

	/// The sign-in email, for the account page.
	pub async fn email_of(
		db: impl PgExecutor<'_>,
		id: UserId,
	) -> Result<Option<String>, sqlx::Error> {
		sqlx::query_scalar!("SELECT email FROM users WHERE id = $1", id as UserId)
			.fetch_optional(db)
			.await
	}

	pub async fn find_by_email(
		db: impl PgExecutor<'_>,
		email: &str,
	) -> Result<Option<User>, sqlx::Error> {
		sqlx::query_as!(
			User,
			r#"
				SELECT id AS "id: UserId", password_hash
				FROM users WHERE email = $1
			"#,
			email
		)
		.fetch_optional(db)
		.await
	}
}

mod email {
	/*
	SPDX-License-Identifier: Unlicense

	Made by unitsuki / detski.

	This file is dedicated to the public domain, to the fullest extent permitted by law.

	Anyone may use, copy, modify, distribute, and sell this file, for any purpose,
	with or without attribution.

	This file is provided "as is", without warranty of any kind, express or implied,
	including but not limited to merchantability, fitness for a particular purpose,
	and noninfringement.

	In no event shall the author be liable for any claim, damages, or other
	liability arising from or related to this file or its use.
	*/

	//! - Local-part: `atext` characters (RFC 5321 §4.1.2) plus `.` with rules.
	//!   Allowed: ``a-z A-Z 0-9 ! # $ % & ' * + - / = ? ^ _ ` { | } ~``
	//!   Dots: allowed but not at start, end, or consecutively.
	//!   Max 64 characters.
	//! - Domain: standard hostname (RFC 1123).
	//!   Labels: alphanumeric + hyphens, no leading/trailing hyphen, max 63 chars each.
	//!   TLD: alphabetic only, minimum 2 characters.
	//!   Minimum 2 labels (e.g. `example.com`). Max 255 characters total.
	//! - Total length ≤ 254 characters (RFC 5321 §4.5.3).
	//! - ASCII only, use punycode for international domain names.
	//!
	//! ## Rejects (intentionally)
	//!
	//! - Quoted strings: `"foo bar"@example.com`
	//! - IP address literals: `user@[192.168.1.1]`
	//! - Comments, folding whitespace, obsolete RFC 822/2822 syntax
	//! - Non-ASCII / EAI (SMTPUTF8) addresses (may add better support in the future)
	//! - `localhost` and single-label domains

	// RFC 5321 §4.5.3
	const MAX_EMAIL_LEN: usize = 254;
	const MAX_LOCAL_LEN: usize = 64;
	const MAX_DOMAIN_LEN: usize = 255;
	const MAX_LABEL_LEN: usize = 63;
	const MIN_TLD_LEN: usize = 2;

	// lookup tables so we don't have a giant match chain for every byte
	// (also slightly faster, not that it matters much here...)

	/// Valid `atext` characters (RFC 5321 §4.1.2), dot excluded.
	/// Index with `byte as usize`; only bytes 0–127 are valid indices.
	/// Non-ASCII bytes (≥ 128) must be rejected before indexing.
	const ATEXT_MAP: [bool; 128] = {
		let mut t = [false; 128];
		let mut b = b'a';
		while b <= b'z' {
			t[b as usize] = true;
			b += 1;
		}
		b = b'A';
		while b <= b'Z' {
			t[b as usize] = true;
			b += 1;
		}
		b = b'0';
		while b <= b'9' {
			t[b as usize] = true;
			b += 1;
		}
		t[b'!' as usize] = true;
		t[b'#' as usize] = true;
		t[b'$' as usize] = true;
		t[b'%' as usize] = true;
		t[b'&' as usize] = true;
		t[b'\'' as usize] = true;
		t[b'*' as usize] = true;
		t[b'+' as usize] = true;
		t[b'-' as usize] = true;
		t[b'/' as usize] = true;
		t[b'=' as usize] = true;
		t[b'?' as usize] = true;
		t[b'^' as usize] = true;
		t[b'_' as usize] = true;
		t[b'`' as usize] = true;
		t[b'{' as usize] = true;
		t[b'|' as usize] = true;
		t[b'}' as usize] = true;
		t[b'~' as usize] = true;
		t
	};

	/// Valid non-TLD domain-label characters: `a-z A-Z 0-9 -` (RFC 1123).
	const LABEL_MAP: [bool; 128] = {
		let mut t = [false; 128];
		let mut b = b'a';
		while b <= b'z' {
			t[b as usize] = true;
			b += 1;
		}
		b = b'A';
		while b <= b'Z' {
			t[b as usize] = true;
			b += 1;
		}
		b = b'0';
		while b <= b'9' {
			t[b as usize] = true;
			b += 1;
		}
		t[b'-' as usize] = true;
		t
	};

	/// Every possible rejection reason, in order of where it is checked.
	#[derive(Debug, Clone, PartialEq, Eq)]
	pub enum EmailError {
		// top-level
		Empty,
		TooLong,
		MissingAt,
		MultipleAt,
		// local-part
		LocalPartEmpty,
		LocalPartTooLong,
		LocalPartLeadingDot,
		LocalPartTrailingDot,
		LocalPartConsecutiveDots,
		LocalPartInvalidChar(char),
		// domain
		DomainEmpty,
		DomainTooLong,
		DomainIpLiteralForbidden,
		DomainTrailingDot,
		/// Covers a leading dot and consecutive dots (both produce an empty label).
		DomainLabelEmpty,
		DomainLabelTooLong,
		DomainLabelLeadingHyphen,
		DomainLabelTrailingHyphen,
		DomainLabelInvalidChar(char),
		DomainMissingTld,
		DomainTldTooShort,
		/// TLD must contain only ASCII letters (a-z / A-Z).
		DomainTldNotAlpha,
		// encoding
		/// Found a non-ASCII character; encode international domains as punycode.
		NonAscii(char),
	}

	impl std::fmt::Display for EmailError {
		fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
			match self {
				Self::Empty => write!(f, "email is empty"),
				Self::TooLong => write!(f, "email exceeds {MAX_EMAIL_LEN} characters"),
				Self::MissingAt => write!(f, "missing '@'"),
				Self::MultipleAt => write!(f, "multiple '@' characters"),
				Self::LocalPartEmpty => write!(f, "local part (before '@') is empty"),
				Self::LocalPartTooLong => {
					write!(f, "local part exceeds {MAX_LOCAL_LEN} characters")
				}
				Self::LocalPartLeadingDot => write!(f, "local part starts with '.'"),
				Self::LocalPartTrailingDot => write!(f, "local part ends with '.'"),
				Self::LocalPartConsecutiveDots => write!(f, "local part contains consecutive dots"),
				Self::LocalPartInvalidChar(c) => {
					write!(f, "invalid character in local part: {c:?}")
				}
				Self::DomainEmpty => write!(f, "domain (after '@') is empty"),
				Self::DomainTooLong => write!(f, "domain exceeds {MAX_DOMAIN_LEN} characters"),
				Self::DomainIpLiteralForbidden => write!(f, "IP address literals are not accepted"),
				Self::DomainTrailingDot => write!(f, "domain ends with '.'"),
				Self::DomainLabelEmpty => write!(
					f,
					"domain contains an empty label (leading or consecutive dots)"
				),
				Self::DomainLabelTooLong => {
					write!(f, "domain label exceeds {MAX_LABEL_LEN} characters")
				}
				Self::DomainLabelLeadingHyphen => write!(f, "domain label starts with '-'"),
				Self::DomainLabelTrailingHyphen => write!(f, "domain label ends with '-'"),
				Self::DomainLabelInvalidChar(c) => {
					write!(f, "invalid character in domain label: {c:?}")
				}
				Self::DomainMissingTld => {
					write!(f, "domain has no TLD (must contain at least one dot)")
				}
				Self::DomainTldTooShort => {
					write!(f, "TLD must be at least {MIN_TLD_LEN} characters")
				}
				Self::DomainTldNotAlpha => write!(f, "TLD must contain only letters (a-z)"),
				Self::NonAscii(c) => write!(
					f,
					"non-ASCII character {c:?}; encode international domains as punycode"
				),
			}
		}
	}

	impl std::error::Error for EmailError {}

	/// Proof that the wrapped `&str` has passed [`validate_email`].
	///
	/// The idea is that you validate once and then pass this around instead of
	/// a raw &str, so the type carries the guarantee and callers don't re-check.
	// could add an into_string() or something later if needed
	#[derive(Debug, Clone, Copy, PartialEq, Eq)]
	pub struct ValidatedEmail<'a>(&'a str);

	impl ValidatedEmail<'_> {
		#[inline]
		pub fn normalize(self) -> String {
			normalize_email(self.0)
		}
	}

	fn validate_local(local: &str) -> Result<(), EmailError> {
		if local.is_empty() {
			return Err(EmailError::LocalPartEmpty);
		}
		if local.len() > MAX_LOCAL_LEN {
			return Err(EmailError::LocalPartTooLong);
		}

		let bytes = local.as_bytes();
		if bytes[0] == b'.' {
			return Err(EmailError::LocalPartLeadingDot);
		}
		if *bytes.last().unwrap() == b'.' {
			return Err(EmailError::LocalPartTrailingDot);
		}

		let mut prev_dot = false;
		for (i, &b) in bytes.iter().enumerate() {
			if b >= 128 {
				// all bytes before i were ASCII so this has to be a lead byte, i is a valid boundary
				let c = local[i..].chars().next().unwrap();
				return Err(EmailError::NonAscii(c));
			}
			if b == b'.' {
				if prev_dot {
					return Err(EmailError::LocalPartConsecutiveDots);
				}
				prev_dot = true;
			} else if ATEXT_MAP[b as usize] {
				prev_dot = false;
			} else {
				return Err(EmailError::LocalPartInvalidChar(b as char));
			}
		}
		Ok(())
	}

	#[inline]
	fn validate_label(label: &str) -> Result<(), EmailError> {
		if label.is_empty() {
			return Err(EmailError::DomainLabelEmpty);
		}
		if label.len() > MAX_LABEL_LEN {
			return Err(EmailError::DomainLabelTooLong);
		}

		let bytes = label.as_bytes();
		if bytes[0] == b'-' {
			return Err(EmailError::DomainLabelLeadingHyphen);
		}
		if *bytes.last().unwrap() == b'-' {
			return Err(EmailError::DomainLabelTrailingHyphen);
		}

		for (i, &b) in bytes.iter().enumerate() {
			if b >= 128 {
				let c = label[i..].chars().next().unwrap();
				return Err(EmailError::NonAscii(c));
			}
			if !LABEL_MAP[b as usize] {
				return Err(EmailError::DomainLabelInvalidChar(b as char));
			}
		}
		Ok(())
	}

	fn validate_domain(domain: &str) -> Result<(), EmailError> {
		if domain.is_empty() {
			return Err(EmailError::DomainEmpty);
		}
		if domain.as_bytes()[0] == b'[' {
			return Err(EmailError::DomainIpLiteralForbidden);
		}
		if domain.len() > MAX_DOMAIN_LEN {
			return Err(EmailError::DomainTooLong);
		}
		if domain.ends_with('.') {
			return Err(EmailError::DomainTrailingDot);
		}

		// rfind to split off the TLD, avoids collecting into a vec just to grab the last element
		let last_dot = domain.rfind('.').ok_or(EmailError::DomainMissingTld)?;
		let body = &domain[..last_dot];
		let tld = &domain[last_dot + 1..];

		if body.is_empty() {
			return Err(EmailError::DomainLabelEmpty);
		}

		for label in body.split('.') {
			validate_label(label)?;
		}

		// TLD: letters only, at least 2 chars
		if tld.len() < MIN_TLD_LEN {
			return Err(EmailError::DomainTldTooShort);
		}
		for (i, &b) in tld.as_bytes().iter().enumerate() {
			if b >= 128 {
				let c = tld[i..].chars().next().unwrap();
				return Err(EmailError::NonAscii(c));
			}
			if !b.is_ascii_alphabetic() {
				return Err(EmailError::DomainTldNotAlpha);
			}
		}

		Ok(())
	}

	/// Validates `email` against a strict subset of RFC 5321/5322.
	///
	/// On success returns a [`ValidatedEmail`] wrapper. Pass that to downstream
	/// functions that need a valid address.
	///
	/// # Errors
	///
	/// Returns [`EmailError`] describing which part of the address failed
	/// (empty, too long, bad local part, bad domain, and so on).
	pub fn validate_email(email: &str) -> Result<ValidatedEmail<'_>, EmailError> {
		if email.is_empty() {
			return Err(EmailError::Empty);
		}
		if email.len() > MAX_EMAIL_LEN {
			return Err(EmailError::TooLong);
		}

		// single pass: find '@' and bail immediately if there's a second one
		let mut at_pos: Option<usize> = None;
		for (i, &b) in email.as_bytes().iter().enumerate() {
			if b == b'@' {
				if at_pos.is_some() {
					return Err(EmailError::MultipleAt);
				}
				at_pos = Some(i);
			}
		}
		let at = at_pos.ok_or(EmailError::MissingAt)?;

		validate_local(&email[..at])?;
		validate_domain(&email[at + 1..])?;

		Ok(ValidatedEmail(email))
	}

	/// Returns the normalised form: domain lowercased, local-part preserved.
	///
	/// DNS is case-insensitive; the local-part is case-sensitive per RFC 5321. (tbh I would make everything lowercase)
	pub fn normalize_email(email: &str) -> String {
		let at = email.as_bytes().iter().position(|&b| b == b'@').unwrap();
		let mut out = String::with_capacity(email.len());
		out.push_str(&email[..=at]);
		out.push_str(&email[at + 1..].to_ascii_lowercase());
		out
	}

	#[cfg(test)]
	mod tests {
		use super::*;

		#[test]
		fn accepts_valid_addresses() {
			for email in [
				"user@example.com",
				"user+tag@mail.example.com",
				"first.last@example.co",
				"a!#$%&'*+-/=?^_`{|}~b@example.com",
				"x@a-b.example.org",
			] {
				assert!(validate_email(email).is_ok(), "{email}");
			}
		}

		#[test]
		fn rejects_invalid_addresses() {
			let long_local = format!("{}@example.com", "a".repeat(65));
			let long_label = format!("a@{}.com", "a".repeat(64));
			let cases: [(&str, EmailError); 18] = [
				("", EmailError::Empty),
				("userexample.com", EmailError::MissingAt),
				("a@b@example.com", EmailError::MultipleAt),
				("@example.com", EmailError::LocalPartEmpty),
				(".a@example.com", EmailError::LocalPartLeadingDot),
				("a.@example.com", EmailError::LocalPartTrailingDot),
				("a..b@example.com", EmailError::LocalPartConsecutiveDots),
				("a b@example.com", EmailError::LocalPartInvalidChar(' ')),
				("a@", EmailError::DomainEmpty),
				("a@[192.168.1.1]", EmailError::DomainIpLiteralForbidden),
				("a@example.com.", EmailError::DomainTrailingDot),
				("a@.example.com", EmailError::DomainLabelEmpty),
				("a@-x.example.com", EmailError::DomainLabelLeadingHyphen),
				("a@x-.example.com", EmailError::DomainLabelTrailingHyphen),
				("a@localhost", EmailError::DomainMissingTld),
				("a@example.c", EmailError::DomainTldTooShort),
				("a@example.c0m", EmailError::DomainTldNotAlpha),
				("a@exämple.com", EmailError::NonAscii('ä')),
			];
			for (email, want) in cases {
				assert_eq!(validate_email(email).unwrap_err(), want, "{email}");
			}
			assert_eq!(
				validate_email(&long_local).unwrap_err(),
				EmailError::LocalPartTooLong
			);
			assert_eq!(
				validate_email(&long_label).unwrap_err(),
				EmailError::DomainLabelTooLong
			);
		}

		#[test]
		fn normalize_lowercases_only_the_domain() {
			let ve = validate_email("User.Name@Mail.EXAMPLE.com").unwrap();
			assert_eq!(ve.normalize(), "User.Name@mail.example.com");
		}
	}
}

pub use email::validate_email;

/// An email as stored by registration (domain lowercased) when it is valid,
/// else the input unchanged, so a lookup finds what sign-up stored.
pub(crate) fn normalize_email_lenient(raw: &str) -> String {
	validate_email(raw.trim()).map_or_else(|_| raw.to_owned(), email::ValidatedEmail::normalize)
}
