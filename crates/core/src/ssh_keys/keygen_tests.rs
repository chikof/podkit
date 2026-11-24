//! Key generation and import. The fixtures are throwaway keys made with `ssh-keygen`, next
//! to the fingerprint it printed for each (`ssh-keygen -lf`).

use super::*;

fn fixture(name: &str) -> String {
	let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/ssh_keys/fixtures");
	std::fs::read_to_string(dir.join(name)).unwrap()
}

fn expected(name: &str) -> String {
	fixture(&format!("{name}.fp")).trim().to_owned()
}

/// `ssh-keygen -lf` on a public line, when the tool is installed.
fn ssh_keygen_fingerprint(public_key: &str) -> Option<String> {
	let dir = tempfile::tempdir().unwrap();
	let file = dir.path().join("k.pub");
	std::fs::write(&file, format!("{public_key}\n")).unwrap();
	let out = std::process::Command::new("ssh-keygen")
		.arg("-lf")
		.arg(&file)
		.output()
		.ok()?;
	out.status.success().then(|| {
		String::from_utf8_lossy(&out.stdout)
			.split_whitespace()
			.nth(1)
			.unwrap_or_default()
			.to_owned()
	})
}

#[test]
fn ed25519_generates_and_reads_back_to_the_same_key() {
	let made = generate(Kind::Ed25519).unwrap();
	assert_eq!(made.kind, Kind::Ed25519);
	assert!(made.public_key.starts_with("ssh-ed25519 "));
	assert!(made.public_key.ends_with(" podkit"));
	assert!(made.fingerprint.starts_with("SHA256:"));
	assert!(
		made.private_openssh
			.starts_with("-----BEGIN OPENSSH PRIVATE KEY-----")
	);
	assert!(made.private_openssh.ends_with('\n'));
	assert!(!made.public_key.contains("PRIVATE"));

	let again = import(&made.private_openssh).unwrap();
	assert_eq!(again.public_key, made.public_key);
	assert_eq!(again.fingerprint, made.fingerprint);
	assert_eq!(
		derive(&made.private_openssh).unwrap().fingerprint,
		made.fingerprint
	);
	if let Some(real) = ssh_keygen_fingerprint(&made.public_key) {
		assert_eq!(real, made.fingerprint, "ssh-keygen -lf disagrees");
	}
}

#[test]
fn rsa_4096_generates_and_reads_back_to_the_same_key() {
	let made = generate(Kind::Rsa).unwrap();
	assert_eq!(made.kind, Kind::Rsa);
	assert!(made.public_key.starts_with("ssh-rsa "));
	let again = import(&made.private_openssh).unwrap();
	assert_eq!(again.fingerprint, made.fingerprint);
	if let Some(real) = ssh_keygen_fingerprint(&made.public_key) {
		assert_eq!(real, made.fingerprint, "ssh-keygen -lf disagrees");
	}
	// 4096 bits: the modulus alone is 512 bytes of the blob.
	let blob = STANDARD.decode(blob(&made.public_key).unwrap()).unwrap();
	assert!(blob.len() > 512, "{} bytes", blob.len());
}

#[test]
fn every_supported_format_imports_with_the_fingerprint_ssh_keygen_printed() {
	for (name, kind) in [
		("ed25519_openssh", Kind::Ed25519),
		("ed25519_pkcs8", Kind::Ed25519),
		("rsa_pkcs1", Kind::Rsa),
		("rsa_pkcs8", Kind::Rsa),
		("ecdsa_openssh", Kind::Ecdsa),
	] {
		let key = import(&fixture(name)).unwrap_or_else(|e| panic!("{name}: {e}"));
		assert_eq!(key.kind, kind, "{name}");
		assert_eq!(key.fingerprint, expected(name), "{name}");
		// Whatever it came in as, it is stored as an OpenSSH key.
		assert!(
			key.private_openssh
				.starts_with("-----BEGIN OPENSSH PRIVATE KEY-----"),
			"{name}"
		);
		assert_eq!(
			derive(&key.private_openssh).unwrap().fingerprint,
			key.fingerprint
		);
		if let Some(real) = ssh_keygen_fingerprint(&key.public_key) {
			assert_eq!(real, key.fingerprint, "{name}");
		}
	}
}

#[test]
fn the_importers_comment_never_reaches_the_public_key() {
	let key = import(&fixture("ed25519_openssh")).unwrap();
	assert!(!key.public_key.contains("someone@laptop"));
	assert!(key.public_key.ends_with(" podkit"));
}

#[test]
fn passphrase_protected_keys_are_refused_with_the_way_out() {
	for name in ["ed25519_passphrase", "rsa_passphrase_pem"] {
		let err = import(&fixture(name)).err().unwrap();
		assert_eq!(err, KeyError::Passphrase, "{name}");
		assert!(err.to_string().contains("passphrase"));
		assert!(err.to_string().contains("ssh-keygen -p"));
	}
	let pkcs8 = "-----BEGIN ENCRYPTED PRIVATE KEY-----\nMIIFH\n-----END ENCRYPTED PRIVATE KEY-----";
	assert_eq!(import(pkcs8).err(), Some(KeyError::Passphrase));
}

#[test]
fn garbage_and_public_keys_are_refused_by_name() {
	assert_eq!(import("hello").err(), Some(KeyError::Unreadable));
	assert_eq!(import("").err(), Some(KeyError::Unreadable));
	assert_eq!(
		import("-----BEGIN OPENSSH PRIVATE KEY-----\nAAAA\n-----END OPENSSH PRIVATE KEY-----")
			.err(),
		Some(KeyError::Unreadable)
	);
	let made = generate(Kind::Ed25519).unwrap();
	assert_eq!(import(&made.public_key).err(), Some(KeyError::PublicKey));
}

#[test]
fn the_blob_is_the_second_token() {
	assert_eq!(blob("ssh-ed25519 AAAAC3 podkit"), Some("AAAAC3"));
	assert_eq!(blob("ssh-ed25519"), None);
}
