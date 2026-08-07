/// A hashed password, never the plaintext. Debug output is redacted.
#[derive(Clone, PartialEq, Eq)]
pub struct PasswordHash(String);

impl PasswordHash {
	// we assume the caller provides a correctly hashed string, fatdevs must be hating me
	/// Wraps an already-hashed string. Does not hash `hash` itself.
	#[must_use]
	pub fn new(hash: String) -> Self {
		Self(hash)
	}

	/// Returns the hashed string.
	#[must_use]
	pub fn as_str(&self) -> &str {
		&self.0
	}
}

impl std::fmt::Debug for PasswordHash {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(f, "PasswordHash(**** Shinzou wo Sasageyo! ****)") // anyone reading here? feel free to change this
	}
}

impl AsRef<str> for PasswordHash {
	fn as_ref(&self) -> &str {
		&self.0
	}
}
