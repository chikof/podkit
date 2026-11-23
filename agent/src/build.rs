//! `POST /v1/build`: clone a repository and build its image on this server, without
//! running it. This is what a build server does; the control plane then moves the image to
//! the server that runs it (`GET /v1/images/export` here, `POST /v1/images/import` there).

use std::sync::Arc;

use proto::{BuildRequest, Event};
use tokio::sync::mpsc;

use crate::Ctx;
use crate::deploy::build_git_image;

/// Whether `service` is safe in an image tag.
fn valid_service(service: &str) -> bool {
	!service.is_empty()
		&& service.len() <= 128
		&& service
			.bytes()
			.all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'))
}

/// Whether `reference` names an image podkit built (`podkit-<service>:<tag>`), the only ones
/// the image routes will move.
#[must_use]
pub fn valid_image_reference(reference: &str) -> bool {
	reference.starts_with("podkit-")
		&& reference.len() <= 255
		&& reference
			.bytes()
			.all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-' | b':'))
}

/// Runs a build, sending progress on `tx` and exactly one terminal event
/// ([`Event::Built`] or [`Event::Failed`]) unless the client went away.
pub async fn run(ctx: Arc<Ctx>, req: BuildRequest, tx: mpsc::Sender<Event>) {
	let terminal = if valid_service(&req.service) {
		match build_git_image(
			&ctx,
			&ctx.podman(),
			&req.service,
			(&req.repo_url, &req.git_ref, &req.auth, &req.build),
			&req.options,
			&tx,
		)
		.await
		{
			Ok((image, commit_sha)) => Event::Built { image, commit_sha },
			Err(message) => Event::Failed { message },
		}
	} else {
		Event::Failed {
			message: "invalid service name".to_string(),
		}
	};
	let _ = tx.send(terminal).await;
}

#[cfg(test)]
mod build_cache {
	use super::*;

	use crate::podman::build_query;

	fn options(cache: bool, cpu: Option<f64>, memory: Option<i64>) -> proto::BuildOptions {
		proto::BuildOptions {
			cache,
			cpu_cores: cpu,
			memory_bytes: memory,
			cache_max_bytes: None,
		}
	}

	#[test]
	fn build_cache_default_options_cap_nothing_and_reuse_layers() {
		let q = build_query("t:1", "Dockerfile", &proto::BuildOptions::default());
		assert!(!q.nocache);
		assert_eq!(q.memory, None);
		assert_eq!(q.cpuquota, None);
		assert_eq!(q.cpuperiod, None);
	}

	#[test]
	fn build_cache_off_asks_for_a_clean_build() {
		let q = build_query("t:1", "Dockerfile", &options(false, None, None));
		assert!(q.nocache);
	}

	#[test]
	fn build_cache_limits_reach_the_build_request() {
		let q = build_query(
			"t:1",
			"Dockerfile",
			&options(true, Some(1.5), Some(512 * 1024 * 1024)),
		);
		assert_eq!(q.memory, Some(512 * 1024 * 1024));
		assert_eq!(q.memswap, q.memory, "no swap on top of the cap");
		assert_eq!(q.cpuperiod, Some(100_000));
		assert_eq!(q.cpuquota, Some(150_000));
	}

	#[test]
	fn build_cache_a_huge_memory_cap_is_clamped_not_wrapped() {
		let q = build_query("t:1", "Dockerfile", &options(true, None, Some(64 << 30)));
		assert_eq!(q.memory, Some(i32::MAX));
	}

	#[test]
	fn build_cache_only_podkit_images_can_be_moved() {
		assert!(valid_image_reference("podkit-app-123:0123456789ab"));
		for bad in [
			"alpine:3",
			"podkit-x/../../etc",
			"podkit-a b",
			"",
			"docker.io/library/postgres:18",
		] {
			assert!(!valid_image_reference(bad), "{bad:?}");
		}
	}
}
