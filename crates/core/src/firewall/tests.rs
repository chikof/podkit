//! Server firewall tests: what the form may ask for, the policy the helper is given, the apply
//! with its confirm window (against the helper's real engine on a fake host), the revert when
//! podkit cannot reach the server again, permissions, another team's server, and escaping.
//!
//! `cargo test -p podkit-core -p podkit-ui --all-features -- firewall`

use super::{Rule, RuleInput, Settings, parse_ssh_sources, policy};
use crate::error::ServerError;
use crate::ids::FirewallRuleId;

// --- the form ------------------------------------------------------------------------------

fn input(proto: &str, port: &str, source: &str, note: &str) -> RuleInput {
	RuleInput {
		proto: proto.to_owned(),
		port: port.to_owned(),
		source: source.to_owned(),
		note: note.to_owned(),
	}
}

#[test]
fn firewall_an_allow_is_checked_and_its_range_normalized() {
	let ok = input("udp", " 51820 ", "10.0.0.7/24", " wireguard ")
		.parse()
		.unwrap();
	assert_eq!(ok.port, 51820);
	assert_eq!(ok.source.as_deref(), Some("10.0.0.0/24"));
	assert_eq!(ok.note, "wireguard");
	// A bare protocol field is TCP, an empty source is anyone.
	let any = input("", "5432", "", "").parse().unwrap();
	assert_eq!(any.proto, podkit_firewall::policy::Proto::Tcp);
	assert_eq!(any.source, None);
	for bad in [
		input("icmp", "1", "", ""),
		input("tcp", "0", "", ""),
		input("tcp", "65536", "", ""),
		input("tcp", "http", "", ""),
		input("tcp", "80", "10.0.0.0/33", ""),
		input("tcp", "80", "not-an-address", ""),
		input("tcp", "80", "", &"x".repeat(81)),
		input("tcp", "80", "", "a\nb"),
	] {
		assert!(
			matches!(bad.parse(), Err(ServerError::Validation(_))),
			"{bad:?}"
		);
	}
}

#[test]
fn firewall_ssh_ranges_are_deduplicated_and_must_parse() {
	assert_eq!(
		parse_ssh_sources("10.0.0.7/24, 203.0.113.9\n10.0.0.0/24").unwrap(),
		["10.0.0.0/24", "203.0.113.9/32"]
	);
	assert!(parse_ssh_sources("").unwrap().is_empty());
	assert!(parse_ssh_sources("10.0.0.0/8 nope").is_err());
}

#[test]
fn firewall_the_stored_rows_become_the_helpers_policy() {
	let rows = vec![
		Rule {
			id: FirewallRuleId(1),
			proto: "tcp".to_owned(),
			port: 5432,
			source: Some("10.0.0.0/24".to_owned()),
			note: String::new(),
		},
		Rule {
			id: FirewallRuleId(2),
			proto: "udp".to_owned(),
			port: 51820,
			source: None,
			note: String::new(),
		},
	];
	let settings = Settings {
		ssh_sources: vec!["203.0.113.0/24".to_owned()],
		..Settings::default()
	};
	let p = policy(2222, &settings, &rows).unwrap();
	assert_eq!(p.ssh_port, 2222);
	assert_eq!(p.ssh_sources.len(), 1);
	assert_eq!(p.allows.len(), 2);
	assert_eq!(p.allows[1].proto.as_str(), "udp");
	p.validate().unwrap();
	assert!(policy(0, &settings, &rows).unwrap().validate().is_err());
}
