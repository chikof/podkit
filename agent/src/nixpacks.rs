//! Build plan generation with the `nixpacks` CLI.

use std::path::Path;
use std::process::Stdio;

use tokio::process::Command;

/// Where [`generate_plan`] writes the Dockerfile, relative to `app_dir`.
pub const GENERATED_DOCKERFILE_PATH: &str = ".nixpacks/Dockerfile";

/// Runs `nixpacks build <app_dir> -o <app_dir>` to auto-detect the app's
/// stack and write a build plan (`.nixpacks/Dockerfile` + support files)
/// directly into `app_dir`. No docker/podman access is needed for this step,
/// it's pure plan generation.
///
/// From here the caller follows the same path as a user-supplied Dockerfile:
/// tar `app_dir` (now containing `.nixpacks/`) and build with
/// `dockerfile = GENERATED_DOCKERFILE_PATH`. This is proven end to end in
/// `agent/tests/nixpacks_spike.rs`.
///
/// Requires the `nixpacks` CLI on `PATH`.
///
/// # Errors
/// The `nixpacks` binary can't be run or it fails to produce a plan (e.g. an
/// unrecognized/unsupported stack).
pub async fn generate_plan(app_dir: &Path) -> Result<(), String> {
	let output = Command::new("nixpacks")
		.arg("build")
		.arg(app_dir)
		.arg("-o")
		.arg(app_dir)
		.stdin(Stdio::null())
		.stdout(Stdio::piped())
		.stderr(Stdio::piped())
		.kill_on_drop(true)
		.output()
		.await
		.map_err(|e| format!("failed to run nixpacks: {e}"))?;

	if !output.status.success() {
		return Err(format!(
			"nixpacks build (plan-only) failed: {}",
			String::from_utf8_lossy(&output.stderr).trim()
		));
	}

	Ok(())
}
