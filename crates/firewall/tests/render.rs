//! Ruleset rendering: a snapshot, and the invariant that SSH is never locked out.

use std::net::IpAddr;

use podkit_firewall::policy::{Allow, Policy, Proto, Source};
use podkit_firewall::render::{allows_input, render};

fn src(s: &str) -> Source {
	Source::try_from(s.to_owned()).unwrap()
}

fn ip(s: &str) -> IpAddr {
	s.parse().unwrap()
}

/// Compares with `tests/snapshots/<name>.nft`; `UPDATE_SNAPSHOTS=1` rewrites it.
fn snapshot(name: &str, actual: &str) {
	let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
		.join("tests/snapshots")
		.join(format!("{name}.nft"));
	if std::env::var_os("UPDATE_SNAPSHOTS").is_some() {
		std::fs::write(&path, actual).unwrap();
	}
	let expected = std::fs::read_to_string(&path).unwrap();
	assert_eq!(actual, expected, "snapshot {name} differs");
}

#[test]
fn default_policy_snapshot() {
	snapshot("default", &render(&Policy::default(), &[]));
}

#[test]
fn full_policy_snapshot() {
	let policy = Policy {
		ssh_port: 2222,
		ssh_sources: vec![src("203.0.113.0/24"), src("2001:db8::/32")],
		allows: vec![
			Allow {
				proto: Proto::Tcp,
				port: 5432,
				source: Some(src("10.0.0.0/8")),
			},
			Allow {
				proto: Proto::Udp,
				port: 51820,
				source: None,
			},
			Allow {
				proto: Proto::Tcp,
				port: 6379,
				source: Some(src("2001:db8:1::/48")),
			},
			// A duplicate renders once.
			Allow {
				proto: Proto::Udp,
				port: 51820,
				source: None,
			},
		],
	};
	snapshot("full", &render(&policy, &[ip("198.51.100.7")]));
}

#[test]
fn only_the_podkit_table_is_touched() {
	let policy = Policy {
		allows: vec![Allow {
			proto: Proto::Tcp,
			port: 9,
			source: None,
		}],
		..Policy::default()
	};
	let script = render(&policy, &[]);
	for line in script.lines() {
		assert!(!line.contains("flush"), "{line}");
		assert!(!line.contains("ruleset"), "{line}");
		if line.contains("table") {
			assert!(line.contains("inet podkit"), "{line}");
		}
	}
	assert!(script.starts_with("add table inet podkit\ndelete table inet podkit\n"));
	assert_eq!(script.matches("table inet podkit {").count(), 1);
}

#[test]
fn container_to_host_traffic_is_accepted_in_input() {
	// aardvark-dns on the bridge gateway and host.containers.internal arrive via input.
	let script = render(&Policy::default(), &[]);
	let input = script.split("chain input").nth(1).unwrap();
	let input = input.split("chain forward").next().unwrap();
	assert!(input.contains("iifname \"podman*\" accept"));
}

#[test]
fn published_ports_are_filtered_in_forward() {
	let policy = Policy {
		allows: vec![Allow {
			proto: Proto::Tcp,
			port: 5432,
			source: Some(src("10.0.0.0/8")),
		}],
		..Policy::default()
	};
	let script = render(&policy, &[]);
	let forward = script.split("chain forward").nth(1).unwrap();
	assert!(forward.contains(
		"ct status dnat ip saddr 10.0.0.0/8 meta l4proto tcp ct original proto-dst 5432 accept"
	));
	assert!(forward.contains("ct status dnat drop"));
	// Container-to-container traffic and answers are not touched.
	assert!(forward.contains("iifname \"podman*\" accept"));
	assert!(forward.contains("ct state established,related accept"));
}

/// A small deterministic generator, so the property test needs no extra crate.
struct Rng(u64);

impl Rng {
	fn next(&mut self) -> u64 {
		self.0 ^= self.0 << 13;
		self.0 ^= self.0 >> 7;
		self.0 ^= self.0 << 17;
		self.0
	}

	fn below(&mut self, n: u64) -> u64 {
		self.next() % n
	}

	fn ip(&mut self) -> IpAddr {
		if self.below(4) == 0 {
			let mut b = [0u8; 16];
			for x in &mut b {
				*x = u8::try_from(self.below(256)).unwrap();
			}
			IpAddr::from(b)
		} else {
			IpAddr::from([
				u8::try_from(self.below(256)).unwrap(),
				u8::try_from(self.below(256)).unwrap(),
				u8::try_from(self.below(256)).unwrap(),
				u8::try_from(self.below(256)).unwrap(),
			])
		}
	}

