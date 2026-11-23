//! Runs the helper for real inside a throwaway container with `NET_ADMIN`, applies rules with
//! confirm, reads them back with `nft list table inet podkit` and checks that an unconfirmed
//! change disappears. The container has its own network namespace, so the host's firewall is
//! never touched.
//!
//! `devenv shell -- cargo test -p podkit-firewall -- --ignored`
//!
//! Needs podman, the `alpine:3.21` image and an `nft` binary: `PODKIT_TEST_NFT`, else `nft`
//! on PATH, else the newest `nftables-*` in `/nix/store`. The host's `/nix/store` is mounted
//! read-only so the test binary and nft run in any image.

use std::path::PathBuf;
use std::process::{Command, Output};
use std::time::Duration;

use podkit_firewall::policy::{Allow, Policy, Proto, Source};
use podkit_firewall::protocol::{Request, Response};

const NAME: &str = "pk-t_bfd6556c-fw";
const IMAGE: &str = "docker.io/library/alpine:3.21";

fn podman_bin() -> String {
	if let Some(p) = std::env::var_os("PODKIT_TEST_PODMAN") {
		return p.to_string_lossy().into_owned();
	}
	for dir in std::env::var("PATH").unwrap_or_default().split(':') {
		let p = PathBuf::from(dir).join("podman");
		if p.exists() {
			return p.to_string_lossy().into_owned();
		}
	}
	"/run/current-system/sw/bin/podman".to_owned()
}

fn nft_bin() -> String {
	if let Some(p) = std::env::var_os("PODKIT_TEST_NFT") {
		return p.to_string_lossy().into_owned();
	}
	for dir in std::env::var("PATH").unwrap_or_default().split(':') {
		let p = PathBuf::from(dir).join("nft");
		if p.exists() {
			return p.to_string_lossy().into_owned();
		}
	}
	let mut found: Vec<PathBuf> = std::fs::read_dir("/nix/store")
		.into_iter()
		.flatten()
		.flatten()
		.map(|e| e.path())
		.filter(|p| {
			p.file_name().and_then(|n| n.to_str()).is_some_and(|n| {
				n.contains("-nftables-")
					&& std::path::Path::new(n)
						.extension()
						.is_none_or(|e| e != "drv")
			}) && p.join("bin/nft").exists()
		})
		.collect();
	found.sort();
	found
		.pop()
		.expect("no nft: set PODKIT_TEST_NFT")
		.join("bin/nft")
		.to_string_lossy()
		.into_owned()
}

fn podman(args: &[&str]) -> Output {
	Command::new(podman_bin())
		.args(args)
		.output()
		.expect("podman runs")
}

fn ok(args: &[&str]) -> String {
	let out = podman(args);
	assert!(
		out.status.success(),
		"podman {args:?}: {}",
		String::from_utf8_lossy(&out.stderr)
	);
	String::from_utf8_lossy(&out.stdout).into_owned()
}

struct Container;

impl Drop for Container {
	fn drop(&mut self) {
		let _ = podman(&["rm", "-f", "-t", "0", NAME]);
	}
}

struct Lab {
	nft: String,
	_guard: Container,
	_dir: tempfile::TempDir,
}

impl Lab {
	fn start() -> Self {
		let _ = podman(&["rm", "-f", "-t", "0", NAME]);
		let dir = tempfile::tempdir().expect("tempdir");
		let helper = env!("CARGO_BIN_EXE_podkit-firewall");
		let work = dir.path().to_string_lossy().into_owned();
		ok(&[
			"run",
			"-d",
			"--name",
			NAME,
			"--cap-add",
			"NET_ADMIN",
			"--network",
			"none",
			"-v",
			"/nix/store:/nix/store:ro",
			"-v",
			&format!("{helper}:/podkit-firewall:ro"),
			"-v",
			&format!("{work}:/work"),
			IMAGE,
			"sleep",
			"600",
		]);
		let lab = Self {
			nft: nft_bin(),
			_guard: Container,
			_dir: dir,
		};
		// The container's root is the user running this test, so that is the allowed uid.
		ok(&[
			"exec",
			"-d",
			NAME,
			"/podkit-firewall",
			"--uid",
			"0",
			"--socket",
			"/work/fw.sock",
			"--state-dir",
			"/work/state",
			"--nft",
			&lab.nft,
		]);
		for _ in 0..50 {
			if podman(&["exec", NAME, "test", "-S", "/work/fw.sock"])
				.status
				.success()
			{
				return lab;
			}
			std::thread::sleep(Duration::from_millis(100));
		}
		panic!("helper did not come up");
	}

