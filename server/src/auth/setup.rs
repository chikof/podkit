//! The first-run setup token. While no account exists, the first sign-up becomes the instance
//! administrator (see `quotas::service::is_instance_admin`), so it must come from whoever can
//! read podkit's log, not from whoever reaches the page first.
//!
//! At start, when no account exists, podkit arms a token: `SETUP_TOKEN` when the operator set
//! one (an automated install), otherwise 32 random bytes that are printed to the log, and only
//! there. Only the SHA-256 of the token is held, in memory: it is never stored in the database,
//! never put in a response and never written to a page. A new random one is made on every
//! restart while there is still no account. Once the first account exists the token is spent
//! for good: [`SetupToken::spend`] forgets the digest, and the sign-up checks that no account
//! exists before they look at the token at all.
//!
//! A presented token is compared in constant time (digest against digest, so its length says
//! nothing either), and every wrong one is counted by the sign-in throttle like a wrong password.

use std::sync::RwLock;

use sha2::{Digest, Sha256};

use crate::error::{AppResult, ServerError};
use crate::throttle::{self, Who};

/// The least length of a `SETUP_TOKEN` the operator sets; the random one is 64 hex characters.
pub const MIN_CONFIGURED_LEN: usize = 32;

/// Random bytes in a generated token.
const RANDOM_BYTES: usize = 32;

/// Where the setup token stands.
#[derive(Debug, Clone, PartialEq, Eq)]
enum State {
	/// Waiting for the first account: the hex SHA-256 of the token.
	Armed(String),
	/// An account exists (or the instance started with one): the first-run step is over.
	Spent,
	/// No setup token at all. Only tests start like this, so their many sign-ups need none.
	#[cfg(test)]
	NotRequired,
}

/// The token's digest, shared by every request; see the module docs.
#[derive(Debug)]
pub struct SetupToken(RwLock<State>);

impl Default for SetupToken {
	fn default() -> Self {
		Self(RwLock::new(State::Spent))
	}
}

/// Where the token came from, for the start-up log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Armed {
	/// Made now; the caller prints it once.
	Generated(String),
	/// The operator's `SETUP_TOKEN`; never printed.
	Configured,
}

fn digest(token: &str) -> String {
	use std::fmt::Write as _;
	Sha256::digest(token.as_bytes())
		.iter()
		.fold(String::with_capacity(64), |mut s, b| {
			let _ = write!(s, "{b:02x}");
			s
		})
}

impl SetupToken {
	/// A token that is already spent: the state of an instance that has accounts.
	pub fn spent() -> Self {
		Self::default()
	}

	/// Arms `configured` when the operator set one, else a fresh random token.
	///
	/// # Errors
	/// `Validation` when `configured` is shorter than [`MIN_CONFIGURED_LEN`], so a guessable
	/// value stops podkit from starting instead of guarding the administrator account.
	pub fn arm(configured: Option<&str>) -> AppResult<(Self, Armed)> {
		if let Some(token) = configured.map(str::trim).filter(|t| !t.is_empty()) {
			if token.chars().count() < MIN_CONFIGURED_LEN {
				return Err(ServerError::Validation(format!(
					"SETUP_TOKEN must be at least {MIN_CONFIGURED_LEN} characters"
				)));
			}
			return Ok((Self::armed_with(token), Armed::Configured));
		}
		let token = crypto::generate_token(RANDOM_BYTES);
		Ok((Self::armed_with(&token), Armed::Generated(token)))
	}

	fn armed_with(token: &str) -> Self {
		Self(RwLock::new(State::Armed(digest(token))))
	}

	/// No token is asked for. Tests only: production always starts armed or spent.
	#[cfg(test)]
	pub fn not_required() -> Self {
		Self(RwLock::new(State::NotRequired))
	}

	/// A token armed with a value the test knows.
	#[cfg(test)]
	pub fn for_test(token: &str) -> Self {
		Self::armed_with(token)
	}

	fn state(&self) -> State {
		self.0.read().map_or(State::Spent, |guard| guard.clone())
	}

	/// Whether a sign-up while no account exists has to present the token.
	pub fn required(&self) -> bool {
		match self.state() {
			State::Armed(_) | State::Spent => true,
			#[cfg(test)]
			State::NotRequired => false,
		}
	}

	/// Whether `presented` is the armed token. Always false once spent.
	fn matches(&self, presented: &str) -> bool {
		match self.state() {
			State::Armed(want) => crypto::constant_time_eq(&digest(presented.trim()), &want),
			State::Spent => false,
			#[cfg(test)]
			State::NotRequired => true,
		}
	}

