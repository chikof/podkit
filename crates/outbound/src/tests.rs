use std::collections::VecDeque;
use std::sync::Mutex;
use std::sync::atomic::AtomicUsize;

use axum::Router;
use axum::http::{StatusCode, header};
use axum::routing::any;

use super::*;

fn provider() {
	let _ = rustls::crypto::ring::default_provider().install_default();
}

fn ip(s: &str) -> IpAddr {
	s.parse().unwrap()
}

#[test]
fn every_blocked_range_is_blocked() {
	for blocked in [
		// loopback, unspecified, "this network"
		"127.0.0.1",
		"127.255.255.254",
		"0.0.0.0",
		"0.1.2.3",
		"::1",
		"::",
		// link-local, the metadata addresses
		"169.254.169.254",
		"169.254.0.1",
		"fe80::1",
		"febf::1",
		"fd00:ec2::254",
		"100.100.100.200",
		// multicast, reserved, broadcast
		"224.0.0.1",
		"239.255.255.250",
		"240.0.0.1",
		"255.255.255.255",
		"ff02::1",
		// documentation, benchmarking, protocol assignments
		"192.0.2.1",
		"198.51.100.7",
		"203.0.113.9",
		"2001:db8::1",
		"198.18.0.1",
		"192.0.0.8",
		// IPv4 carried in IPv6
		"::ffff:127.0.0.1",
		"::ffff:169.254.169.254",
		"::ffff:0.0.0.0",
		"::127.0.0.1",
		"64:ff9b::7f00:1",
		"2002:7f00:1::1",
		"::ffff:7f00:1",
		// site-local, Teredo
		"fec0::1",
		"2001:0:4136:e378:8000:63bf:3fff:fdd2",
	] {
		assert_eq!(classify(ip(blocked)), Class::Blocked, "{blocked}");
	}
}

#[test]
fn the_private_ranges_are_private_and_the_rest_public() {
	for private in [
		"10.0.0.5",
		"10.255.255.255",
		"172.16.0.1",
		"172.31.255.255",
		"192.168.1.1",
		"100.64.0.1",
		"100.127.255.255",
		"fc00::1",
		"fd12:3456::1",
		"::ffff:10.0.0.1",
		"64:ff9b::a00:1",
	] {
		assert_eq!(classify(ip(private)), Class::Private, "{private}");
	}
	for public in [
		"8.8.8.8",
		"1.1.1.1",
		"172.15.0.1",
		"172.32.0.1",
		"100.63.255.255",
		"100.128.0.1",
		"192.169.0.1",
		"2606:4700:4700::1111",
		"::ffff:8.8.8.8",
	] {
		assert_eq!(classify(ip(public)), Class::Public, "{public}");
	}
}

/// A resolver that answers from a script and counts its calls.
struct Script {
	answers: Mutex<VecDeque<Vec<IpAddr>>>,
	calls: AtomicUsize,
}

impl Script {
	fn new(answers: &[&[&str]]) -> Arc<Self> {
		Arc::new(Self {
			answers: Mutex::new(
				answers
					.iter()
					.map(|a| a.iter().map(|s| ip(s)).collect())
					.collect(),
			),
			calls: AtomicUsize::new(0),
		})
	}
}

impl Lookup for Script {
	fn lookup<'a>(
		&'a self,
		_host: &'a str,
	) -> Pin<Box<dyn Future<Output = io::Result<Vec<IpAddr>>> + Send + 'a>> {
		self.calls.fetch_add(1, Ordering::SeqCst);
		let next = self.answers.lock().unwrap().pop_front();
		Box::pin(async move { next.ok_or_else(|| io::Error::other("no such name")) })
	}
}

fn policy(allow_private: bool, script: &Arc<Script>) -> Outbound {
	provider();
	Outbound::with_lookup(allow_private, Vec::new(), script.clone()).unwrap()
}

