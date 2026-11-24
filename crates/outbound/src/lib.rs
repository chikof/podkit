//! The one policy for every request the control plane (or an agent) makes to an address a
//! person typed: a notification webhook, an importer's instance, an uptime check, a git
//! provider, an identity provider, a backup target, a log backend.
//!
//! What it guarantees:
//!
//! - **Which addresses.** Loopback, link-local (the cloud metadata address included),
//!   unspecified, multicast, reserved and documentation ranges, and the control plane's own
//!   listen address are never reachable. Private ranges (RFC 1918, `fc00::/7`, CGNAT) are
//!   reachable only while the instance allows them. IPv4 addresses written as IPv6
//!   (`::ffff:127.0.0.1`, NAT64, 6to4) are judged as the IPv4 address they carry.
//! - **No rebinding.** A name is resolved once and the connection goes to the address that was
//!   vetted: the check lives in the resolver the HTTP client connects with, so there is no gap
//!   between "checked" and "used". An address written as a literal never reaches a resolver and
//!   is vetted before the request is built.
//! - **No redirects, no proxy.** A redirect is a new address nobody vetted, and a proxy would
//!   see the connection instead of the policy. A caller that wants to follow a redirect does it
//!   itself, one hop at a time, through [`Outbound::get`] again.
//! - **Nothing to learn from a refusal.** Every refusal is the same [`Denied`], whether the
//!   address is forbidden or the name does not resolve, so the message is no probe.

use std::future::Future;
use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, UdpSocket};
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use bytes::{Bytes, BytesMut};
use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use reqwest::{Method, RequestBuilder, Response};
use tokio::net::TcpStream;
use url::{Host, Url};

/// Connecting gives up after this long, whatever the caller's own timeout says.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// A request without a timeout of its own gives up after this long.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// What an address is, for the policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Class {
	/// Never reachable.
	Blocked,
	/// A private network: reachable only while the instance allows it.
	Private,
	/// Anything else.
	Public,
}

const fn v4(a: u8, b: u8, c: u8, d: u8) -> Ipv4Addr {
	Ipv4Addr::new(a, b, c, d)
}

fn in_v4(ip: Ipv4Addr, base: Ipv4Addr, prefix: u32) -> bool {
	let mask = u32::MAX.checked_shl(32 - prefix).unwrap_or(0);
	u32::from(ip) & mask == u32::from(base) & mask
}

/// Judges an IPv4 address.
#[must_use]
pub fn classify_v4(ip: Ipv4Addr) -> Class {
	const BLOCKED: &[(Ipv4Addr, u32)] = &[
		(v4(0, 0, 0, 0), 8),          // "this network", the unspecified address
		(v4(127, 0, 0, 0), 8),        // loopback
		(v4(169, 254, 0, 0), 16),     // link-local, the cloud metadata address
		(v4(192, 0, 0, 0), 24),       // IETF protocol assignments
		(v4(192, 0, 2, 0), 24),       // documentation (TEST-NET-1)
		(v4(192, 88, 99, 0), 24),     // the retired 6to4 relay anycast
		(v4(198, 18, 0, 0), 15),      // benchmarking
		(v4(198, 51, 100, 0), 24),    // documentation (TEST-NET-2)
		(v4(203, 0, 113, 0), 24),     // documentation (TEST-NET-3)
		(v4(224, 0, 0, 0), 4),        // multicast
		(v4(240, 0, 0, 0), 4),        // reserved, and the broadcast address
		(v4(100, 100, 100, 200), 32), // Alibaba's metadata service, inside CGNAT
	];
	const PRIVATE: &[(Ipv4Addr, u32)] = &[
		(v4(10, 0, 0, 0), 8),
		(v4(172, 16, 0, 0), 12),
		(v4(192, 168, 0, 0), 16),
		(v4(100, 64, 0, 0), 10), // CGNAT, and Tailscale
	];
	if BLOCKED.iter().any(|&(base, p)| in_v4(ip, base, p)) {
		Class::Blocked
	} else if PRIVATE.iter().any(|&(base, p)| in_v4(ip, base, p)) {
		Class::Private
	} else {
		Class::Public
	}
}

