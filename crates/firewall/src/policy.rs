//! What a caller may ask of the firewall.

use std::fmt;

use proto::cidr::Cidr;
use serde::{Deserialize, Serialize};

/// Most explicit allows one policy may carry.
pub const MAX_ALLOWS: usize = 256;
/// Most SSH source ranges one policy may carry.
pub const MAX_SSH_SOURCES: usize = 64;

/// Transport protocol of an allow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Proto {
	/// TCP.
	Tcp,
	/// UDP.
	Udp,
}

impl Proto {
	/// The nft keyword.
	#[must_use]
	pub const fn as_str(self) -> &'static str {
		match self {
			Self::Tcp => "tcp",
			Self::Udp => "udp",
		}
	}
}

/// A source address range; travels as a string like `10.0.0.0/24`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Source(pub Cidr);

impl Source {
	/// Whether the range is IPv6.
	#[must_use]
	pub fn is_v6(self) -> bool {
		self.0.to_string().contains(':')
	}
}

impl fmt::Display for Source {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		self.0.fmt(f)
	}
}

impl TryFrom<String> for Source {
	type Error = String;

	fn try_from(raw: String) -> Result<Self, String> {
		Cidr::parse(&raw).map(Self)
	}
}

impl From<Source> for String {
	fn from(s: Source) -> Self {
		s.to_string()
	}
}

impl Serialize for Source {
	fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
		s.collect_str(self)
	}
}

impl<'de> Deserialize<'de> for Source {
	fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
		let raw = String::deserialize(d)?;
		Self::try_from(raw).map_err(serde::de::Error::custom)
	}
}

/// One explicit allow: inbound `proto` to `port`, from `source` or from anywhere.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Allow {
	/// Transport protocol.
	pub proto: Proto,
	/// Destination port as the outside sees it (the published port for a container).
	pub port: u16,
	/// Only from this range; `None` is anyone.
	#[serde(default)]
	pub source: Option<Source>,
}

/// The whole ruleset a caller wants. Established traffic, loopback, ICMP, 80 and 443 are
/// always allowed and are not part of it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Policy {
	/// The sshd port, 22 unless the server uses another.
	pub ssh_port: u16,
	/// Ranges SSH is limited to; empty is anywhere.
	#[serde(default)]
	pub ssh_sources: Vec<Source>,
	/// Explicit allows beyond the defaults.
	#[serde(default)]
	pub allows: Vec<Allow>,
}

impl Default for Policy {
	fn default() -> Self {
		Self {
			ssh_port: 22,
			ssh_sources: Vec::new(),
			allows: Vec::new(),
		}
	}
}

impl Policy {
	/// Checks limits that serde cannot.
	///
	/// # Errors
	/// A message for the first thing wrong.
	pub fn validate(&self) -> Result<(), String> {
		if self.ssh_port == 0 {
			return Err("SSH port must be between 1 and 65535".to_owned());
		}
		if self.ssh_sources.len() > MAX_SSH_SOURCES {
			return Err(format!("at most {MAX_SSH_SOURCES} SSH sources"));
		}
		if self.allows.len() > MAX_ALLOWS {
			return Err(format!("at most {MAX_ALLOWS} allow rules"));
		}
		if self.allows.iter().any(|a| a.port == 0) {
			return Err("port must be between 1 and 65535".to_owned());
		}
		Ok(())
	}
}
