//! Account use cases shared by the JSON API and the web UI.

use sqlx::PgPool;
use zeroize::Zeroizing;

use super::setup::SetupToken;
use super::{users, validate_email};
use crate::error::{AppResult, ServerError};
use crate::state::Registration;

const MAX_NAME_LEN: usize = 100;
const MIN_PASSWORD_LEN: usize = 8;

/// Whether the instance accepts a sign-up right now. A `first-user` instance
/// stops accepting once any account exists.
pub async fn registration_allowed(db: &PgPool, policy: Registration) -> AppResult<bool> {
	Ok(match policy {
		Registration::Open => true,
		Registration::Closed => false,
		Registration::FirstUser => !users::any(db).await?,
	})
}

/// Whether the sign-up page must ask for the setup token: no account exists yet, the policy
/// lets somebody sign up, and the token is in force (see [`super::setup`]).
pub async fn setup_token_needed(
	db: &PgPool,
	policy: Registration,
	setup: &SetupToken,
) -> AppResult<bool> {
	Ok(policy != Registration::Closed && setup.required() && !users::any(db).await?)
}

/// What a person types to sign up.
#[derive(Debug, Clone, Copy)]
pub struct SignUp<'a> {
	pub name: &'a str,
	pub email: &'a str,
	pub password: &'a str,
	/// The first-run setup token; looked at only while no account exists.
	pub setup_token: Option<&'a str>,
}

/// The name, trimmed, when it fits; refuses a password that is too short.
///
/// # Errors
/// `Validation` with a sentence for the person.
pub(crate) fn check_details<'a>(name: &'a str, password: &str) -> AppResult<&'a str> {
	let name = name.trim();
	if name.is_empty() || name.chars().count() > MAX_NAME_LEN {
		return Err(ServerError::Validation(format!(
			"Name must be 1 to {MAX_NAME_LEN} characters."
		)));
	}
	if password.chars().count() < MIN_PASSWORD_LEN {
		return Err(ServerError::Validation(format!(
			"Password must be at least {MIN_PASSWORD_LEN} characters."
		)));
	}
	Ok(name)
}

/// Argon2id of a new password.
///
/// # Errors
/// `Internal` when hashing fails (logged).
pub(crate) async fn hash_password(password: &str) -> AppResult<String> {
	crypto::hash(Zeroizing::new(password.to_owned()))
		.await
		.map_err(|e| {
			tracing::error!(error = %format!("{e:#}"), "password hashing failed");
			ServerError::Internal
		})
}

/// A duplicate email as `EmailTaken`, anything else as it is.
pub(crate) fn email_taken(e: sqlx::Error) -> ServerError {
	if crate::db::is_unique_violation(&e, users::EMAIL_UNIQUE) {
		ServerError::EmailTaken
	} else {
		e.into()
	}
}

/// Creates an account if the registration policy allows it.
///
/// While no account exists the sign-up makes the instance administrator, so on any policy
/// that lets somebody in it must carry the setup token (checked before anything else is
/// looked at, and counted by the sign-in throttle when wrong). The first account spends the
/// token. An invite link is the other way in; see `members::invites`.
///
/// # Errors
/// `RegistrationClosed` when the policy refuses, `SetupTokenRequired` / `InvalidSetupToken`
/// for the first account, `Validation` for a bad name, email or password, `EmailTaken` for a
/// duplicate email.
pub async fn register(
	db: &PgPool,
	policy: Registration,
	setup: &SetupToken,
	form: SignUp<'_>,
) -> AppResult<()> {
	// Refuse early so a closed instance doesn't spend CPU hashing passwords.
	if !registration_allowed(db, policy).await? {
		return Err(ServerError::RegistrationClosed);
	}
	let first = !users::any(db).await?;
	if first {
		setup.check(db, form.setup_token).await?;
	}

	let email = validate_email(form.email)
		.map_err(|e| ServerError::Validation(e.to_string()))?
		.normalize();
	let name = check_details(form.name, form.password)?;
	let password_hash = hash_password(form.password).await?;

	// The first account goes in only while the table is still empty, whatever the policy, so
	// two people holding the token cannot both become "the first".
	let inserted = if first {
		users::insert_if_no_users(db, name, &email, &password_hash).await
	} else {
		Ok(None)
	}
	.map_err(email_taken)?;
	if inserted.is_some() {
		setup.spend();
		return Ok(());
	}
	match policy {
		Registration::Open => users::insert(db, name, &email, &password_hash)
			.await
			.map(|_| ())
			.map_err(email_taken),
		Registration::FirstUser | Registration::Closed => Err(ServerError::RegistrationClosed),
	}
}

