//! The address of whoever made a request: one answer for the audit log, the throttles and
//! the "add my IP" button.
//!
//! The socket peer is the default. `X-Forwarded-For` and `Forwarded` are believed only when
//! the peer is one of the **trusted proxies** (`TRUSTED_PROXIES`, a comma-separated list of
//! addresses or CIDR ranges; empty by default). A LAN client that talks to podkit directly
//! cannot name itself something else.

use std::net::{IpAddr, SocketAddr};
use std::sync::OnceLock;

use axum::extract::ConnectInfo;
use axum::http::HeaderMap;
use proto::cidr::Cidr;

/// The proxies whose forwarding headers are believed.
#[derive(Debug, Clone, Default)]
pub struct TrustedProxies(Vec<Cidr>);

impl TrustedProxies {
	/// Parses `10.0.0.1, 172.18.0.0/16` (commas or whitespace). Empty is the default: trust
	/// nobody.
	///
	/// # Errors
	/// A message naming the entry that is not an address or range.
	pub fn parse(raw: &str) -> Result<Self, String> {
		raw.split(|c: char| c == ',' || c.is_whitespace())
			.filter(|p| !p.is_empty())
			.map(Cidr::parse)
			.collect::<Result<_, _>>()
			.map(Self)
	}

	fn trusts(&self, ip: IpAddr) -> bool {
		let ip = unmap(ip);
		self.0.iter().any(|c| c.contains_ip(ip))
	}
}

static TRUSTED: OnceLock<TrustedProxies> = OnceLock::new();

/// Sets the process-wide list once, at start-up. A second call is ignored.
pub fn set_trusted(proxies: TrustedProxies) {
	let _ = TRUSTED.set(proxies);
}

/// `::ffff:a.b.c.d` is `a.b.c.d`.
fn unmap(ip: IpAddr) -> IpAddr {
	match ip {
		IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(ip, IpAddr::V4),
		v4 @ IpAddr::V4(_) => v4,
	}
}

/// The client of a request with this socket `peer` and `headers`, against the configured
/// trusted proxies. `peer` is `None` only for in-process requests (tests), where there is
/// no socket to distrust.
#[must_use]
pub fn client_addr(peer: Option<IpAddr>, headers: &HeaderMap) -> Option<IpAddr> {
	client_addr_with(
		TRUSTED.get().unwrap_or(&TrustedProxies::default()),
		peer,
		headers,
	)
}

/// [`client_addr`] against an explicit list.
#[must_use]
pub fn client_addr_with(
	trusted: &TrustedProxies,
	peer: Option<IpAddr>,
	headers: &HeaderMap,
) -> Option<IpAddr> {
	let Some(peer) = peer else {
		return chain(headers).first().copied();
	};
	let peer = unmap(peer);
	if !trusted.trusts(peer) {
		return Some(peer);
	}
	// Walk the chain from the proxy side: each hop we trust vouches for the one before it,
	// and the first address that is not a trusted proxy is the client. Entries to the left
	// of it are whatever the client chose to send.
	let hops = chain(headers);
	Some(
		hops.iter()
			.rev()
			.copied()
			.find(|ip| !trusted.trusts(*ip))
			.or_else(|| hops.first().copied())
			.unwrap_or(peer),
	)
}

/// The forwarded addresses, client first: `X-Forwarded-For`, else `Forwarded: for=`.
fn chain(headers: &HeaderMap) -> Vec<IpAddr> {
	let xff: Vec<IpAddr> = headers
		.get_all("x-forwarded-for")
		.iter()
		.filter_map(|v| v.to_str().ok())
		.flat_map(|v| v.split(','))
		.filter_map(|p| node(p.trim()))
		.map(unmap)
		.collect();
	if !xff.is_empty() {
		return xff;
	}
	headers
		.get_all("forwarded")
		.iter()
		.filter_map(|v| v.to_str().ok())
		.flat_map(|v| v.split(','))
		.flat_map(|element| element.split(';'))
		.filter_map(|pair| {
			let (k, v) = pair.split_once('=')?;
			k.trim().eq_ignore_ascii_case("for").then(|| v.trim())
		})
		.filter_map(node)
		.map(unmap)
		.collect()
}

/// An address as a proxy writes it: `1.2.3.4`, `1.2.3.4:80`, `"[::1]:80"`, `[::1]`, `::1`.
fn node(raw: &str) -> Option<IpAddr> {
	let raw = raw.trim().trim_matches('"');
	if let Ok(ip) = raw.parse::<IpAddr>() {
		return Some(ip);
	}
	if let Ok(sock) = raw.parse::<SocketAddr>() {
		return Some(sock.ip());
	}
	raw.strip_prefix('[')?
		.split_once(']')
		.and_then(|(ip, _)| ip.parse().ok())
}

/// The socket peer of a request's extensions, for callers that have only `Parts`.
#[must_use]
pub fn peer_of(extensions: &axum::http::Extensions) -> Option<IpAddr> {
	extensions
		.get::<ConnectInfo<SocketAddr>>()
		.map(|c| c.0.ip())
}

#[cfg(test)]
mod tests {
	use axum::http::HeaderValue;

	use super::*;

	fn ip(s: &str) -> IpAddr {
		s.parse().unwrap()
	}

