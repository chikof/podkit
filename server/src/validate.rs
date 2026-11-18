//! Input validators for values that end up in shell commands, file paths, or
//! DNS/ingress config. This is a security boundary: each fn accepts a strict
//! subset and handlers must reject anything else with `ServerError::Validation`.

use std::net::IpAddr;

/// Lowercase DNS hostname: total length <= 253, at least two labels, each
/// label 1-63 chars of `[a-z0-9-]` not starting or ending with `-`, and an
/// alphabetic last label.
pub fn hostname(s: &str) -> bool {
	if s.len() > 253 {
		return false;
	}
	let mut labels = 0;
	let mut last = "";
	for label in s.split('.') {
		let b = label.as_bytes();
		let ok = (1..=63).contains(&b.len())
			&& b[0] != b'-'
			&& b[b.len() - 1] != b'-'
			&& b.iter()
				.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-');
		if !ok {
			return false;
		}
		labels += 1;
		last = label;
	}
	labels >= 2 && last.bytes().all(|c| c.is_ascii_lowercase())
}

/// A URL-safe identifier: 1-60 chars of `[a-z0-9-]`, not starting or ending
/// with `-`.
pub fn slug(s: &str) -> bool {
	let b = s.as_bytes();
	(1..=60).contains(&b.len())
		&& b[0] != b'-'
		&& b[b.len() - 1] != b'-'
		&& b.iter()
			.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-')
}

/// A hostname or a literal IP address.
pub fn server_host(s: &str) -> bool {
	hostname(s) || s.parse::<IpAddr>().is_ok()
}

/// POSIX-style login name: `^[a-z_][a-z0-9_-]{0,31}$`.
pub fn ssh_user(s: &str) -> bool {
	let b = s.as_bytes();
	if b.is_empty() || b.len() > 32 {
		return false;
	}
	(b[0].is_ascii_lowercase() || b[0] == b'_')
		&& b[1..]
			.iter()
			.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'_' || *c == b'-')
}

/// `https://...`, `ssh://...`, or scp form `user@host:path`. No whitespace or
/// control characters anywhere, so the value cannot smuggle extra git arguments.
pub fn repo_url(s: &str) -> bool {
	if s.chars().any(|c| c.is_whitespace() || c.is_control()) {
		return false;
	}
	if let Some(rest) = s
		.strip_prefix("https://")
		.or_else(|| s.strip_prefix("ssh://"))
	{
		return !rest.is_empty();
	}
	let Some((user, rest)) = s.split_once('@') else {
		return false;
	};
	let Some((host, path)) = rest.split_once(':') else {
		return false;
	};
	ssh_user(user) && server_host(host) && !path.is_empty() && !path.starts_with('-')
}

/// Absolute path other than `/`, with no `..` component, using only
/// `[A-Za-z0-9._/-]`.
pub fn abs_path(s: &str) -> bool {
	s.starts_with('/')
		&& s != "/"
		&& s.split('/').all(|part| part != "..")
		&& s.bytes()
			.all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'/' | b'-'))
}

/// Non-empty relative path with no `..` component (used for `dockerfile_path`).
pub fn rel_path(s: &str) -> bool {
	!s.is_empty() && !s.starts_with('/') && s.split('/').all(|part| part != "..")
}

#[cfg(test)]
mod tests {
	use super::*;

	fn check(name: &str, f: fn(&str) -> bool, cases: &[(&str, bool)]) {
		for (input, want) in cases {
			assert_eq!(f(input), *want, "{name}({input:?})");
		}
	}

	#[test]
	fn slug_cases() {
		let long = "a".repeat(61);
		check(
			"slug",
			slug,
			&[
				("web", true),
				("my-app-2", true),
				("", false),
				("-a", false),
				("a-", false),
				("A", false),
				("a b", false),
				("a_b", false),
				(long.as_str(), false),
			],
		);
	}

