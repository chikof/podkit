//! The real machine: `nft`, `/proc/net/tcp` and a state file.

use std::io::Write as _;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::os::unix::fs::PermissionsExt as _;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use crate::engine::{Active, Host};

/// [`Host`] on a real Linux machine.
pub struct SystemHost {
	nft: PathBuf,
	state_file: PathBuf,
}

impl SystemHost {
	/// `nft` is the binary to run; `state_file` holds the confirmed ruleset between restarts.
	#[must_use]
	pub const fn new(nft: PathBuf, state_file: PathBuf) -> Self {
		Self { nft, state_file }
	}
}

fn short(text: &[u8]) -> String {
	String::from_utf8_lossy(text)
		.trim()
		.chars()
		.take(400)
		.collect()
}

impl Host for SystemHost {
	fn nft_apply(&self, script: &str) -> Result<(), String> {
		let mut child = Command::new(&self.nft)
			.args(["-f", "-"])
			.stdin(Stdio::piped())
			.stdout(Stdio::null())
			.stderr(Stdio::piped())
			.spawn()
			.map_err(|e| format!("cannot run nft: {e}"))?;
		child
			.stdin
			.take()
			.ok_or("nft has no stdin")?
			.write_all(script.as_bytes())
			.map_err(|e| format!("cannot write to nft: {e}"))?;
		let out = child.wait_with_output().map_err(|e| format!("nft: {e}"))?;
		if out.status.success() {
			Ok(())
		} else {
			Err(format!("nft refused the rules: {}", short(&out.stderr)))
		}
	}

	fn ssh_peers(&self, port: u16) -> Vec<IpAddr> {
		let mut peers = Vec::new();
		for (path, v6) in [("/proc/net/tcp", false), ("/proc/net/tcp6", true)] {
			if let Ok(text) = std::fs::read_to_string(path) {
				peers.extend(parse_proc_net_tcp(&text, port, v6));
			}
		}
		peers
	}

	fn load(&self) -> Option<Active> {
		let raw = std::fs::read(&self.state_file).ok()?;
		serde_json::from_slice(&raw)
			.map_err(|e| tracing::error!(error = %e, "saved firewall state is unreadable"))
			.ok()
	}

	fn save(&self, active: Option<&Active>) -> Result<(), String> {
		let Some(active) = active else {
			return match std::fs::remove_file(&self.state_file) {
				Ok(()) => Ok(()),
				Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
				Err(e) => Err(format!("cannot remove saved state: {e}")),
			};
		};
		let json = serde_json::to_vec(active).map_err(|e| e.to_string())?;
		let tmp = self.state_file.with_extension("tmp");
		let write = || -> std::io::Result<()> {
			std::fs::write(&tmp, json)?;
			std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
			std::fs::rename(&tmp, &self.state_file)
		};
		write().map_err(|e| format!("cannot save state: {e}"))
	}
}

/// The peers of ESTABLISHED connections whose local port is `port`, from the text of
/// `/proc/net/tcp` (or `tcp6` with `v6`). Loopback peers are left out: loopback is always
/// allowed. IPv4-mapped IPv6 addresses become IPv4.
///
/// Format: <https://www.kernel.org/doc/html/latest/networking/proc_net_tcp.html>. Each address
/// is the kernel's in-memory words printed in hex, so every 32-bit word is little-endian.
#[must_use]
pub fn parse_proc_net_tcp(text: &str, port: u16, v6: bool) -> Vec<IpAddr> {
	let mut out = Vec::new();
	for line in text.lines().skip(1) {
		let f: Vec<&str> = line.split_whitespace().collect();
		let (Some(local), Some(remote), Some(state)) = (f.get(1), f.get(2), f.get(3)) else {
			continue;
		};
		if *state != "01" {
			continue;
		}
		let Some((_, local_port)) = local.rsplit_once(':') else {
			continue;
		};
		if u16::from_str_radix(local_port, 16) != Ok(port) {
			continue;
		}
		let Some((addr, _)) = remote.rsplit_once(':') else {
			continue;
		};
		let Some(ip) = parse_addr(addr, v6) else {
			continue;
		};
		if !ip.is_loopback() && !ip.is_unspecified() {
			out.push(ip);
		}
	}
	out
}

fn parse_addr(hex: &str, v6: bool) -> Option<IpAddr> {
	if !v6 {
		let word = u32::from_str_radix(hex, 16).ok()?;
		return Some(IpAddr::V4(Ipv4Addr::from(word.to_le_bytes())));
	}
	if hex.len() != 32 {
		return None;
	}
	let mut bytes = [0u8; 16];
	for (i, chunk) in bytes.chunks_mut(4).enumerate() {
		let word = u32::from_str_radix(hex.get(i * 8..i * 8 + 8)?, 16).ok()?;
		chunk.copy_from_slice(&word.to_le_bytes());
	}
	let ip = Ipv6Addr::from(bytes);
	Some(ip.to_ipv4_mapped().map_or(IpAddr::V6(ip), IpAddr::V4))
}

#[cfg(test)]
mod tests {
	use super::*;

	const TCP: &str = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode
   0: 0100007F:0016 0100007F:9C40 01 00000000:00000000 00:00000000 00000000     0        0 1 1 0
   1: 0F00A8C0:0016 0900A8C0:D2F4 01 00000000:00000000 00:00000000 00000000     0        0 2 1 0
   2: 0F00A8C0:0016 0A00A8C0:D2F5 06 00000000:00000000 00:00000000 00000000     0        0 3 1 0
   3: 0F00A8C0:1F90 0B00A8C0:D2F6 01 00000000:00000000 00:00000000 00000000     0        0 4 1 0
";

	#[test]
	fn finds_established_ssh_peers_only() {
		let peers = parse_proc_net_tcp(TCP, 22, false);
		assert_eq!(peers, vec!["192.168.0.9".parse::<IpAddr>().unwrap()]);
	}

	#[test]
	fn reads_ipv6_and_mapped_addresses() {
		let tcp6 = "header
   0: 00000000000000000000000001000000:0016 00000000000000000000000001000000:9C40 01 0:0 0:0 0 0 0 1 1 0
   1: 0000000000000000FFFF00000F00A8C0:0016 0000000000000000FFFF00000900A8C0:D2F4 01 0:0 0:0 0 0 0 1 1 0
   2: 00000000000000000000000000000000:0016 B80D0120000000000000000001000000:D2F4 01 0:0 0:0 0 0 0 1 1 0
";
		let peers = parse_proc_net_tcp(tcp6, 22, true);
		assert_eq!(
			peers,
			vec![
				"192.168.0.9".parse::<IpAddr>().unwrap(),
				"2001:db8::1".parse::<IpAddr>().unwrap()
			]
		);
	}

	#[test]
	fn state_round_trips_through_the_file() {
		let dir = tempfile::tempdir().unwrap();
		let host = SystemHost::new("nft".into(), dir.path().join("active.json"));
		assert!(host.load().is_none());
		let active = Active {
			policy: crate::policy::Policy::default(),
			script: "x".to_owned(),
		};
		host.save(Some(&active)).unwrap();
		assert_eq!(host.load(), Some(active));
		host.save(None).unwrap();
		assert!(host.load().is_none());
		host.save(None).unwrap();
	}
}
