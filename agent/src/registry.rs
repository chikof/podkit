//! Registry login check: the Docker registry v2 handshake a pull starts with. The control
//! plane uses it to tell a saved registry's owner whether the password works before any
//! deploy depends on it. Podman has no login endpoint, so this speaks to `/v2/` itself.
//!
//! The same handshake answers image auto-update's question, which manifest a tag names right
//! now ([`remote_digest`]): one `HEAD` of the manifest, the way `podman auto-update` asks,
//! without pulling anything.

use std::fmt::Write as _;
use std::time::Duration;

use proto::{ImageDigestRequest, RegistryAuth, RegistryLogin, RegistryLoginResult};
use reqwest::header::{ACCEPT, AUTHORIZATION, WWW_AUTHENTICATE};
use reqwest::{Client, StatusCode};
use sha2::{Digest as _, Sha256};

const TIMEOUT: Duration = Duration::from_secs(10);

fn result(ok: bool, message: impl Into<String>) -> RegistryLoginResult {
	RegistryLoginResult {
		ok,
		message: message.into(),
	}
}

/// Docker Hub answers on its own host; the name people type is an alias.
fn api_host(server: &str) -> &str {
	match server {
		"docker.io" | "index.docker.io" => "registry-1.docker.io",
		other => other,
	}
}

/// Only the machine itself may be spoken to in clear text.
fn is_local(host: &str) -> bool {
	let name = host.rsplit_once(':').map_or(host, |(name, _)| name);
	let name = name.trim_matches(['[', ']']);
	name == "localhost" || name == "::1" || name.starts_with("127.")
}

/// One challenge of a `WWW-Authenticate` header.
#[derive(Debug, PartialEq, Eq)]
enum Challenge {
	Basic,
	Bearer {
		realm: String,
		service: Option<String>,
	},
}

fn param(params: &str, key: &str) -> Option<String> {
	let start = params.find(&format!("{key}=\""))? + key.len() + 2;
	let end = params[start..].find('"')? + start;
	Some(params[start..end].to_owned())
}

fn challenge(header: &str) -> Option<Challenge> {
	let (scheme, params) = header.split_once(' ').unwrap_or((header, ""));
	match scheme.to_ascii_lowercase().as_str() {
		"basic" => Some(Challenge::Basic),
		"bearer" => Some(Challenge::Bearer {
			realm: param(params, "realm")?,
			service: param(params, "service"),
		}),
		_ => None,
	}
}

/// Logs in the way `docker login` does: ask `/v2/`, answer its challenge with the
/// credentials, and see whether the registry lets them through.
pub async fn check(http: &Client, login: &RegistryLogin) -> RegistryLoginResult {
	let host = api_host(&login.server);
	let mut schemes = vec!["https"];
	if is_local(host) {
		schemes.push("http");
	}
	let mut last = String::new();
	for scheme in schemes {
		match attempt(http, scheme, host, login).await {
			Ok(done) => return done,
			Err(e) => last = e,
		}
	}
	result(false, format!("Could not reach {}: {last}", login.server))
}

/// `Err` only when nothing answered at `scheme`.
async fn attempt(
	http: &Client,
	scheme: &str,
	host: &str,
	login: &RegistryLogin,
) -> Result<RegistryLoginResult, String> {
	let url = format!("{scheme}://{host}/v2/");
	let first = http
		.get(&url)
		.timeout(TIMEOUT)
		.send()
		.await
		.map_err(|e| e.without_url().to_string())?;
	match first.status() {
		StatusCode::OK => Ok(result(
			true,
			"The registry accepted the request without a login, so the credentials were not checked.",
		)),
		StatusCode::UNAUTHORIZED => {
			let header = first
				.headers()
				.get(WWW_AUTHENTICATE)
				.and_then(|v| v.to_str().ok())
				.unwrap_or_default()
				.to_owned();
			Ok(answer(http, &url, &header, login).await)
		}
		status => Ok(result(
			false,
			format!("The registry answered {status} where a v2 registry was expected."),
		)),
	}
}