#[tokio::test]
async fn a_name_that_resolves_to_a_blocked_address_is_refused() {
	for target in ["127.0.0.1", "169.254.169.254", "::1", "::ffff:127.0.0.1"] {
		let script = Script::new(&[&[target]]);
		let out = policy(true, &script);
		assert!(
			out.resolve("innocent.example", 80).await.is_err(),
			"{target}"
		);
	}
	// One forbidden answer among good ones refuses the whole name.
	let script = Script::new(&[&["93.184.216.35", "127.0.0.1"]]);
	assert!(
		policy(true, &script)
			.resolve("mixed.example", 80)
			.await
			.is_err()
	);
	// A name that does not resolve is refused the same way.
	let script = Script::new(&[]);
	let err = policy(true, &script)
		.resolve("nothing.example", 80)
		.await
		.unwrap_err();
	assert_eq!(
		err.to_string(),
		Denied::new("a reserved address").to_string(),
		"a refusal says nothing about why"
	);
}

#[tokio::test]
async fn the_request_to_a_blocked_address_is_refused_before_it_is_built() {
	let script = Script::new(&[&["93.184.216.34"]]);
	let out = policy(true, &script);
	for url in [
		"http://127.0.0.1:2375/containers/prune",
		"http://169.254.169.254/latest/meta-data/",
		"http://[::1]:8080/",
		"http://[::ffff:127.0.0.1]/",
		"http://[::ffff:7f00:1]/",
		"http://2130706433/",
		"http://0x7f.1/",
		"http://0/",
		"ftp://example.com/",
		"file:///etc/passwd",
	] {
		assert!(out.get(url).await.is_err(), "{url}");
	}
	// A public literal goes through.
	assert!(out.get("http://93.184.216.34/").await.is_ok());
	assert_eq!(
		script.calls.load(Ordering::SeqCst),
		0,
		"a literal is no resolver's business"
	);
}

#[tokio::test]
async fn private_addresses_follow_the_setting() {
	let script = Script::new(&[&["192.168.1.20"], &["192.168.1.20"]]);
	let out = policy(false, &script);
	assert!(out.get("http://10.0.0.5/").await.is_err());
	assert!(out.get("http://192.168.1.20:8080/").await.is_err());
	assert!(out.get("http://[fd00::5]/").await.is_err());
	assert!(out.get("http://100.64.1.1/").await.is_err());
	assert!(out.resolve("ntfy.lan", 80).await.is_err());
	out.set_allow_private(true);
	assert!(out.allow_private());
	assert!(out.get("http://10.0.0.5/").await.is_ok());
	assert!(out.get("http://192.168.1.20:8080/").await.is_ok());
	assert!(out.get("http://[fd00::5]/").await.is_ok());
	assert!(out.get("http://100.64.1.1/").await.is_ok());
	assert_eq!(out.resolve("ntfy.lan", 80).await.unwrap().len(), 1);
	// Whatever the setting, the always-blocked stay blocked.
	assert!(out.get("http://127.0.0.1/").await.is_err());
	assert!(out.get("http://169.254.169.254/").await.is_err());
}

/// DNS rebinding: the name answers a public address first and a private one at the second
/// lookup. The connection must go to the first, and there must be no second lookup.
#[tokio::test]
async fn a_name_is_resolved_once_and_the_connection_goes_to_that_answer() {
	let script = Script::new(&[&["127.0.0.1"], &["10.0.0.1"]]);
	let out = policy(false, &script);
	out.loopback_ok(true);
	let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
	let port = listener.local_addr().unwrap().port();
	let accepted = tokio::spawn(async move { listener.accept().await.is_ok() });
	let stream = out
		.connect("rebind.example", port, Duration::from_secs(5))
		.await
		.expect("the first answer is the one that is used");
	assert_eq!(stream.peer_addr().unwrap().ip(), ip("127.0.0.1"));
	assert!(accepted.await.unwrap());
	assert_eq!(
		script.calls.load(Ordering::SeqCst),
		1,
		"one resolution, no second look"
	);
	// And what the second answer would have been is refused if it ever is asked for.
	assert!(out.resolve("rebind.example", port).await.is_err());
}

