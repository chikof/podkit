//! The probes: one HTTP(S) request, one TCP connect and a look at a server's certificate.
//!
//! An outcome's error is a short plain phrase that goes into the history and the private views.
//! It never carries the URL, the host or a response body: whatever the check points at stays
//! with the people who set it up.

use std::sync::Arc;
use std::time::{Duration, Instant};

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{CryptoProvider, ring};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{ClientConfig, DigitallySignedStruct, SignatureScheme};
use time::{Date, Month, OffsetDateTime, PrimitiveDateTime, Time};
use tokio_rustls::TlsConnector;

use crate::net::outbound::{ConnectError, Outbound};

/// A redirect is followed this many times.
const MAX_REDIRECTS: usize = 3;
const USER_AGENT: &str = concat!("podkit-uptime/", env!("CARGO_PKG_VERSION"));

/// The most of a response body read to look for a keyword.
const BODY_LIMIT: usize = 1024 * 1024;

/// What one run found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
	pub ok: bool,
	pub latency_ms: Option<i32>,
	pub status_code: Option<i32>,
	pub error: Option<String>,
}

impl Outcome {
	fn good(started: Instant, status: Option<u16>) -> Self {
		Self {
			ok: true,
			latency_ms: Some(millis(started)),
			status_code: status.map(i32::from),
			error: None,
		}
	}

	fn bad(status: Option<u16>, error: impl Into<String>) -> Self {
		Self {
			ok: false,
			latency_ms: None,
			status_code: status.map(i32::from),
			error: Some(error.into()),
		}
	}
}

fn millis(started: Instant) -> i32 {
	i32::try_from(started.elapsed().as_millis()).unwrap_or(i32::MAX)
}

/// What an HTTP check asks for.
#[derive(Debug, Clone)]
pub struct HttpSpec {
	pub url: String,
	pub head: bool,
	pub expected_status: u16,
	pub keyword: Option<String>,
	pub timeout: Duration,
}

/// Sends the request and judges the answer: the status must be the expected one and, when a
/// keyword is set, the first megabyte of the body must contain it.
///
/// The address goes through the outbound policy. A redirect is followed here, by hand, at most
/// [`MAX_REDIRECTS`] times, and each hop is vetted like the first request: a public address that
/// redirects to the metadata address is refused at the second request.
pub async fn http(outbound: &Outbound, spec: &HttpSpec) -> Outcome {
	let started = Instant::now();
	let method = if spec.head {
		reqwest::Method::HEAD
	} else {
		reqwest::Method::GET
	};
	let Ok(mut url) = url::Url::parse(&spec.url) else {
		return Outcome::bad(None, "the request failed");
	};
	let mut hops = 0;
	let response = loop {
		let request = match outbound.request_url(method.clone(), url.clone()).await {
			Ok(r) => r,
			Err(denied) => {
				tracing::debug!(reason = denied.reason(), "uptime: the address was refused");
				return Outcome::bad(None, format!("cannot connect: {denied}"));
			}
		};
		let response = match request
			.header(reqwest::header::USER_AGENT, USER_AGENT)
			.timeout(spec.timeout)
			.send()
			.await
		{
			Ok(r) => r,
			Err(e) => return Outcome::bad(None, describe(&e, spec.timeout)),
		};
		let next = response
			.status()
			.is_redirection()
			.then(|| response.headers().get(reqwest::header::LOCATION))
			.flatten()
			.and_then(|v| v.to_str().ok())
			.and_then(|l| url.join(l).ok());
		match next {
			Some(next) => {
				hops += 1;
				if hops > MAX_REDIRECTS {
					return Outcome::bad(None, "too many redirects");
				}
				url = next;
			}
			None => break response,
		}
	};
	let status = response.status().as_u16();
	if spec.expected_status == 0 && status >= 400 {
		return Outcome::bad(Some(status), format!("answered {status}"));
	}
	if spec.expected_status != 0 && status != spec.expected_status {
		return Outcome::bad(
			Some(status),
			format!("answered {status}, expected {}", spec.expected_status),
		);
	}
	let Some(keyword) = spec.keyword.as_deref().filter(|k| !k.is_empty()) else {
		return Outcome::good(started, Some(status));
	};
	let mut response = response;
	let mut body: Vec<u8> = Vec::new();
	loop {
		match response.chunk().await {
			Ok(Some(chunk)) => {
				body.extend_from_slice(&chunk);
				if String::from_utf8_lossy(&body).contains(keyword) {
					return Outcome::good(started, Some(status));
				}
				if body.len() >= BODY_LIMIT {
					break;
				}
			}
			Ok(None) => break,
			Err(e) => return Outcome::bad(Some(status), describe(&e, spec.timeout)),
		}
	}
	Outcome::bad(Some(status), "the keyword is not in the response")
}

