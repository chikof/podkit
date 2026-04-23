/*
SPDX-License-Identifier: Unlicense

Made by unitsuki / detski.

This file is dedicated to the public domain, to the fullest extent permitted by law.

Anyone may use, copy, modify, distribute, and sell this file, for any purpose,
with or without attribution.

This file is provided "as is", without warranty of any kind, express or implied,
including but not limited to merchantability, fitness for a particular purpose,
and noninfringement.

In no event shall the author be liable for any claim, damages, or other
liability arising from or related to this file or its use.
*/

//! - Local-part: `atext` characters (RFC 5321 §4.1.2) plus `.` with rules.
//!   Allowed: `a-z A-Z 0-9 ! # $ % & ' * + - / = ? ^ _ ` { | } ~`
//!   Dots: allowed but not at start, end, or consecutively.
//!   Max 64 characters.
//! - Domain: standard hostname (RFC 1123).
//!   Labels: alphanumeric + hyphens, no leading/trailing hyphen, max 63 chars each.
//!   TLD: alphabetic only, minimum 2 characters.
//!   Minimum 2 labels (e.g. `example.com`). Max 255 characters total.
//! - Total length ≤ 254 characters (RFC 5321 §4.5.3).
//! - ASCII only — use punycode for international domain names.
//!
//! ## Rejects (intentionally)
//!
//! - Quoted strings: `"foo bar"@example.com`
//! - IP address literals: `user@[192.168.1.1]`
//! - Comments, folding whitespace, obsolete RFC 822/2822 syntax
//! - Non-ASCII / EAI (SMTPUTF8) addresses (may add better support in the future)
//! - `localhost` and single-label domains

// RFC 5321 §4.5.3
const MAX_EMAIL_LEN: usize = 254;
const MAX_LOCAL_LEN: usize = 64;
const MAX_DOMAIN_LEN: usize = 255;
const MAX_LABEL_LEN: usize = 63;
const MIN_TLD_LEN: usize = 2;

// lookup tables so we don't have a giant match chain for every byte
// (also slightly faster, not that it matters much here...)

/// Valid `atext` characters (RFC 5321 §4.1.2), dot excluded.
/// Index with `byte as usize`; only bytes 0–127 are valid indices.
/// Non-ASCII bytes (≥ 128) must be rejected before indexing.
const ATEXT_MAP: [bool; 128] = {
	let mut t = [false; 128];
	let mut b = b'a';
	while b <= b'z' {
		t[b as usize] = true;
		b += 1;
	}
	b = b'A';
	while b <= b'Z' {
		t[b as usize] = true;
		b += 1;
	}
	b = b'0';
	while b <= b'9' {
		t[b as usize] = true;
		b += 1;
	}
	t[b'!' as usize] = true;
	t[b'#' as usize] = true;
	t[b'$' as usize] = true;
	t[b'%' as usize] = true;
	t[b'&' as usize] = true;
	t[b'\'' as usize] = true;
	t[b'*' as usize] = true;
	t[b'+' as usize] = true;
	t[b'-' as usize] = true;
	t[b'/' as usize] = true;
	t[b'=' as usize] = true;
	t[b'?' as usize] = true;
	t[b'^' as usize] = true;
	t[b'_' as usize] = true;
	t[b'`' as usize] = true;
	t[b'{' as usize] = true;
	t[b'|' as usize] = true;
	t[b'}' as usize] = true;
	t[b'~' as usize] = true;
	t
};

/// Valid non-TLD domain-label characters: `a-z A-Z 0-9 -` (RFC 1123).
const LABEL_MAP: [bool; 128] = {
	let mut t = [false; 128];
	let mut b = b'a';
	while b <= b'z' {
		t[b as usize] = true;
		b += 1;
	}
	b = b'A';
	while b <= b'Z' {
		t[b as usize] = true;
		b += 1;
	}
	b = b'0';
	while b <= b'9' {
		t[b as usize] = true;
		b += 1;
	}
	t[b'-' as usize] = true;
	t
};