/// Judges an IPv6 address; one that carries an IPv4 address is judged as that address.
#[must_use]
pub fn classify_v6(ip: Ipv6Addr) -> Class {
	if let Some(inner) = ip.to_ipv4_mapped() {
		return classify_v4(inner);
	}
	let o = ip.octets();
	let seg = ip.segments();
	let embedded = |i: usize| Ipv4Addr::new(o[i], o[i + 1], o[i + 2], o[i + 3]);
	// AWS's IPv6 metadata address sits inside the unique-local range.
	if ip == Ipv6Addr::new(0xfd00, 0x0ec2, 0, 0, 0, 0, 0, 0x0254) {
		return Class::Blocked;
	}
	if ip.is_unspecified() || ip.is_loopback() || ip.is_multicast() {
		return Class::Blocked;
	}
	// ::a.b.c.d (the deprecated IPv4-compatible form) and 64:ff9b::/96 (NAT64) carry an IPv4 address.
	if seg[..6] == [0, 0, 0, 0, 0, 0] {
		// The IPv4-compatible form names no host anybody uses: only ever a way round the check.
		return Class::Blocked;
	}
	if seg[..6] == [0x64, 0xff9b, 0, 0, 0, 0] {
		return classify_v4(embedded(12));
	}
	// 6to4 carries one in bytes 2..6.
	if seg[0] == 0x2002 {
		return classify_v4(embedded(2));
	}
	let blocked = seg[0] & 0xffc0 == 0xfe80 // link-local
		|| seg[0] & 0xffc0 == 0xfec0 // site-local, deprecated
		|| (seg[0] == 0x2001 && seg[1] == 0x0db8) // documentation
		|| (seg[0] == 0x3fff && seg[1] & 0xf000 == 0) // documentation, 3fff::/20
		|| (seg[0] == 0x2001 && seg[1] == 0) // Teredo: a tunnel to anywhere
		|| (seg[0] == 0x0100 && seg[1] == 0 && seg[2] == 0 && seg[3] == 0); // discard-only
	if blocked {
		Class::Blocked
	} else if seg[0] & 0xfe00 == 0xfc00 {
		Class::Private
	} else {
		Class::Public
	}
}

/// Judges any address.
#[must_use]
pub fn classify(ip: IpAddr) -> Class {
	match ip {
		IpAddr::V4(v) => classify_v4(v),
		IpAddr::V6(v) => classify_v6(v),
	}
}

/// The address with an IPv4 address carried in IPv6 written as IPv4, so two spellings of one
/// address compare equal.
fn canonical(ip: IpAddr) -> IpAddr {
	match ip {
		IpAddr::V6(v) => v.to_ipv4_mapped().map_or(ip, IpAddr::V4),
		v4 @ IpAddr::V4(_) => v4,
	}
}

/// Whether `ip` is one of this machine's own addresses: a UDP socket "connected" to it (no
/// packet leaves) takes it as its own source address only when it is.
fn is_local(ip: IpAddr) -> bool {
	let bind = if ip.is_ipv4() { "0.0.0.0:0" } else { "[::]:0" };
	UdpSocket::bind(bind)
		.and_then(|s| s.connect((ip, 9)).map(|()| s))
		.and_then(|s| s.local_addr())
		.is_ok_and(|local| canonical(local.ip()) == canonical(ip))
}

/// The refusal. It says nothing about why: a forbidden address and a name that does not resolve
/// read the same, so it cannot be used to probe the network behind the control plane.
#[derive(Debug, Clone)]
pub struct Denied {
	reason: &'static str,
}

impl Denied {
	const fn new(reason: &'static str) -> Self {
		Self { reason }
	}

	/// Why, for the server's own log. Never for a person.
	#[must_use]
	pub const fn reason(&self) -> &'static str {
		self.reason
	}
}

impl std::fmt::Display for Denied {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.write_str("that address cannot be reached from here")
	}
}

impl std::error::Error for Denied {}

/// Why [`Outbound::connect`] failed.
#[derive(Debug)]
pub enum ConnectError {
	/// The policy refused the address.
	Denied(Denied),
	/// The address was allowed and the connection failed.
	Io(io::Error),
}

impl std::fmt::Display for ConnectError {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		match self {
			Self::Denied(d) => d.fmt(f),
			Self::Io(e) => e.fmt(f),
		}
	}
}

impl std::error::Error for ConnectError {}

