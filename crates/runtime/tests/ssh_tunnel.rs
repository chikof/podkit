//! Proves `ServerConnection`/`SshTunnel` actually route container traffic
//! through an ssh unix-socket forward, not just to the local socket.
//!
//! Spins up its own throwaway `sshd` + keypair (no fixture state, no
//! external service assumed) and tunnels to the *real* local podman.sock,
//! then runs a full container lifecycle over that tunnel.
//!
//! Requires `ssh`, `ssh-keygen`, `sshd` on `PATH` and a reachable podman
//! socket. Run explicitly: `cargo test -p runtime --test ssh_tunnel -- --ignored`.

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use crypto::age::SecretBox;
use podkit_core::domain::runtime::container_runtime::ContainerRuntime;
use podkit_core::domain::runtime::entity::{
	BuildSpec, ContainerId, ContainerSpec, ImageRef, LogsQuery,
};
use podkit_core::domain::server::entity::Server;
use podkit_core::domain::shared::ids::{ServerId, TeamId};
use runtime::ServerConnection;

/// A throwaway local sshd instance for the duration of one test.
struct TestSshd {
	child: std::process::Child,
	dir: tempfile::TempDir,
	port: u16,
}

impl TestSshd {
	fn spawn() -> Self {
		let dir = tempfile::tempdir().expect("tempdir");
		let host_key = dir.path().join("host_key");
		let client_key = dir.path().join("client_key");
		let authorized_keys = dir.path().join("authorized_keys");

		keygen(&host_key);
		keygen(&client_key);
		std::fs::copy(client_key.with_extension("pub"), &authorized_keys).expect("copy pubkey");

		let port = 32222;
		let config = dir.path().join("sshd_config");
		std::fs::write(
			&config,
			format!(
				"Port {port}\n\
				 ListenAddress 127.0.0.1\n\
				 HostKey {}\n\
				 AuthorizedKeysFile {}\n\
				 PubkeyAuthentication yes\n\
				 PasswordAuthentication no\n\
				 PermitRootLogin no\n\
				 UsePAM no\n\
				 StrictModes no\n\
				 PidFile {}\n",
				host_key.display(),
				authorized_keys.display(),
				dir.path().join("sshd.pid").display(),
			),
		)
		.expect("write sshd_config");

		let sshd_path = resolve_on_path("sshd");
		let child = Command::new(sshd_path)
			.arg("-D")
			.arg("-e")
			.arg("-f")
			.arg(&config)
			.stdin(Stdio::null())
			.stdout(Stdio::null())
			.stderr(Stdio::null())
			.spawn()
			.expect("spawn sshd failed, is `sshd` on PATH?");

		wait_for_port(port);

		Self { child, dir, port }
	}

	fn client_key_pem(&self) -> String {
		std::fs::read_to_string(self.dir.path().join("client_key")).expect("read client key")
	}
}

impl Drop for TestSshd {
	fn drop(&mut self) {
		let _ = self.child.kill();
		let _ = self.child.wait();
	}
}

fn keygen(path: &Path) {
	let status = Command::new("ssh-keygen")
		.args(["-t", "ed25519", "-f"])
		.arg(path)
		.args(["-N", ""])
		.arg("-q")
		.status()
		.expect("run ssh-keygen failed, is it on PATH?");
	assert!(status.success(), "ssh-keygen failed");
}

fn resolve_on_path(bin: &str) -> String {
	let output = Command::new("sh")
		.arg("-c")
		.arg(format!("command -v {bin}"))
		.output()
		.expect("resolve binary path");
	String::from_utf8(output.stdout)
		.expect("utf8")
		.trim()
		.to_string()
}

fn wait_for_port(port: u16) {
	for _ in 0..50 {
		if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
			return;
		}
		std::thread::sleep(Duration::from_millis(100));
	}
	panic!("sshd never came up on port {port}");
}

fn make_tar(dockerfile: &[u8]) -> Vec<u8> {
	let mut tar = tar::Builder::new(Vec::new());
	let mut header = tar::Header::new_gnu();
	header.set_path("Dockerfile").unwrap();
	header.set_size(dockerfile.len() as u64);
	header.set_mode(0o644);
	header.set_cksum();
	tar.append(&header, dockerfile).unwrap();
	tar.into_inner().unwrap()
}

#[tokio::test]
#[ignore = "spawns a real sshd and talks to a live podman.sock"]
async fn container_lifecycle_over_ssh_tunnel() {
	let sshd = TestSshd::spawn();
	let user = std::env::var("USER").expect("USER env var");

	let secrets = SecretBox::from_identity_str(&crypto::age::generate_identity()).unwrap();
	let encrypted_key = secrets.encrypt(&sshd.client_key_pem()).unwrap();

	let server = Server::new_remote(
		ServerId(1),
		TeamId(1),
		"ssh-tunnel-test".to_string(),
		"127.0.0.1".to_string(),
		i32::from(sshd.port),
		user,
		encrypted_key,
		runtime::local_socket_path(),
	);

	let connection = ServerConnection::connect(&server, &secrets)
		.await
		.expect("connect over ssh tunnel");

	// From here it's the same lifecycle already proven against a local
	// socket, just now running entirely through the tunneled one.
	let dockerfile = b"FROM docker.io/library/alpine:3.21\nRUN echo tunnel-test > /marker\nCMD [\"cat\", \"/marker\"]\n";
	let tag = ImageRef("podkit-tunnel-test:latest".to_string());
	connection
		.runtime
		.build_image(BuildSpec {
			tag: tag.clone(),
			dockerfile_path: "Dockerfile".to_string(),
			context_tar: make_tar(dockerfile),
		})
		.await
		.expect("build image over tunnel");

	let name = "podkit-tunnel-test-container";
	let _ = connection
		.runtime
		.remove_container(&ContainerId(name.to_string()), true)
		.await;

	let id = connection
		.runtime
		.create_container(ContainerSpec {
			name: name.to_string(),
			image: tag,
			command: None,
			env: vec![],
			ports: vec![],
			networks: vec![],
			labels: vec![],
			binds: vec![],
			resource_limits: podkit_core::domain::runtime::entity::ResourceLimits::default(),
			restart_policy: podkit_core::domain::runtime::entity::RestartPolicy::Never,
		})
		.await
		.expect("create container over tunnel");

	connection
		.runtime
		.start_container(&id)
		.await
		.expect("start");
	tokio::time::sleep(Duration::from_millis(800)).await;

	let logs = connection
		.runtime
		.logs(&id, LogsQuery::default())
		.await
		.expect("logs over tunnel");
	assert!(
		logs.iter().any(|l| l.contains("tunnel-test")),
		"expected marker in logs, got: {logs:?}"
	);

	connection.runtime.stop_container(&id).await.expect("stop");
	connection
		.runtime
		.remove_container(&id, true)
		.await
		.expect("remove");
}