/// Every possible rejection reason, in order of where it is checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EmailError {
	// top-level
	Empty,
	TooLong,
	MissingAt,
	MultipleAt,
	// local-part
	LocalPartEmpty,
	LocalPartTooLong,
	LocalPartLeadingDot,
	LocalPartTrailingDot,
	LocalPartConsecutiveDots,
	LocalPartInvalidChar(char),
	// domain
	DomainEmpty,
	DomainTooLong,
	DomainIpLiteralForbidden,
	DomainTrailingDot,
	/// Covers a leading dot and consecutive dots (both produce an empty label).
	DomainLabelEmpty,
	DomainLabelTooLong,
	DomainLabelLeadingHyphen,
	DomainLabelTrailingHyphen,
	DomainLabelInvalidChar(char),
	DomainMissingTld,
	DomainTldTooShort,
	/// TLD must contain only ASCII letters (a-z / A-Z).
	DomainTldNotAlpha,
	// encoding
	/// Found a non-ASCII character; encode international domains as punycode.
	NonAscii(char),
}

impl std::fmt::Display for EmailError {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		match self {
			Self::Empty => write!(f, "email is empty"),
			Self::TooLong => write!(f, "email exceeds {MAX_EMAIL_LEN} characters"),
			Self::MissingAt => write!(f, "missing '@'"),
			Self::MultipleAt => write!(f, "multiple '@' characters"),
			Self::LocalPartEmpty => write!(f, "local part (before '@') is empty"),
			Self::LocalPartTooLong => write!(f, "local part exceeds {MAX_LOCAL_LEN} characters"),
			Self::LocalPartLeadingDot => write!(f, "local part starts with '.'"),
			Self::LocalPartTrailingDot => write!(f, "local part ends with '.'"),
			Self::LocalPartConsecutiveDots => write!(f, "local part contains consecutive dots"),
			Self::LocalPartInvalidChar(c) => write!(f, "invalid character in local part: {c:?}"),
			Self::DomainEmpty => write!(f, "domain (after '@') is empty"),
			Self::DomainTooLong => write!(f, "domain exceeds {MAX_DOMAIN_LEN} characters"),
			Self::DomainIpLiteralForbidden => write!(f, "IP address literals are not accepted"),
			Self::DomainTrailingDot => write!(f, "domain ends with '.'"),
			Self::DomainLabelEmpty => write!(
				f,
				"domain contains an empty label (leading or consecutive dots)"
			),
			Self::DomainLabelTooLong => {
				write!(f, "domain label exceeds {MAX_LABEL_LEN} characters")
			}
			Self::DomainLabelLeadingHyphen => write!(f, "domain label starts with '-'"),
			Self::DomainLabelTrailingHyphen => write!(f, "domain label ends with '-'"),
			Self::DomainLabelInvalidChar(c) => {
				write!(f, "invalid character in domain label: {c:?}")
			}
			Self::DomainMissingTld => {
				write!(f, "domain has no TLD (must contain at least one dot)")
			}
			Self::DomainTldTooShort => write!(f, "TLD must be at least {MIN_TLD_LEN} characters"),
			Self::DomainTldNotAlpha => write!(f, "TLD must contain only letters (a-z)"),
			Self::NonAscii(c) => write!(
				f,
				"non-ASCII character {c:?}; encode international domains as punycode"
			),
		}
	}
}

impl std::error::Error for EmailError {}

/// Proof that the wrapped `&str` has passed [`validate_email`].
///
/// The idea is that you validate once and then pass this around instead of
/// a raw &str — the type carries the guarantee so you don't have to re-check.
// could add an into_string() or something later if needed
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ValidatedEmail<'a>(&'a str);

impl<'a> ValidatedEmail<'a> {
	#[inline]
	pub fn as_str(self) -> &'a str {
		self.0
	}

	#[inline]
	pub fn normalize(self) -> String {
		normalize_email(self.0)
	}
}

impl std::fmt::Display for ValidatedEmail<'_> {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.write_str(self.0)
	}
}