/// Why [`read_capped`] stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BodyError {
	/// The body is longer than the cap.
	TooLarge,
	/// The connection broke while reading.
	Transport,
}

/// Reads at most `max` bytes of a response body.
///
/// # Errors
/// [`BodyError::TooLarge`] when the body (or its announced length) is over `max`.
pub async fn read_capped(mut response: Response, max: usize) -> Result<Bytes, BodyError> {
	if response
		.content_length()
		.is_some_and(|n| n > u64::try_from(max).unwrap_or(u64::MAX))
	{
		return Err(BodyError::TooLarge);
	}
	let mut body = BytesMut::new();
	while let Some(chunk) = response.chunk().await.map_err(|_| BodyError::Transport)? {
		if body.len() + chunk.len() > max {
			return Err(BodyError::TooLarge);
		}
		body.extend_from_slice(&chunk);
	}
	Ok(body.freeze())
}

/// How a name becomes addresses. The system's resolver in production; a test puts its own.
pub trait Lookup: Send + Sync + 'static {
	/// Resolves `host`.
	fn lookup<'a>(
		&'a self,
		host: &'a str,
	) -> Pin<Box<dyn Future<Output = io::Result<Vec<IpAddr>>> + Send + 'a>>;
}

struct System;

impl Lookup for System {
	fn lookup<'a>(
		&'a self,
		host: &'a str,
	) -> Pin<Box<dyn Future<Output = io::Result<Vec<IpAddr>>> + Send + 'a>> {
		Box::pin(async move {
			let found = tokio::net::lookup_host((host, 0)).await?;
			Ok(found.map(|a| a.ip()).collect())
		})
	}
}

/// The shared half: the settings, and the judgement. The resolver the client connects with holds
/// one too, so the setting is read at connect time.
struct Shared {
	allow_private: AtomicBool,
	/// Loopback allowed: a test reaching a stub on 127.0.0.1. Settable only with `test-util`.
	loopback_ok: AtomicBool,
	/// The control plane's own listen addresses.
	own: Vec<(IpAddr, u16)>,
	lookup: Arc<dyn Lookup>,
}

impl Shared {
	fn judge(&self, ip: IpAddr) -> Result<(), Denied> {
		let ip = canonical(ip);
		if self.loopback_ok.load(Ordering::Relaxed) && ip.is_loopback() {
			return Ok(());
		}
		match classify(ip) {
			Class::Blocked => Err(Denied::new("a reserved address")),
			Class::Private if !self.allow_private.load(Ordering::Relaxed) => Err(Denied::new(
				"a private address, and private addresses are off",
			)),
			Class::Private | Class::Public => Ok(()),
		}
	}

	/// Whether `ip:port` is the control plane itself.
	fn is_own(&self, ip: IpAddr, port: u16) -> bool {
		let ip = canonical(ip);
		self.own.iter().any(|&(own_ip, own_port)| {
			own_port == port
				&& if own_ip.is_unspecified() {
					is_local(ip)
				} else {
					canonical(own_ip) == ip
				}
		})
	}

	fn wants_own_check(&self, port: u16) -> bool {
		self.own.iter().any(|&(_, p)| p == port)
	}

	/// The vetted addresses of `host`: one resolution, every answer judged, one bad answer
	/// refuses all (a name that points at both a public and a forbidden address is a trick).
	async fn resolve(&self, host: &str) -> Result<Vec<IpAddr>, Denied> {
		let bare = host.trim_start_matches('[').trim_end_matches(']');
		let found = if let Ok(ip) = bare.parse::<IpAddr>() {
			vec![ip]
		} else {
			self.lookup
				.lookup(host)
				.await
				.map_err(|_| Denied::new("the name does not resolve"))?
		};
		if found.is_empty() {
			return Err(Denied::new("the name does not resolve"));
		}
		for ip in &found {
			self.judge(*ip)?;
		}
		Ok(found)
	}
}

/// The resolver of the shared client.
struct Vetting(Arc<Shared>);

impl Resolve for Vetting {
	fn resolve(&self, name: Name) -> Resolving {
		let shared = Arc::clone(&self.0);
		Box::pin(async move {
			let ips = shared.resolve(name.as_str()).await.map_err(|d| {
				tracing::warn!(reason = d.reason(), "outbound: a name was refused");
				Box::new(d) as Box<dyn std::error::Error + Send + Sync>
			})?;
			let addrs: Addrs = Box::new(ips.into_iter().map(|ip| SocketAddr::new(ip, 0)));
			Ok(addrs)
		})
	}
}

