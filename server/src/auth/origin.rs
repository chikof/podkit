//! The address people reach podkit at, as the browser sees it. Redirect addresses for
//! identity providers and the WebAuthn origin are built from it.

use axum::http::{HeaderMap, header};

use crate::state::Config;

/// `https://podkit.example.com`, without a trailing slash: `PUBLIC_URL` when it is set,
/// else the scheme and host of this request (behind a proxy the scheme is
/// `X-Forwarded-Proto`). A forged `Host` only changes where a sign-in sends the forger.
pub fn external_base(config: &Config, headers: &HeaderMap) -> String {
	if let Some(url) = &config.public_url {
		return url.trim_end_matches('/').to_owned();
	}
	let scheme = if super::routes::reached_over_https(headers) {
		"https"
	} else {
		"http"
	};
	let host = headers
		.get(header::HOST)
		.and_then(|h| h.to_str().ok())
		.filter(|h| !h.is_empty() && h.bytes().all(|b| b.is_ascii_graphic()))
		.unwrap_or("localhost");
	format!("{scheme}://{host}")
}