	fn headers(pairs: &[(&'static str, &str)]) -> HeaderMap {
		let mut h = HeaderMap::new();
		for (k, v) in pairs {
			h.append(*k, HeaderValue::from_str(v).unwrap());
		}
		h
	}

	fn proxies(raw: &str) -> TrustedProxies {
		TrustedProxies::parse(raw).unwrap()
	}

	#[test]
	fn sec_cleanup_a_forwarding_header_is_ignored_from_a_peer_that_is_not_a_trusted_proxy() {
		let spoof = headers(&[("x-forwarded-for", "203.0.113.9")]);
		// The default: no proxy is trusted, not even a private-network or loopback peer.
		let none = TrustedProxies::default();
		for peer in [
			"192.168.1.50",
			"10.0.0.7",
			"127.0.0.1",
			"172.18.0.2",
			"fe80::1",
			"198.51.100.4",
		] {
			assert_eq!(
				client_addr_with(&none, Some(ip(peer)), &spoof),
				Some(ip(peer)),
				"{peer} with no trusted proxies"
			);
		}
		// A list that does not include the peer changes nothing for it.
		let some = proxies("10.1.0.0/16, 2001:db8::1");
		assert_eq!(
			client_addr_with(&some, Some(ip("192.168.1.50")), &spoof),
			Some(ip("192.168.1.50"))
		);
		// `Forwarded` is no different.
		let fwd = headers(&[("forwarded", "for=203.0.113.9")]);
		assert_eq!(
			client_addr_with(&none, Some(ip("192.168.1.50")), &fwd),
			Some(ip("192.168.1.50"))
		);
	}

	#[test]
	fn sec_cleanup_a_forwarding_header_is_honoured_from_a_trusted_proxy() {
		let trusted = proxies("127.0.0.1, 172.18.0.0/16, fd00::/8");
		let one = headers(&[("x-forwarded-for", "198.51.100.4")]);
		assert_eq!(
			client_addr_with(&trusted, Some(ip("127.0.0.1")), &one),
			Some(ip("198.51.100.4"))
		);
		assert_eq!(
			client_addr_with(&trusted, Some(ip("172.18.3.9")), &one),
			Some(ip("198.51.100.4"))
		);
		// A v4 peer that arrives as `::ffff:a.b.c.d` is still that address.
		assert_eq!(
			client_addr_with(&trusted, Some(ip("::ffff:127.0.0.1")), &one),
			Some(ip("198.51.100.4"))
		);
		// `Forwarded`, with a port, quotes and an IPv6 address.
		for (value, want) in [
			("for=198.51.100.4", "198.51.100.4"),
			("for=\"198.51.100.4:4711\";proto=https", "198.51.100.4"),
			("for=\"[2001:db8::7]:4711\", for=172.18.0.1", "2001:db8::7"),
		] {
			assert_eq!(
				client_addr_with(
					&trusted,
					Some(ip("127.0.0.1")),
					&headers(&[("forwarded", value)])
				),
				Some(ip(want)),
				"{value}"
			);
		}
		// Nothing forwarded, or garbage: the proxy's own address.
		assert_eq!(
			client_addr_with(&trusted, Some(ip("127.0.0.1")), &HeaderMap::new()),
			Some(ip("127.0.0.1"))
		);
		let junk = headers(&[("x-forwarded-for", "not-an-address")]);
		assert_eq!(
			client_addr_with(&trusted, Some(ip("127.0.0.1")), &junk),
			Some(ip("127.0.0.1"))
		);
	}

	#[test]
	fn sec_cleanup_a_client_cannot_put_a_fake_address_in_front_of_the_real_one() {
		let trusted = proxies("127.0.0.1, 10.9.0.0/24");
		// The client sent `1.2.3.4`; Traefik (a trusted proxy) appended the client it saw, then
		// a second trusted hop appended itself.
		let chain = headers(&[("x-forwarded-for", "1.2.3.4, 198.51.100.4, 10.9.0.2")]);
		assert_eq!(
			client_addr_with(&trusted, Some(ip("127.0.0.1")), &chain),
			Some(ip("198.51.100.4"))
		);
		// Several header lines count as one chain.
		let lines = headers(&[
			("x-forwarded-for", "1.2.3.4"),
			("x-forwarded-for", "198.51.100.4"),
		]);
		assert_eq!(
			client_addr_with(&trusted, Some(ip("127.0.0.1")), &lines),
			Some(ip("198.51.100.4"))
		);
	}

	#[test]
	fn sec_cleanup_trusted_proxies_are_parsed_strictly() {
		assert!(TrustedProxies::parse("").unwrap().0.is_empty());
		assert_eq!(
			TrustedProxies::parse("10.0.0.1, 192.168.0.0/16\n::1")
				.unwrap()
				.0
				.len(),
			3
		);
		assert!(TrustedProxies::parse("10.0.0.1, nonsense").is_err());
		assert!(TrustedProxies::parse("10.0.0.0/33").is_err());
	}

	#[test]
	fn sec_cleanup_without_a_socket_the_header_is_all_there_is() {
		// In-process requests (tests) have no peer; a real server always has one.
		assert_eq!(
			client_addr_with(
				&TrustedProxies::default(),
				None,
				&headers(&[("x-forwarded-for", "198.51.100.4")])
			),
			Some(ip("198.51.100.4"))
		);
		assert_eq!(client_addr(None, &HeaderMap::new()), None);
	}
}
