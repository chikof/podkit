//! Exercises the full container lifecycle against a real podman socket: build,
//! create, start, logs, inspect, stop, remove. Requires a reachable rootless
//! podman.sock (`systemctl --user enable --now podman.socket`).
//! Run explicitly: `cargo test -p agent --test podman -- --ignored`.

mod common;

use agent::podman::{ContainerSpec, Podman, local_socket_path};
use futures_util::StreamExt;

#[tokio::test]
#[ignore = "requires a live podman.sock"]
async fn full_lifecycle_against_live_podman() {
	let podman = Podman::connect(&local_socket_path()).expect("connect to podman.sock");
	assert_ne!(podman.version().await.expect("version"), "");

	let dir = tempfile::tempdir().unwrap();
	let dockerfile =
		b"FROM docker.io/library/alpine:3.21\nRUN echo agent-crate-test > /marker\nCMD [\"cat\", \"/marker\"]\n";
	let context = common::dockerfile_tar(dir.path(), dockerfile);
	let tag = "podkit-agent-test:latest";
	common::build(&podman, tag, "Dockerfile", &context).await;

	let name = "podkit-agent-test-container";
	podman.remove(name).await.expect("clean slate");

	let id = podman
		.create_container(ContainerSpec {
			name: name.to_string(),
			image: tag.to_string(),
			env: vec![("PODKIT_TEST".to_string(), "1".to_string())],
			..Default::default()
		})
		.await
		.expect("create container");
	podman.start(name).await.expect("start container");

	tokio::time::sleep(std::time::Duration::from_millis(800)).await;

	let info = podman
		.inspect(name)
		.await
		.expect("inspect container")
		.expect("container exists");
	assert_eq!(info.id.as_deref(), Some(id.as_str()));

	let mut lines = Vec::new();
	let mut logs = std::pin::pin!(podman.logs(name, None, false));
	while let Some(output) = logs.next().await {
		lines.push(output.expect("log chunk").to_string());
	}
	assert!(
		lines.iter().any(|l| l.contains("agent-crate-test")),
		"expected marker line in logs, got: {lines:?}"
	);

	let listed = podman.list("podkit-agent-test-").await.expect("list");
	assert_eq!(listed.len(), 1);

	podman.stop(name).await.expect("stop container");
	podman.remove(name).await.expect("remove container");
	assert!(podman.inspect(name).await.expect("inspect").is_none());
}
