//! Two-step verification use cases: the authenticator app (TOTP), recovery codes, and what
//! a person has set up. Passkeys are in [`super::passkeys`], the team policy in
//! [`super::policy`].
//!
//! A person "has 2FA" when a confirmed authenticator app or at least one passkey exists.
//! Recovery codes only exist next to one of those and are never a factor of their own.

use sha2::{Digest, Sha256};
use sqlx::PgPool;
use time::OffsetDateTime;
use zeroize::Zeroizing;

use super::totp;
use crate::audit::{Diff, Target};
use crate::auth::record_everywhere;
use crate::error::{AppResult, ServerError};
use crate::ids::{RecoveryCodeId, UserId};
use crate::state::AppState;

/// How many recovery codes a person gets.
pub const RECOVERY_CODES: usize = 10;

/// Shown next to the account name in an authenticator app.
const ISSUER: &str = "podkit";

/// What a person has set up, for the Security page.
#[derive(Debug, Clone)]
pub struct Status {
	/// A confirmed authenticator app.
	pub totp: bool,
	/// Recovery codes not used yet.
	pub recovery_left: i64,
	pub passkeys: Vec<super::passkeys::Passkey>,
}

impl Status {
	/// Whether the account has a second factor at all.
	pub fn enrolled(&self) -> bool {
		self.totp || !self.passkeys.is_empty()
	}
}

/// An authenticator app being set up: its secret, for the QR code and for typing in.
#[derive(Debug, Clone)]
pub struct Enrollment {
	pub secret: String,
	pub uri: String,
}

pub async fn status(db: &PgPool, user: UserId) -> AppResult<Status> {
	let totp = sqlx::query_scalar!(
		r#"SELECT EXISTS (SELECT 1 FROM totp_credentials WHERE user_id = $1 AND confirmed_at IS NOT NULL) AS "e!""#,
		user.0
	)
	.fetch_one(db)
	.await?;
	let recovery_left = sqlx::query_scalar!(
		r#"SELECT COUNT(*) AS "n!" FROM recovery_codes WHERE user_id = $1 AND used_at IS NULL"#,
		user.0
	)
	.fetch_one(db)
	.await?;
	Ok(Status {
		totp,
		recovery_left,
		passkeys: super::passkeys::list(db, user).await?,
	})
}

/// Whether `user` has a second factor.
pub async fn enrolled(db: &PgPool, user: UserId) -> Result<bool, sqlx::Error> {
	sqlx::query_scalar!(
		r#"
			SELECT (
				EXISTS (SELECT 1 FROM totp_credentials WHERE user_id = $1 AND confirmed_at IS NOT NULL)
				OR EXISTS (SELECT 1 FROM passkeys WHERE user_id = $1)
			) AS "e!"
		"#,
		user.0
	)
	.fetch_one(db)
	.await
}

/// Whether the account has a password a person knows (an account made by an identity
/// provider does not), so the Security page knows whether to ask for it.
pub async fn has_password(db: &PgPool, user: UserId) -> Result<bool, sqlx::Error> {
	sqlx::query_scalar!("SELECT has_password FROM users WHERE id = $1", user.0)
		.fetch_one(db)
		.await
}

/// Checks the password of an account that has one. A person who signs in only through an
/// identity provider has none, so there is nothing to ask.
pub async fn confirm_password(db: &PgPool, user: UserId, password: &str) -> AppResult<()> {
	let row = sqlx::query!(
		"SELECT password_hash, has_password FROM users WHERE id = $1",
		user.0
	)
	.fetch_optional(db)
	.await?
	.ok_or(ServerError::InvalidCredentials)?;
	if !row.has_password {
		return Ok(());
	}
	let ok = crypto::verify(Zeroizing::new(password.to_owned()), row.password_hash)
		.await
		.map_err(|e| {
			tracing::error!(error = %format!("{e:#}"), "password check failed");
			ServerError::Internal
		})?;
	if ok {
		Ok(())
	} else {
		Err(ServerError::Validation(
			"That password is not right.".to_owned(),
		))
	}
}

fn account_label(email: &str) -> String {
	email.to_owned()
}

