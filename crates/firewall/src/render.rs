//! A [`Policy`] as an nft script, and the same decision as plain code.
//!
//! The script is a full replacement of `inet podkit`: `add table` then `delete table` makes
//! it work whether or not the table exists, and `nft -f` applies the file atomically.
//! No line ever names another table or flushes the ruleset.
//!
//! Published ports: netavark DNATs a published port in `prerouting`, so the packet then goes
//! through the `forward` hook, never `input`. The `forward` chain here therefore matches
//! `ct status dnat` and the port as the outside saw it (`ct original proto-dst`).
//! nft: <https://wiki.nftables.org/wiki-nftables/index.php/Quick_reference-nftables_in_10_minutes>
//! and `man nft` (CONNTRACK EXPRESSIONS: `ct original proto-dst`, `ct status dnat`).

use std::fmt::Write as _;
use std::net::IpAddr;

use crate::policy::{Allow, Policy, Proto, Source};

/// Ports that are always open: HTTP and HTTPS for the ingress.
pub const DEFAULT_PORTS: [u16; 2] = [80, 443];

/// The lines that make an apply idempotent.
pub const PRELUDE: &str = "add table inet podkit\ndelete table inet podkit\n";

/// What removes the table, whether or not it exists.
#[must_use]
pub fn delete_script() -> &'static str {
	PRELUDE
}

fn saddr(source: Source) -> String {
	if source.is_v6() {
		format!("ip6 saddr {source}")
	} else {
		format!("ip saddr {source}")
	}
}

fn host(ip: IpAddr) -> Source {
	// An address always parses as a range of one.
	Source(proto::cidr::Cidr::parse(&ip.to_string()).unwrap_or_else(|_| unreachable!()))
}

/// The ranges SSH is open to: nothing when it is open to anyone, else the configured ones
/// plus every address in `keep` (the sources of the SSH sessions open right now).
fn ssh_sources(policy: &Policy, keep: &[IpAddr]) -> Vec<Source> {
	if policy.ssh_sources.is_empty() {
		return Vec::new();
	}
	let mut all = policy.ssh_sources.clone();
	for ip in keep {
		let s = host(*ip);
		if !all.iter().any(|a| a.0.contains(s.0)) {
			all.push(s);
		}
	}
	all
}

fn push_allow(out: &mut String, prefix: &str, allow: &Allow) {
	let from = allow
		.source
		.map(|s| format!("{} ", saddr(s)))
		.unwrap_or_default();
	let _ = writeln!(
		out,
		"\t\t{prefix}{from}{} dport {} accept",
		allow.proto.as_str(),
		allow.port
	);
}

fn push_forward_allow(out: &mut String, allow: &Allow) {
	let from = allow
		.source
		.map(|s| format!("{} ", saddr(s)))
		.unwrap_or_default();
	let _ = writeln!(
		out,
		"\t\tct status dnat {from}meta l4proto {} ct original proto-dst {} accept",
		allow.proto.as_str(),
		allow.port
	);
}

/// The nft script for `policy`. `keep` are the sources of the SSH sessions open now; they
/// stay allowed when SSH is limited to some ranges.
#[must_use]
pub fn render(policy: &Policy, keep: &[IpAddr]) -> String {
	let mut out = String::from(PRELUDE);
	out.push_str("table inet podkit {\n");

	out.push_str("\tchain input {\n");
	out.push_str("\t\ttype filter hook input priority filter; policy drop;\n");
	out.push_str("\t\tct state established,related accept\n");
	out.push_str("\t\tct state invalid drop\n");
	out.push_str("\t\tiifname \"lo\" accept\n");
	out.push_str("\t\tiifname \"podman*\" accept\n");
	out.push_str("\t\tip protocol icmp accept\n");
	out.push_str("\t\tmeta l4proto ipv6-icmp accept\n");
	let sources = ssh_sources(policy, keep);
	if sources.is_empty() {
		let _ = writeln!(out, "\t\ttcp dport {} accept", policy.ssh_port);
	} else {
		for s in sources {
			let _ = writeln!(out, "\t\t{} tcp dport {} accept", saddr(s), policy.ssh_port);
		}
	}
	let _ = writeln!(out, "\t\ttcp dport {{ 80, 443 }} accept");
	let mut seen = Vec::new();
	for allow in &policy.allows {
		if !seen.contains(&allow) {
			push_allow(&mut out, "", allow);
			seen.push(allow);
		}
	}
	out.push_str("\t}\n");

	out.push_str("\tchain forward {\n");
	out.push_str("\t\ttype filter hook forward priority filter; policy accept;\n");
	out.push_str("\t\tct state established,related accept\n");
	out.push_str("\t\tiifname \"lo\" accept\n");
	// Containers reaching each other, or a published port through the host's address.
	out.push_str("\t\tiifname \"podman*\" accept\n");
	out.push_str("\t\tct status dnat meta l4proto tcp ct original proto-dst { 80, 443 } accept\n");
	for allow in &seen {
		push_forward_allow(&mut out, allow);
	}
	out.push_str("\t\tct status dnat drop\n");
	out.push_str("\t}\n");

	out.push_str("}\n");
	out
}

/// Whether a new inbound connection from `src` to `port` is let through `input`; the
/// decision [`render`] writes as rules. Tests use it to hold the SSH invariant.
#[must_use]
pub fn allows_input(
	policy: &Policy,
	keep: &[IpAddr],
	src: IpAddr,
	proto: Proto,
	port: u16,
) -> bool {
	if proto == Proto::Tcp {
		if port == policy.ssh_port {
			let sources = ssh_sources(policy, keep);
			if sources.is_empty() || sources.iter().any(|s| s.0.contains_ip(src)) {
				return true;
			}
		}
		if DEFAULT_PORTS.contains(&port) {
			return true;
		}
	}
	policy.allows.iter().any(|a| {
		a.proto == proto && a.port == port && a.source.is_none_or(|s| s.0.contains_ip(src))
	})
}
