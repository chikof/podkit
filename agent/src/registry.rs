//! Registry login check: the Docker registry v2 handshake a pull starts with. The control
//! plane uses it to tell a saved registry's owner whether the password works before any
//! deploy depends on it. Podman has no login endpoint, so this speaks to `/v2/` itself.

use std::fmt::Write as _;
use std::time::Duration;

use proto::{RegistryLogin, RegistryLoginResult};
use reqwest::header::WWW_AUTHENTICATE;
use reqwest::{Client, StatusCode};

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
}
