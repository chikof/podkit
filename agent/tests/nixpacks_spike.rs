//! Spike test: does a nixpacks-generated build actually work through
//! `Podman::build_image` (podman.sock via bollard), with zero podman-specific
//! changes to nixpacks itself?
//!
//! Findings (see also agent/src/nixpacks.rs doc comment):
//! - `nixpacks build <dir> -o <dir>` writes `.nixpacks/Dockerfile` (plus
//!   support files) *into* the app directory. No docker/podman access is
//!   needed for this step at all, it's pure plan generation.
//! - That Dockerfile's `COPY . /app` assumes the build context is the app
//!   directory itself (not `.nixpacks/`), so it composes cleanly with the
//!   tar-the-whole-clone approach: tar the directory nixpacks just wrote
//!   into, same as the Dockerfile-strategy path, just with
//!   `dockerfile = ".nixpacks/Dockerfile"` instead of `"Dockerfile"`.
//!
//! Requires the `nixpacks` CLI on PATH (`nix-shell -p nixpacks` on a Nix
//! system) and a live podman.sock. Run explicitly:
//! `cargo test -p agent --test nixpacks_spike -- --ignored`.

mod common;

use agent::nixpacks;
use agent::podman::{ContainerSpec, Podman, local_socket_path};

#[tokio::test]
#[ignore = "requires the nixpacks CLI on PATH and a live podman.sock"]
async fn nixpacks_output_builds_and_runs_via_podman() {
	let app_dir = tempfile::tempdir().unwrap();

	std::fs::write(
		app_dir.path().join("package.json"),
		r#"{"name":"nixpacks-spike","version":"1.0.0","scripts":{"start":"node index.js"}}"#,
	)
	.unwrap();
	std::fs::write(
		app_dir.path().join("index.js"),
		"const http = require('http');\n\
		 http.createServer((req, res) => { res.end('nixpacks-spike-ok'); }).listen(process.env.PORT || 3000);\n",
	)
	.unwrap();

	nixpacks::generate_plan(app_dir.path())
		.await
		.expect("generate_plan failed, is `nixpacks` on PATH? (`nix-shell -p nixpacks`)");
	assert!(
		app_dir
			.path()
			.join(nixpacks::GENERATED_DOCKERFILE_PATH)
			.is_file(),
		"nixpacks did not write .nixpacks/Dockerfile"
	);

	let podman = Podman::connect(&local_socket_path()).expect("connect to podman.sock");
	let tag = "podkit-nixpacks-spike:latest";
	let out = tempfile::tempdir().unwrap();
	let context = out.path().join("context.tar");
	common::directory_tar(app_dir.path(), &context);
	common::build(&podman, tag, nixpacks::GENERATED_DOCKERFILE_PATH, &context).await;

	let name = "podkit-nixpacks-spike-container";
	podman.remove(name).await.expect("clean slate");

	podman
		.create_container(ContainerSpec {
			name: name.to_string(),
			image: tag.to_string(),
			env: vec![("PORT".to_string(), "3000".to_string())],
			..Default::default()
		})
		.await
		.expect("create container from nixpacks image");
	podman.start(name).await.expect("start container");

	tokio::time::sleep(std::time::Duration::from_secs(2)).await;

	let running = podman
		.inspect(name)
		.await
		.expect("inspect container")
		.and_then(|i| i.state)
		.and_then(|s| s.running);
	assert_eq!(
		running,
		Some(true),
		"nixpacks-built container did not stay running"
	);

	podman.remove(name).await.expect("cleanup container");
}
