//! Making and reading SSH keys: generation (Ed25519 or RSA-4096), and import of OpenSSH and
//! PEM (PKCS#1, PKCS#8) private keys. Pure functions, no database.
//!
//! Every key leaves here in OpenSSH format, whatever it came in as, so the `ssh` binary reads
//! it back the same way.

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use rsa::pkcs1::DecodeRsaPrivateKey;
use rsa::pkcs8::DecodePrivateKey;
use ssh_key::private::{Ed25519Keypair, KeypairData, RsaKeypair};
use ssh_key::rand_core::OsRng;
use ssh_key::{Algorithm, HashAlg, LineEnding, PrivateKey};
use zeroize::Zeroizing;

/// Size of the RSA keys podkit generates.
pub const RSA_BITS: usize = 4096;

/// The comment on every public key podkit hands out.
const COMMENT: &str = "podkit";

/// What a generated key is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
	#[default]
	Ed25519,
	Rsa,
	/// Only ever imported.
	#[serde(skip)]
	Ecdsa,
}

impl Kind {
	/// The stored name.
	#[must_use]
	pub const fn as_str(self) -> &'static str {
		match self {
			Self::Ed25519 => "ed25519",
			Self::Rsa => "rsa",
			Self::Ecdsa => "ecdsa",
		}
	}

	/// What the page calls it.
	#[must_use]
	pub fn label(name: &str) -> &'static str {
		match name {
			"ed25519" => "Ed25519",
			"rsa" => "RSA-4096",
			"ecdsa" => "ECDSA",
			_ => "Unknown",
		}
	}
}

/// A key as it is stored and shown: the private half is for the vault only.
pub struct Material {
	pub kind: Kind,
	/// The OpenSSH private key, `-----BEGIN OPENSSH PRIVATE KEY-----`.
	pub private_openssh: Zeroizing<String>,
	/// One `authorized_keys` line.
	pub public_key: String,
	/// `SHA256:<base64>`, as `ssh-keygen -lf` prints it.
	pub fingerprint: String,
}

/// Why a key was refused. The text is shown to the user.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum KeyError {
	#[error(
		"This key is protected by a passphrase, and podkit cannot ask for it when it connects. \
		Remove the passphrase first (ssh-keygen -p -N \"\" -f <keyfile>) and import it again."
	)]
	Passphrase,
	#[error("That is a public key. Paste the private key (it starts with -----BEGIN).")]
	PublicKey,
	#[error(
		"That is not a private key podkit can read. Paste an OpenSSH or PEM private key \
		(-----BEGIN OPENSSH PRIVATE KEY-----, -----BEGIN RSA PRIVATE KEY----- or -----BEGIN PRIVATE KEY-----)."
	)]
	Unreadable,
	#[error("{0} keys are not supported. Use Ed25519, RSA or ECDSA.")]
	Unsupported(String),
	#[error("Could not generate the key.")]
	Generate,
}

fn material(key: &PrivateKey) -> Result<Material, KeyError> {
	let kind = match key.algorithm() {
		Algorithm::Ed25519 => Kind::Ed25519,
		Algorithm::Rsa { .. } => Kind::Rsa,
		Algorithm::Ecdsa { .. } => Kind::Ecdsa,
		other => return Err(KeyError::Unsupported(other.to_string())),
	};
	// The comment is the importer's own text (a mail address, a host name); podkit's
	// public keys all carry the same one.
	let mut key = key.clone();
	key.set_comment(COMMENT);
	let private_openssh = key
		.to_openssh(LineEnding::LF)
		.map_err(|_| KeyError::Unreadable)?;
	let public_key = key
		.public_key()
		.to_openssh()
		.map_err(|_| KeyError::Unreadable)?;
	Ok(Material {
		kind,
		private_openssh,
		public_key,
		fingerprint: key.fingerprint(HashAlg::Sha256).to_string(),
	})
}

/// Makes a new key of `kind` (never ECDSA).
///
/// # Errors
/// `Generate` when the randomness source or the RSA search fails.
pub fn generate(kind: Kind) -> Result<Material, KeyError> {
	let pair = match kind {
		Kind::Ed25519 | Kind::Ecdsa => KeypairData::Ed25519(Ed25519Keypair::random(&mut OsRng)),
		Kind::Rsa => KeypairData::Rsa(
			RsaKeypair::random(&mut OsRng, RSA_BITS).map_err(|_| KeyError::Generate)?,
		),
	};
	let key = PrivateKey::new(pair, COMMENT).map_err(|_| KeyError::Generate)?;
	material(&key)
}