async fn answer(
	http: &Client,
	url: &str,
	header: &str,
	login: &RegistryLogin,
) -> RegistryLoginResult {
	let refused = result(false, "The registry refused the username or password.");
	let sent = match challenge(header) {
		Some(Challenge::Basic) => http
			.get(url)
			.basic_auth(&login.username, Some(&login.password))
			.timeout(TIMEOUT)
			.send()
			.await
			.map(|r| r.status()),
		Some(Challenge::Bearer { realm, service }) => {
			let mut target = realm;
			if let Some(service) = service {
				target.push(if target.contains('?') { '&' } else { '?' });
				target.push_str("service=");
				for b in service.bytes() {
					if b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_') {
						target.push(char::from(b));
					} else {
						let _ = write!(target, "%{b:02X}");
					}
				}
			}
			http.get(&target)
				.basic_auth(&login.username, Some(&login.password))
				.timeout(TIMEOUT)
				.send()
				.await
				.map(|r| r.status())
		}
		None => {
			return result(
				false,
				"The registry asked for a login method podkit does not speak.",
			);
		}
	};
	match sent {
		Ok(status) if status.is_success() => result(true, "The registry accepted the login."),
		Ok(StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN) => refused,
		Ok(status) => result(
			false,
			format!("The registry answered {status} to the login."),
		),
		Err(e) => result(
			false,
			format!("The login request failed: {}", e.without_url()),
		),
	}
}

/// The manifest kinds a pull accepts, multi-platform indexes first: the digest of an index is
/// what a pull by tag records, so it is the one to compare.
const MANIFEST_TYPES: &str = "application/vnd.oci.image.index.v1+json, \
	application/vnd.docker.distribution.manifest.list.v2+json, \
	application/vnd.oci.image.manifest.v1+json, \
	application/vnd.docker.distribution.manifest.v2+json";

/// An image reference split the way the registry API names it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageRef {
	/// Host and port as a pull names it (`docker.io`, `ghcr.io`, `127.0.0.1:5000`).
	pub host: String,
	/// `library/alpine`, `me/app`.
	pub repository: String,
	/// `latest` when the reference names none.
	pub tag: String,
}

fn valid_repository(repo: &str) -> bool {
	!repo.is_empty()
		&& repo.len() <= 255
		&& !repo.starts_with('/')
		&& !repo.ends_with('/')
		&& !repo.contains("//")
		&& repo.bytes().all(|b| {
			b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-' | b'/')
		})
}

fn valid_tag(tag: &str) -> bool {
	(1..=128).contains(&tag.len())
		&& !tag.starts_with(['.', '-'])
		&& tag
			.bytes()
			.all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// Splits `ghcr.io/me/app:1` into host, repository and tag, the way `docker pull` reads it:
/// no host means Docker Hub (and its `library/` for one-word names), no tag means `latest`.
///
/// # Errors
/// A message for a reference pinned by digest (it has no tag to follow) or one that is not
/// an image reference.
pub fn parse_reference(reference: &str) -> Result<ImageRef, String> {
	let reference = reference.trim();
	if reference.contains('@') {
		return Err("The image is pinned by digest, so it has no tag to follow.".to_owned());
	}
	let (host, rest) = match reference.split_once('/') {
		Some((first, rest))
			if first.contains('.') || first.contains(':') || first == "localhost" =>
		{
			(first.to_ascii_lowercase(), rest)
		}
		_ => ("docker.io".to_owned(), reference),
	};
	let last = rest.rsplit('/').next().unwrap_or(rest);
	let (repository, tag) = match last.split_once(':') {
		Some((_, tag)) => (&rest[..rest.len() - tag.len() - 1], tag),
		None => (rest, "latest"),
	};
	let repository = if host == "docker.io" && !repository.contains('/') {
		format!("library/{repository}")
	} else {
		repository.to_owned()
	};
	let host_ok = !host.is_empty()
		&& host.len() <= 255
		&& host
			.chars()
			.all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | ':' | '[' | ']'));
	if !host_ok || !valid_repository(&repository) || !valid_tag(tag) {
		return Err(format!("{reference:?} is not an image reference."));
	}
	Ok(ImageRef {
		host,
		repository,
		tag: tag.to_owned(),
	})
}

/// `sha256:` and 64 lowercase hex digits (or `sha512:` and 128).
#[must_use]
pub fn valid_digest(digest: &str) -> bool {
	let hex = |s: &str, n: usize| {
		s.len() == n
			&& s.bytes()
				.all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
	};
	match digest.split_once(':') {
		Some(("sha256", h)) => hex(h, 64),
		Some(("sha512", h)) => hex(h, 128),
		_ => false,
	}
}

/// The digests of `repo@sha256:…` references, each once, in order.
#[must_use]
pub fn digests_of(references: &[String]) -> Vec<String> {
	let mut out: Vec<String> = Vec::new();
	for r in references {
		if let Some((_, d)) = r.rsplit_once('@')
			&& valid_digest(d)
			&& !out.iter().any(|x| x == d)
		{
			out.push(d.to_owned());
		}
	}
	out
}

