//! Account use cases shared by the JSON API and the web UI.

use sqlx::PgPool;
use zeroize::Zeroizing;

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

/// Creates an account if the registration policy allows it.
///
/// # Errors
/// `RegistrationClosed` when the policy refuses, `Validation` for a bad name,
/// email or password, `EmailTaken` for a duplicate email.
pub async fn register(
	db: &PgPool,
	policy: Registration,
	name: &str,
	email: &str,
	password: &str,
) -> AppResult<()> {
	// Refuse early so a closed instance doesn't spend CPU hashing passwords.
	if !registration_allowed(db, policy).await? {
		return Err(ServerError::RegistrationClosed);
	}

	let email = validate_email(email)
		.map_err(|e| ServerError::Validation(e.to_string()))?
		.normalize();
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

	let password_hash = crypto::hash(Zeroizing::new(password.to_owned()))
		.await
		.map_err(|e| {
			tracing::error!(error = %format!("{e:#}"), "password hashing failed");
			ServerError::Internal
		})?;

	let inserted = match policy {
		Registration::Open => users::insert(db, name, &email, &password_hash)
			.await
			.map(Some),
		Registration::FirstUser => {
			users::insert_if_no_users(db, name, &email, &password_hash).await
		}
		Registration::Closed => return Err(ServerError::RegistrationClosed),
	}
	.map_err(|e| {
		if crate::db::is_unique_violation(&e, users::EMAIL_UNIQUE) {
			ServerError::EmailTaken
		} else {
			e.into()
		}
	})?;

	match inserted {
		Some(_) => Ok(()),
		None => Err(ServerError::RegistrationClosed),
	}
}

#[cfg(test)]
mod tests {
	use super::register;
	use crate::error::ServerError;
	use crate::state::Registration;
	use crate::tests::{PgPool, count, db_test};

	#[db_test]
	async fn registration_concurrent_first_users_yield_exactly_one_account(db: PgPool) {
		for round in 0..3 {
			sqlx::query("DELETE FROM users WHERE id > 0")
				.execute(&db)
				.await
				.unwrap();
			let attempts = (0..8).map(|i| {
				let db = db.clone();
				tokio::spawn(async move {
					register(
						&db,
						Registration::FirstUser,
						"racer",
						&format!("racer{round}-{i}@example.com"),
						"correct horse battery",
					)
					.await
				})
			});
			let mut won = 0;
			for attempt in attempts {
				match attempt.await.unwrap() {
					Ok(()) => won += 1,
					Err(ServerError::RegistrationClosed) => {}
					Err(e) => panic!("unexpected error: {e}"),
				}
			}
			assert_eq!(won, 1, "round {round}");
			assert_eq!(count(&db, "users").await, 1, "round {round}");
		}
	}
}