fn describe(e: &reqwest::Error, timeout: Duration) -> String {
	if e.is_timeout() {
		format!("no answer within {} s", timeout.as_secs())
	} else if e.is_connect() {
		// The chain holds the cause ("connection refused", a certificate problem, DNS) without the URL.
		let mut cause = String::new();
		let mut source = std::error::Error::source(e);
		while let Some(s) = source {
			cause = s.to_string();
			source = s.source();
		}
		if cause.is_empty() {
			"cannot connect".to_owned()
		} else {
			format!(
				"cannot connect: {}",
				crate::notifications::event::clip(&cause, 120)
			)
		}
	} else if e.is_redirect() {
		"too many redirects".to_owned()
	} else {
		"the request failed".to_owned()
	}
}

/// Opens a TCP connection and closes it again. The address goes through the outbound policy.
pub async fn tcp(outbound: &Outbound, host: &str, port: u16, timeout: Duration) -> Outcome {
	let started = Instant::now();
	match outbound.connect(host, port, timeout).await {
		Ok(_) => Outcome::good(started, None),
		Err(ConnectError::Denied(denied)) => {
			tracing::debug!(reason = denied.reason(), "uptime: the address was refused");
			Outcome::bad(None, format!("cannot connect: {denied}"))
		}
		Err(ConnectError::Io(e)) if e.kind() == std::io::ErrorKind::TimedOut => {
			Outcome::bad(None, format!("no answer within {} s", timeout.as_secs()))
		}
		Err(ConnectError::Io(e)) => {
			Outcome::bad(None, format!("cannot connect: {}", io_reason(&e)))
		}
	}
}

fn io_reason(e: &std::io::Error) -> String {
	match e.kind() {
		std::io::ErrorKind::ConnectionRefused => "connection refused".to_owned(),
		std::io::ErrorKind::TimedOut => "timed out".to_owned(),
		_ => crate::notifications::event::clip(&e.to_string(), 80),
	}
}

// --- certificate expiry ---------------------------------------------------------------------

/// Takes any certificate: the point is to read its date, not to trust it. Whether the site is
/// trusted is the HTTP check's business, which fails on an invalid certificate by itself.
#[derive(Debug)]
struct AcceptAny(Arc<CryptoProvider>);

impl ServerCertVerifier for AcceptAny {
	fn verify_server_cert(
		&self,
		_end_entity: &CertificateDer<'_>,
		_intermediates: &[CertificateDer<'_>],
		_server_name: &ServerName<'_>,
		_ocsp: &[u8],
		_now: UnixTime,
	) -> Result<ServerCertVerified, rustls::Error> {
		Ok(ServerCertVerified::assertion())
	}

	fn verify_tls12_signature(
		&self,
		_message: &[u8],
		_cert: &CertificateDer<'_>,
		_dss: &DigitallySignedStruct,
	) -> Result<HandshakeSignatureValid, rustls::Error> {
		Ok(HandshakeSignatureValid::assertion())
	}

	fn verify_tls13_signature(
		&self,
		_message: &[u8],
		_cert: &CertificateDer<'_>,
		_dss: &DigitallySignedStruct,
	) -> Result<HandshakeSignatureValid, rustls::Error> {
		Ok(HandshakeSignatureValid::assertion())
	}

	fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
		self.0.signature_verification_algorithms.supported_schemes()
	}
}

