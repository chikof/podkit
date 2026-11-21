//! Time-based one-time passwords, RFC 6238 over HMAC-SHA1 (RFC 4226 truncation): the codes
//! authenticator apps show. SHA-1 is what every app supports; the SHA-2 variants are not
//! understood by Google Authenticator and others, and HMAC-SHA1 is not weakened by SHA-1's
//! collision attacks.

use hmac::{Hmac, KeyInit, Mac};
use sha1::Sha1;

/// Seconds a code is valid for.
pub const STEP_SECS: u64 = 30;
/// Digits in a code.
pub const DIGITS: u32 = 6;
/// A code from one step before or after is accepted: clocks drift, and people type slowly.
pub const SKEW_STEPS: u64 = 1;
/// 160 bits, the RFC 4226 recommendation.
pub const SECRET_LEN: usize = 20;

const BASE32: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";

/// A fresh random secret.
pub fn generate_secret() -> [u8; SECRET_LEN] {
	let mut secret = [0u8; SECRET_LEN];
	getrandom::fill(&mut secret).expect("OS RNG unavailable");
	secret
}

/// RFC 4648 base32 without padding, the form apps take for a typed-in secret.
pub fn base32_encode(bytes: &[u8]) -> String {
	let mut out = String::with_capacity((bytes.len() * 8).div_ceil(5));
	let mut buffer = 0u32;
	let mut bits = 0u32;
	for &b in bytes {
		buffer = (buffer << 8) | u32::from(b);
		bits += 8;
		while bits >= 5 {
			bits -= 5;
			out.push(char::from(BASE32[((buffer >> bits) & 31) as usize]));
		}
	}
	if bits > 0 {
		out.push(char::from(BASE32[((buffer << (5 - bits)) & 31) as usize]));
	}
	out
}

/// The inverse of [`base32_encode`]; `None` for anything that is not base32.
pub fn base32_decode(text: &str) -> Option<Vec<u8>> {
	let mut out = Vec::with_capacity(text.len() * 5 / 8);
	let mut buffer = 0u32;
	let mut bits = 0u32;
	for c in text.bytes().filter(|c| *c != b'=') {
		let value = BASE32.iter().position(|b| *b == c.to_ascii_uppercase())?;
		buffer = (buffer << 5) | u32::try_from(value).ok()?;
		bits += 5;
		if bits >= 8 {
			bits -= 8;
			out.push(u8::try_from((buffer >> bits) & 0xff).ok()?);
		}
	}
	Some(out)
}

/// The code for time step `step` (RFC 4226 §5.3, dynamic truncation), with `digits` digits.
pub fn code_at(secret: &[u8], step: u64, digits: u32) -> u32 {
	let mut mac = Hmac::<Sha1>::new_from_slice(secret).expect("HMAC takes any key length");
	mac.update(&step.to_be_bytes());
	let hash = mac.finalize().into_bytes();
	let offset = usize::from(hash[hash.len() - 1] & 0x0f);
	let truncated = u32::from_be_bytes([
		hash[offset] & 0x7f,
		hash[offset + 1],
		hash[offset + 2],
		hash[offset + 3],
	]);
	truncated % 10u32.pow(digits)
}

/// The time step of a unix time.
pub const fn step_of(unix_secs: u64) -> u64 {
	unix_secs / STEP_SECS
}

/// Checks a typed code against `secret` at `unix_secs`, allowing [`SKEW_STEPS`] of drift.
/// Returns the step it matched, which the caller stores: a step at or before `last_step`
/// was already used, so replaying the same code is refused.
pub fn verify(secret: &[u8], unix_secs: u64, code: &str, last_step: u64) -> Option<u64> {
	let typed: String = code.chars().filter(|c| !c.is_whitespace()).collect();
	if typed.len() != DIGITS as usize || !typed.bytes().all(|b| b.is_ascii_digit()) {
		return None;
	}
	let typed: u32 = typed.parse().ok()?;
	let now = step_of(unix_secs);
	// Every candidate is compared, whichever matches first: the work does not depend on
	// which step (or whether any) was right.
	let mut hit = None;
	for step in now.saturating_sub(SKEW_STEPS)..=now + SKEW_STEPS {
		if code_at(secret, step, DIGITS) == typed
			&& step > last_step
			&& hit.is_none_or(|h| step > h)
		{
			hit = Some(step);
		}
	}
	hit
}

/// Percent-encodes everything but unreserved characters.
fn encode(text: &str) -> String {
	use std::fmt::Write as _;
	let mut out = String::with_capacity(text.len());
	for b in text.bytes() {
		if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
			out.push(char::from(b));
		} else {
			let _ = write!(out, "%{b:02X}");
		}
	}
	out
}

/// The `otpauth://` address an authenticator app reads from a QR code.
/// Format: <https://github.com/google/google-authenticator/wiki/Key-Uri-Format>.
pub fn otpauth_uri(issuer: &str, account: &str, secret_b32: &str) -> String {
	format!(
		"otpauth://totp/{}:{}?secret={secret_b32}&issuer={}&algorithm=SHA1&digits={DIGITS}&period={STEP_SECS}",
		encode(issuer),
		encode(account),
		encode(issuer),
	)
}