use tokio::net::TcpListener;

async fn stub(app: Router) -> u16 {
	let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
	let port = listener.local_addr().unwrap().port();
	tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
	port
}

#[tokio::test]
async fn a_redirect_is_not_followed() {
	let target_hits = Arc::new(AtomicUsize::new(0));
	let hits = Arc::clone(&target_hits);
	let target = stub(Router::new().fallback(any(move || {
		hits.fetch_add(1, Ordering::SeqCst);
		async { "reached" }
	})))
	.await;
	let front = stub(Router::new().fallback(any(move || async move {
		(
			StatusCode::FOUND,
			[(header::LOCATION, format!("http://127.0.0.1:{target}/inner"))],
		)
	})))
	.await;
	let script = Script::new(&[]);
	let out = policy(false, &script);
	out.loopback_ok(true);
	let response = out
		.get(&format!("http://127.0.0.1:{front}/"))
		.await
		.unwrap()
		.send()
		.await
		.unwrap();
	assert_eq!(response.status(), StatusCode::FOUND);
	assert_eq!(target_hits.load(Ordering::SeqCst), 0);
	// The caller that wants a hop vets it: this one points at the metadata address.
	assert!(out.get("http://169.254.169.254/latest").await.is_err());
}

#[tokio::test]
async fn read_capped_stops_at_the_cap() {
	let port = stub(Router::new().fallback(any(|| async { "x".repeat(5000) }))).await;
	let script = Script::new(&[]);
	let out = policy(false, &script);
	out.loopback_ok(true);
	let get = || async {
		out.get(&format!("http://127.0.0.1:{port}/"))
			.await
			.unwrap()
			.send()
			.await
			.unwrap()
	};
	assert_eq!(read_capped(get().await, 10_000).await.unwrap().len(), 5000);
	assert_eq!(
		read_capped(get().await, 4000).await.unwrap_err(),
		BodyError::TooLarge
	);
}

#[tokio::test]
async fn the_control_planes_own_address_is_refused() {
	provider();
	let script = Script::new(&[&["192.168.1.9"], &["192.168.1.9"]]);
	// Listening on one address: that address and port.
	let out = Outbound::with_lookup(true, vec![(ip("192.168.1.9"), 8080)], script.clone()).unwrap();
	assert!(out.get("http://192.168.1.9:8080/").await.is_err());
	assert!(out.get("http://192.168.1.9:8081/").await.is_ok());
	assert!(out.get("http://192.168.1.10:8080/").await.is_ok());
	// A name that resolves to it, on its port: refused; on another port it is fine.
	assert!(out.get("http://podkit.lan:8080/").await.is_err());
	assert!(out.get("http://podkit.lan:9000/").await.is_ok());
	// Listening on every address: this machine's addresses, whatever they are written as.
	let out = Outbound::with_lookup(true, vec![(ip("0.0.0.0"), 8080)], Script::new(&[])).unwrap();
	assert!(out.get("http://127.0.0.1:8080/").await.is_err());
}

#[tokio::test]
async fn the_machines_own_lan_address_is_the_control_plane_on_its_port() {
	provider();
	// The address this machine would use to reach the LAN, found the way `is_local` does.
	let probe = UdpSocket::bind("0.0.0.0:0").unwrap();
	if probe.connect(("192.168.255.254", 9)).is_err() {
		return;
	}
	let mine = probe.local_addr().unwrap().ip();
	if classify(mine) != Class::Private {
		return;
	}
	let out = Outbound::with_lookup(true, vec![(ip("0.0.0.0"), 8080)], Script::new(&[])).unwrap();
	assert!(out.get(&format!("http://{mine}:8080/")).await.is_err());
	assert!(out.get(&format!("http://{mine}:8081/")).await.is_ok());
}
