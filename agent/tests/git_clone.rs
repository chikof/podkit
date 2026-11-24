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
		("main", None),
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
		("main", None),
		&GitAuth::None,
		&Build::Dockerfile {
			path: "Dockerfile".to_string(),
		},
	)
	.await
	.unwrap_err();
	assert!(err.to_string().contains("Dockerfile"), "got: {err}");
}

fn rev(dir: &std::path::Path) -> String {
	let out = Command::new("git")
		.arg("-C")
		.arg(dir)
		.args(["rev-parse", "HEAD"])
		.output()
		.unwrap();
	String::from_utf8(out.stdout).unwrap().trim().to_owned()
}

/// A bare repository (what a git server holds) and a working copy that pushes to it.
fn bare_with_work() -> (tempfile::TempDir, tempfile::TempDir) {
	let bare = tempfile::tempdir().unwrap();
	git(bare.path(), &["init", "-q", "--bare", "-b", "main"]);
	let work = tempfile::tempdir().unwrap();
	init_repo(work.path());
	git(
		work.path(),
		&["remote", "add", "origin", &bare.path().to_string_lossy()],
	);
	(bare, work)
}

fn commit(work: &std::path::Path, marker: &str) -> String {
	std::fs::write(
		work.join("Dockerfile"),
		format!("FROM docker.io/library/alpine:3.21\nRUN echo {marker} > /marker\n"),
	)
	.unwrap();
	git(work, &["add", "-A"]);
	git(work, &["commit", "-q", "-m", marker]);
	git(work, &["push", "-q", "-f", "origin", "main"]);
	rev(work)
}

fn dockerfile_of(tar_path: &std::path::Path) -> String {
	let mut archive = tar::Archive::new(std::fs::File::open(tar_path).unwrap());
	for entry in archive.entries().unwrap() {
		let mut entry = entry.unwrap();
		if entry
			.path()
			.unwrap()
			.to_string_lossy()
			.ends_with("Dockerfile")
		{
			let mut text = String::new();
			std::io::Read::read_to_string(&mut entry, &mut text).unwrap();
			return text;
		}
	}
	panic!("no Dockerfile in the context");
}

async fn prepare_at(
	bare: &std::path::Path,
	commit: Option<&str>,
) -> Result<source::Prepared, source::SourceError> {
	let work = tempfile::tempdir().unwrap();
	let data = tempfile::tempdir().unwrap();
	source::prepare(
		work.path(),
		data.path(),
		&format!("file://{}", bare.display()),
		("main", commit),
		&GitAuth::None,
		&Build::Dockerfile {
			path: "Dockerfile".to_string(),
		},
	)
	.await
	.map(|mut p| {
		// The temp dirs go when this returns; keep the tar readable.
		let keep = std::env::temp_dir().join(format!("pk-git-clone-{}.tar", std::process::id()));
		std::fs::copy(&p.context_tar, &keep).unwrap();
		p.context_tar = keep;
		p
	})
}

#[tokio::test]
async fn previews_agent_checks_out_the_pinned_commit_after_the_branch_moved() {
	let (bare, work) = bare_with_work();
	let approved = commit(work.path(), "approved-code");
	let pushed_later = commit(work.path(), "malicious-code");
	assert_ne!(approved, pushed_later);

	// Without a pin the build gets the tip.
	let tip = prepare_at(bare.path(), None).await.unwrap();
	assert_eq!(tip.commit_sha, pushed_later);

	// With the pin it gets exactly the approved commit, though the branch is elsewhere.
	let pinned = prepare_at(bare.path(), Some(&approved)).await.unwrap();
	assert_eq!(pinned.commit_sha, approved);
	let text = dockerfile_of(&pinned.context_tar);
	assert!(text.contains("approved-code"), "{text}");
	assert!(!text.contains("malicious-code"), "{text}");
	let _ = std::fs::remove_file(&pinned.context_tar);
	let _ = std::fs::remove_file(&tip.context_tar);
}

#[tokio::test]
async fn previews_agent_refuses_a_pin_that_is_not_a_full_commit_or_not_there() {
	let (bare, work) = bare_with_work();
	let head = commit(work.path(), "one");

	for bad in ["main", "--upload-pack=x", &head[..12]] {
		let err = prepare_at(bare.path(), Some(bad)).await.unwrap_err();
		assert!(
			err.to_string().contains("not a full commit id"),
			"{bad}: {err}"
		);
	}
	let missing = "0123456789abcdef0123456789abcdef01234567";
	let err = prepare_at(bare.path(), Some(missing)).await.unwrap_err();
	assert!(err.to_string().contains("moved after approval"), "{err}");
}
