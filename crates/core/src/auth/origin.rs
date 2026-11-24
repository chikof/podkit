//! The address people reach podkit at, as the browser sees it. Redirect addresses for
//! identity providers and the `WebAuthn` origin are built from it.

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

/// Whether a request comes from podkit's own pages: its `Origin` is this site. The expected
/// site is `X-Forwarded-Host` when a proxy sets it, else `Host`, so it works over plain HTTP.
/// A request with no `Origin` (curl, not a browser form) passes unless the browser says it
/// came from another site. The web UI's origin guard and the terminal's WebSocket use it.
pub fn same_origin(headers: &HeaderMap) -> bool {
	let Some(origin) = headers.get(header::ORIGIN) else {
		return !headers
			.get("sec-fetch-site")
			.and_then(|v| v.to_str().ok())
			.is_some_and(|site| site == "cross-site" || site == "same-site");
	};
	let Some(authority) = origin
		.to_str()
		.ok()
		.and_then(|o| o.split_once("://"))
		.map(|(_, authority)| authority)
	else {
		return false;
	};
	let host = headers
		.get("x-forwarded-host")
		.or_else(|| headers.get(header::HOST))
		.and_then(|h| h.to_str().ok())
		.and_then(|h| h.split(',').next())
		.map(str::trim);
	host.is_some_and(|host| host.eq_ignore_ascii_case(authority))
}