	/// The first account exists: no token works from now on, until a restart finds the
	/// instance empty again.
	pub fn spend(&self) {
		if let Ok(mut guard) = self.0.write() {
			*guard = State::Spent;
		}
	}

	/// Checks the token a first sign-up presented. A missing one is refused without counting;
	/// a wrong one is booked with the sign-in throttle (per client address), so a flood of
	/// guesses waits like a flood of passwords.
	///
	/// # Errors
	/// `SetupTokenRequired` when none was given, `InvalidSetupToken` when it is wrong or spent,
	/// `TooManyAttempts` while the address waits.
	pub async fn check(&self, db: &sqlx::PgPool, presented: Option<&str>) -> AppResult<()> {
		if !self.required() {
			return Ok(());
		}
		let Some(presented) = presented.map(str::trim).filter(|t| !t.is_empty()) else {
			return Err(ServerError::SetupTokenRequired);
		};
		let attempt = throttle::begin(db, Who::setup()).await?;
		let right = self.matches(presented);
		attempt.finish(right).await;
		if right {
			Ok(())
		} else {
			Err(ServerError::InvalidSetupToken)
		}
	}
}

/// The address to open for the first sign-up, for the start-up log: `PUBLIC_URL` when set,
/// else the listen address (with a wildcard written as `localhost`).
pub fn register_url(public_url: Option<&str>, host: &str, port: i32) -> String {
	if let Some(base) = public_url {
		return format!("{base}/register");
	}
	let host = match host {
		"0.0.0.0" | "::" | "[::]" | "" => "localhost",
		other => other,
	};
	let host = if host.contains(':') && !host.starts_with('[') {
		format!("[{host}]")
	} else {
		host.to_owned()
	};
	format!("http://{host}:{port}/register")
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn a_generated_token_matches_only_itself_and_only_until_spent() {
		let (setup, armed) = SetupToken::arm(None).unwrap();
		let Armed::Generated(token) = armed else {
			panic!("expected a generated token");
		};
		assert_eq!(token.len(), RANDOM_BYTES * 2);
		assert!(setup.required());
		assert!(setup.matches(&token));
		assert!(setup.matches(&format!("  {token}\n")), "pasted with spaces");
		assert!(!setup.matches(&token.to_uppercase()));
		assert!(!setup.matches(&token[1..]));
		assert!(!setup.matches(""));

		setup.spend();
		assert!(setup.required(), "spent still asks, and nothing matches");
		assert!(!setup.matches(&token));
	}

	#[test]
	fn two_starts_make_two_tokens() {
		let (_, a) = SetupToken::arm(None).unwrap();
		let (_, b) = SetupToken::arm(None).unwrap();
		assert_ne!(a, b);
	}

	#[test]
	fn a_configured_token_is_used_and_never_echoed() {
		let (setup, armed) = SetupToken::arm(Some("operator-chosen-secret-0123456789")).unwrap();
		assert_eq!(armed, Armed::Configured);
		assert!(setup.matches("operator-chosen-secret-0123456789"));
		assert!(!setup.matches("operator-chosen-secret-012345678"));
		// Blank means unset: a random one is made.
		let (_, armed) = SetupToken::arm(Some("   ")).unwrap();
		assert!(matches!(armed, Armed::Generated(_)));
	}

	#[test]
	fn a_short_configured_token_is_refused() {
		for short in [
			"short",
			"sixteen-chars-ok",
			&"x".repeat(MIN_CONFIGURED_LEN - 1),
		] {
			assert!(
				matches!(
					SetupToken::arm(Some(short)),
					Err(ServerError::Validation(_))
				),
				"{short}"
			);
		}
	}

	#[test]
	fn the_digest_is_held_not_the_token() {
		let setup = SetupToken::for_test("the-test-setup-token");
		let held = format!("{setup:?}");
		assert!(!held.contains("the-test-setup-token"), "{held}");
	}

	#[test]
	fn the_register_url_prefers_public_url() {
		assert_eq!(
			register_url(Some("https://pk.example.com"), "0.0.0.0", 8080),
			"https://pk.example.com/register"
		);
		assert_eq!(
			register_url(None, "0.0.0.0", 8080),
			"http://localhost:8080/register"
		);
		assert_eq!(
			register_url(None, "192.168.1.10", 9000),
			"http://192.168.1.10:9000/register"
		);
		assert_eq!(register_url(None, "::1", 80), "http://[::1]:80/register");
	}
}