fn describe(secret_b32: &str, email: &str) -> Enrollment {
	Enrollment {
		secret: secret_b32.to_owned(),
		uri: totp::otpauth_uri(ISSUER, &account_label(email), secret_b32),
	}
}

fn seal(s: &AppState, secret_b32: &str) -> AppResult<Vec<u8>> {
	s.secrets.encrypt(secret_b32).map_err(|e| {
		tracing::error!(error = %e, "cannot encrypt a TOTP secret");
		ServerError::Internal
	})
}

fn unseal(s: &AppState, sealed: &[u8]) -> AppResult<Zeroizing<Vec<u8>>> {
	let text = Zeroizing::new(s.secrets.decrypt(sealed).map_err(|e| {
		tracing::error!(error = %e, "cannot decrypt a TOTP secret");
		ServerError::Internal
	})?);
	totp::base32_decode(&text)
		.map(Zeroizing::new)
		.ok_or(ServerError::Internal)
}

/// Starts setting up an authenticator app: a fresh secret, kept until it is confirmed with
/// a code. Starting again replaces an unconfirmed secret. Refused when one is already
/// confirmed: turn it off first.
pub async fn begin_totp(s: &AppState, user: UserId) -> AppResult<Enrollment> {
	let email = crate::auth::service::email_of(&s.db, user)
		.await?
		.ok_or(ServerError::NotFound)?;
	let secret = totp::base32_encode(&totp::generate_secret());
	let sealed = seal(s, &secret)?;
	let written = sqlx::query!(
		"INSERT INTO totp_credentials (user_id, secret) VALUES ($1, $2)
		ON CONFLICT (user_id) DO UPDATE SET secret = EXCLUDED.secret, created_at = NOW()
		WHERE totp_credentials.confirmed_at IS NULL",
		user.0,
		sealed
	)
	.execute(&s.db)
	.await?
	.rows_affected();
	if written == 0 {
		return Err(ServerError::Conflict(
			"An authenticator app is already set up. Turn it off first to set up another."
				.to_owned(),
		));
	}
	Ok(describe(&secret, &email))
}

/// The unconfirmed secret again, for a page reload in the middle of setup.
pub async fn pending_totp(s: &AppState, user: UserId) -> AppResult<Option<Enrollment>> {
	let Some(sealed) = sqlx::query_scalar!(
		"SELECT secret FROM totp_credentials WHERE user_id = $1 AND confirmed_at IS NULL",
		user.0
	)
	.fetch_optional(&s.db)
	.await?
	else {
		return Ok(None);
	};
	let email = crate::auth::service::email_of(&s.db, user)
		.await?
		.ok_or(ServerError::NotFound)?;
	let secret = Zeroizing::new(s.secrets.decrypt(&sealed).map_err(|e| {
		tracing::error!(error = %e, "cannot decrypt a TOTP secret");
		ServerError::Internal
	})?);
	Ok(Some(describe(&secret, &email)))
}

fn now_unix() -> u64 {
	u64::try_from(OffsetDateTime::now_utc().unix_timestamp()).unwrap_or(0)
}

/// Confirms the authenticator app with a code it shows. Returns the recovery codes when
/// this is the person's first second factor, to be shown once (none otherwise).
pub async fn confirm_totp(s: &AppState, user: UserId, code: &str) -> AppResult<Vec<String>> {
	let sealed = sqlx::query_scalar!(
		"SELECT secret FROM totp_credentials WHERE user_id = $1 AND confirmed_at IS NULL",
		user.0
	)
	.fetch_optional(&s.db)
	.await?
	.ok_or_else(|| ServerError::Validation("Start the setup first.".to_owned()))?;
	let secret = unseal(s, &sealed)?;
	let Some(step) = totp::verify(&secret, now_unix(), code, 0) else {
		return Err(ServerError::Validation(
			"That code is not right. Check the time on your phone and try the next one.".to_owned(),
		));
	};
	let step = i64::try_from(step).unwrap_or(i64::MAX);
	let done = sqlx::query!(
		"UPDATE totp_credentials SET confirmed_at = NOW(), last_step = $2
		WHERE user_id = $1 AND confirmed_at IS NULL",
		user.0,
		step
	)
	.execute(&s.db)
	.await?
	.rows_affected();
	if done == 0 {
		return Err(ServerError::Validation("Start the setup first.".to_owned()));
	}
	let codes = ensure_recovery_codes(&s.db, user).await?;
	record_everywhere(
		&s.db,
		user,
		"two_factor.enable",
		Target::none("two_factor").named("authenticator app"),
		Diff::new().set("method", "totp"),
	)
	.await;
	Ok(codes)
}

