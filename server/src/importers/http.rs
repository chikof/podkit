//! Reading a source over HTTP.
//!
//! One [`Client`] per import. Every request goes through the instance's outbound policy
//! (`crate::net::outbound`): no loopback, link-local or metadata address, private networks only
//! where the instance allows them, one resolution per name and no redirects (the credential
//! must go only where the person pointed it). It gives up on a slow instance and refuses an
//! answer that is absurdly large. A credential is only ever in a request header; no message
//! made here contains it, or anything the other side said.
//!
//! **No oracle.** The address is whatever the person typed, so what a failure says would
//! otherwise map the network behind podkit ("cannot connect", 401, 403, 404, "not JSON" tell a
//! closed port from a web server from an API). Every failure to read from the address, for
//! whatever reason, is the same message; the reason goes to the log only. Importing needs
//! `create project`, the same permission that lets a person point podkit at their own servers.

use std::time::Duration;

use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde_json::Value;

use super::{Credentials, Source};
use crate::error::ServerError;
use crate::net::outbound::{Outbound, read_capped};

const TIMEOUT: Duration = Duration::from_secs(30);
/// The largest response taken: a Coolify project with a few hundred variables is some
/// hundreds of kilobytes.
const MAX_BODY: usize = 32 * 1024 * 1024;
/// A hard stop for an import that would otherwise ask forever.
const MAX_REQUESTS: usize = 3000;

pub struct Client {
	outbound: Outbound,
	/// `https://host[:port][/prefix]` with no trailing slash.
	base: String,
	headers: HeaderMap,
	source: Source,
	sent: std::sync::atomic::AtomicUsize,
}

fn failed(source: Source, what: impl std::fmt::Display) -> ServerError {
	ServerError::Import(format!("{}: {what}", source.label()))
}

/// The one message for every failure to read from the address: unreachable, refused by the
/// policy, a wrong credential, a missing page, an answer that is not what was expected.
fn unusable(source: Source, why: &str, detail: impl std::fmt::Display) -> ServerError {
	tracing::warn!("import from {} failed: {why}: {detail}", source.as_str());
	let credential = source.credential().to_lowercase();
	let extra = if source == Source::Coolify {
		" A Coolify token needs the read and read:sensitive abilities."
	} else {
		""
	};
	failed(
		source,
		format!(
			"podkit could not read from that address. Check the address and the {credential}, and that the address is one this podkit may reach: private network addresses need the \"private addresses\" setting in Settings → Registration, and loopback never works.{extra}"
		),
	)
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
	pub async fn connect(
		outbound: &Outbound,
		source: Source,
		creds: &Credentials,
	) -> Result<Self, ServerError> {
		let base = base_url(&creds.url)?;
		if creds.token.trim().is_empty() {
			return Err(ServerError::Validation(format!(
				"Enter the {}.",
				source.credential().to_lowercase()
			)));
		}
		let http = outbound.clone();
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
			outbound: http,
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
			.outbound
			.post(&url)
			.await
			.map_err(|d| unusable(self.source, "refused by the outbound policy", d.reason()))?
			.timeout(TIMEOUT)
			.header(ns_k.clone(), ns_v.clone())
			.header("content-type", "application/json")
			.body(serde_json::json!({ "password": password }).to_string())
			.send()
			.await
			.map_err(|e| unreachable(self.source, e))?;
		let body = self.read(response, "log in").await?;
		let answer: Value = serde_json::from_slice(&body)
			.map_err(|_| unusable(self.source, "the login is not JSON", ""))?;
		if answer["status"].as_i64() != Some(100) {
			return Err(unusable(self.source, "the login was refused", ""));
		}
		let token = answer["data"]["token"]
			.as_str()
			.ok_or_else(|| unusable(self.source, "the login answered without a token", ""))?;
		let (k, v) = header("x-captain-auth", token)?;
		self.headers.insert(k, v);
		self.headers.insert(ns_k, ns_v);
		Ok(())
	}

	async fn read(&self, response: reqwest::Response, doing: &str) -> Result<Vec<u8>, ServerError> {
		let status = response.status();
		let body = read_capped(response, MAX_BODY).await.map_err(|e| {
			unusable(
				self.source,
				"the answer could not be read",
				format!("{e:?}"),
			)
		})?;
		if status.is_success() {
			return Ok(body.to_vec());
		}
		// Which status it was goes to the log. The person is told one thing, whatever it was.
		Err(unusable(
			self.source,
			&format!("answered {} while trying to {doing}", status.as_u16()),
			"",
		))
	}

	/// `GET {base}{path}` as JSON.
	///
	/// # Errors
	/// `Import` when the instance cannot be reached, refuses, or answers something that is
	/// not JSON.
	pub async fn get(&self, path: &str) -> Result<Value, ServerError> {
		self.get_opt(path)
			.await?
			.ok_or_else(|| unusable(self.source, "a page is missing", path))
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
			.outbound
			.get(&url)
			.await
			.map_err(|d| unusable(self.source, "refused by the outbound policy", d.reason()))?
			.timeout(TIMEOUT)
			.headers(self.headers.clone())
			.header("accept", "application/json")
			.send()
			.await
			.map_err(|e| unreachable(self.source, e))?;
		if response.status().as_u16() == 404 {
			return Ok(None);
		}
		let body = self.read(response, &format!("read {path}")).await?;
		let value = serde_json::from_slice(&body)
			.map_err(|_| unusable(self.source, "an answer is not JSON", path))?;
		Ok(Some(value))
	}
}

/// What went wrong with the request goes to the log; never the URL of the request, which holds
/// nothing secret but is the person's own business in a log, and never to the person.
fn unreachable(source: Source, e: reqwest::Error) -> ServerError {
	unusable(source, "the request failed", e.without_url())
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
