use crate::domain::shared::errors::{DomainError, DomainResult};
use crate::validation::validate_email;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Email(String);

impl Email {
	pub fn new(raw: &str) -> DomainResult<Self> {
		let validated = validate_email(raw).map_err(|e| DomainError::Validation(e.to_string()))?; // I'm not really sure about to_string, but this serves as an example so...
		Ok(Self(validated.normalize()))
	}

	#[inline]
	pub fn as_str(&self) -> &str {
		&self.0
	}
}

// We could add auth providers in the future :P
// pub struct Identity {
//     pub email: Email,
//     pub verified: bool,
//     pub provider: Option<AuthProvider>,
// }
