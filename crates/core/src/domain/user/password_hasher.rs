use crate::domain::shared::errors::DomainResult;
use crate::domain::user::value_objects::PasswordHash;
use async_trait::async_trait;

// contract owned by the domain, implemented in infrastructure
// application layer receives this as a dependency and domain never calls it
/// Hashes and verifies passwords. Contract owned by the domain, implemented in infrastructure.
#[async_trait]
pub trait PasswordHasher: Send + Sync {
	/// Hashes a plaintext password.
	///
	/// # Errors
	///
	/// Returns an error if the hashing algorithm fails.
	async fn hash(&self, plaintext: &str) -> DomainResult<PasswordHash>;

	/// Verifies a plaintext password against a stored hash.
	///
	/// # Errors
	///
	/// Returns an error if the underlying verification operation fails.
	async fn verify(&self, plaintext: &str, hash: &PasswordHash) -> DomainResult<bool>;
}