/// Throws away an unconfirmed setup.
pub async fn cancel_totp(db: &PgPool, user: UserId) -> AppResult<()> {
	sqlx::query!(
		"DELETE FROM totp_credentials WHERE user_id = $1 AND confirmed_at IS NULL",
		user.0
	)
	.execute(db)
	.await?;
	Ok(())
}

/// Turns the authenticator app off. Asks for the password of an account that has one.
pub async fn disable_totp(db: &PgPool, user: UserId, password: &str) -> AppResult<()> {
	confirm_password(db, user, password).await?;
	let removed = sqlx::query!(
		"DELETE FROM totp_credentials WHERE user_id = $1 AND confirmed_at IS NOT NULL",
		user.0
	)
	.execute(db)
	.await?
	.rows_affected();
	if removed == 0 {
		return Err(ServerError::NotFound);
	}
	drop_recovery_if_unneeded(db, user).await?;
	record_everywhere(
		db,
		user,
		"two_factor.disable",
		Target::none("two_factor").named("authenticator app"),
		Diff::new().set("method", "totp"),
	)
	.await;
	Ok(())
}

/// Recovery codes mean nothing once the last second factor is gone.
pub async fn drop_recovery_if_unneeded(db: &PgPool, user: UserId) -> Result<(), sqlx::Error> {
	if !enrolled(db, user).await? {
		sqlx::query!("DELETE FROM recovery_codes WHERE user_id = $1", user.0)
			.execute(db)
			.await?;
	}
	Ok(())
}

fn normalize_recovery(code: &str) -> String {
	code.chars()
		.filter(|c| c.is_ascii_alphanumeric())
		.map(|c| c.to_ascii_lowercase())
		.collect()
}

fn hash_recovery(code: &str) -> Vec<u8> {
	Sha256::digest(normalize_recovery(code).as_bytes()).to_vec()
}

/// A recovery code: 80 random bits as `xxxx-xxxx-xxxx-xxxx`.
fn new_recovery_code() -> String {
	let mut bytes = [0u8; 10];
	getrandom::fill(&mut bytes).expect("OS RNG unavailable");
	let text = totp::base32_encode(&bytes).to_ascii_lowercase();
	text.as_bytes()
		.chunks(4)
		.map(|c| String::from_utf8_lossy(c).into_owned())
		.collect::<Vec<_>>()
		.join("-")
}

/// Replaces every recovery code with [`RECOVERY_CODES`] new ones and returns them: this is
/// the only time they are readable.
async fn replace_recovery_codes(db: &PgPool, user: UserId) -> Result<Vec<String>, sqlx::Error> {
	let mut tx = db.begin().await?;
	sqlx::query!("DELETE FROM recovery_codes WHERE user_id = $1", user.0)
		.execute(&mut *tx)
		.await?;
	let mut codes = Vec::with_capacity(RECOVERY_CODES);
	for _ in 0..RECOVERY_CODES {
		let code = new_recovery_code();
		sqlx::query!(
			"INSERT INTO recovery_codes (id, user_id, code_hash) VALUES ($1, $2, $3)",
			RecoveryCodeId::generate().0,
			user.0,
			hash_recovery(&code)
		)
		.execute(&mut *tx)
		.await?;
		codes.push(code);
	}
	tx.commit().await?;
	Ok(codes)
}

/// Makes the first set of codes when the person has none yet; `[]` when they already do.
pub async fn ensure_recovery_codes(db: &PgPool, user: UserId) -> Result<Vec<String>, sqlx::Error> {
	let have = sqlx::query_scalar!(
		r#"SELECT EXISTS (SELECT 1 FROM recovery_codes WHERE user_id = $1) AS "e!""#,
		user.0
	)
	.fetch_one(db)
	.await?;
	if have {
		return Ok(Vec::new());
	}
	replace_recovery_codes(db, user).await
}

