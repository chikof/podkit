//! Network rules shared by the deploy path and the `/v1/networks` routes.

use proto::{ENV_NETWORK_PREFIX, STACK_NETWORK_PREFIX, USER_NETWORK_PREFIX};

/// Networks Podman or podkit itself owns; no container of a user may join them.
const RESERVED: [&str; 7] = [
	"host",
	"bridge",
	"none",
	"default",
	"podman",
	"pasta",
	"slirp4netns",
];

fn valid_chars(name: &str) -> bool {
	!name.is_empty()
		&& name.len() <= 128
		&& name
			.bytes()
			.all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'))
}

/// Whether a deploy may put a container on `name`: a network podkit made or
/// one the user imported, never a system or host network.
#[must_use]
pub fn valid_member_network(name: &str) -> bool {
	valid_chars(name) && name != proto::SYSTEM_NETWORK && !RESERVED.contains(&name)
}

/// Whether `name` is one of the networks podkit creates itself (the only ones
/// the agent creates or removes).
#[must_use]
pub fn is_managed(name: &str) -> bool {
	valid_chars(name)
		&& [
			ENV_NETWORK_PREFIX,
			USER_NETWORK_PREFIX,
			STACK_NETWORK_PREFIX,
		]
		.iter()
		.any(|p| name.starts_with(p) && name.len() > p.len())
}

/// The network a throwaway probe container joins to reach a container that
/// is on `networks`: its environment network, else any other podkit network,
/// else the system network.
#[must_use]
pub fn probe_network(mut networks: Vec<&str>) -> String {
	networks.sort_unstable();
	networks
		.iter()
		.find(|n| n.starts_with(ENV_NETWORK_PREFIX))
		.or_else(|| networks.iter().find(|n| valid_member_network(n)))
		.map_or_else(|| proto::SYSTEM_NETWORK.to_owned(), |n| (*n).to_owned())
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn probes_use_the_environment_network() {
		assert_eq!(
			probe_network(vec!["podkit-net-1", "podkit-env-4", "podkit"]),
			"podkit-env-4"
		);
		assert_eq!(
			probe_network(vec!["podkit", "podkit-net-1"]),
			"podkit-net-1"
		);
		assert_eq!(probe_network(vec!["podkit"]), "podkit");
		assert_eq!(probe_network(vec![]), "podkit");
	}

	#[test]
	fn member_networks_exclude_system_and_host_networks() {
		assert!(valid_member_network("podkit-env-7"));
		assert!(valid_member_network("podkit-net-3"));
		assert!(valid_member_network("my_imported.net"));
		for bad in ["podkit", "host", "bridge", "none", "", "a/b", "a b", "x:y"] {
			assert!(!valid_member_network(bad), "{bad}");
		}
	}

	#[test]
	fn only_podkit_prefixed_networks_are_managed() {
		assert!(is_managed("podkit-env-1"));
		assert!(is_managed("podkit-net-9"));
		assert!(is_managed("podkit-stack-2"));
		assert!(!is_managed("podkit-env-"));
		assert!(!is_managed("podkit"));
		assert!(!is_managed("podkit-env-../x"));
		assert!(!is_managed("imported"));
	}
}
