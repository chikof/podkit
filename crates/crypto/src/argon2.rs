use std::sync::LazyLock;

use anyhow::{Context, anyhow};
use argon2::{
	Algorithm, Argon2, Params, PasswordHasher, PasswordVerifier, Version, password_hash,
	password_hash::phc::PasswordHash,
};
use tokio::{sync::Semaphore, task};
use zeroize::Zeroizing;

/// One hash or verification at a time, so a sign-in never costs the server more than the
/// 64 MiB of a single hash, however many arrive at once and whatever the machine: one
/// permit per core let a 16-core host spend 1 GiB on a burst. The others wait their turn,
/// about 100 ms per hash ahead of them.
static LIMIT: Semaphore = Semaphore::const_new(1);

/// Builds an Argon2id instance with hardened parameters:
/// m=64 MiB, t=3 iterations, p=4 lanes
fn argon2() -> anyhow::Result<Argon2<'static>> {
	Ok(Argon2::new(
		Algorithm::Argon2id,
		Version::V0x13,
		Params::new(64 * 1024, 3, 4, None).map_err(|e| anyhow!(e))?,
	))
}

/// A valid Argon2id hash nobody knows the password of, for [`verify_unknown`]. An invalid
/// PHC string would short-circuit `PasswordHash::new` and return in microseconds instead of
/// the full ~100 ms argon2 window.
static DUMMY_HASH: LazyLock<String> = LazyLock::new(|| {
	argon2()
		.expect("valid argon2 params")
		.hash_password(b"__podkit_timing_sentinel__")
		.expect("failed to compute dummy hash")
		.to_string()
});

/// Hashes a password using Argon2id and a cryptographically random salt.
///
/// The password is zeroed from memory after hashing. The returned string is a
/// PHC-formatted hash safe to store directly in the database.
///
/// Runs on a blocking thread pool to avoid stalling the async runtime.
///
/// # Errors
/// Fails if the Argon2 parameters are invalid or the hashing operation fails.
pub async fn hash(password: Zeroizing<String>) -> anyhow::Result<String> {
	let _permit = LIMIT.acquire().await.context("argon2 limiter closed")?;
	task::spawn_blocking(move || {
		Ok(argon2()?
			.hash_password(password.as_bytes())
			.map_err(|e| anyhow!(e).context("failed to hash password"))?
			.to_string())
	})
	.await
	.context("panic in hash()")?
}

/// Verifies a plaintext password against an Argon2id PHC hash.
///
/// Returns `true` if the password matches, `false` if it does not.
/// The password is zeroed from memory after verification.
///
/// Runs on a blocking thread pool to avoid stalling the async runtime.
///
/// # Errors
/// Fails if `hash` is not a valid PHC string or the Argon2 operation fails.
pub async fn verify(password: Zeroizing<String>, hash: String) -> anyhow::Result<bool> {
	check(password, move || hash).await
}

/// Spends the time a [`verify`] takes, for a sign-in whose user does not exist: the
/// response time then does not say which emails have an account.
///
/// # Errors
/// Fails if the Argon2 operation fails.
pub async fn verify_unknown(password: Zeroizing<String>) -> anyhow::Result<()> {
	check(password, || DUMMY_HASH.clone()).await.map(drop)
}

/// `hash` runs on the blocking thread with the permit held, so the first [`DUMMY_HASH`],
/// itself a full hash, is neither on an async worker nor outside the limit.
async fn check(
	password: Zeroizing<String>,
	hash: impl FnOnce() -> String + Send + 'static,
) -> anyhow::Result<bool> {
	let _permit = LIMIT.acquire().await.context("argon2 limiter closed")?;
	task::spawn_blocking(move || {
		let hash = hash();
		let hash =
			PasswordHash::new(&hash).map_err(|e| anyhow!(e).context("invalid password hash"))?;

		match argon2()?.verify_password(password.as_bytes(), &hash) {
			Ok(()) => Ok(true),
			Err(password_hash::Error::PasswordInvalid) => Ok(false),
			Err(e) => Err(anyhow!(e).context("failed to verify password")),
		}
	})
	.await
	.context("panic in verify()")?
}
