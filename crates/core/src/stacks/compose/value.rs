//! Scalar helpers of the compose parser: `${VAR}` interpolation, durations,
//! byte sizes and port mappings. Pure functions, no I/O.

use std::collections::{BTreeMap, BTreeSet};

use proto::Protocol;
use serde_json::Value;

use super::PortMapping;

/// Expands `$VAR`, `${VAR}`, `${VAR:-d}`, `${VAR-d}`, `${VAR:?e}`, `${VAR?e}`,
/// `${VAR:+alt}`, `${VAR+alt}` and `$$` against `env`. A variable that is not
/// set expands to nothing and lands in `missing`, as in `docker compose`.
///
/// `:-`, `:?` and `:+` treat an empty value like an unset one; `-`, `?` and
/// `+` only an unset one. Defaults may contain further `${...}`.
///
/// # Errors
/// The message of a `${VAR:?message}` whose variable is unset, or a malformed
/// expression.
pub fn interpolate(
	input: &str,
	env: &BTreeMap<String, String>,
	missing: &mut BTreeSet<String>,
) -> Result<String, String> {
	if !input.contains('$') {
		return Ok(input.to_string());
	}
	// A default inside a default is a call inside a call: a thousand of them run the
	// thread out of stack, which ends the process and not the request.
	let (mut depth, mut deepest) = (0usize, 0usize);
	for pair in input.as_bytes().windows(2) {
		if pair == b"${" {
			depth += 1;
			deepest = deepest.max(depth);
		} else if pair[0] == b'}' {
			depth = depth.saturating_sub(1);
		}
	}
	if deepest > MAX_NESTING {
		return Err(format!("\"${{\" is nested more than {MAX_NESTING} deep"));
	}
	expand_all(input, env, missing)
}

/// How deep `${A:-${B:-...}}` may go.
const MAX_NESTING: usize = 16;

fn expand_all(
	input: &str,
	env: &BTreeMap<String, String>,
	missing: &mut BTreeSet<String>,
) -> Result<String, String> {
	let bytes = input.as_bytes();
	let mut out = String::with_capacity(input.len());
	let mut i = 0;
	while i < bytes.len() {
		if bytes[i] != b'$' {
			let ch = input[i..].chars().next().unwrap_or('\u{fffd}');
			out.push(ch);
			i += ch.len_utf8();
			continue;
		}
		match bytes.get(i + 1) {
			Some(b'$') => {
				out.push('$');
				i += 2;
			}
			Some(b'{') => {
				let close = matching_brace(input, i + 2)
					.ok_or_else(|| format!("unterminated \"${{\" in {input:?}"))?;
				out.push_str(&expand(&input[i + 2..close], env, missing)?);
				i = close + 1;
			}
			Some(&c) if is_name_start(c) => {
				let end = name_end(bytes, i + 1);
				let name = &input[i + 1..end];
				match env.get(name) {
					Some(value) => out.push_str(value),
					None => {
						missing.insert(name.to_string());
					}
				}
				i = end;
			}
			// A lone `$` is literal.
			_ => {
				out.push('$');
				i += 1;
			}
		}
	}
	Ok(out)
}

fn is_name_start(b: u8) -> bool {
	b.is_ascii_alphabetic() || b == b'_'
}

fn name_end(bytes: &[u8], start: usize) -> usize {
	let mut end = start;
	while end < bytes.len() && (bytes[end].is_ascii_alphanumeric() || bytes[end] == b'_') {
		end += 1;
	}
	end
}

/// Index of the `}` closing a `${` whose body starts at `start`.
fn matching_brace(input: &str, start: usize) -> Option<usize> {
	let bytes = input.as_bytes();
	let mut depth = 1_u32;
	let mut i = start;
	while i < bytes.len() {
		match bytes[i] {
			b'$' if bytes.get(i + 1) == Some(&b'{') => {
				depth += 1;
				i += 2;
				continue;
			}
			b'}' => {
				depth -= 1;
				if depth == 0 {
					return Some(i);
				}
			}
			_ => {}
		}
		i += 1;
	}
	None
}