fn validate_local(local: &str) -> Result<(), EmailError> {
	if local.is_empty() {
		return Err(EmailError::LocalPartEmpty);
	}
	if local.len() > MAX_LOCAL_LEN {
		return Err(EmailError::LocalPartTooLong);
	}

	let bytes = local.as_bytes();
	if bytes[0] == b'.' {
		return Err(EmailError::LocalPartLeadingDot);
	}
	if *bytes.last().unwrap() == b'.' {
		return Err(EmailError::LocalPartTrailingDot);
	}

	let mut prev_dot = false;
	for (i, &b) in bytes.iter().enumerate() {
		if b >= 128 {
			// all bytes before i were ASCII so this has to be a lead byte, i is a valid boundary
			let c = local[i..].chars().next().unwrap();
			return Err(EmailError::NonAscii(c));
		}
		if b == b'.' {
			if prev_dot {
				return Err(EmailError::LocalPartConsecutiveDots);
			}
			prev_dot = true;
		} else if ATEXT_MAP[b as usize] {
			prev_dot = false;
		} else {
			return Err(EmailError::LocalPartInvalidChar(b as char));
		}
	}
	Ok(())
}

#[inline]
fn validate_label(label: &str) -> Result<(), EmailError> {
	if label.is_empty() {
		return Err(EmailError::DomainLabelEmpty);
	}
	if label.len() > MAX_LABEL_LEN {
		return Err(EmailError::DomainLabelTooLong);
	}

	let bytes = label.as_bytes();
	if bytes[0] == b'-' {
		return Err(EmailError::DomainLabelLeadingHyphen);
	}
	if *bytes.last().unwrap() == b'-' {
		return Err(EmailError::DomainLabelTrailingHyphen);
	}

	for (i, &b) in bytes.iter().enumerate() {
		if b >= 128 {
			let c = label[i..].chars().next().unwrap();
			return Err(EmailError::NonAscii(c));
		}
		if !LABEL_MAP[b as usize] {
			return Err(EmailError::DomainLabelInvalidChar(b as char));
		}
	}
	Ok(())
}

fn validate_domain(domain: &str) -> Result<(), EmailError> {
	if domain.is_empty() {
		return Err(EmailError::DomainEmpty);
	}
	if domain.as_bytes()[0] == b'[' {
		return Err(EmailError::DomainIpLiteralForbidden);
	}
	if domain.len() > MAX_DOMAIN_LEN {
		return Err(EmailError::DomainTooLong);
	}
	if domain.ends_with('.') {
		return Err(EmailError::DomainTrailingDot);
	}

	// rfind to split off the TLD — avoids collecting into a vec just to grab the last element
	let last_dot = domain.rfind('.').ok_or(EmailError::DomainMissingTld)?;
	let body = &domain[..last_dot];
	let tld = &domain[last_dot + 1..];

	if body.is_empty() {
		return Err(EmailError::DomainLabelEmpty);
	}

	for label in body.split('.') {
		validate_label(label)?;
	}

	// TLD: letters only, at least 2 chars
	if tld.len() < MIN_TLD_LEN {
		return Err(EmailError::DomainTldTooShort);
	}
	for (i, &b) in tld.as_bytes().iter().enumerate() {
		if b >= 128 {
			let c = tld[i..].chars().next().unwrap();
			return Err(EmailError::NonAscii(c));
		}
		if !b.is_ascii_alphabetic() {
			return Err(EmailError::DomainTldNotAlpha);
		}
	}

	Ok(())
}

/// Validates `email` against a strict subset of RFC 5321/5322.
///
/// On success returns a [`ValidatedEmail`] wrapper. Pass that to downstream
/// functions that need a valid address.
///
/// # Example
/// ```
/// let ve = validate_email("user+tag@mail.example.com").unwrap();
/// assert_eq!(ve.normalize(), "user+tag@mail.example.com");
/// assert!(validate_email("bad..dots@example.com").is_err());
/// ```
pub fn validate_email(email: &str) -> Result<ValidatedEmail<'_>, EmailError> {
	if email.is_empty() {
		return Err(EmailError::Empty);
	}
	if email.len() > MAX_EMAIL_LEN {
		return Err(EmailError::TooLong);
	}

	// single pass: find '@' and bail immediately if there's a second one
	let mut at_pos: Option<usize> = None;
	for (i, &b) in email.as_bytes().iter().enumerate() {
		if b == b'@' {
			if at_pos.is_some() {
				return Err(EmailError::MultipleAt);
			}
			at_pos = Some(i);
		}
	}
	let at = at_pos.ok_or(EmailError::MissingAt)?;

	validate_local(&email[..at])?;
	validate_domain(&email[at + 1..])?;

	Ok(ValidatedEmail(email))
}