/// Whether credentials saved for `server` belong to the image's registry `host`.
fn same_registry(server: &str, host: &str) -> bool {
	let fold = |h: &str| match h.trim().to_ascii_lowercase().as_str() {
		"index.docker.io" | "registry-1.docker.io" | "registry.hub.docker.com" => {
			"docker.io".to_owned()
		}
		other => other.to_owned(),
	};
	fold(server) == fold(host)
}

/// The header a request is authorized with.
enum Authorization {
	Bearer(String),
	Basic(String, String),
}

/// Why one scheme gave no digest: nothing answered (try the next), or the registry did.
enum Miss {
	Unreachable(String),
	Answer(String),
}

/// Which manifest `req.reference` names at its registry now.
///
/// # Errors
/// A message for the user: the reference cannot be followed, the registry cannot be
/// reached, refused the login, or has no such tag.
pub async fn remote_digest(http: &Client, req: &ImageDigestRequest) -> Result<String, String> {
	let image = parse_reference(&req.reference)?;
	// A login is only ever sent to the registry it was saved for.
	let auth = req.auth.as_ref().filter(|a| {
		a.server
			.as_deref()
			.is_none_or(|s| same_registry(s, &image.host))
	});
	let host = api_host(&image.host);
	let mut schemes = vec!["https"];
	if is_local(host) {
		schemes.push("http");
	}
	let mut last = String::new();
	for scheme in schemes {
		match lookup(http, scheme, host, &image, auth).await {
			Ok(digest) => return Ok(digest),
			Err(Miss::Answer(message)) => return Err(message),
			Err(Miss::Unreachable(e)) => last = e,
		}
	}
	Err(format!("Could not reach {}: {last}", image.host))
}

async fn send(
	http: &Client,
	method: reqwest::Method,
	url: &str,
	authz: Option<&Authorization>,
) -> Result<reqwest::Response, String> {
	let mut request = http
		.request(method, url)
		.header(ACCEPT, MANIFEST_TYPES)
		.timeout(TIMEOUT);
	request = match authz {
		Some(Authorization::Bearer(token)) => {
			request.header(AUTHORIZATION, format!("Bearer {token}"))
		}
		Some(Authorization::Basic(user, password)) => request.basic_auth(user, Some(password)),
		None => request,
	};
	request
		.send()
		.await
		.map_err(|e| e.without_url().to_string())
}

async fn lookup(
	http: &Client,
	scheme: &str,
	host: &str,
	image: &ImageRef,
	auth: Option<&RegistryAuth>,
) -> Result<String, Miss> {
	let url = format!(
		"{scheme}://{host}/v2/{}/manifests/{}",
		image.repository, image.tag
	);
	let first = send(http, reqwest::Method::HEAD, &url, None)
		.await
		.map_err(Miss::Unreachable)?;
	let (resp, authz) = if first.status() == StatusCode::UNAUTHORIZED {
		let header = first
			.headers()
			.get(WWW_AUTHENTICATE)
			.and_then(|v| v.to_str().ok())
			.unwrap_or_default()
			.to_owned();
		let authz = authorize(http, &header, image, auth).await?;
		let resp = send(http, reqwest::Method::HEAD, &url, Some(&authz))
			.await
			.map_err(|e| Miss::Answer(format!("The manifest request failed: {e}")))?;
		(resp, Some(authz))
	} else {
		(first, None)
	};
	let name = format!("{}:{}", image.repository, image.tag);
	match resp.status() {
		StatusCode::OK => {}
		StatusCode::NOT_FOUND => {
			return Err(Miss::Answer(format!("The registry has no image {name}.")));
		}
		StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
			return Err(Miss::Answer(format!(
				"The registry refused access to {name}. Check the registry login."
			)));
		}
		status => {
			return Err(Miss::Answer(format!(
				"The registry answered {status} for {name}."
			)));
		}
	}
	let header = resp
		.headers()
		.get("docker-content-digest")
		.and_then(|v| v.to_str().ok())
		.map(str::trim)
		.filter(|d| valid_digest(d))
		.map(str::to_owned);
	if let Some(digest) = header {
		return Ok(digest);
	}
	// A registry that leaves the header out of a HEAD: the digest is the hash of the bytes.
	let body = send(http, reqwest::Method::GET, &url, authz.as_ref())
		.await
		.map_err(|e| Miss::Answer(format!("The manifest request failed: {e}")))?;
	if body.status() != StatusCode::OK {
		return Err(Miss::Answer(format!(
			"The registry answered {} for {name}.",
			body.status()
		)));
	}
	let bytes = body.bytes().await.map_err(|e| {
		Miss::Answer(format!(
			"The manifest could not be read: {}",
			e.without_url()
		))
	})?;
	Ok(sha256_digest(&bytes))
}