/// The `notAfter` of the certificate `host:port` serves.
///
/// # Errors
/// A short phrase: the host does not answer, does not speak TLS or sent something unreadable.
pub async fn tls_expiry(
	outbound: &Outbound,
	host: &str,
	port: u16,
	timeout: Duration,
) -> Result<OffsetDateTime, String> {
	let provider = Arc::new(ring::default_provider());
	let config = ClientConfig::builder_with_provider(provider.clone())
		.with_safe_default_protocol_versions()
		.map_err(|_| "no TLS version".to_owned())?
		.dangerous()
		.with_custom_certificate_verifier(Arc::new(AcceptAny(provider)))
		.with_no_client_auth();
	let name = ServerName::try_from(host.to_owned()).map_err(|_| "invalid host name".to_owned())?;
	let connect = async {
		let tcp = outbound
			.connect(host, port, timeout)
			.await
			.map_err(|e| match e {
				ConnectError::Denied(d) => d.to_string(),
				ConnectError::Io(e) => io_reason(&e),
			})?;
		TlsConnector::from(Arc::new(config))
			.connect(name, tcp)
			.await
			.map_err(|_| "the TLS handshake failed".to_owned())
	};
	let stream = tokio::time::timeout(timeout, connect)
		.await
		.map_err(|_| "no answer".to_owned())??;
	let (_, session) = stream.get_ref();
	let cert = session
		.peer_certificates()
		.and_then(|c| c.first())
		.ok_or("no certificate")?;
	not_after(cert.as_ref()).ok_or_else(|| "unreadable certificate".to_owned())
}

/// Whole days from `now` until `at`; negative once it has passed.
pub fn days_left(at: OffsetDateTime, now: OffsetDateTime) -> i64 {
	(at - now).whole_days()
}

/// One DER element: its tag, its content and what follows it.
fn element(d: &[u8]) -> Option<(u8, &[u8], &[u8])> {
	let (&tag, rest) = d.split_first()?;
	let (&first, rest) = rest.split_first()?;
	let (len, rest) = if first < 0x80 {
		(usize::from(first), rest)
	} else {
		let n = usize::from(first & 0x7f);
		if n == 0 || n > 4 || rest.len() < n {
			return None;
		}
		let len = rest[..n]
			.iter()
			.fold(0usize, |acc, b| (acc << 8) | usize::from(*b));
		(len, &rest[n..])
	};
	if rest.len() < len {
		return None;
	}
	Some((tag, &rest[..len], &rest[len..]))
}

/// The end of the validity of a DER X.509 certificate (RFC 5280, section 4.1):
/// `Certificate ::= SEQUENCE { tbsCertificate, ... }` and
/// `TBSCertificate ::= SEQUENCE { [0] version?, serial, signature, issuer, validity, ... }`.
pub fn not_after(der: &[u8]) -> Option<OffsetDateTime> {
	let (_, cert, _) = element(der)?;
	let (_, tbs, _) = element(cert)?;
	let (tag, _, after) = element(tbs)?;
	// An explicit version comes first; the serial number is the next element either way.
	let rest = if tag == 0xA0 {
		element(after)?.2
	} else {
		after
	};
	let (_, _, rest) = element(rest)?; // signature algorithm
	let (_, _, rest) = element(rest)?; // issuer
	let (_, validity, _) = element(rest)?;
	let (_, _, after_start) = element(validity)?;
	let (tag, text, _) = element(after_start)?;
	parse_time(tag, text)
}

/// `UTCTime` (`YYMMDDHHMMSSZ`, a two-digit year below 50 is 20xx) or `GeneralizedTime`
/// (`YYYYMMDDHHMMSSZ`).
fn parse_time(tag: u8, text: &[u8]) -> Option<OffsetDateTime> {
	let text = std::str::from_utf8(text).ok()?.strip_suffix('Z')?;
	let (year, rest) = match tag {
		0x17 if text.len() == 12 => {
			let yy: i32 = text[..2].parse().ok()?;
			(if yy < 50 { 2000 + yy } else { 1900 + yy }, &text[2..])
		}
		0x18 if text.len() == 14 => (text[..4].parse().ok()?, &text[4..]),
		_ => return None,
	};
	let part = |i: usize| rest.get(i..i + 2)?.parse::<u8>().ok();
	let date = Date::from_calendar_date(year, Month::try_from(part(0)?).ok()?, part(2)?).ok()?;
	let time = Time::from_hms(part(4)?, part(6)?, part(8)?).ok()?;
	Some(PrimitiveDateTime::new(date, time).assume_utc())
}