/// The policy and the HTTP client that obeys it. Cheap to clone.
#[derive(Clone)]
pub struct Outbound {
	shared: Arc<Shared>,
	client: reqwest::Client,
}

fn builder(shared: &Arc<Shared>) -> reqwest::ClientBuilder {
	reqwest::Client::builder()
		.redirect(reqwest::redirect::Policy::none())
		.no_proxy()
		.connect_timeout(CONNECT_TIMEOUT)
		.timeout(DEFAULT_TIMEOUT)
		.dns_resolver(Vetting(Arc::clone(shared)))
}

impl Outbound {
	/// A policy that resolves with the system's resolver. `own` are the control plane's listen
	/// addresses (an unspecified IP stands for every address of this machine).
	///
	/// # Errors
	/// The HTTP client cannot be built (no TLS provider installed).
	pub fn new(allow_private: bool, own: Vec<(IpAddr, u16)>) -> Result<Self, reqwest::Error> {
		Self::with_lookup(allow_private, own, Arc::new(System))
	}

	/// [`Outbound::new`] with a resolver of the caller's.
	///
	/// # Errors
	/// The HTTP client cannot be built.
	pub fn with_lookup(
		allow_private: bool,
		own: Vec<(IpAddr, u16)>,
		lookup: Arc<dyn Lookup>,
	) -> Result<Self, reqwest::Error> {
		let shared = Arc::new(Shared {
			allow_private: AtomicBool::new(allow_private),
			loopback_ok: AtomicBool::new(false),
			own,
			lookup,
		});
		let client = builder(&shared).build()?;
		Ok(Self { shared, client })
	}

	/// Whether private networks are reachable.
	#[must_use]
	pub fn allow_private(&self) -> bool {
		self.shared.allow_private.load(Ordering::Relaxed)
	}

	/// Changes whether private networks are reachable, for the requests that follow.
	pub fn set_allow_private(&self, allow: bool) {
		self.shared.allow_private.store(allow, Ordering::Relaxed);
	}

	/// Lets requests reach 127.0.0.1, so a test can run a stub there. Every other forbidden
	/// range stays forbidden.
	#[cfg(any(test, feature = "test-util"))]
	pub fn loopback_ok(&self, ok: bool) {
		self.shared.loopback_ok.store(ok, Ordering::Relaxed);
	}

	/// Vets `url` without sending anything: for the moment an address is saved. A name is
	/// resolved too, so an address that points somewhere forbidden is refused when it is made
	/// and not only when it is first used. Returns it parsed.
	///
	/// # Errors
	/// [`Denied`] for anything but an `http` or `https` address whose host is allowed.
	pub async fn vet_url(&self, url: &str) -> Result<Url, Denied> {
		let parsed = Url::parse(url.trim()).map_err(|_| Denied::new("not an address"))?;
		self.vet(&parsed).await?;
		if let (Some(Host::Domain(name)), Some(port)) =
			(parsed.host(), parsed.port_or_known_default())
		{
			self.resolve(name, port).await?;
		}
		Ok(parsed)
	}

	/// Vets an `http` or `https` URL; see [`Outbound::vet_url`].
	///
	/// # Errors
	/// [`Denied`].
	pub async fn vet(&self, url: &Url) -> Result<(), Denied> {
		if !matches!(url.scheme(), "http" | "https") {
			return Err(Denied::new("not an http address"));
		}
		let port = url.port_or_known_default().ok_or(Denied::new("no port"))?;
		match url.host() {
			None => Err(Denied::new("no host")),
			Some(Host::Ipv4(ip)) => self.vet_ip(IpAddr::V4(ip), port),
			Some(Host::Ipv6(ip)) => self.vet_ip(IpAddr::V6(ip), port),
			Some(Host::Domain(name)) => {
				// Only a name on the control plane's own port needs looking at here; the rest
				// is vetted by the resolver the connection itself uses.
				if self.shared.wants_own_check(port) {
					self.resolve(name, port).await?;
				}
				Ok(())
			}
		}
	}

	fn vet_ip(&self, ip: IpAddr, port: u16) -> Result<(), Denied> {
		self.shared.judge(ip)?;
		if self.shared.is_own(ip, port) {
			return Err(Denied::new("the control plane itself"));
		}
		Ok(())
	}

