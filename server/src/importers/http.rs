//! Reading a source over HTTP.
//!
//! One [`Client`] per import. It follows no redirects (the credential must go only where the
//! person pointed it), gives up on a slow instance, and refuses an answer that is absurdly
//! large. A credential is only ever in a request header; no message made here contains it, or
//! the response body of a failed request, which may echo it.
//!
//! The address is whatever the person typed, a homelab's LAN address included, so the only
//! check is that it is an `http` or `https` address. Importing needs `create project`, the
//! same permission that lets a person point podkit at their own servers.

use std::time::Duration;

use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde_json::Value;

use super::{Credentials, Source};
use crate::error::ServerError;

const TIMEOUT: Duration = Duration::from_secs(30);
/// The largest response taken: a Coolify project with a few hundred variables is some
/// hundreds of kilobytes.
const MAX_BODY: usize = 32 * 1024 * 1024;
/// A hard stop for an import that would otherwise ask forever.
const MAX_REQUESTS: usize = 3000;

pub struct Client {
	http: reqwest::Client,
	/// `https://host[:port][/prefix]` with no trailing slash.
	base: String,
	headers: HeaderMap,
	source: Source,
	sent: std::sync::atomic::AtomicUsize,
}

fn failed(source: Source, what: impl std::fmt::Display) -> ServerError {
	ServerError::Import(format!("{}: {what}", source.label()))
}

/// The address as `scheme://host[:port][/path]`.
///
/// # Errors
/// `Validation` for anything but an `http` or `https` address with a host.
pub fn base_url(raw: &str) -> Result<String, ServerError> {
	let bad = |why: &str| ServerError::Validation(format!("The address is not usable: {why}."));
	let parsed = url::Url::parse(raw.trim())
		.map_err(|_| bad("write it as https://host or http://host:port"))?;
	if !matches!(parsed.scheme(), "http" | "https") {
		return Err(bad("it must start with http:// or https://"));
	}
	if parsed.host_str().is_none() {
		return Err(bad("it has no host"));
	}
	if !parsed.username().is_empty() || parsed.password().is_some() {
		return Err(bad("leave the login out of the address"));
	}
	let mut base = format!(
		"{}://{}",
		parsed.scheme(),
		parsed.host_str().unwrap_or_default()
	);
	if let Some(port) = parsed.port() {
		base = format!("{base}:{port}");
	}
	base.push_str(parsed.path().trim_end_matches('/'));
	Ok(base)
}

fn header(name: &'static str, value: &str) -> Result<(HeaderName, HeaderValue), ServerError> {
	let mut value = HeaderValue::from_str(value).map_err(|_| {
		ServerError::Validation("The credential has characters a header cannot carry.".to_owned())
	})?;
	value.set_sensitive(true);
	Ok((HeaderName::from_static(name), value))
}

impl Client {
	/// A client for `source`. CapRover logs in here (one `POST`); Coolify and Dokploy send
	/// their credential with each request.
	///
	/// # Errors
	/// `Validation` for an unusable address or credential, `Import` when the instance does
	/// not answer or refuses the credential.
	pub async fn connect(source: Source, creds: &Credentials) -> Result<Self, ServerError> {
		let base = base_url(&creds.url)?;
		if creds.token.trim().is_empty() {
			return Err(ServerError::Validation(format!(
				"Enter the {}.",
				source.credential().to_lowercase()
			)));
		}
		let http = reqwest::Client::builder()
			.timeout(TIMEOUT)
			.redirect(reqwest::redirect::Policy::none())
			.build()
			.map_err(|_| ServerError::Internal)?;
		let mut headers = HeaderMap::new();
		let token = creds.token.trim();
		match source {
			Source::Coolify => {
				let mut v = HeaderValue::from_str(&format!("Bearer {token}")).map_err(|_| {
					ServerError::Validation(
						"The credential has characters a header cannot carry.".to_owned(),
					)
				})?;
				v.set_sensitive(true);
				headers.insert(reqwest::header::AUTHORIZATION, v);
			}
			Source::Dokploy => {
				let (k, v) = header("x-api-key", token)?;
				headers.insert(k, v);
			}
			Source::Caprover => {}
		}
		let mut client = Self {
			http,
			base,
			headers,
			source,
			sent: std::sync::atomic::AtomicUsize::new(0),
		};
		if source == Source::Caprover {
			client.caprover_login(token).await?;
		}
		Ok(client)
	}

