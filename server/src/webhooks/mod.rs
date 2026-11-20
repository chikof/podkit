//! Push-to-deploy webhooks: authenticates an inbound request against the
//! per-application secret. Supports GitHub's HMAC signature, GitLab's token
//! header, and a plain `X-Webhook-Secret` header for everything else.

mod routes;

use axum::http::HeaderMap;
use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;

pub use routes::router;

type HmacSha256 = Hmac<Sha256>;

/// True when `header` is `sha256=<hex>` and the hex is the HMAC-SHA256 of
/// `body` under `secret`. The final comparison is done by
/// [`Mac::verify_slice`], which is constant time.
pub(crate) fn verify_github_signature(secret: &str, body: &[u8], header: &str) -> bool {
	let Some(tag) = header.strip_prefix("sha256=").and_then(decode_hex) else {
		return false;
	};
	let Ok(mut mac) = HmacSha256::new_from_slice(secret.as_bytes()) else {
		return false;
	};
	mac.update(body);
	mac.verify_slice(&tag).is_ok()
}

/// True when `header` is the bare hex HMAC-SHA256 of `body` under `secret`, as Gitea's
/// `X-Gitea-Signature` sends it. Constant time, like [`verify_github_signature`].
fn verify_hex_signature(secret: &str, body: &[u8], header: &str) -> bool {
	let Some(tag) = decode_hex(header) else {
		return false;
	};
	let Ok(mut mac) = HmacSha256::new_from_slice(secret.as_bytes()) else {
		return false;
	};
	mac.update(body);
	mac.verify_slice(&tag).is_ok()
}

/// The id a provider gave this delivery, which it repeats on a redelivery.
pub(crate) fn delivery_id(headers: &HeaderMap) -> Option<&str> {
	[
		"x-github-delivery",
		"x-gitea-delivery",
		"x-gitlab-event-uuid",
	]
	.into_iter()
	.find_map(|name| header(headers, name))
	.filter(|id| !id.is_empty() && id.len() <= 100)
}

fn decode_hex(s: &str) -> Option<Vec<u8>> {
	if !s.len().is_multiple_of(2) || !s.is_ascii() {
		return None;
	}
	s.as_bytes()
		.chunks(2)
		.map(|pair| {
			let hi = char::from(pair[0]).to_digit(16)?;
			let lo = char::from(pair[1]).to_digit(16)?;
			u8::try_from((hi << 4) | lo).ok()
		})
		.collect()
}

pub(crate) fn header<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
	headers.get(name)?.to_str().ok()
}

/// True when the request carries any one valid credential for `secret`:
/// a matching `X-Hub-Signature-256` or `X-Gitea-Signature`, `X-Gitlab-Token`, or
/// `X-Webhook-Secret`.
fn is_authorized(headers: &HeaderMap, body: &[u8], secret: &str) -> bool {
	let signed = header(headers, "x-hub-signature-256")
		.is_some_and(|sig| verify_github_signature(secret, body, sig));
	let token =
		|name: &str| header(headers, name).is_some_and(|v| crypto::constant_time_eq(v, secret));

	let gitea_signed = header(headers, "x-gitea-signature")
		.is_some_and(|sig| verify_hex_signature(secret, body, sig));

	signed || gitea_signed || token("x-gitlab-token") || token("x-webhook-secret")
}

#[cfg(test)]
mod tests {
	use super::*;

	const SECRET: &str = "It's a Secret to Everybody";
	const BODY: &[u8] = b"Hello, World!";
	const SIGNATURE: &str =
		"sha256=757107ea0eb2509fc211221cce984b8a37570b6d7586c22c46f4379c8b043e17";

	fn headers(name: &'static str, value: &str) -> HeaderMap {
		let mut h = HeaderMap::new();
		h.insert(name, value.parse().unwrap());
		h
	}

	#[test]
	fn github_docs_vector_verifies() {
		assert!(verify_github_signature(SECRET, BODY, SIGNATURE));
	}

	#[test]
	fn github_signature_rejects_tampering() {
		assert!(!verify_github_signature(
			SECRET,
			b"Hello, World?",
			SIGNATURE
		));
		assert!(!verify_github_signature("wrong", BODY, SIGNATURE));
		assert!(!verify_github_signature(SECRET, BODY, &SIGNATURE[7..]));
		assert!(!verify_github_signature(SECRET, BODY, "sha256=zz"));
		assert!(!verify_github_signature(SECRET, BODY, "sha256=7571"));
		assert!(!verify_github_signature(SECRET, BODY, "sha256="));
	}

	#[test]
	fn any_one_credential_authorizes() {
		assert!(is_authorized(
			&headers("x-hub-signature-256", SIGNATURE),
			BODY,
			SECRET
		));
		assert!(is_authorized(
			&headers("x-gitlab-token", SECRET),
			BODY,
			SECRET
		));
		assert!(is_authorized(
			&headers("x-webhook-secret", SECRET),
			BODY,
			SECRET
		));
	}

	#[test]
	fn missing_or_wrong_credentials_are_rejected() {
		assert!(!is_authorized(&HeaderMap::new(), BODY, SECRET));
		assert!(!is_authorized(
			&headers("x-webhook-secret", "nope"),
			BODY,
			SECRET
		));
		assert!(!is_authorized(
			&headers("x-hub-signature-256", SIGNATURE),
			b"other body",
			SECRET
		));
	}
}
