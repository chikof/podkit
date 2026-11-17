//! Proves `source::prepare` against a real local git repo (no network, no
//! auth), the exact shape needed before handing the tar to `Podman::build_image`.

use std::io::Write;
use std::process::{Command, Stdio};

use agent::source;
use proto::{Build, GitAuth};

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

fn init_repo(dir: &std::path::Path) {
	git(dir, &["init", "-q", "-b", "main"]);
	git(dir, &["config", "user.email", "test@podkit.dev"]);
	git(dir, &["config", "user.name", "podkit test"]);
}

#[tokio::test]
async fn clones_local_repo_and_produces_valid_build_context() {
	let repo_dir = tempfile::tempdir().unwrap();
	init_repo(repo_dir.path());

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

	let work = tempfile::tempdir().unwrap();
	let data = tempfile::tempdir().unwrap();
	let repo_url = format!("file://{}", repo_dir.path().display());
	let prepared = source::prepare(
		work.path(),
		data.path(),
		&repo_url,
		"main",
		&GitAuth::None,
		&Build::Dockerfile {
			path: "Dockerfile".to_string(),
		},
	)
	.await
	.expect("prepare");

	assert_eq!(prepared.commit_sha, expected_sha);
	assert_eq!(prepared.dockerfile, "Dockerfile");

	let mut archive = tar::Archive::new(std::fs::File::open(&prepared.context_tar).unwrap());
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
	init_repo(repo_dir.path());
	std::fs::write(repo_dir.path().join("README.md"), "no dockerfile here").unwrap();
	git(repo_dir.path(), &["add", "-A"]);
	git(repo_dir.path(), &["commit", "-q", "-m", "initial"]);

	let work = tempfile::tempdir().unwrap();
	let data = tempfile::tempdir().unwrap();
	let repo_url = format!("file://{}", repo_dir.path().display());
	let err = source::prepare(
		work.path(),
		data.path(),
		&repo_url,
		"main",
		&GitAuth::None,
		&Build::Dockerfile {
			path: "Dockerfile".to_string(),
		},
	)
	.await
	.unwrap_err();
	assert!(err.to_string().contains("Dockerfile"), "got: {err}");
}