	/// CapRover: `POST /api/v2/login` with the password, an `x-namespace: captain` header,
	/// answers `{ "status": 100, "data": { "token": … } }`; the token goes back as
	/// `x-captain-auth`. Source: `src/routes/login/LoginRouter.ts` and
	/// `src/utils/CaptainConstants.ts` of <https://github.com/caprover/caprover>.
	async fn caprover_login(&mut self, password: &str) -> Result<(), ServerError> {
		let url = format!("{}/api/v2/login", self.base);
		let (ns_k, ns_v) = header("x-namespace", "captain")?;
		let response = self
			.http
			.post(&url)
			.header(ns_k.clone(), ns_v.clone())
			.header("content-type", "application/json")
			.body(serde_json::json!({ "password": password }).to_string())
			.send()
			.await
			.map_err(|e| unreachable(self.source, e))?;
		let body = self.read(response, "log in").await?;
		let answer: Value = serde_json::from_slice(&body).map_err(|_| {
			failed(
				self.source,
				"the login answered something that is not JSON; is this the CapRover address?",
			)
		})?;
		if answer["status"].as_i64() != Some(100) {
			let why = answer["description"]
				.as_str()
				.unwrap_or("it refused the login");
			return Err(failed(self.source, format!("login failed: {why}")));
		}
		let token = answer["data"]["token"]
			.as_str()
			.ok_or_else(|| failed(self.source, "the login answered without a token"))?;
		let (k, v) = header("x-captain-auth", token)?;
		self.headers.insert(k, v);
		self.headers.insert(ns_k, ns_v);
		Ok(())
	}

	async fn read(
		&self,
		mut response: reqwest::Response,
		doing: &str,
	) -> Result<Vec<u8>, ServerError> {
		let status = response.status();
		let mut body = Vec::new();
		while let Some(chunk) = response
			.chunk()
			.await
			.map_err(|e| unreachable(self.source, e))?
		{
			body.extend_from_slice(&chunk);
			if body.len() > MAX_BODY {
				return Err(failed(self.source, "an answer was too large to import"));
			}
		}
		if status.is_success() {
			return Ok(body);
		}
		let why = match status.as_u16() {
			401 => "it refused the credential (401); check the token and its permissions".to_owned(),
			403 => "the credential is not allowed to read this (403); a Coolify token needs the read and read:sensitive abilities".to_owned(),
			404 => format!("it does not know this address while trying to {doing} (404); is this the right instance and version?"),
			code if (300..400).contains(&code) => format!("it redirected the request ({code}); use the address it redirects to"),
			code => format!("it answered {code} while trying to {doing}"),
		};
		Err(failed(self.source, why))
	}

	/// `GET {base}{path}` as JSON.
	///
	/// # Errors
	/// `Import` when the instance cannot be reached, refuses, or answers something that is
	/// not JSON.
	pub async fn get(&self, path: &str) -> Result<Value, ServerError> {
		self.get_opt(path)
			.await?
			.ok_or_else(|| failed(self.source, format!("{path} is not on this instance")))
	}

	/// As [`Self::get`], and `None` when the instance has no such endpoint or object (404).
	///
	/// # Errors
	/// As [`Self::get`].
	pub async fn get_opt(&self, path: &str) -> Result<Option<Value>, ServerError> {
		let sent = self.sent.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
		if sent >= MAX_REQUESTS {
			return Err(failed(
				self.source,
				"this instance has more objects than one import reads; import a part of it",
			));
		}
		let url = format!("{}{path}", self.base);
		let response = self
			.http
			.get(&url)
			.headers(self.headers.clone())
			.header("accept", "application/json")
			.send()
			.await
			.map_err(|e| unreachable(self.source, e))?;
		if response.status().as_u16() == 404 {
			return Ok(None);
		}
		let body = self.read(response, &format!("read {path}")).await?;
		let value = serde_json::from_slice(&body).map_err(|_| {
			failed(
				self.source,
				format!("{path} answered something that is not JSON; is this the right address?"),
			)
		})?;
		Ok(Some(value))
	}
}

/// Never the URL of the request or the error's detail beyond its kind: neither holds the
/// credential, but a hostname is the person's own business in a log.
fn unreachable(source: Source, e: reqwest::Error) -> ServerError {
	let why = if e.is_timeout() {
		"it did not answer in time"
	} else if e.is_connect() {
		"podkit could not connect to it; check the address, and that podkit's host can reach it"
	} else {
		"the request failed"
	};
	tracing::warn!(
		"import from {} failed: {}",
		source.as_str(),
		e.without_url()
	);
	failed(source, why)
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn an_address_is_reduced_to_scheme_host_port_and_prefix() {
		assert_eq!(
			base_url("https://c.example.com/").unwrap(),
			"https://c.example.com"
		);
		assert_eq!(
			base_url(" http://10.0.0.5:8000/api/ ").unwrap(),
			"http://10.0.0.5:8000/api"
		);
		assert_eq!(
			base_url("https://d.example.com?x=1#y").unwrap(),
			"https://d.example.com"
		);
	}

	#[test]
	fn other_addresses_are_refused() {
		for bad in [
			"",
			"coolify.example.com",
			"ftp://x",
			"file:///etc/passwd",
			"https://u:p@x.example",
			"http://",
		] {
			assert!(base_url(bad).is_err(), "{bad}");
		}
	}
}
