use crate::domain::shared::errors::DomainResult;
use crate::domain::user::value_objects::PasswordHash;

// contract owned by the domain, implemented in infrastructure
// application layer receives this as a dependency and domain never calls it
pub trait PasswordHasher: Send + Sync {
	fn hash(&self, plaintext: &str) -> DomainResult<PasswordHash>;
	fn verify(&self, plaintext: &str, hash: &PasswordHash) -> DomainResult<bool>;
}
