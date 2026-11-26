//! The `${...}` expressions of a template: which helpers exist, what each
//! generates, and how a string with several of them is evaluated.
//!
//! The helper names and the meaning of their argument follow Dokploy's
//! `processors.ts` (MIT), so templates written for it behave the same:
//!
//! | Expression | Value |
//! |---|---|
//! | `${password}`, `${password:N}` | `N` characters from `a-z0-9`, 16 by default |
//! | `${base64}`, `${base64:N}` | `N` random bytes, standard base64 with padding, 32 by default |
//! | `${hash}`, `${hash:N}` | `N` lowercase hex characters, 8 by default |
//! | `${jwt}`, `${jwt:N}` | `N` random bytes as hex; without `N` an HS256 token |
//! | `${jwt:secret:payload}` | an HS256 token signed with variable `secret`, claims from variable `payload` |
//! | `${uuid}` | a version 4 UUID |
//! | `${domain}` | the default hostname of the service the variable routes to |
//! | `${email}`, `${username}` | a random address or user name |
//!
//! Any other `${name}` is a reference to a variable of the template; one that
//! names nothing is left as written (shell scripts in file mounts use
//! `${HOME}` and the like).

use std::collections::BTreeMap;

use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;

/// The longest `N` of a helper.
pub const MAX_LEN: usize = 1024;

const PASSWORD_CHARS: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";

/// A helper with its argument, parsed from the text between `${` and `}`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Helper {
	/// `password:N`
	Password(usize),
	/// `base64:N`, N bytes.
	Base64(usize),
	/// `hash:N`
	Hash(usize),
	/// `jwt:N`: N random bytes as hex.
	JwtHex(usize),
	/// `jwt`, or `jwt:secret:payload`: an HS256 token.
	Jwt {
		/// Variable holding the signing secret, or the secret itself.
		secret: Option<String>,
		/// Variable holding the JSON claims.
		payload: Option<String>,
	},
	/// `uuid`
	Uuid,
	/// `domain`
	Domain,
	/// `email`
	Email,
	/// `username`
	Username,
}

impl Helper {
	/// Whether the value is a credential: stored encrypted and never shown.
	#[must_use]
	pub fn is_secret(&self) -> bool {
		!matches!(self, Self::Domain | Self::Email | Self::Username)
	}
}

/// What `${...}` can turn out to be.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Expr {
	/// A generator.
	Helper(Helper),
	/// A reference to a variable, or text to leave alone.
	Name(String),
}

/// Helpers Dokploy has that podkit does not generate.
const UNSUPPORTED: &[&str] = &["timestamp", "timestamps", "timestampms", "randomPort"];

fn length(arg: Option<&str>, default: usize, name: &str) -> Result<usize, String> {
	let Some(arg) = arg else {
		return Ok(default);
	};
	match arg.parse::<usize>() {
		Ok(n) if (1..=MAX_LEN).contains(&n) => Ok(n),
		_ => Err(format!(
			"${{{name}:{arg}}}: the length must be a number from 1 to {MAX_LEN}"
		)),
	}
}

/// Parses the text between `${` and `}`.
///
/// # Errors
/// An unsupported helper, or a bad length.
pub fn parse(inner: &str) -> Result<Expr, String> {
	let (name, arg) = match inner.split_once(':') {
		Some((name, arg)) => (name, Some(arg)),
		None => (inner, None),
	};
	let helper = match name {
		"password" => Helper::Password(length(arg, 16, name)?),
		"base64" => Helper::Base64(length(arg, 32, name)?),
		"hash" => Helper::Hash(length(arg, 8, name)?),
		"jwt" => match arg {
			None => Helper::Jwt {
				secret: None,
				payload: None,
			},
			Some(a) if a.chars().all(|c| c.is_ascii_digit()) && !a.is_empty() => {
				Helper::JwtHex(length(Some(a), 32, name)?)
			}
			Some(a) => {
				let (secret, payload) = match a.split_once(':') {
					Some((s, p)) => (s, Some(p)),
					None => (a, None),
				};
				Helper::Jwt {
					secret: Some(secret.to_string()).filter(|s| !s.is_empty()),
					payload: payload.map(str::to_string).filter(|s| !s.is_empty()),
				}
			}
		},
		"uuid" if arg.is_none() => Helper::Uuid,
		"domain" if arg.is_none() => Helper::Domain,
		"email" if arg.is_none() => Helper::Email,
		"username" if arg.is_none() => Helper::Username,
		n if UNSUPPORTED.contains(&n) => {
			return Err(format!("${{{inner}}} is not supported"));
		}
		_ => return Ok(Expr::Name(inner.to_string())),
	};
	Ok(Expr::Helper(helper))
}

