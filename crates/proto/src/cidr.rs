//! IP ranges for a route's access rules.
//!
//! Traefik has an IP allow list but no deny list (see the middleware overview,
//! <https://doc.traefik.io/traefik/master/reference/routing-configuration/http/middlewares/overview/>),
//! so a deny list is turned into the ranges that remain: [`effective_allow`] subtracts the
//! denied ranges from what is allowed (everything when nothing is), and the result is the
//! allow list Traefik gets.

use std::fmt;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// An IPv4 or IPv6 range. The address is always the first one of the range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cidr {
	/// The first address, as a number.
	addr: u128,
	/// Leading bits that are fixed.
	prefix: u8,
	/// IPv6 when set.
	v6: bool,
}

impl Cidr {
	/// Parses `a.b.c.d`, `a.b.c.d/n`, an IPv6 address or `addr/n`. A bare address is a range
	/// of one; host bits below the prefix are cleared (`10.0.0.7/24` is `10.0.0.0/24`).
	///
	/// # Errors
	/// A message naming `raw` when it is not an address, or the prefix is out of range.
	pub fn parse(raw: &str) -> Result<Self, String> {
		let raw = raw.trim();
		let bad = || format!("{raw:?} is not an IP address or a range like 10.0.0.0/24");
		let (ip, prefix) = match raw.split_once('/') {
			Some((ip, prefix)) => (ip, Some(prefix)),
			None => (raw, None),
		};
		let ip: IpAddr = ip.parse().map_err(|_| bad())?;
		let (bits, v6, addr) = match ip {
			IpAddr::V4(a) => (32u8, false, u128::from(u32::from(a))),
			IpAddr::V6(a) => (128u8, true, u128::from(a)),
		};
		let prefix = match prefix {
			None => bits,
			Some(p) => {
				if p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit()) {
					return Err(bad());
				}
				p.parse::<u8>().map_err(|_| bad())?
			}
		};
		if prefix > bits {
			return Err(bad());
		}
		Ok(Self {
			addr: addr & mask(bits, prefix),
			prefix,
			v6,
		})
	}

	fn bits(self) -> u8 {
		if self.v6 { 128 } else { 32 }
	}

	/// Whether every address of `other` is in `self`.
	#[must_use]
	pub fn contains(self, other: Self) -> bool {
		self.v6 == other.v6
			&& self.prefix <= other.prefix
			&& other.addr & mask(self.bits(), self.prefix) == self.addr
	}

	/// Whether `ip` is in the range.
	#[must_use]
	pub fn contains_ip(self, ip: IpAddr) -> bool {
		let other = match ip {
			IpAddr::V4(a) => Self {
				addr: u128::from(u32::from(a)),
				prefix: 32,
				v6: false,
			},
			IpAddr::V6(a) => Self {
				addr: u128::from(a),
				prefix: 128,
				v6: true,
			},
		};
		self.contains(other)
	}

	/// The two halves of the range, `None` for a single address.
	fn halves(self) -> Option<(Self, Self)> {
		if self.prefix >= self.bits() {
			return None;
		}
		let prefix = self.prefix + 1;
		let high = 1u128 << (self.bits() - prefix);
		Some((
			Self { prefix, ..self },
			Self {
				addr: self.addr | high,
				prefix,
				v6: self.v6,
			},
		))
	}

	/// What is left of `self` once `cut` is taken out.
	fn minus(self, cut: Self, out: &mut Vec<Self>) {
		if cut.contains(self) {
			return;
		}
		if !self.contains(cut) {
			out.push(self);
			return;
		}
		if let Some((a, b)) = self.halves() {
			a.minus(cut, out);
			b.minus(cut, out);
		}
	}
}

/// The leading `prefix` bits of a `bits`-wide address.
fn mask(bits: u8, prefix: u8) -> u128 {
	let width = u32::from(bits);
	let host = width - u32::from(prefix);
	let all = if bits == 128 {
		u128::MAX
	} else {
		(1u128 << width) - 1
	};
	if host >= 128 {
		0
	} else {
		all & !((1u128 << host) - 1)
	}
}

impl fmt::Display for Cidr {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		if self.v6 {
			write!(f, "{}/{}", Ipv6Addr::from(self.addr), self.prefix)
		} else {
			// A valid IPv4 range always fits in 32 bits.
			write!(
				f,
				"{}/{}",
				Ipv4Addr::from(u32::try_from(self.addr).unwrap_or(0)),
				self.prefix
			)
		}
	}
}

