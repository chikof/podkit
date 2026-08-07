use std::path::Path;
use std::process::Stdio;

use tokio::process::Command;

use crate::RuntimeError;

/// Where [`generate_plan`] writes the Dockerfile, relative to `app_dir`.
/// Pass this as `BuildSpec::dockerfile_path` after tarring `app_dir`.
pub const GENERATED_DOCKERFILE_PATH: &str = ".nixpacks/Dockerfile";

/// Runs `nixpacks build <app_dir> -o <app_dir>` to auto-detect the app's
/// stack and write a build plan (`.nixpacks/Dockerfile` + support files)
/// directly into `app_dir`. No docker/podman access is needed for this step,
/// it's pure plan generation.
///
/// From here the caller follows the exact same path as a user-supplied
/// Dockerfile: tar `app_dir` (now containing `.nixpacks/`) and hand it to
/// [`crate::PodmanRuntime::build_image`] with
/// `dockerfile_path: GENERATED_DOCKERFILE_PATH`. This is proven end to end in
/// `crates/runtime/tests/nixpacks_spike.rs`, and wired into the deploy
/// pipeline via [`crate::clone_to_tar`]'s `build_strategy` parameter.
///
/// Requires the `nixpacks` CLI on `PATH`.
///
/// # Errors
/// Returns an error if the `nixpacks` binary can't be run or it fails to
/// produce a plan (e.g. an unrecognized/unsupported stack).
pub async fn generate_plan(app_dir: &Path) -> Result<(), RuntimeError> {
	let output = Command::new("nixpacks")
		.arg("build")
		.arg(app_dir)
		.arg("-o")
		.arg(app_dir)
		.stdin(Stdio::null())
		.stdout(Stdio::piped())
		.stderr(Stdio::piped())
		.output()
		.await?;

	if !output.status.success() {
		return Err(RuntimeError::Nixpacks(format!(
			"nixpacks build (plan-only) failed: {}",
			String::from_utf8_lossy(&output.stderr).trim()
		)));
	}

	Ok(())
}
