use thiserror::Error;

pub type DomainResult<T> = Result<T, DomainError>;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum DomainError {
    #[error("validation error: {0}")]
    Validation(String),

    #[error("entity already exists")]
    AlreadyExists,

    #[error("role already assigned to membership")]
    AlreadyAssigned,
}

// Okay... we may need namespaced errors T.T