/// Makes new recovery codes; the old ones stop working. Asks for the password of an
/// account that has one.
pub async fn regenerate_recovery_codes(
	db: &PgPool,
	user: UserId,
	password: &str,
) -> AppResult<Vec<String>> {
	confirm_password(db, user, password).await?;
	if !enrolled(db, user).await? {
		return Err(ServerError::Validation(
			"Set up an authenticator app or a passkey first.".to_owned(),
		));
	}
	let codes = replace_recovery_codes(db, user).await?;
	record_everywhere(
		db,
		user,
		"two_factor.rotate",
		Target::none("two_factor").named("recovery codes"),
		Diff::new(),
	)
	.await;
	Ok(codes)
}

/// Whether `code` looks like a recovery code rather than an authenticator app code.
fn is_app_code(code: &str) -> bool {
	let typed: String = code.chars().filter(|c| !c.is_whitespace()).collect();
	typed.len() == totp::DIGITS as usize && typed.bytes().all(|b| b.is_ascii_digit())
}

/// Checks the second factor a person typed: a code from their authenticator app or one of
/// their recovery codes. A code works once: an app code only for a time step newer than
/// the last one accepted, a recovery code until it is marked used.
pub async fn check_code(s: &AppState, user: UserId, code: &str) -> AppResult<bool> {
	if is_app_code(code) {
		let Some(row) = sqlx::query!(
			"SELECT secret, last_step FROM totp_credentials
			WHERE user_id = $1 AND confirmed_at IS NOT NULL",
			user.0
		)
		.fetch_optional(&s.db)
		.await?
		else {
			return Ok(false);
		};
		let secret = unseal(s, &row.secret)?;
		let last = u64::try_from(row.last_step).unwrap_or(0);
		let Some(step) = totp::verify(&secret, now_unix(), code, last) else {
			return Ok(false);
		};
		let step = i64::try_from(step).unwrap_or(i64::MAX);
		// The compare-and-set makes two requests with the same code race for one win.
		let won = sqlx::query!(
			"UPDATE totp_credentials SET last_step = $2 WHERE user_id = $1 AND last_step < $2",
			user.0,
			step
		)
		.execute(&s.db)
		.await?
		.rows_affected();
		return Ok(won == 1);
	}
	let used = sqlx::query!(
		"UPDATE recovery_codes SET used_at = NOW()
		WHERE user_id = $1 AND code_hash = $2 AND used_at IS NULL",
		user.0,
		hash_recovery(code)
	)
	.execute(&s.db)
	.await?
	.rows_affected();
	Ok(used == 1)
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::tests::{PgPool, db_test, state};

	async fn user(db: &PgPool) -> UserId {
		let hash = crypto::hash(Zeroizing::new("correct horse battery".to_owned()))
			.await
			.unwrap();
		let id = UserId::generate();
		sqlx::query(
			"INSERT INTO users (id, name, email, password_hash) VALUES ($1, 'A', 'a@example.com', $2)",
		)
		.bind(id.0)
		.bind(hash)
		.execute(db)
		.await
		.unwrap();
		id
	}

	async fn current_code(s: &AppState, user: UserId) -> String {
		let sealed: Vec<u8> =
			sqlx::query_scalar("SELECT secret FROM totp_credentials WHERE user_id = $1")
				.bind(user.0)
				.fetch_one(&s.db)
				.await
				.unwrap();
		let secret = unseal(s, &sealed).unwrap();
		format!(
			"{:06}",
			totp::code_at(&secret, totp::step_of(now_unix()), totp::DIGITS)
		)
	}

	#[db_test]
	async fn two_factor_enrolling_totp_stores_the_secret_encrypted_and_needs_a_code(db: PgPool) {
		let s = state(db.clone());
		let u = user(&db).await;
		let setup = begin_totp(&s, u).await.unwrap();
		let sealed: Vec<u8> = sqlx::query_scalar("SELECT secret FROM totp_credentials")
			.fetch_one(&db)
			.await
			.unwrap();
		assert!(!String::from_utf8_lossy(&sealed).contains(&setup.secret));
		assert!(
			setup
				.uri
				.starts_with("otpauth://totp/podkit:a%40example.com?secret=")
		);
		assert!(
			!enrolled(&db, u).await.unwrap(),
			"not enrolled until confirmed"
		);

		// A wrong code confirms nothing.
		assert!(
			confirm_totp(&s, u, "000000").await.is_err() || current_code(&s, u).await == "000000"
		);
		assert!(!enrolled(&db, u).await.unwrap());

		let codes = confirm_totp(&s, u, &current_code(&s, u).await)
			.await
			.unwrap();
		assert_eq!(codes.len(), RECOVERY_CODES);
		assert!(enrolled(&db, u).await.unwrap());
		// No second set of codes for a second factor.
		assert!(ensure_recovery_codes(&db, u).await.unwrap().is_empty());
		// And no second app next to the first.
		assert!(matches!(
			begin_totp(&s, u).await,
			Err(ServerError::Conflict(_))
		));
	}

	#[db_test]
	async fn two_factor_an_app_code_works_once_per_time_step(db: PgPool) {
		let s = state(db.clone());
		let u = user(&db).await;
		begin_totp(&s, u).await.unwrap();
		confirm_totp(&s, u, &current_code(&s, u).await)
			.await
			.unwrap();

		// The code that confirmed the setup is spent; the next step's is fresh.
		let code = current_code(&s, u).await;
		assert!(
			!check_code(&s, u, &code).await.unwrap(),
			"replay of the confirming code"
		);
		let sealed: Vec<u8> = sqlx::query_scalar("SELECT secret FROM totp_credentials")
			.fetch_one(&db)
			.await
			.unwrap();
		let secret = unseal(&s, &sealed).unwrap();
		let next = format!(
			"{:06}",
			totp::code_at(&secret, totp::step_of(now_unix()) + 1, totp::DIGITS)
		);
		assert!(check_code(&s, u, &next).await.unwrap());
		assert!(
			!check_code(&s, u, &next).await.unwrap(),
			"the same code twice"
		);
	}

	#[db_test]
	async fn two_factor_recovery_codes_are_single_use_and_stored_hashed(db: PgPool) {
		let s = state(db.clone());
		let u = user(&db).await;
		begin_totp(&s, u).await.unwrap();
		let codes = confirm_totp(&s, u, &current_code(&s, u).await)
			.await
			.unwrap();

		let stored: Vec<Vec<u8>> = sqlx::query_scalar("SELECT code_hash FROM recovery_codes")
			.fetch_all(&db)
			.await
			.unwrap();
		assert_eq!(stored.len(), RECOVERY_CODES);
		for code in &codes {
			assert!(!stored.iter().any(|h| h == code.as_bytes()));
		}

		// Case and dashes do not matter; once used, never again; the others still work.
		let first = codes[0].to_uppercase().replace('-', " ");
		assert!(check_code(&s, u, &first).await.unwrap());
		assert!(!check_code(&s, u, &codes[0]).await.unwrap());
		assert!(check_code(&s, u, &codes[1]).await.unwrap());
		assert_eq!(status(&db, u).await.unwrap().recovery_left, 8);
		assert!(!check_code(&s, u, "aaaa-bbbb-cccc-dddd").await.unwrap());

		// New codes retire the old ones.
		let fresh = regenerate_recovery_codes(&db, u, "correct horse battery")
			.await
			.unwrap();
		assert!(!check_code(&s, u, &codes[2]).await.unwrap());
		assert!(check_code(&s, u, &fresh[0]).await.unwrap());
		assert!(regenerate_recovery_codes(&db, u, "wrong").await.is_err());
	}

	#[db_test]
	async fn two_factor_turning_the_app_off_needs_the_password_and_clears_the_codes(db: PgPool) {
		let s = state(db.clone());
		let u = user(&db).await;
		begin_totp(&s, u).await.unwrap();
		confirm_totp(&s, u, &current_code(&s, u).await)
			.await
			.unwrap();

		assert!(disable_totp(&db, u, "wrong password").await.is_err());
		assert!(enrolled(&db, u).await.unwrap());
		disable_totp(&db, u, "correct horse battery").await.unwrap();
		assert!(!enrolled(&db, u).await.unwrap());
		assert_eq!(status(&db, u).await.unwrap().recovery_left, 0);
		assert_eq!(crate::tests::count(&db, "recovery_codes").await, 0);
	}
}