/// Calls `f` with the text of every `${...}` in `text`, in order.
fn each(text: &str, mut f: impl FnMut(&str)) {
	let mut rest = text;
	while let Some(start) = rest.find("${") {
		let after = &rest[start + 2..];
		let Some(end) = after.find('}') else {
			return;
		};
		f(&after[..end]);
		rest = &after[end + 1..];
	}
}

/// Every expression in `text`.
///
/// # Errors
/// The first unsupported helper.
pub fn scan(text: &str) -> Result<Vec<Expr>, String> {
	let mut out = Vec::new();
	let mut error = None;
	each(text, |inner| match parse(inner) {
		Ok(e) => out.push(e),
		Err(e) => {
			error.get_or_insert(e);
		}
	});
	error.map_or(Ok(out), Err)
}

/// The names `text` refers to, helpers excluded.
#[must_use]
pub fn references(text: &str) -> Vec<String> {
	let mut out = Vec::new();
	for e in scan(text).unwrap_or_default() {
		match e {
			Expr::Name(n) => out.push(n),
			// The arguments of a signed token are variables too.
			Expr::Helper(Helper::Jwt { secret, payload }) => {
				out.extend(secret.into_iter().chain(payload));
			}
			Expr::Helper(_) => {}
		}
	}
	out
}

/// Where randomness comes from; tests use a fixed one.
pub trait Random {
	/// Fills `buf` with random bytes.
	fn fill(&mut self, buf: &mut [u8]);
}

/// The operating system's generator.
pub struct Os;

impl Random for Os {
	fn fill(&mut self, buf: &mut [u8]) {
		getrandom::fill(buf).expect("OS RNG unavailable");
	}
}

/// A counter that is not random, for checking a template without making
/// secrets: the same template always renders the same.
pub struct Counter(pub u8);

impl Random for Counter {
	fn fill(&mut self, buf: &mut [u8]) {
		for b in buf {
			self.0 = self.0.wrapping_mul(31).wrapping_add(7);
			*b = self.0;
		}
	}
}

fn hex(bytes: &[u8]) -> String {
	use std::fmt::Write;
	bytes.iter().fold(String::new(), |mut s, b| {
		let _ = write!(s, "{b:02x}");
		s
	})
}

fn uniform(rng: &mut dyn Random, n: usize, alphabet: &[u8]) -> String {
	// Rejection sampling keeps every character equally likely.
	let limit = 256 - (256 % alphabet.len());
	let mut out = String::with_capacity(n);
	let mut buf = [0u8; 64];
	while out.len() < n {
		rng.fill(&mut buf);
		for &b in &buf {
			if usize::from(b) < limit && out.len() < n {
				out.push(char::from(alphabet[usize::from(b) % alphabet.len()]));
			}
		}
	}
	out
}

fn bytes(rng: &mut dyn Random, n: usize) -> Vec<u8> {
	let mut buf = vec![0u8; n];
	rng.fill(&mut buf);
	buf
}

fn uuid_v4(rng: &mut dyn Random) -> String {
	let mut b = [0u8; 16];
	rng.fill(&mut b);
	b[6] = (b[6] & 0x0f) | 0x40;
	b[8] = (b[8] & 0x3f) | 0x80;
	let h = hex(&b);
	format!(
		"{}-{}-{}-{}-{}",
		&h[0..8],
		&h[8..12],
		&h[12..16],
		&h[16..20],
		&h[20..32]
	)
}