	fn source(&mut self) -> Source {
		let ip = self.ip();
		let bits = if ip.is_ipv4() { 32 } else { 128 };
		src(&format!("{ip}/{}", self.below(bits + 1)))
	}

	fn port(&mut self) -> u16 {
		u16::try_from(1 + self.below(65535)).unwrap()
	}

	fn policy(&mut self) -> Policy {
		let ssh_sources = (0..self.below(4)).map(|_| self.source()).collect();
		let allows = (0..self.below(8))
			.map(|_| Allow {
				proto: if self.below(2) == 0 {
					Proto::Tcp
				} else {
					Proto::Udp
				},
				port: self.port(),
				source: (self.below(2) == 0).then(|| self.source()),
			})
			.collect();
		Policy {
			ssh_port: if self.below(3) == 0 { 22 } else { self.port() },
			ssh_sources,
			allows,
		}
	}
}

fn saddr_text(ip: IpAddr) -> String {
	match ip {
		IpAddr::V4(_) => format!("ip saddr {ip}/32"),
		IpAddr::V6(_) => format!("ip6 saddr {ip}/128"),
	}
}

/// Whatever the policy says, the sources of the SSH sessions open now (and every configured
/// SSH source) can reach the SSH port: both in the decision function and as a rule in the
/// script. Not for 'any policy', since that is the property under test, but for 3000 random
/// ones.
#[test]
fn rendered_rules_always_allow_the_ssh_source() {
	let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
	for case in 0..3000 {
		let policy = rng.policy();
		policy.validate().unwrap();
		let keep: Vec<IpAddr> = (0..=rng.below(3)).map(|_| rng.ip()).collect();
		let script = render(&policy, &keep);

		for k in &keep {
			assert!(
				allows_input(&policy, &keep, *k, Proto::Tcp, policy.ssh_port),
				"case {case}: {k} locked out by {policy:?}"
			);
			// In the script: an own rule, or a configured range holding it, or SSH is open
			// to anyone.
			let own = format!("{} tcp dport {} accept", saddr_text(*k), policy.ssh_port);
			let covered = policy.ssh_sources.is_empty()
				|| policy.ssh_sources.iter().any(|s| s.0.contains_ip(*k));
			assert!(
				covered || script.contains(&own),
				"case {case}: no rule for {k} in\n{script}"
			);
		}
		for s in &policy.ssh_sources {
			let line = if s.is_v6() {
				format!("ip6 saddr {s} tcp dport {} accept", policy.ssh_port)
			} else {
				format!("ip saddr {s} tcp dport {} accept", policy.ssh_port)
			};
			assert!(script.contains(&line), "case {case}: {line} missing");
		}
		if policy.ssh_sources.is_empty() {
			assert!(script.contains(&format!("\t\ttcp dport {} accept\n", policy.ssh_port)));
		}
		// Nothing a policy can say drops SSH before the accept: the only drop in the input
		// chain is for invalid conntrack state, which a new connection never is.
		let input = script
			.split("chain input")
			.nth(1)
			.unwrap()
			.split("chain forward")
			.next()
			.unwrap();
		let drops: Vec<&str> = input.lines().filter(|l| l.contains(" drop")).collect();
		assert_eq!(drops.len(), 2, "case {case}: unexpected drops {drops:?}");
	}
}

#[test]
fn the_default_ports_and_nothing_else_are_open_by_default() {
	let policy = Policy::default();
	let any = ip("192.0.2.1");
	assert!(allows_input(&policy, &[], any, Proto::Tcp, 22));
	assert!(allows_input(&policy, &[], any, Proto::Tcp, 80));
	assert!(allows_input(&policy, &[], any, Proto::Tcp, 443));
	assert!(!allows_input(&policy, &[], any, Proto::Udp, 443));
	assert!(!allows_input(&policy, &[], any, Proto::Tcp, 5432));
}

#[test]
fn a_limited_ssh_still_lets_the_current_source_in() {
	let policy = Policy {
		ssh_sources: vec![src("10.0.0.0/8")],
		..Policy::default()
	};
	assert!(!allows_input(
		&policy,
		&[],
		ip("203.0.113.9"),
		Proto::Tcp,
		22
	));
	assert!(allows_input(
		&policy,
		&[ip("203.0.113.9")],
		ip("203.0.113.9"),
		Proto::Tcp,
		22
	));
	assert!(allows_input(&policy, &[], ip("10.1.2.3"), Proto::Tcp, 22));
}