	/// Resolves `host` once and returns the vetted addresses, with `port`.
	///
	/// # Errors
	/// [`Denied`] when the name does not resolve or any answer is forbidden.
	pub async fn resolve(&self, host: &str, port: u16) -> Result<Vec<SocketAddr>, Denied> {
		let ips = self.shared.resolve(host).await?;
		if ips.iter().any(|ip| self.shared.is_own(*ip, port)) {
			return Err(Denied::new("the control plane itself"));
		}
		Ok(ips
			.into_iter()
			.map(|ip| SocketAddr::new(ip, port))
			.collect())
	}

	/// A request to `url`, vetted first. A name on the control plane's own port is resolved here,
	/// once, and the request connects to exactly those addresses.
	///
	/// # Errors
	/// [`Denied`].
	pub async fn request(&self, method: Method, url: &str) -> Result<RequestBuilder, Denied> {
		let parsed = Url::parse(url.trim()).map_err(|_| Denied::new("not an address"))?;
		self.request_url(method, parsed).await
	}

	/// [`Outbound::request`] for a parsed URL.
	///
	/// # Errors
	/// [`Denied`].
	pub async fn request_url(&self, method: Method, url: Url) -> Result<RequestBuilder, Denied> {
		if !matches!(url.scheme(), "http" | "https") {
			return Err(Denied::new("not an http address"));
		}
		let port = url.port_or_known_default().ok_or(Denied::new("no port"))?;
		match url.host() {
			None => Err(Denied::new("no host")),
			Some(Host::Ipv4(ip)) => {
				self.vet_ip(IpAddr::V4(ip), port)?;
				Ok(self.client.request(method, url))
			}
			Some(Host::Ipv6(ip)) => {
				self.vet_ip(IpAddr::V6(ip), port)?;
				Ok(self.client.request(method, url))
			}
			Some(Host::Domain(name)) if self.shared.wants_own_check(port) => {
				let addrs = self.resolve(name, port).await?;
				let pinned = builder(&self.shared)
					.resolve_to_addrs(name, &addrs)
					.build()
					.map_err(|_| Denied::new("no client"))?;
				Ok(pinned.request(method, url))
			}
			Some(Host::Domain(_)) => Ok(self.client.request(method, url)),
		}
	}

	/// A `GET`; see [`Outbound::request`].
	///
	/// # Errors
	/// [`Denied`].
	pub async fn get(&self, url: &str) -> Result<RequestBuilder, Denied> {
		self.request(Method::GET, url).await
	}

	/// A `POST`; see [`Outbound::request`].
	///
	/// # Errors
	/// [`Denied`].
	pub async fn post(&self, url: &str) -> Result<RequestBuilder, Denied> {
		self.request(Method::POST, url).await
	}

	/// The client, for code that builds requests in many places from one base address that was
	/// vetted with [`Outbound::vet_url`] first (a git provider). It follows no redirect and
	/// resolves through the policy; only an address written as a literal skips the check, which
	/// is why the base address must have been vetted.
	#[must_use]
	pub const fn vetted_client(&self) -> &reqwest::Client {
		&self.client
	}

	/// Opens a TCP connection to `host:port` (an SMTP relay, a certificate to read): resolves
	/// once, vets every answer and connects to the vetted addresses in turn.
	///
	/// # Errors
	/// [`ConnectError::Denied`] when the policy refuses, [`ConnectError::Io`] when no address
	/// answers within `timeout`.
	pub async fn connect(
		&self,
		host: &str,
		port: u16,
		timeout: Duration,
	) -> Result<TcpStream, ConnectError> {
		let addrs = self
			.resolve(host, port)
			.await
			.map_err(ConnectError::Denied)?;
		let mut last = io::Error::new(io::ErrorKind::NotFound, "no address");
		for addr in addrs {
			match tokio::time::timeout(timeout, TcpStream::connect(addr)).await {
				Ok(Ok(stream)) => return Ok(stream),
				Ok(Err(e)) => last = e,
				Err(_) => last = io::Error::new(io::ErrorKind::TimedOut, "timed out"),
			}
		}
		Err(ConnectError::Io(last))
	}
}

#[cfg(test)]
mod tests;