	#[test]
	fn hostname_cases() {
		let long_ok = format!("{}.com", "a".repeat(63));
		let label_over = format!("{}.com", "a".repeat(64));
		let long_total = format!("{0}.{0}.{0}.{0}.com", "a".repeat(62));
		check(
			"hostname",
			hostname,
			&[
				("example.com", true),
				("a.b.example.co", true),
				("my-app.example.com", true),
				("1.example.com", true),
				(long_ok.as_str(), true),
				(label_over.as_str(), false),
				(long_total.as_str(), false),
				("", false),
				("localhost", false),
				("example", false),
				("Example.com", false),
				("example.COM", false),
				("-a.example.com", false),
				("a-.example.com", false),
				("a..example.com", false),
				(".example.com", false),
				("example.com.", false),
				("example.c0m", false),
				("example.123", false),
				("exa_mple.com", false),
				("exa mple.com", false),
				("exämple.com", false),
				("example.com/path", false),
				("1.2.3.4", false),
			],
		);
	}

	#[test]
	fn server_host_cases() {
		check(
			"server_host",
			server_host,
			&[
				("example.com", true),
				("10.0.0.1", true),
				("::1", true),
				("2001:db8::1", true),
				("localhost", false),
				("", false),
				("10.0.0.256", false),
				("host name", false),
				("a.com;ls", false),
			],
		);
	}

	#[test]
	fn ssh_user_cases() {
		let max = format!("a{}", "b".repeat(31));
		let over = format!("a{}", "b".repeat(32));
		check(
			"ssh_user",
			ssh_user,
			&[
				("root", true),
				("deploy", true),
				("_svc", true),
				("a", true),
				("deploy-user_1", true),
				(max.as_str(), true),
				(over.as_str(), false),
				("", false),
				("Root", false),
				("1user", false),
				("-user", false),
				("us er", false),
				("user;id", false),
				("user$", false),
				("usér", false),
			],
		);
	}

	#[test]
	fn repo_url_cases() {
		check(
			"repo_url",
			repo_url,
			&[
				("https://github.com/acme/app.git", true),
				("ssh://git@github.com/acme/app.git", true),
				("git@github.com:acme/app.git", true),
				("git@10.0.0.5:acme/app.git", true),
				("git@github.com:/srv/git/app.git", true),
				("", false),
				("https://", false),
				("ssh://", false),
				("http://github.com/acme/app.git", false),
				("file:///etc/passwd", false),
				("ext::sh -c id", false),
				("ext::id", false),
				("github.com:acme/app.git", false),
				("git@github.com", false),
				("git@github.com:", false),
				("git@github.com:-oProxyCommand=id", false),
				("Git@github.com:acme/app.git", false),
				("git@Github.com:acme/app.git", false),
				("git@local host:acme/app.git", false),
				("https://github.com/acme/app.git --upload-pack=id", false),
				("https://github.com/acme/app.git\n", false),
				("https://github.com/acme/\tapp.git", false),
				("-https://github.com/acme/app.git", false),
			],
		);
	}

	#[test]
	fn abs_path_cases() {
		check(
			"abs_path",
			abs_path,
			&[
				("/run/podman/podman.sock", true),
				("/run/user/1000/podman/podman.sock", true),
				("/a", true),
				("/a/b-c_d.e", true),
				("/a/./b", true),
				("/a/..b", true),
				("", false),
				("/", false),
				("a/b", false),
				("./a", false),
				("/a/..", false),
				("/../a", false),
				("/a/../b", false),
				("/a b", false),
				("/a;b", false),
				("/a$b", false),
				("/a\\b", false),
				("/a\nb", false),
				("/ä", false),
			],
		);
	}

	#[test]
	fn rel_path_cases() {
		check(
			"rel_path",
			rel_path,
			&[
				("Dockerfile", true),
				("docker/Dockerfile.prod", true),
				("./Dockerfile", true),
				("a..b/Dockerfile", true),
				("", false),
				("/Dockerfile", false),
				("..", false),
				("../Dockerfile", false),
				("a/../Dockerfile", false),
				("a/..", false),
			],
		);
	}
}