fn expand(
	inner: &str,
	env: &BTreeMap<String, String>,
	missing: &mut BTreeSet<String>,
) -> Result<String, String> {
	let bytes = inner.as_bytes();
	if bytes.is_empty() || !is_name_start(bytes[0]) {
		return Err(format!("invalid interpolation \"${{{inner}}}\""));
	}
	let end = name_end(bytes, 1);
	let (name, op) = inner.split_at(end);
	let value = env.get(name);
	let set = value.is_some();
	let non_empty = value.is_some_and(|v| !v.is_empty());
	let rest = |skip: usize| &op[skip..];
	match op {
		"" => {
			if let Some(v) = value {
				return Ok(v.clone());
			}
			missing.insert(name.to_string());
			Ok(String::new())
		}
		_ if op.starts_with(":-") => {
			if non_empty {
				Ok(value.cloned().unwrap_or_default())
			} else {
				expand_all(rest(2), env, missing)
			}
		}
		_ if op.starts_with('-') => {
			if set {
				Ok(value.cloned().unwrap_or_default())
			} else {
				expand_all(rest(1), env, missing)
			}
		}
		_ if op.starts_with(":?") || op.starts_with('?') => {
			let colon = op.starts_with(':');
			let ok = if colon { non_empty } else { set };
			if ok {
				return Ok(value.cloned().unwrap_or_default());
			}
			let message = expand_all(rest(if colon { 2 } else { 1 }), env, missing)?;
			Err(if message.is_empty() {
				format!("required variable {name} is missing a value")
			} else {
				format!("required variable {name} is missing a value: {message}")
			})
		}
		_ if op.starts_with(":+") => {
			if non_empty {
				expand_all(rest(2), env, missing)
			} else {
				Ok(String::new())
			}
		}
		_ if op.starts_with('+') => {
			if set {
				expand_all(rest(1), env, missing)
			} else {
				Ok(String::new())
			}
		}
		_ => Err(format!("invalid interpolation \"${{{inner}}}\"")),
	}
}

/// A scalar as a string: strings as they are, numbers and booleans spelled
/// the way YAML writes them. `None` for null, sequences and mappings.
pub fn scalar(value: &Value) -> Option<String> {
	match value {
		Value::String(s) => Some(s.clone()),
		Value::Number(n) => Some(n.to_string()),
		Value::Bool(b) => Some(b.to_string()),
		_ => None,
	}
}

/// A boolean, written as one or as `true`/`false`/`yes`/`no` text.
pub fn boolean(value: &Value) -> Option<bool> {
	match value {
		Value::Bool(b) => Some(*b),
		Value::String(s) => match s.to_ascii_lowercase().as_str() {
			"true" | "yes" | "on" | "1" => Some(true),
			"false" | "no" | "off" | "0" | "" => Some(false),
			_ => None,
		},
		_ => None,
	}
}

/// An unsigned integer, written as a number or as numeric text.
pub fn unsigned(value: &Value) -> Option<u64> {
	match value {
		Value::Number(n) => n.as_u64(),
		Value::String(s) => s.trim().parse().ok(),
		_ => None,
	}
}

/// A positive float, written as a number or as numeric text.
pub fn float(value: &Value) -> Option<f64> {
	match value {
		Value::Number(n) => n.as_f64(),
		Value::String(s) => s.trim().parse().ok(),
		_ => None,
	}
}

/// `1m30s`, `500ms`, `2h`, `1.5s` in milliseconds. A bare number is seconds.
pub fn duration_ms(value: &Value) -> Result<u64, String> {
	let text = match value {
		Value::Number(n) => format!("{n}s"),
		Value::String(s) => s.trim().to_string(),
		_ => return Err("expected a duration like 30s".to_string()),
	};
	if text.is_empty() {
		return Err("expected a duration like 30s".to_string());
	}
	let bad = || format!("invalid duration {text:?}");
	let mut rest = text.as_str();
	let mut total = 0.0_f64;
	while !rest.is_empty() {
		let digits = rest
			.find(|c: char| !(c.is_ascii_digit() || c == '.'))
			.unwrap_or(rest.len());
		if digits == 0 {
			return Err(bad());
		}
		let number: f64 = rest[..digits].parse().map_err(|_| bad())?;
		rest = &rest[digits..];
		let unit_len = rest
			.find(|c: char| c.is_ascii_digit() || c == '.')
			.unwrap_or(rest.len());
		let factor = match &rest[..unit_len] {
			"ns" => 1e-6,
			"us" | "µs" => 1e-3,
			"ms" => 1.0,
			"s" | "" => 1e3,
			"m" => 60e3,
			"h" => 3600e3,
			_ => return Err(bad()),
		};
		total += number * factor;
		rest = &rest[unit_len..];
	}
	#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
	Ok(total.round() as u64)
}

