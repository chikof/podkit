//! Spike test: does a nixpacks-generated build actually work through our
//! existing `ContainerRuntime::build_image` (podman.sock via bollard), with
//! zero podman-specific changes to nixpacks itself?
//!
//! Findings (see also crates/runtime/src/nixpacks.rs doc comment):
//! - `nixpacks build <dir> -o <dir>` writes `.nixpacks/Dockerfile` (plus
//!   support files) *into* the app directory. No docker/podman access is
//!   needed for this step at all, it's pure plan generation.
//! - That Dockerfile's `COPY . /app` assumes the build context is the app
//!   directory itself (not `.nixpacks/`), so it composes cleanly with our
//!   existing tar-the-whole-clone approach: tar the directory nixpacks just
//!   wrote into, same as the Dockerfile-strategy path, just with
//!   `dockerfile_path = ".nixpacks/Dockerfile"` instead of `"Dockerfile"`.
//! - The generated image is then built via the exact same
//!   `ContainerRuntime::build_image` call as any other strategy, proven
//!   below against a real podman.sock.
//!
//! Requires the `nixpacks` CLI on PATH (`nix-shell -p nixpacks` on a Nix
//! system) and a live podman.sock. Run explicitly:
//! `cargo test -p runtime --test nixpacks_spike -- --ignored`.

use podkit_core::domain::runtime::container_runtime::ContainerRuntime;
use podkit_core::domain::runtime::entity::{
	BuildSpec, ContainerId, ContainerSpec, ImageRef, LogsQuery,
};
use runtime::PodmanRuntime;

fn tar_directory(dir: &std::path::Path) -> Vec<u8> {
	let mut builder = tar::Builder::new(Vec::new());
	builder.append_dir_all(".", dir).unwrap();
	builder.into_inner().unwrap()
}

#[tokio::test]
#[ignore = "requires the nixpacks CLI on PATH and a live podman.sock"]
async fn nixpacks_output_builds_and_runs_via_podman_runtime() {
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

	// Step 1: nixpacks generates .nixpacks/Dockerfile in-place, no docker/podman involved yet.
	runtime::nixpacks::generate_plan(app_dir.path())
		.await
		.expect("generate_plan failed, is `nixpacks` on PATH? (`nix-shell -p nixpacks`)");
	assert!(
		app_dir
			.path()
			.join(runtime::nixpacks::GENERATED_DOCKERFILE_PATH)
			.is_file(),
		"nixpacks did not write .nixpacks/Dockerfile"
	);

	// Step 2: same path as the Dockerfile strategy from here, tar the
	// directory, hand it to the real ContainerRuntime.
	let rt = PodmanRuntime::connect_local_default().expect("connect to podman.sock");
	let tag = ImageRef("podkit-nixpacks-spike:latest".to_string());

	rt.build_image(BuildSpec {
		tag: tag.clone(),
		dockerfile_path: ".nixpacks/Dockerfile".to_string(),
		context_tar: tar_directory(app_dir.path()),
	})
	.await
	.expect("build nixpacks-generated image via ContainerRuntime");

	let name = "podkit-nixpacks-spike-container";
	let _ = rt
		.remove_container(&ContainerId(name.to_string()), true)
		.await;

	let id = rt
		.create_container(ContainerSpec {
			name: name.to_string(),
			image: tag,
			command: None,
			env: vec![("PORT".to_string(), "3000".to_string())],
			ports: vec![],
			networks: vec![],
			labels: vec![],
			binds: vec![],
			resource_limits: podkit_core::domain::runtime::entity::ResourceLimits::default(),
			restart_policy: podkit_core::domain::runtime::entity::RestartPolicy::Never,
		})
		.await
		.expect("create container from nixpacks image");
	rt.start_container(&id).await.expect("start container");

	tokio::time::sleep(std::time::Duration::from_secs(2)).await;

	let status = rt.inspect(&id).await.expect("inspect container");
	let logs = rt.logs(&id, LogsQuery::default()).await.unwrap_or_default();
	assert_eq!(
		status.state,
		podkit_core::domain::runtime::entity::ContainerState::Running,
		"nixpacks-built container did not stay running; logs: {logs:?}"
	);

	rt.remove_container(&id, true)
		.await
		.expect("cleanup container");
}
