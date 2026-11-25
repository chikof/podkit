//! RS256 JSON Web Tokens, as GitHub wants them from an app: signed with the app's private key,
//! valid for minutes, only ever used to ask for an installation token.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rsa::pkcs1::DecodeRsaPrivateKey;
use rsa::pkcs8::DecodePrivateKey;
use rsa::{Pkcs1v15Sign, RsaPrivateKey};
use serde_json::json;
use sha2::{Digest, Sha256};

/// The ASN.1 `DigestInfo` header of a SHA-256 digest (RFC 8017, section 9.2). The signing
/// scheme below is the unprefixed one because `rsa` and `sha2` use different `digest` crates.
pub(super) const SHA256_PREFIX: [u8; 19] = [
	0x30, 0x31, 0x30, 0x0d, 0x06, 0x09, 0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02, 0x01, 0x05,
	0x00, 0x04, 0x20,
];

/// Why a token could not be made.
#[derive(Debug, PartialEq, Eq)]
pub enum JwtError {
	/// The key is neither a PKCS#1 nor a PKCS#8 RSA private key in PEM.
	BadKey,
	/// Signing failed.
	Sign,
}

/// Reads an RSA private key in PEM, the way GitHub hands it out (PKCS#1) or PKCS#8.
pub fn parse_key(pem: &str) -> Result<RsaPrivateKey, JwtError> {
	RsaPrivateKey::from_pkcs1_pem(pem)
		.or_else(|_| RsaPrivateKey::from_pkcs8_pem(pem))
		.map_err(|_| JwtError::BadKey)
}

/// The signed input of a token and the digest GitHub checks, for tests that verify one.
pub(super) fn digest_info(signing_input: &str) -> Vec<u8> {
	let mut info = SHA256_PREFIX.to_vec();
	info.extend_from_slice(&Sha256::digest(signing_input.as_bytes()));
	info
}

/// A JWT for GitHub App `app_id`, issued at `now` (unix seconds) minus a minute for clock
/// drift and valid for nine minutes: GitHub accepts at most ten.
pub fn app_token(pem: &str, app_id: i64, now: i64) -> Result<String, JwtError> {
	let key = parse_key(pem)?;
	let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"RS256","typ":"JWT"}"#);
	let claims = URL_SAFE_NO_PAD.encode(
		json!({ "iat": now - 60, "exp": now + 9 * 60, "iss": app_id.to_string() }).to_string(),
	);
	let input = format!("{header}.{claims}");
	// With an RNG the private-key operation is blinded: its timing says nothing about the key
	// (RUSTSEC-2023-0071, ignored in .cargo/audit.toml on that ground).
	let signature = key
		.sign_with_rng(
			&mut rsa::rand_core::OsRng,
			Pkcs1v15Sign::new_unprefixed(),
			&digest_info(&input),
		)
		.map_err(|_| JwtError::Sign)?;
	Ok(format!("{input}.{}", URL_SAFE_NO_PAD.encode(signature)))
}