	fn ask(req: &Request) -> Response {
		let json = serde_json::to_string(req).expect("json");
		let out = ok(&[
			"exec",
			NAME,
			"/podkit-firewall",
			"ctl",
			"/work/fw.sock",
			&json,
		]);
		serde_json::from_str(out.trim()).unwrap_or_else(|e| panic!("bad answer {out:?}: {e}"))
	}

	fn nft(&self, args: &[&str]) -> Output {
		let mut all = vec!["exec", NAME, self.nft.as_str()];
		all.extend_from_slice(args);
		podman(&all)
	}

	/// `nft list table inet podkit`, `None` when the table does not exist.
	fn table(&self) -> Option<String> {
		let out = self.nft(&["list", "table", "inet", "podkit"]);
		out.status
			.success()
			.then(|| String::from_utf8_lossy(&out.stdout).into_owned())
	}
}

fn policy() -> Policy {
	Policy {
		ssh_port: 22,
		ssh_sources: Vec::new(),
		allows: vec![
			Allow {
				proto: Proto::Tcp,
				port: 5432,
				source: Some(Source::try_from("10.0.0.0/8".to_owned()).unwrap()),
			},
			Allow {
				proto: Proto::Udp,
				port: 51820,
				source: None,
			},
		],
	}
}

#[test]
#[ignore = "needs podman, alpine:3.21 and nft; run with --ignored"]
fn unconfirmed_rules_are_reverted_and_confirmed_ones_stay() {
	let lab = Lab::start();

	// Another table that is not ours must survive everything below.
	let made = lab.nft(&["add", "table", "inet", "someone_else"]);
	assert!(made.status.success(), "{made:?}");

	// Preview changes nothing.
	let Response::Preview(p) = Lab::ask(&Request::Preview { policy: policy() }) else {
		panic!("preview");
	};
	assert!(p.diff.iter().any(|l| l.op == '+'));
	assert!(lab.table().is_none(), "a preview must not apply");

	// Apply, with a 5 s window.
	let Response::Applied { token, .. } = Lab::ask(&Request::Apply {
		policy: policy(),
		confirm_secs: Some(5),
	}) else {
		panic!("apply");
	};
	let table = lab.table().expect("table exists after apply");
	assert!(table.contains("tcp dport 22 accept"), "{table}");
	assert!(table.contains("ip saddr 10.0.0.0/8 tcp dport 5432 accept"));
	assert!(table.contains("ct original proto-dst 5432"), "{table}");
	assert!(token > 0);

	// No confirm: after the window the table is gone, the other one is not.
	std::thread::sleep(Duration::from_secs(8));
	assert!(lab.table().is_none(), "unconfirmed rules must be reverted");
	let tables = String::from_utf8_lossy(&lab.nft(&["list", "tables"]).stdout).into_owned();
	assert!(tables.contains("someone_else"), "{tables}");

	// Apply again and confirm: it stays past the window.
	let Response::Applied { token, .. } = Lab::ask(&Request::Apply {
		policy: policy(),
		confirm_secs: Some(5),
	}) else {
		panic!("apply");
	};
	assert_eq!(Lab::ask(&Request::Confirm { token }), Response::Done);
	std::thread::sleep(Duration::from_secs(8));
	assert!(lab.table().is_some(), "confirmed rules must stay");

	// A change that is not confirmed goes back to the confirmed ruleset, not to nothing.
	let mut wider = policy();
	wider.allows.push(Allow {
		proto: Proto::Tcp,
		port: 6379,
		source: None,
	});
	Lab::ask(&Request::Apply {
		policy: wider,
		confirm_secs: Some(5),
	});
	assert!(lab.table().unwrap().contains("dport 6379 accept"));
	std::thread::sleep(Duration::from_secs(8));
	let back = lab.table().expect("still there");
	assert!(!back.contains("6379"), "{back}");
	assert!(back.contains("5432"));

	// Disable removes ours and only ours.
	assert_eq!(Lab::ask(&Request::Disable), Response::Done);
	assert!(lab.table().is_none());
	let tables = String::from_utf8_lossy(&lab.nft(&["list", "tables"]).stdout).into_owned();
	assert!(tables.contains("someone_else"), "{tables}");
}