/// The ranges Traefik's allow list gets for `allow` minus `deny`: with an empty `allow`,
/// every address. Ranges are merged away when another one holds them.
///
/// # Errors
/// A message naming the entry that does not parse.
pub fn effective_allow(allow: &[String], deny: &[String]) -> Result<Vec<Cidr>, String> {
	let parse = |list: &[String]| {
		list.iter()
			.map(|c| Cidr::parse(c))
			.collect::<Result<Vec<_>, _>>()
	};
	let allow = parse(allow)?;
	let deny = parse(deny)?;
	let mut ranges = if allow.is_empty() {
		vec![
			Cidr {
				addr: 0,
				prefix: 0,
				v6: false,
			},
			Cidr {
				addr: 0,
				prefix: 0,
				v6: true,
			},
		]
	} else {
		allow
	};
	for cut in deny {
		let mut next = Vec::new();
		for range in ranges {
			range.minus(cut, &mut next);
		}
		ranges = next;
	}
	let held: Vec<Cidr> = ranges.clone();
	ranges.retain(|r| !held.iter().any(|o| o != r && o.contains(*r)));
	ranges.dedup();
	Ok(ranges)
}

#[cfg(test)]
mod tests {
	use super::*;

	fn strings(list: &[&str]) -> Vec<String> {
		list.iter().map(ToString::to_string).collect()
	}

	fn shown(list: &[Cidr]) -> Vec<String> {
		list.iter().map(ToString::to_string).collect()
	}

	#[test]
	fn parse_accepts_ranges_and_single_addresses() {
		assert_eq!(Cidr::parse("10.0.0.0/8").unwrap().to_string(), "10.0.0.0/8");
		assert_eq!(
			Cidr::parse("192.168.1.7").unwrap().to_string(),
			"192.168.1.7/32"
		);
		assert_eq!(
			Cidr::parse("10.0.0.7/24").unwrap().to_string(),
			"10.0.0.0/24"
		);
		assert_eq!(Cidr::parse("::1").unwrap().to_string(), "::1/128");
		assert_eq!(
			Cidr::parse("2001:db8::/32").unwrap().to_string(),
			"2001:db8::/32"
		);
		assert_eq!(Cidr::parse("0.0.0.0/0").unwrap().to_string(), "0.0.0.0/0");
	}

	#[test]
	fn parse_refuses_what_is_not_a_range() {
		for bad in [
			"",
			"abc",
			"10.0.0.0/33",
			"10.0.0.0/",
			"10.0.0.0/-1",
			"::1/129",
			"10.0.0/8",
			"10.0.0.0/8/8",
			"1.1.1.1, 2.2.2.2",
			"10.0.0.0/ 8",
		] {
			assert!(Cidr::parse(bad).is_err(), "{bad}");
		}
	}

	#[test]
	fn allow_without_deny_is_unchanged_and_empty_means_everyone() {
		let allow = effective_allow(&strings(&["10.0.0.0/8", "192.168.0.0/16"]), &[]).unwrap();
		assert_eq!(shown(&allow), ["10.0.0.0/8", "192.168.0.0/16"]);
		let all = effective_allow(&[], &[]).unwrap();
		assert_eq!(shown(&all), ["0.0.0.0/0", "::/0"]);
	}

	#[test]
	fn deny_is_cut_out_of_everything() {
		let ranges = effective_allow(&[], &strings(&["203.0.113.7"])).unwrap();
		let ip = |s: &str| s.parse::<IpAddr>().unwrap();
		assert!(!ranges.iter().any(|r| r.contains_ip(ip("203.0.113.7"))));
		for ok in [
			"203.0.113.6",
			"203.0.113.8",
			"8.8.8.8",
			"0.0.0.0",
			"255.255.255.255",
			"::1",
			"2001:db8::1",
		] {
			assert!(ranges.iter().any(|r| r.contains_ip(ip(ok))), "{ok}");
		}
		// One split per bit of the denied address, plus the whole IPv6 space.
		assert_eq!(ranges.len(), 33);
	}

	#[test]
	fn deny_inside_allow_and_whole_range_denied() {
		let ranges =
			effective_allow(&strings(&["10.0.0.0/24"]), &strings(&["10.0.0.128/25"])).unwrap();
		assert_eq!(shown(&ranges), ["10.0.0.0/25"]);
		let none = effective_allow(&strings(&["10.0.0.0/24"]), &strings(&["10.0.0.0/8"])).unwrap();
		assert!(none.is_empty());
		// A deny of one family leaves the other alone.
		let v6 = effective_allow(&strings(&["2001:db8::/32"]), &strings(&["10.0.0.0/8"])).unwrap();
		assert_eq!(shown(&v6), ["2001:db8::/32"]);
	}

	#[test]
	fn contains_checks_family_and_prefix() {
		let net = Cidr::parse("10.0.0.0/8").unwrap();
		assert!(net.contains(Cidr::parse("10.1.2.3").unwrap()));
		assert!(!net.contains(Cidr::parse("11.0.0.0/8").unwrap()));
		assert!(!net.contains(Cidr::parse("::a00:1").unwrap()));
	}
}
