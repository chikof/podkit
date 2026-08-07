use thiserror::Error;

/// Shorthand for `Result<T, DomainError>`, used throughout the domain layer.
pub type DomainResult<T> = Result<T, DomainError>;

/// Errors the domain layer can produce. Infrastructure adapters map their own
/// errors into these before returning to callers.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum DomainError {
	/// Input failed a domain rule; the message is safe to show to a caller.
	#[error("validation error: {0}")]
	Validation(String),

	/// A uniqueness constraint was violated (e.g. duplicate slug).
	#[error("entity already exists")]
	AlreadyExists,

	/// The role is already assigned to this membership.
	#[error("role already assigned to membership")]
	AlreadyAssigned,

	/// An underlying infrastructure operation failed (db, runtime, etc).
	#[error("infrastructure error: {0}")]
	Infrastructure(String),
}

// Okay... we may need namespaced errors T.T
