//! Two builds of the same application against a real podman socket: the second one reuses the
//! layers of the first and is faster. A third, with the cache off, is slow again.
//! Run explicitly: `cargo test -p agent --test build_cache -- --ignored --nocapture`.
//!
//! The images are tagged `podkit-t1d45e7ee-cache:*` and removed at the end. Nothing here prunes
//! the cache: that would touch the layers of everything else in the store.

mod common;

use std::time::{Duration, Instant};

use agent::podman::{Podman, local_socket_path};
use futures_util::StreamExt;

/// Builds with `options` and returns how long it took.
async fn timed_build(
	podman: &Podman,
	tag: &str,
	context: &std::path::Path,
	options: &proto::BuildOptions,
) -> Duration {
	let start = Instant::now();
	let output = podman
		.build_image(tag, "Dockerfile", context, options)
		.await
		.expect("open build context");
	let mut output = std::pin::pin!(output);
	while let Some(chunk) = output.next().await {
		chunk.expect("build output");
	}
	start.elapsed()
}

#[tokio::test]
#[ignore = "requires a live podman.sock"]
async fn build_cache_live_the_second_build_is_faster() {
	let podman = Podman::connect(&local_socket_path()).expect("connect to podman.sock");
	let dir = tempfile::tempdir().unwrap();
	// Each slow step stands in for an install or a compile; a cache mount keeps what a package
	// manager downloaded between builds (see docs/builds.md).
	let dockerfile = b"FROM docker.io/library/alpine:3.21\n\
		RUN sleep 4 && echo deps > /deps\n\
		RUN --mount=type=cache,target=/var/cache/apk sleep 4 && echo build > /build\n\
		CMD [\"cat\", \"/build\"]\n";
	let context = common::dockerfile_tar(dir.path(), dockerfile);
	let cold_tag = "podkit-t1d45e7ee-cache:cold";
	let warm_tag = "podkit-t1d45e7ee-cache:warm";
	let clean_tag = "podkit-t1d45e7ee-cache:clean";
	let cached = proto::BuildOptions::default();
	let uncached = proto::BuildOptions {
		cache: false,
		..proto::BuildOptions::default()
	};

	// Pull the base image first so the cold build does not measure the download.
	timed_build(
		&podman,
		"podkit-t1d45e7ee-cache:warmup",
		&context,
		&uncached,
	)
	.await;

	let cold = timed_build(&podman, cold_tag, &context, &uncached).await;
	let warm = timed_build(&podman, warm_tag, &context, &cached).await;
	let no_cache = timed_build(&podman, clean_tag, &context, &uncached).await;
	println!("cold (cache off): {cold:?}");
	println!("with cache on (layers reused): {warm:?}");
	println!("again with cache off:     {no_cache:?}");

	for tag in [
		cold_tag,
		warm_tag,
		clean_tag,
		"podkit-t1d45e7ee-cache:warmup",
	] {
		let _ = podman.remove_image(tag).await;
	}

	assert!(
		warm * 2 < cold,
		"a build that reuses its layers should take under half as long ({warm:?} vs {cold:?})"
	);
	assert!(
		no_cache > warm * 2,
		"turning the cache off builds every step again ({no_cache:?} vs {warm:?})"
	);
}