fn sha256_digest(bytes: &[u8]) -> String {
	Sha256::digest(bytes)
		.iter()
		.fold(String::from("sha256:"), |mut out, b| {
			let _ = write!(out, "{b:02x}");
			out
		})
}

/// Answers the registry's challenge: a token from its token service for a Bearer one, the
/// login itself for Basic.
async fn authorize(
	http: &Client,
	header: &str,
	image: &ImageRef,
	auth: Option<&RegistryAuth>,
) -> Result<Authorization, Miss> {
	match challenge(header) {
		Some(Challenge::Basic) => auth
			.map(|a| Authorization::Basic(a.username.clone(), a.password.clone()))
			.ok_or_else(|| {
				Miss::Answer(
					"The registry needs a login. Pick a saved registry for this application."
						.to_owned(),
				)
			}),
		Some(Challenge::Bearer { realm, service }) => {
			let mut url = reqwest::Url::parse(&realm).map_err(|_| {
				Miss::Answer("The registry named a token service podkit cannot use.".to_owned())
			})?;
			let realm_host = url.host_str().unwrap_or_default().to_owned();
			if url.scheme() != "https" && !(url.scheme() == "http" && is_local(&realm_host)) {
				return Err(Miss::Answer(
					"The registry's token service is not on HTTPS.".to_owned(),
				));
			}
			{
				let mut query = url.query_pairs_mut();
				if let Some(service) = &service {
					query.append_pair("service", service);
				}
				query.append_pair("scope", &format!("repository:{}:pull", image.repository));
			}
			let mut request = http.get(url).timeout(TIMEOUT);
			if let Some(a) = auth {
				request = request.basic_auth(&a.username, Some(&a.password));
			}
			let resp = request.send().await.map_err(|e| {
				Miss::Answer(format!("The token request failed: {}", e.without_url()))
			})?;
			match resp.status() {
				s if s.is_success() => {}
				StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
					return Err(Miss::Answer(
						"The registry refused the username or password.".to_owned(),
					));
				}
				s => {
					return Err(Miss::Answer(format!(
						"The registry's token service answered {s}."
					)));
				}
			}
			let body: serde_json::Value = resp.json().await.map_err(|_| {
				Miss::Answer("The registry's token service sent no token.".to_owned())
			})?;
			body.get("token")
				.or_else(|| body.get("access_token"))
				.and_then(serde_json::Value::as_str)
				.filter(|t| !t.is_empty())
				.map(|t| Authorization::Bearer(t.to_owned()))
				.ok_or_else(|| {
					Miss::Answer("The registry's token service sent no token.".to_owned())
				})
		}
		None => Err(Miss::Answer(
			"The registry asked for a login method podkit does not speak.".to_owned(),
		)),
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn challenges_are_parsed() {
		assert_eq!(
			challenge("Basic realm=\"Registry Realm\""),
			Some(Challenge::Basic)
		);
		assert_eq!(
			challenge(
				"Bearer realm=\"https://ghcr.io/token\",service=\"ghcr.io\",scope=\"repository:x:pull\""
			),
			Some(Challenge::Bearer {
				realm: "https://ghcr.io/token".to_owned(),
				service: Some("ghcr.io".to_owned()),
			})
		);
		assert_eq!(challenge("Negotiate"), None);
		assert_eq!(challenge(""), None);
	}

	#[test]
	fn only_loopback_may_use_plain_http() {
		for local in [
			"localhost:5000",
			"127.0.0.1:5000",
			"[::1]:5000",
			"127.0.0.1",
		] {
			assert!(is_local(local), "{local}");
		}
		for remote in ["ghcr.io", "10.0.0.5:5000", "localhost.example.com"] {
			assert!(!is_local(remote), "{remote}");
		}
	}

	#[test]
	fn docker_hub_aliases_map_to_its_api_host() {
		assert_eq!(api_host("docker.io"), "registry-1.docker.io");
		assert_eq!(api_host("ghcr.io"), "ghcr.io");
	}

	#[test]
	fn references_split_like_docker_pull() {
		let r = |host: &str, repo: &str, tag: &str| ImageRef {
			host: host.to_owned(),
			repository: repo.to_owned(),
			tag: tag.to_owned(),
		};
		for (input, want) in [
			("alpine", r("docker.io", "library/alpine", "latest")),
			("alpine:3.20", r("docker.io", "library/alpine", "3.20")),
			("traefik/whoami:v1", r("docker.io", "traefik/whoami", "v1")),
			("ghcr.io/me/app:latest", r("ghcr.io", "me/app", "latest")),
			("GHCR.io/me/app", r("ghcr.io", "me/app", "latest")),
			(
				"registry.example.com:5000/team/svc/api:2",
				r("registry.example.com:5000", "team/svc/api", "2"),
			),
			("localhost:5000/app", r("localhost:5000", "app", "latest")),
			("localhost/app:x", r("localhost", "app", "x")),
		] {
			assert_eq!(parse_reference(input), Ok(want), "{input}");
		}
		for bad in [
			"ghcr.io/me/app@sha256:0000000000000000000000000000000000000000000000000000000000000000",
			"ghcr.io/me/app:1@sha256:abc",
			"",
			"ghcr.io/Me/App",
			"ghcr.io/me/app:",
			"ghcr.io/me/app:-x",
			"ghcr.io//app",
			"ghcr.io/me/app:a b",
		] {
			assert!(parse_reference(bad).is_err(), "{bad:?}");
		}
		assert!(
			parse_reference("ghcr.io/me/app@sha256:1")
				.unwrap_err()
				.contains("pinned")
		);
	}

	#[test]
	fn digests_are_checked_and_read_from_references() {
		let d = format!("sha256:{}", "a1".repeat(32));
		assert!(valid_digest(&d));
		assert!(!valid_digest(&d.to_uppercase()));
		assert!(!valid_digest("sha256:abc"));
		assert!(!valid_digest("md5:d41d8cd98f00b204e9800998ecf8427e"));
		assert!(valid_digest(&format!("sha512:{}", "0f".repeat(64))));
		let other = format!("sha256:{}", "b2".repeat(32));
		assert_eq!(
			digests_of(&[
				format!("ghcr.io/me/app@{d}"),
				format!("docker.io/me/app@{d}"),
				format!("ghcr.io/me/app@{other}"),
				"ghcr.io/me/app:latest".to_owned(),
				"ghcr.io/me/app@sha256:short".to_owned(),
			]),
			vec![d, other]
		);
	}

	#[test]
	fn a_login_belongs_only_to_its_own_registry() {
		assert!(same_registry("ghcr.io", "GHCR.io"));
		assert!(same_registry("index.docker.io", "docker.io"));
		assert!(!same_registry("ghcr.io", "evil.example.com"));
	}

	#[test]
	fn the_digest_of_bytes_is_their_sha256() {
		assert_eq!(
			sha256_digest(b""),
			"sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
		);
	}

	mod mock {
		//! A registry on loopback that speaks the token handshake, to drive [`remote_digest`]
		//! end to end over plain HTTP (allowed only for loopback).

		use std::sync::Arc;
		use std::sync::atomic::{AtomicUsize, Ordering};

		use axum::Router;
		use axum::extract::{Path, Query, State};
		use axum::http::{HeaderMap, StatusCode as Code, header};
		use axum::response::{IntoResponse, Response};
		use axum::routing::get;

		use super::super::*;

		const DIGEST: &str =
			"sha256:1111111111111111111111111111111111111111111111111111111111111111";
		const BODY: &str = "{\"schemaVersion\":2}";

		#[derive(Clone)]
		struct Mock {
			base: String,
			token_calls: Arc<AtomicUsize>,
		}

		async fn token(
			State(m): State<Mock>,
			Query(q): Query<std::collections::HashMap<String, String>>,
			headers: HeaderMap,
		) -> Response {
			m.token_calls.fetch_add(1, Ordering::SeqCst);
			// "robot:secret"
			let basic = headers
				.get(header::AUTHORIZATION)
				.and_then(|v| v.to_str().ok())
				.unwrap_or_default();
			let scope = q.get("scope").map(String::as_str).unwrap_or_default();
			if q.get("service").map(String::as_str) != Some("mock") {
				return Code::BAD_REQUEST.into_response();
			}
			if scope == "repository:private/app:pull" && basic != "Basic cm9ib3Q6c2VjcmV0" {
				return Code::UNAUTHORIZED.into_response();
			}
			axum::Json(serde_json::json!({ "token": format!("t-{scope}") })).into_response()
		}

		async fn manifest(
			State(m): State<Mock>,
			Path((ns, name, tag)): Path<(String, String, String)>,
			headers: HeaderMap,
		) -> Response {
			let repo = format!("{ns}/{name}");
			let want = format!("Bearer t-repository:{repo}:pull");
			let given = headers
				.get(header::AUTHORIZATION)
				.and_then(|v| v.to_str().ok());
			if given != Some(want.as_str()) {
				let challenge = format!(
					"Bearer realm=\"{}/token\",service=\"mock\",scope=\"repository:{repo}:pull\"",
					m.base
				);
				return (Code::UNAUTHORIZED, [(header::WWW_AUTHENTICATE, challenge)])
					.into_response();
			}
			let accept = headers
				.get(header::ACCEPT)
				.and_then(|v| v.to_str().ok())
				.unwrap_or_default();
			if !accept.contains("image.index") {
				return Code::NOT_ACCEPTABLE.into_response();
			}
			match (repo.as_str(), tag.as_str()) {
				("public/app" | "private/app", "latest") => {
					([("docker-content-digest", DIGEST)], BODY).into_response()
				}
				// No digest header: the client hashes the body.
				("public/nohead", "latest") => BODY.into_response(),
				_ => Code::NOT_FOUND.into_response(),
			}
		}

		async fn serve() -> (Mock, String) {
			let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
			let host = listener.local_addr().unwrap().to_string();
			let mock = Mock {
				base: format!("http://{host}"),
				token_calls: Arc::new(AtomicUsize::new(0)),
			};
			let app = Router::new()
				.route("/token", get(token))
				.route("/v2/{ns}/{name}/manifests/{tag}", get(manifest))
				.with_state(mock.clone());
			tokio::spawn(async move { axum::serve(listener, app).await });
			(mock, host)
		}

		fn client() -> Client {
			let _ = rustls::crypto::ring::default_provider().install_default();
			Client::builder().no_proxy().build().unwrap()
		}

		fn req(host: &str, image: &str, auth: Option<RegistryAuth>) -> ImageDigestRequest {
			ImageDigestRequest {
				reference: format!("{host}/{image}"),
				auth,
				local: None,
			}
		}

		fn robot(server: Option<&str>) -> RegistryAuth {
			RegistryAuth {
				username: "robot".to_owned(),
				password: "secret".to_owned(),
				server: server.map(str::to_owned),
			}
		}

		#[tokio::test]
		async fn a_public_tag_is_read_through_an_anonymous_token() {
			let (_, host) = serve().await;
			let got = remote_digest(&client(), &req(&host, "public/app", None)).await;
			assert_eq!(got.as_deref(), Ok(DIGEST));
		}

		#[tokio::test]
		async fn a_private_tag_needs_its_login() {
			let (mock, host) = serve().await;
			let http = client();
			let refused = remote_digest(&http, &req(&host, "private/app:latest", None)).await;
			assert!(refused.unwrap_err().contains("refused"));
			let got = remote_digest(&http, &req(&host, "private/app", Some(robot(None)))).await;
			assert_eq!(got.as_deref(), Ok(DIGEST));
			let saved =
				remote_digest(&http, &req(&host, "private/app", Some(robot(Some(&host))))).await;
			assert_eq!(saved.as_deref(), Ok(DIGEST));
			// A login saved for another registry is not sent here.
			let calls = mock.token_calls.load(Ordering::SeqCst);
			let other = remote_digest(
				&http,
				&req(&host, "private/app", Some(robot(Some("ghcr.io")))),
			)
			.await;
			assert!(other.is_err());
			assert!(mock.token_calls.load(Ordering::SeqCst) > calls);
		}

		#[tokio::test]
		async fn without_the_header_the_digest_is_the_hash_of_the_manifest() {
			let (_, host) = serve().await;
			let got = remote_digest(&client(), &req(&host, "public/nohead", None)).await;
			assert_eq!(got, Ok(sha256_digest(BODY.as_bytes())));
		}

		#[tokio::test]
		async fn a_missing_tag_says_so() {
			let (_, host) = serve().await;
			let got = remote_digest(&client(), &req(&host, "public/app:v9", None)).await;
			assert!(got.unwrap_err().contains("has no image public/app:v9"));
		}
	}
}