/// The email a person signs in with, for their account page.
pub async fn email_of(db: &PgPool, id: crate::ids::UserId) -> AppResult<Option<String>> {
	Ok(users::email_of(db, id).await?)
}

/// `user`'s open sessions, newest first, with the one making the request marked.
pub async fn sessions(
	db: &PgPool,
	user: crate::ids::UserId,
	current_token: &str,
) -> AppResult<Vec<super::session::Entry>> {
	Ok(super::session::list(db, user, current_token).await?)
}

/// Signs one of `user`'s other sessions out. A session of someone else answers as if it did
/// not exist.
pub async fn revoke_session(db: &PgPool, user: crate::ids::UserId, id: i64) -> AppResult<()> {
	if !super::session::revoke_id(db, user, id).await? {
		return Err(ServerError::NotFound);
	}
	super::record_everywhere(
		db,
		user,
		"session.revoke",
		crate::audit::Target::new("session", id),
		crate::audit::Diff::new(),
	)
	.await;
	Ok(())
}

/// Signs every session of `user` out except the one making the request; returns how many.
pub async fn revoke_other_sessions(
	db: &PgPool,
	user: crate::ids::UserId,
	current_token: &str,
) -> AppResult<u64> {
	let n = super::session::revoke_others(db, user, current_token).await?;
	if n > 0 {
		super::record_everywhere(
			db,
			user,
			"session.revoke",
			crate::audit::Target::none("session").named("all other sessions"),
			crate::audit::Diff::new().set("count", n),
		)
		.await;
	}
	Ok(n)
}

#[cfg(test)]
mod tests {
	use super::{SignUp, register};
	use crate::auth::setup::SetupToken;
	use crate::auth::users;
	use crate::error::ServerError;
	use crate::state::Registration;
	use crate::tests::{PgPool, count, db_test};

	/// A transaction that has taken the first-user slot but not committed
	/// yet must make a second registration wait for it, then be refused.
	/// Without the lock, the second one doesn't see the uncommitted row.
	#[db_test]
	async fn registration_waits_for_an_uncommitted_first_user(db: PgPool) {
		let mut first = db.begin().await.unwrap();
		let got = users::insert_if_no_users_in(&mut first, "a", "a@example.com", "x")
			.await
			.unwrap();
		assert!(got.is_some());

		// Straight to the insert: `register` also spends time hashing, which
		// would hide a missing lock.
		let second = {
			let db = db.clone();
			tokio::spawn(
				async move { users::insert_if_no_users(&db, "b", "b@example.com", "x").await },
			)
		};
		tokio::time::sleep(std::time::Duration::from_millis(500)).await;
		assert!(!second.is_finished(), "the second sign-up must wait");

		first.commit().await.unwrap();
		assert!(second.await.unwrap().unwrap().is_none());
		assert_eq!(count(&db, "users").await, 1);
	}

	#[db_test]
	async fn registration_concurrent_first_users_yield_exactly_one_account(db: PgPool) {
		for round in 0..3 {
			sqlx::query("DELETE FROM users WHERE id > 0")
				.execute(&db)
				.await
				.unwrap();
			// Losers that find the token spent count as wrong guesses; start each round clean.
			sqlx::query("DELETE FROM auth_failures")
				.execute(&db)
				.await
				.unwrap();
			let setup = std::sync::Arc::new(SetupToken::for_test("the-race-setup-token"));
			let attempts = (0..8).map(|i| {
				let db = db.clone();
				let setup = setup.clone();
				tokio::spawn(async move {
					let email = format!("racer{round}-{i}@example.com");
					register(
						&db,
						Registration::FirstUser,
						&setup,
						SignUp {
							name: "racer",
							email: &email,
							password: "correct horse battery",
							setup_token: Some("the-race-setup-token"),
						},
					)
					.await
				})
			});
			let mut won = 0;
			for attempt in attempts {
				match attempt.await.unwrap() {
					Ok(()) => won += 1,
					// A loser either finds the table taken or the token already spent.
					Err(
						ServerError::RegistrationClosed
						| ServerError::InvalidSetupToken
						| ServerError::TooManyAttempts { .. },
					) => {}
					Err(e) => panic!("unexpected error: {e}"),
				}
			}
			assert_eq!(won, 1, "round {round}");
			assert_eq!(count(&db, "users").await, 1, "round {round}");
		}
	}
}