/// A QR code as the geometry of its dark squares, for the page to draw as an SVG `<path>`.
/// Drawing it from numbers, not from a rendered SVG string, keeps the page free of
/// pre-escaped markup.
pub struct Qr {
	/// Modules on a side, quiet zone included.
	pub size: usize,
	/// Path data: one unit square (`M x y h1 v1 h-1z`) per dark module.
	pub path: String,
}

/// Width of the empty border, in modules (the QR standard asks for four).
const QUIET: usize = 4;

/// `data` as a QR code.
pub fn qr(data: &str) -> Option<Qr> {
	use std::fmt::Write as _;
	let code = qrcode::QrCode::new(data.as_bytes()).ok()?;
	let width = code.width();
	let mut path = String::new();
	for (i, color) in code.to_colors().into_iter().enumerate() {
		if color == qrcode::Color::Dark {
			let _ = write!(path, "M{} {}h1v1h-1z", i % width + QUIET, i / width + QUIET);
		}
	}
	Some(Qr {
		size: width + 2 * QUIET,
		path,
	})
}

#[cfg(test)]
mod tests {
	use super::*;

	/// RFC 6238 appendix B, SHA-1: the secret is the ASCII string "12345678901234567890" and
	/// the codes are 8 digits.
	const RFC_SECRET: &[u8] = b"12345678901234567890";

	#[test]
	fn two_factor_totp_matches_the_rfc_6238_vectors() {
		for (time, code) in [
			(59u64, 94_287_082u32),
			(1_111_111_109, 7_081_804),
			(1_111_111_111, 14_050_471),
			(1_234_567_890, 89_005_924),
			(2_000_000_000, 69_279_037),
			(20_000_000_000, 65_353_130),
		] {
			assert_eq!(code_at(RFC_SECRET, step_of(time), 8), code, "t={time}");
		}
	}

	#[test]
	fn two_factor_totp_accepts_a_little_clock_skew_and_no_more() {
		let now = 1_700_000_000u64;
		let at = |t: u64| format!("{:06}", code_at(RFC_SECRET, step_of(t), DIGITS));
		assert!(verify(RFC_SECRET, now, &at(now), 0).is_some());
		// One step early and one late are fine ...
		assert!(verify(RFC_SECRET, now, &at(now - STEP_SECS), 0).is_some());
		assert!(verify(RFC_SECRET, now, &at(now + STEP_SECS), 0).is_some());
		// ... two are not.
		assert!(verify(RFC_SECRET, now, &at(now - 2 * STEP_SECS), 0).is_none());
		assert!(verify(RFC_SECRET, now, &at(now + 2 * STEP_SECS), 0).is_none());
		// Spaces from a grouped display are ignored; junk is not a code.
		let code = at(now);
		let spaced = format!("{} {}", &code[..3], &code[3..]);
		assert!(verify(RFC_SECRET, now, &spaced, 0).is_some());
		assert!(verify(RFC_SECRET, now, "12345", 0).is_none());
		assert!(verify(RFC_SECRET, now, "abcdef", 0).is_none());
	}

	#[test]
	fn two_factor_totp_refuses_a_code_that_was_already_used() {
		let now = 1_700_000_000u64;
		let code = format!("{:06}", code_at(RFC_SECRET, step_of(now), DIGITS));
		let step = verify(RFC_SECRET, now, &code, 0).expect("fresh code");
		assert!(verify(RFC_SECRET, now, &code, step).is_none());
		// An older step cannot be replayed once a newer one was used.
		let old = format!("{:06}", code_at(RFC_SECRET, step - 1, DIGITS));
		assert!(verify(RFC_SECRET, now, &old, step).is_none());
	}

	#[test]
	fn two_factor_base32_round_trips_and_matches_the_rfc_4648_vectors() {
		assert_eq!(base32_encode(b"foobar"), "MZXW6YTBOI");
		assert_eq!(base32_encode(b"fooba"), "MZXW6YTB");
		assert_eq!(base32_decode("MZXW6YTBOI").unwrap(), b"foobar");
		assert_eq!(base32_decode("mzxw6ytboi===").unwrap(), b"foobar");
		assert!(base32_decode("not base32!").is_none());
		let secret = generate_secret();
		assert_eq!(base32_decode(&base32_encode(&secret)).unwrap(), secret);
	}

	#[test]
	fn two_factor_otpauth_uri_escapes_the_account_and_the_qr_is_geometry_only() {
		let uri = otpauth_uri("podkit", "a b+c@example.com", "JBSWY3DPEHPK3PXP");
		assert_eq!(
			uri,
			"otpauth://totp/podkit:a%20b%2Bc%40example.com?secret=JBSWY3DPEHPK3PXP&issuer=podkit&algorithm=SHA1&digits=6&period=30"
		);
		let code = qr(&uri).expect("a QR code");
		assert!(code.size > 8 && code.path.starts_with('M'));
		assert!(!code.path.contains("JBSWY3DPEHPK3PXP"));
	}
}
