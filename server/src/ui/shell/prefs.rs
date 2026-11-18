//! Per-request state the shell reads from cookies, without every handler passing it along:
//!
//! - `sidebar`: `collapsed` when the user folded the sidebar to icons. The server writes the
//!   state into the first byte of the page, so the layout never jumps on load.
//! - `flash`: a message left by an action for the page it redirects to ("Team renamed").
//!   It is shown once as a toast, then the cookie is dropped.
//!
//! The same task-local pattern as `ui::theme`.

use axum::extract::Request;
use axum::http::{HeaderMap, HeaderValue, header};
use axum::middleware::Next;
use axum::response::Response;
use tower_cookies::cookie::SameSite;
use tower_cookies::cookie::time::Duration;
use tower_cookies::{Cookie, Cookies};

use crate::ui::components::Tone;

const FLASH: &str = "flash";
const SIDEBAR: &str = "sidebar";
/// A message longer than this is cut; flash is for one short sentence.
const MAX_FLASH: usize = 200;

/// A message waiting to be shown once.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Flash {
	pub tone: Tone,
	pub message: String,
}

#[derive(Debug, Clone, Default)]
struct Prefs {
	collapsed: bool,
	flash: Option<Flash>,
}

tokio::task_local! {
	static CURRENT: Prefs;
}

/// Whether the sidebar is folded to icons.
pub fn sidebar_collapsed() -> bool {
	CURRENT.try_with(|p| p.collapsed).unwrap_or_default()
}

/// The message left for this page, if any.
pub fn pending_flash() -> Option<Flash> {
	CURRENT.try_with(|p| p.flash.clone()).ok().flatten()
}

const fn tone_name(tone: Tone) -> &'static str {
	match tone {
		Tone::Success => "success",
		Tone::Danger => "danger",
		Tone::Warning => "warning",
		Tone::Progress => "progress",
		Tone::Neutral => "neutral",
	}
}

fn tone_from(name: &str) -> Tone {
	match name {
		"success" => Tone::Success,
		"danger" => Tone::Danger,
		"warning" => Tone::Warning,
		"progress" => Tone::Progress,
		_ => Tone::Neutral,
	}
}

fn encode(flash: &Flash) -> String {
	url::form_urlencoded::Serializer::new(String::new())
		.append_pair("t", tone_name(flash.tone))
		.append_pair(
			"m",
			&flash.message.chars().take(MAX_FLASH).collect::<String>(),
		)
		.finish()
}

fn decode(value: &str) -> Option<Flash> {
	let mut tone = None;
	let mut message = None;
	for (k, v) in url::form_urlencoded::parse(value.as_bytes()) {
		match &*k {
			"t" => tone = Some(tone_from(&v)),
			"m" => message = Some(v.chars().take(MAX_FLASH).collect::<String>()),
			_ => {}
		}
	}
	let message = message.filter(|m| !m.trim().is_empty())?;
	Some(Flash {
		tone: tone.unwrap_or(Tone::Neutral),
		message,
	})
}

/// Leaves `message` for the next page this browser loads. Call it from an action just
/// before redirecting; it is shown once, as a toast.
pub fn flash(cookies: &Cookies, tone: Tone, message: &str) {
	let value = encode(&Flash {
		tone,
		message: message.to_owned(),
	});
	cookies.add(
		Cookie::build((FLASH, value))
			.path("/")
			.http_only(true)
			.same_site(SameSite::Lax)
			.max_age(Duration::seconds(60))
			.build(),
	);
}

fn cookie_value<'h>(headers: &'h HeaderMap, name: &str) -> Option<&'h str> {
	headers
		.get_all(header::COOKIE)
		.iter()
		.filter_map(|v| v.to_str().ok())
		.flat_map(|v| v.split(';'))
		.filter_map(|pair| pair.trim().strip_prefix(name)?.strip_prefix('='))
		.next()
}

fn prefs_from(headers: &HeaderMap) -> Prefs {
	Prefs {
		collapsed: cookie_value(headers, SIDEBAR) == Some("collapsed"),
		flash: cookie_value(headers, FLASH).and_then(decode),
	}
}

/// Makes the request's cookies available to `sidebar_collapsed()` and `pending_flash()`
/// while the handler runs, and drops the flash cookie once a page that shows it has been
/// sent. A redirect keeps it, so it survives to the page it was meant for.
pub async fn scope(req: Request, next: Next) -> Response {
	let prefs = prefs_from(req.headers());
	let had_flash = prefs.flash.is_some();
	let mut resp = CURRENT.scope(prefs, next.run(req)).await;
	let is_page = resp.status().is_success()
		&& resp
			.headers()
			.get(header::CONTENT_TYPE)
			.and_then(|v| v.to_str().ok())
			.is_some_and(|t| t.starts_with("text/html"));
	if had_flash && is_page {
		resp.headers_mut().append(
			header::SET_COOKIE,
			HeaderValue::from_static("flash=; Path=/; Max-Age=0; HttpOnly; SameSite=Lax"),
		);
	}
	resp
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn a_flash_survives_the_cookie() {
		let flash = Flash {
			tone: Tone::Success,
			message: "Team renamed to \"A & B; C\"".to_owned(),
		};
		let value = encode(&flash);
		assert!(
			!value.contains([' ', ';', '"', ',', '\\']),
			"not a legal cookie value: {value}"
		);
		assert_eq!(decode(&value), Some(flash));
	}

	#[test]
	fn a_bad_or_empty_flash_is_ignored_and_a_long_one_is_cut() {
		assert_eq!(decode("garbage"), None);
		assert_eq!(decode("t=success&m=+++"), None);
		let long = encode(&Flash {
			tone: Tone::Danger,
			message: "x".repeat(1000),
		});
		assert_eq!(decode(&long).unwrap().message.len(), MAX_FLASH);
		assert_eq!(decode("m=hi&t=nonsense").unwrap().tone, Tone::Neutral);
	}

	#[test]
	fn prefs_come_from_the_cookie_header() {
		let mut h = HeaderMap::new();
		h.insert(
			header::COOKIE,
			"theme=dark; sidebar=collapsed; flash=t%3Dsuccess%26m%3DDone"
				.parse()
				.unwrap(),
		);
		// `flash` is the encoded pair; the raw header carries it percent-encoded once.
		let p = prefs_from(&h);
		assert!(p.collapsed);
		assert_eq!(
			p.flash, None,
			"the cookie value is the serialized pair, not double-encoded"
		);

		let value = encode(&Flash {
			tone: Tone::Success,
			message: "Done".to_owned(),
		});
		h.insert(
			header::COOKIE,
			format!("sidebar=open; flash={value}").parse().unwrap(),
		);
		let p = prefs_from(&h);
		assert!(!p.collapsed);
		assert_eq!(p.flash.unwrap().message, "Done");
	}
}
