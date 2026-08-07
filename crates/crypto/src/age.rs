use age::secrecy::ExposeSecret;
use age::x25519::Identity;
use thiserror::Error as ThisError;

/// Everything that can go wrong loading an identity or encrypting/decrypting
/// with it.
#[derive(ThisError, Debug)]
pub enum AgeError {
	/// The given string isn't a parseable age X25519 identity.
	#[error("invalid age identity: {0}")]
	InvalidIdentity(&'static str),

	/// Bubbled up straight from the `age` crate's encryption path.
	#[error("encrypt failed: {0}")]
	Encrypt(#[from] age::EncryptError),

	/// Bubbled up straight from the `age` crate's decryption path.
	#[error("decrypt failed: {0}")]
	Decrypt(#[from] age::DecryptError),

	/// Decryption succeeded but the plaintext bytes aren't valid UTF-8.
	#[error("decrypted payload is not valid utf-8")]
	InvalidUtf8,
}

/// Encrypts/decrypts secrets at rest (ssh keys, deploy keys, env var values)
/// with a single server-held age X25519 identity.
///
/// Ciphertext is binary, so store it as `BYTEA` rather than `TEXT`.
pub struct SecretBox {
	identity: Identity,
}

impl SecretBox {
	/// Loads a `SecretBox` from an `AGE-SECRET-KEY-1...` identity string
	/// (as produced by `age-keygen` or [`generate_identity`]).
	///
	/// # Errors
	/// Returns an error if `identity` is not a valid age X25519 identity.
	pub fn from_identity_str(identity: &str) -> Result<Self, AgeError> {
		let identity = identity
			.parse::<Identity>()
			.map_err(AgeError::InvalidIdentity)?;
		Ok(Self { identity })
	}

	/// Encrypts `plaintext` to this box's own recipient key.
	///
	/// # Errors
	/// Returns an error if encryption fails.
	pub fn encrypt(&self, plaintext: &str) -> Result<Vec<u8>, AgeError> {
		let recipient = self.identity.to_public();
		Ok(age::encrypt(&recipient, plaintext.as_bytes())?)
	}

	/// Decrypts `ciphertext` produced by [`Self::encrypt`].
	///
	/// # Errors
	/// Returns an error if decryption fails or the payload isn't valid UTF-8.
	pub fn decrypt(&self, ciphertext: &[u8]) -> Result<String, AgeError> {
		let bytes = age::decrypt(&self.identity, ciphertext)?;
		String::from_utf8(bytes).map_err(|_| AgeError::InvalidUtf8)
	}
}

/// Generates a new age X25519 identity string, e.g. for provisioning
/// `AGE_SECRET_KEY` in a fresh deployment. Not wired to any CLI yet, so call
/// it manually (`age-keygen` works too, this exists so no external binary is
/// required).
#[must_use]
pub fn generate_identity() -> String {
	Identity::generate().to_string().expose_secret().to_string()
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn round_trip() {
		let identity = generate_identity();
		let secrets = SecretBox::from_identity_str(&identity).unwrap();

		let ciphertext = secrets.encrypt("super-secret-ssh-key").unwrap();
		assert_ne!(ciphertext, b"super-secret-ssh-key");

		let plaintext = secrets.decrypt(&ciphertext).unwrap();
		assert_eq!(plaintext, "super-secret-ssh-key");
	}

	#[test]
	fn wrong_identity_fails_to_decrypt() {
		let a = SecretBox::from_identity_str(&generate_identity()).unwrap();
		let b = SecretBox::from_identity_str(&generate_identity()).unwrap();

		let ciphertext = a.encrypt("payload").unwrap();
		assert!(b.decrypt(&ciphertext).is_err());
	}

	#[test]
	fn rejects_malformed_identity() {
		assert!(SecretBox::from_identity_str("not-an-age-key").is_err());
	}
}