/// Reads a private key: OpenSSH, PKCS#1 or PKCS#8 PEM.
///
/// # Errors
/// `Passphrase` for an encrypted key, `PublicKey` for a public one, `Unreadable` or
/// `Unsupported` otherwise.
pub fn import(text: &str) -> Result<Material, KeyError> {
	let text = text.trim();
	if text.starts_with("ssh-") || text.starts_with("ecdsa-") || text.starts_with("sk-") {
		return Err(KeyError::PublicKey);
	}
	if text.contains("BEGIN PUBLIC KEY") || text.contains("BEGIN RSA PUBLIC KEY") {
		return Err(KeyError::PublicKey);
	}
	if text.contains("ENCRYPTED") {
		return Err(KeyError::Passphrase);
	}
	let key = if text.contains("BEGIN OPENSSH PRIVATE KEY") {
		let key = PrivateKey::from_openssh(text).map_err(|_| KeyError::Unreadable)?;
		if key.is_encrypted() {
			return Err(KeyError::Passphrase);
		}
		key
	} else if text.contains("BEGIN RSA PRIVATE KEY") {
		let rsa = rsa::RsaPrivateKey::from_pkcs1_pem(text).map_err(|_| KeyError::Unreadable)?;
		from_rsa(&rsa)?
	} else if text.contains("BEGIN PRIVATE KEY") {
		pkcs8(text)?
	} else if text.contains("BEGIN EC PRIVATE KEY") {
		return Err(KeyError::Unsupported(
			"Legacy EC PEM (use an OpenSSH key; ssh-keygen -p -m RFC4716 converts)".to_owned(),
		));
	} else {
		return Err(KeyError::Unreadable);
	};
	material(&key)
}

fn from_rsa(rsa: &rsa::RsaPrivateKey) -> Result<PrivateKey, KeyError> {
	let pair = RsaKeypair::try_from(rsa).map_err(|_| KeyError::Unreadable)?;
	PrivateKey::new(KeypairData::Rsa(pair), COMMENT).map_err(|_| KeyError::Unreadable)
}

/// The fixed DER prefix of an unencrypted Ed25519 PKCS#8 key (RFC 8410): the 32-byte seed
/// follows it.
const ED25519_PKCS8_PREFIX: [u8; 16] = [
	0x30, 0x2e, 0x02, 0x01, 0x00, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x04, 0x22, 0x04, 0x20,
];

/// A `BEGIN PRIVATE KEY` block holds RSA or Ed25519.
fn pkcs8(text: &str) -> Result<PrivateKey, KeyError> {
	let body: String = text
		.lines()
		.filter(|l| !l.starts_with("-----"))
		.collect::<Vec<_>>()
		.concat();
	let der = STANDARD
		.decode(body.trim())
		.map_err(|_| KeyError::Unreadable)?;
	if der.len() == ED25519_PKCS8_PREFIX.len() + 32 && der.starts_with(&ED25519_PKCS8_PREFIX) {
		let seed: [u8; 32] = der[ED25519_PKCS8_PREFIX.len()..]
			.try_into()
			.map_err(|_| KeyError::Unreadable)?;
		let pair = Ed25519Keypair::from_seed(&seed);
		return PrivateKey::new(KeypairData::Ed25519(pair), COMMENT)
			.map_err(|_| KeyError::Unreadable);
	}
	match rsa::RsaPrivateKey::from_pkcs8_pem(text) {
		Ok(rsa) => from_rsa(&rsa),
		Err(_) => Err(KeyError::Unsupported("This kind of PKCS#8".to_owned())),
	}
}

/// Derives the public line and fingerprint of a stored OpenSSH private key.
///
/// # Errors
/// `Unreadable` when the stored text is not a key.
pub fn derive(private_openssh: &str) -> Result<Material, KeyError> {
	let key = PrivateKey::from_openssh(private_openssh.trim()).map_err(|_| KeyError::Unreadable)?;
	if key.is_encrypted() {
		return Err(KeyError::Passphrase);
	}
	material(&key)
}

/// The base64 key blob of a public-key line: what identifies it in `authorized_keys`
/// whatever its comment or options.
#[must_use]
pub fn blob(public_key: &str) -> Option<&str> {
	let mut tokens = public_key.split_whitespace();
	tokens.next()?;
	tokens.next()
}

#[cfg(test)]
#[path = "keygen_tests.rs"]
mod tests;