/// Returns the normalised form: domain lowercased, local-part preserved.
///
/// DNS is case-insensitive; the local-part is case-sensitive per RFC 5321. (tbh I would make everything lowercase)
pub fn normalize_email(email: &str) -> String {
	let at = email.as_bytes().iter().position(|&b| b == b'@').unwrap();
	let mut out = String::with_capacity(email.len());
	out.push_str(&email[..=at]);
	out.push_str(&email[at + 1..].to_ascii_lowercase());
	out
}

// ⣿⣿⣿⣿⣿⣷⣿⣿⣿⡅⡹⢿⠆⠙⠋⠉⠻⠿⣿⣿⣿⣿⣿⣿⣮⠻⣦⡙⢷⡑⠘⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣷⣌⠡⠌⠂⣙⠻⣛⠻⠷⠐⠈⠛⢱⣮⣷⣽⣿
// ⣿⣿⣿⣿⡇⢿⢹⣿⣶⠐⠁⠀⣀⣠⣤⠄⠀⠀⠈⠙⠻⣿⣿⣿⣦⣵⣌⠻⣷⢝⠦⠚⢿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⢟⣻⣿⣊⡃⠀⣙⠿⣿⣿⣿⣎⢮⡀⢮⣽⣿⣿
// ⢿⣿⣿⣿⣧⡸⡎⡛⡩⠖⠀⣴⣿⣿⣿⠀⠀⠀⠀⠸⠇⠀⠙⢿⣿⣿⣿⣷⣌⢷⣑⢷⣄⠻⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⡿⣫⠶⠛⠉⠀⠁⠀⠈⠈⠀⠠⠜⠻⣿⣆⢿⣼⣿⣿⣿
// ⢐⣿⣿⣿⣿⣧⢧⣧⢻⣦⢀⣹⣿⣿⣿⣇⠀⠄⠀⠀⠀⡀⠀⠈⢻⣿⣿⣿⣿⣷⣝⢦⡹⠷⡙⢿⣿⣿⣿⣿⣿⣿⣿⣿⠈⠁⠀⠀⠀⠁⠀⠀⠀⠱⣶⣄⡀⠀⠈⠛⠜⣿⣿⣿⣿
// ⠀⠊⢫⣿⣏⣿⡌⣼⣄⢫⡌⣿⣿⣿⣿⣿⣦⡈⠲⣄⣤⣤⡡⢀⣠⣿⣿⣿⣿⣿⣿⣷⣼⣍⢬⣦⡙⣿⣿⣿⣿⣿⣯⢁⡄⠀⡀⡀⠀⠄⢈⣠⢪⠀⣿⣿⣿⣦⠀⢉⢂⠹⡿⣿⣿
// ⠀⠀⠄⢹⢃⢻⣟⠙⣿⣦⠱⢻⣿⣿⣿⣿⣿⣿⣷⣬⣍⣭⣥⣾⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣶⡙⢿⣼⡿⣿⣿⣿⣿⣿⣷⣄⠘⣱⢦⣤⡴⡿⢈⣼⣿⣿⣿⣇⣴⣶⣮⣅⢻⣿⡏
// ⠀⠀⠈⠹⣇⢡⢿⡆⠻⣿⣷⠀⢻⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣷⣍⡻⣿⣟⣻⣿⣿⣿⣿⣷⣦⣥⣬⣤⣴⣾⣿⣿⣿⣿⣷⣿⣿⣿⣿⣷⡜⠃
// ⠀⠀⠀⢀⣘⠈⢂⠃⣧⡹⣿⣷⡄⠙⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣮⣅⡙⢿⣟⠿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⠋⡕⠂
// ⠀⠀⠀⠀⠀⠀⠛⢷⣜⢷⡌⠻⣿⣿⣦⣝⣻⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣯⣹⣷⣦⣹⢿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⠿⠉⠃⠀
