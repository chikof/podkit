#![allow(dead_code)]

use std::path::{Path, PathBuf};

use agent::podman::Podman;
use futures_util::StreamExt;

/// Writes a tar holding just `Dockerfile` to `<dir>/context.tar`.
pub fn dockerfile_tar(dir: &Path, dockerfile: &[u8]) -> PathBuf {
	let out = dir.join("context.tar");
	let mut tar = tar::Builder::new(std::fs::File::create(&out).unwrap());
	let mut header = tar::Header::new_gnu();
	header.set_path("Dockerfile").unwrap();
	header.set_size(dockerfile.len() as u64);
	header.set_mode(0o644);
	header.set_cksum();
	tar.append(&header, dockerfile).unwrap();
	tar.finish().unwrap();
	out
}

/// Tars the whole of `src` to `out`.
pub fn directory_tar(src: &Path, out: &Path) {
	let mut tar = tar::Builder::new(std::fs::File::create(out).unwrap());
	tar.append_dir_all(".", src).unwrap();
	tar.finish().unwrap();
}

/// Builds `tag` and drains the output, panicking on a build error.
pub async fn build(podman: &Podman, tag: &str, dockerfile: &str, context_tar: &Path) {
	let output = podman
		.build_image(tag, dockerfile, context_tar)
		.await
		.expect("open build context");
	let mut output = std::pin::pin!(output);
	while let Some(chunk) = output.next().await {
		chunk.expect("build output");
	}
}