/// `512m`, `1.5g`, `64mb`, `1024` in bytes. A number is bytes.
pub fn bytes(value: &Value) -> Result<i64, String> {
	let bad = |text: &str| format!("invalid size {text:?}, use a number with b, k, m or g");
	let text = match value {
		Value::Number(n) => return n.as_i64().ok_or_else(|| bad(&n.to_string())),
		Value::String(s) => s.trim().to_ascii_lowercase(),
		_ => return Err("expected a size like 512m".to_string()),
	};
	let split = text
		.find(|c: char| !(c.is_ascii_digit() || c == '.'))
		.unwrap_or(text.len());
	let number: f64 = text[..split].parse().map_err(|_| bad(&text))?;
	let factor: f64 = match &text[split..] {
		"" | "b" => 1.0,
		"k" | "kb" | "kib" => 1024.0,
		"m" | "mb" | "mib" => 1024.0 * 1024.0,
		"g" | "gb" | "gib" => 1024.0 * 1024.0 * 1024.0,
		"t" | "tb" | "tib" => 1024.0 * 1024.0 * 1024.0 * 1024.0,
		_ => return Err(bad(&text)),
	};
	#[allow(clippy::cast_possible_truncation)]
	let result = (number * factor) as i64;
	if result < 0 {
		return Err(bad(&text));
	}
	Ok(result)
}

/// What a `ports` entry asks for.
pub struct ParsedPort {
	/// `None` for a container port without a host port.
	pub mapping: PortMapping,
	/// The host port was not given.
	pub unpublished: bool,
}

fn port_number(text: &str, what: &str) -> Result<u16, String> {
	if text.contains('-') {
		return Err(format!("port ranges are not supported ({what} {text:?})"));
	}
	text.trim()
		.parse::<u16>()
		.ok()
		.filter(|p| *p > 0)
		.ok_or_else(|| format!("invalid {what} {text:?}"))
}

/// Parses one `ports` entry: `80`, `8080:80`, `127.0.0.1:8080:80`, `8080:80/udp`
/// or the long form `{target, published, protocol, host_ip}`.
pub fn port(value: &Value) -> Result<ParsedPort, String> {
	match value {
		Value::Object(map) => {
			let target = map
				.get("target")
				.and_then(scalar)
				.ok_or_else(|| "a port needs a target".to_string())?;
			let container = port_number(&target, "target port")?;
			let host = map.get("published").and_then(scalar);
			let protocol = match map.get("protocol").and_then(scalar).as_deref() {
				None | Some("tcp") => Protocol::Tcp,
				Some("udp") => Protocol::Udp,
				Some(other) => return Err(format!("unsupported protocol {other:?}")),
			};
			let host_ip = map
				.get("host_ip")
				.and_then(scalar)
				.filter(|ip| !ip.is_empty());
			Ok(ParsedPort {
				unpublished: host.is_none(),
				mapping: PortMapping {
					host: match host {
						Some(h) => port_number(&h, "published port")?,
						None => container,
					},
					container,
					protocol,
					host_ip,
				},
			})
		}
		other => {
			let text = scalar(other).ok_or_else(|| "expected a port".to_string())?;
			let (spec, protocol) = match text.rsplit_once('/') {
				Some((spec, "tcp")) => (spec, Protocol::Tcp),
				Some((spec, "udp")) => (spec, Protocol::Udp),
				Some((_, other)) => return Err(format!("unsupported protocol {other:?}")),
				None => (text.as_str(), Protocol::Tcp),
			};
			// An IPv6 host address would contain colons; only IPv4 is supported.
			let parts: Vec<&str> = spec.split(':').collect();
			let (host_ip, host, container) = match parts.as_slice() {
				[container] => (None, None, *container),
				[host, container] => (None, Some(*host), *container),
				[ip, host, container] => (
					Some((*ip).to_string()),
					Some(*host).filter(|h| !h.is_empty()),
					*container,
				),
				_ => return Err(format!("invalid port {text:?}")),
			};
			let container = port_number(container, "container port")?;
			Ok(ParsedPort {
				unpublished: host.is_none(),
				mapping: PortMapping {
					host: match host {
						Some(h) => port_number(h, "host port")?,
						None => container,
					},
					container,
					protocol,
					host_ip,
				},
			})
		}
	}
}

/// `A=b` lines of an env file; comments and blank lines are skipped, a
/// leading `export ` and one level of matching quotes are removed.
pub fn dotenv(text: &str) -> Vec<(String, String)> {
	text.lines()
		.filter_map(|line| {
			let line = line.trim();
			if line.is_empty() || line.starts_with('#') {
				return None;
			}
			let line = line.strip_prefix("export ").unwrap_or(line);
			let (key, value) = line.split_once('=')?;
			let value = value.trim();
			let unquoted = ["\"", "'"]
				.iter()
				.find_map(|q| value.strip_prefix(q).and_then(|v| v.strip_suffix(q)))
				.unwrap_or(value);
			Some((key.trim().to_string(), unquoted.to_string()))
		})
		.collect()
}
