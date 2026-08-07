use async_trait::async_trait;
use podkit_core::domain::shared::errors::{DomainError, DomainResult};
use podkit_core::domain::user::PasswordHasher;
use podkit_core::domain::user::value_objects::PasswordHash;

use crypto::argon2;
use zeroize::Zeroizing;

/// Argon2-backed implementation of `podkit_core`'s `PasswordHasher` trait.
pub struct Argon2PasswordHasher;

#[async_trait]
impl PasswordHasher for Argon2PasswordHasher {
	async fn hash(&self, plaintext: &str) -> DomainResult<PasswordHash> {
		let hash = argon2::hash(Zeroizing::new(plaintext.to_string()))
			.await
			.map_err(|e| DomainError::Infrastructure(e.to_string()))?;
		Ok(PasswordHash::new(hash))
	}

	async fn verify(&self, plaintext: &str, hash: &PasswordHash) -> DomainResult<bool> {
		argon2::verify(
			Zeroizing::new(plaintext.to_string()),
			hash.as_str().to_string(),
		)
		.await
		.map_err(|e| DomainError::Infrastructure(e.to_string()))
	}
}
