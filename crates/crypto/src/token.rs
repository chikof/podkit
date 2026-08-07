/// Generates a cryptographically random, hex-encoded token for bearer
/// credentials (e.g. webhook secrets), not identifiers. Snowflake ids
/// (`generate_id`) are sequential/predictable and unsuitable for this.
///
/// # Panics
/// Panics if the OS RNG is unavailable (`getrandom` failure). That's treated
/// as fatal, same as any other unrecoverable startup-class error.
#[must_use]
pub fn generate_token(bytes: usize) -> String {
	let mut buf = vec![0u8; bytes];
	getrandom::getrandom(&mut buf).expect("OS RNG unavailable");
	buf.iter()
		.fold(String::with_capacity(bytes * 2), |mut s, b| {
			use std::fmt::Write;
			let _ = write!(s, "{b:02x}");
			s
		})
}

/// Constant-time string equality. Use this for comparing bearer tokens or
/// secrets to avoid timing side channels. Length is not hidden, which is
/// fine here since our tokens are always fixed-length.
#[must_use]
pub fn constant_time_eq(a: &str, b: &str) -> bool {
	let (a, b) = (a.as_bytes(), b.as_bytes());
	if a.len() != b.len() {
		return false;
	}
	a.iter()
		.zip(b.iter())
		.fold(0u8, |acc, (x, y)| acc | (x ^ y))
		== 0
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn generates_requested_hex_length() {
		let token = generate_token(16);
		assert_eq!(token.len(), 32);
		assert!(token.chars().all(|c| c.is_ascii_hexdigit()));
	}

	#[test]
	fn tokens_are_not_repeated() {
		assert_ne!(generate_token(16), generate_token(16));
	}

	#[test]
	fn constant_time_eq_matches_normal_eq() {
		assert!(constant_time_eq("abc123", "abc123"));
		assert!(!constant_time_eq("abc123", "abc124"));
		assert!(!constant_time_eq("abc123", "abc12"));
	}
}
