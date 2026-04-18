use anyhow::{anyhow, Context};
use argon2::password_hash::rand_core::OsRng;
use argon2::password_hash::SaltString;
use argon2::{
	password_hash, Algorithm, Argon2, Params, PasswordHash, PasswordHasher, PasswordVerifier,
	Version,
};
use tokio::task;
use zeroize::Zeroizing;

fn argon2() -> anyhow::Result<Argon2<'static>> {
	Ok(Argon2::new(
		Algorithm::Argon2id,
		Version::V0x13,
		Params::new(64 * 1024, 3, 4, None).map_err(|e| anyhow!(e))?,
	))
}

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