/// An HS256 token like Dokploy's `generateJwt`: claims `iss`, `iat` and
/// `exp` (2030-01-01) unless the payload sets them.
fn jwt(secret: &str, payload: Option<&str>) -> String {
	const EXP: i64 = 1_893_456_000; // 2030-01-01T00:00:00Z
	let mut claims: serde_json::Map<String, serde_json::Value> = payload
		.filter(|p| p.trim_start().starts_with('{') && p.trim_end().ends_with('}'))
		.and_then(|p| serde_json::from_str(p).ok())
		.unwrap_or_default();
	let now = time::OffsetDateTime::now_utc().unix_timestamp();
	claims
		.entry("iss")
		.or_insert_with(|| serde_json::Value::from("podkit"));
	claims
		.entry("iat")
		.or_insert_with(|| serde_json::Value::from(now));
	claims
		.entry("exp")
		.or_insert_with(|| serde_json::Value::from(EXP));
	let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"HS256","typ":"JWT"}"#);
	let body = URL_SAFE_NO_PAD.encode(serde_json::Value::Object(claims).to_string());
	let signing = format!("{header}.{body}");
	let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("any key length");
	mac.update(signing.as_bytes());
	let signature = URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes());
	format!("{signing}.{signature}")
}

/// Everything a helper may need besides randomness.
pub struct Context<'a> {
	/// Source of random bytes.
	pub rng: &'a mut dyn Random,
	/// Values of the variables already known.
	pub variables: &'a BTreeMap<String, String>,
	/// Hostname `${domain}` stands for here.
	pub domain: &'a str,
}

/// The value of `helper`.
pub fn generate(helper: &Helper, ctx: &mut Context<'_>) -> String {
	match helper {
		Helper::Password(n) => uniform(ctx.rng, *n, PASSWORD_CHARS),
		Helper::Base64(n) => STANDARD.encode(bytes(ctx.rng, *n)),
		Helper::Hash(n) => {
			let mut s = hex(&bytes(ctx.rng, n.div_ceil(2)));
			s.truncate(*n);
			s
		}
		Helper::JwtHex(n) => hex(&bytes(ctx.rng, *n)),
		Helper::Jwt { secret, payload } => {
			let lookup = |name: &String| ctx.variables.get(name).cloned();
			let secret = match secret {
				Some(name) => lookup(name).unwrap_or_else(|| name.clone()),
				None => hex(&bytes(ctx.rng, 32)),
			};
			let payload = payload
				.as_ref()
				.map(|name| lookup(name).unwrap_or_default());
			jwt(&secret, payload.as_deref())
		}
		Helper::Uuid => uuid_v4(ctx.rng),
		Helper::Domain => ctx.domain.to_string(),
		Helper::Email => format!("{}@example.com", uniform(ctx.rng, 10, PASSWORD_CHARS)),
		Helper::Username => format!("user-{}", uniform(ctx.rng, 6, PASSWORD_CHARS)),
	}
}

/// The longest a value may get while its `${...}` are replaced.
const MAX_EXPANDED: usize = 64 * 1024;

/// `text` with every `${...}` replaced: helpers by a fresh value, names by
/// their variable's value, anything else as written.
///
/// # Errors
/// An unsupported helper.
pub fn expand(text: &str, ctx: &mut Context<'_>) -> Result<String, String> {
	scan(text)?;
	let mut out = String::with_capacity(text.len());
	let mut rest = text;
	while let Some(start) = rest.find("${") {
		let after = &rest[start + 2..];
		let Some(end) = after.find('}') else {
			break;
		};
		out.push_str(&rest[..start]);
		let inner = &after[..end];
		match parse(inner)? {
			Expr::Helper(h) => out.push_str(&generate(&h, ctx)),
			Expr::Name(name) => {
				if let Some(value) = ctx.variables.get(&name) {
					out.push_str(value);
				} else {
					out.push_str("${");
					out.push_str(inner);
					out.push('}');
				}
			}
		}
		rest = &after[end + 1..];
		// A variable made of copies of another grows a thousandfold a level.
		if out.len() > MAX_EXPANDED {
			return Err(format!("a value grows past {MAX_EXPANDED} bytes"));
		}
	}
	out.push_str(rest);
	Ok(out)
}
