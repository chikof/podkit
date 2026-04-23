use std::sync::LazyLock;

use anyhow::{Context, anyhow};
use argon2::password_hash::SaltString;
use argon2::password_hash::rand_core::OsRng;
use argon2::{
	Algorithm, Argon2, Params, PasswordHash, PasswordHasher, PasswordVerifier, Version,
	password_hash,
};
use tokio::task;
use zeroize::Zeroizing;

/// Builds an Argon2id instance with hardened parameters:
/// m=64 MiB, t=3 iterations, p=4 lanes
fn argon2() -> anyhow::Result<Argon2<'static>> {
	Ok(Argon2::new(
		Algorithm::Argon2id,
		Version::V0x13,
		Params::new(64 * 1024, 3, 4, None).map_err(|e| anyhow!(e))?,
	))
}

/// Pre-computed valid Argon2id hash used for constant-time dummy verification.
///
/// When a user is not found, callers should verify the supplied password against
/// this hash (and discard the result) to prevent user-enumeration via timing analysis.
/// An invalid PHC string short-circuits `PasswordHash::new` and returns in microseconds
/// instead of the full ~100 ms argon2 window, this defeats that.
pub static DUMMY_HASH: LazyLock<String> = LazyLock::new(|| {
	let salt = SaltString::generate(&mut OsRng);
	argon2()
		.expect("valid argon2 params")
		.hash_password(b"__podkit_timing_sentinel__", &salt)
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
	task::spawn_blocking(move || {
		let salt = SaltString::generate(&mut OsRng);
		Ok(argon2()?
			.hash_password(password.as_bytes(), &salt)
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
/// For timing-attack protection when a user is not found, pass [`DUMMY_HASH`]
/// as `hash` and discard the result. This ensures the response time is
/// indistinguishable from a real failed login.
///
/// Runs on a blocking thread pool to avoid stalling the async runtime.
///
/// # Errors
/// Fails if `hash` is not a valid PHC string or the Argon2 operation fails.
pub async fn verify(password: Zeroizing<String>, hash: String) -> anyhow::Result<bool> {
	task::spawn_blocking(move || {
		let hash =
			PasswordHash::new(&hash).map_err(|e| anyhow!(e).context("invalid password hash"))?;

		match argon2()?.verify_password(password.as_bytes(), &hash) {
			Ok(()) => Ok(true),
			Err(password_hash::Error::Password) => Ok(false),
			Err(e) => Err(anyhow!(e).context("failed to verify password")),
		}
	})
	.await
	.context("panic in verify()")?
}
