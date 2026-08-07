//! Proves `clone_to_tar` against a real local git repo (no network, no
//! auth), the exact shape needed before handing the tar to
//! `ContainerRuntime::build_image`.

use podkit_core::domain::application::BuildStrategy;
use std::io::Write;
use std::process::{Command, Stdio};

fn git(dir: &std::path::Path, args: &[&str]) {
	let status = Command::new("git")
		.arg("-C")
		.arg(dir)
		.args(args)
		.stdout(Stdio::null())
		.stderr(Stdio::null())
		.status()
		.expect("run git");
	assert!(status.success(), "git {args:?} failed");
}

#[tokio::test]
async fn clones_local_repo_and_produces_valid_build_context() {
	let repo_dir = tempfile::tempdir().unwrap();

	git(repo_dir.path(), &["init", "-q", "-b", "main"]);
	git(
		repo_dir.path(),
		&["config", "user.email", "test@podkit.dev"],
	);
	git(repo_dir.path(), &["config", "user.name", "podkit test"]);

	std::fs::write(
		repo_dir.path().join("Dockerfile"),
		"FROM docker.io/library/alpine:3.21\nRUN echo cloned-ok > /marker\n",
	)
	.unwrap();
	let mut readme = std::fs::File::create(repo_dir.path().join("README.md")).unwrap();
	writeln!(readme, "test repo").unwrap();

	git(repo_dir.path(), &["add", "-A"]);
	git(repo_dir.path(), &["commit", "-q", "-m", "initial"]);

	let expected_sha = String::from_utf8(
		Command::new("git")
			.arg("-C")
			.arg(repo_dir.path())
			.args(["rev-parse", "HEAD"])
			.output()
			.unwrap()
			.stdout,
	)
	.unwrap()
	.trim()
	.to_string();

	let repo_url = format!("file://{}", repo_dir.path().display());
	let cloned = runtime::clone_to_tar(
		&repo_url,
		"main",
		None,
		BuildStrategy::Dockerfile,
		"Dockerfile",
	)
	.await
	.expect("clone_to_tar");

	assert_eq!(cloned.commit_sha, expected_sha);

	// unpack and verify Dockerfile made it through, .git did not.
	let mut archive = tar::Archive::new(std::io::Cursor::new(&cloned.context_tar));
	let mut saw_dockerfile = false;
	let mut saw_git_dir = false;
	for entry in archive.entries().unwrap() {
		let entry = entry.unwrap();
		let path = entry.path().unwrap().to_string_lossy().to_string();
		if path == "./Dockerfile" || path == "Dockerfile" {
			saw_dockerfile = true;
		}
		if path.contains(".git") {
			saw_git_dir = true;
		}
	}
	assert!(saw_dockerfile, "Dockerfile missing from build context tar");
	assert!(!saw_git_dir, ".git leaked into build context tar");
}

#[tokio::test]
async fn missing_dockerfile_is_a_clear_error() {
	let repo_dir = tempfile::tempdir().unwrap();
	git(repo_dir.path(), &["init", "-q", "-b", "main"]);
	git(
		repo_dir.path(),
		&["config", "user.email", "test@podkit.dev"],
	);
	git(repo_dir.path(), &["config", "user.name", "podkit test"]);
	std::fs::write(repo_dir.path().join("README.md"), "no dockerfile here").unwrap();
	git(repo_dir.path(), &["add", "-A"]);
	git(repo_dir.path(), &["commit", "-q", "-m", "initial"]);

	let repo_url = format!("file://{}", repo_dir.path().display());
	let err = runtime::clone_to_tar(
		&repo_url,
		"main",
		None,
		BuildStrategy::Dockerfile,
		"Dockerfile",
	)
	.await
	.unwrap_err();
	assert!(err.to_string().contains("Dockerfile"), "got: {err}");
}
