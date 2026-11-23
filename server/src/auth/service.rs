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
	use super::register;
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
