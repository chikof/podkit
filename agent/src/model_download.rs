//! `POST /v1/model-downloads`: download Hugging Face files into a volume with
//! a throwaway `hf download` container.
//!
//! The volume survives failures and cancellation, and `hf download
//! --local-dir` keeps partial files in `/models/.cache`, so running the same
//! request again resumes. Cancellation is dropping the future (the route does
//! that when the client disconnects); [`Cleanup`] then removes the container.
//!
//! The token only ever lives in the container's environment. It is never put
//! in the command line, and every line the container prints is scrubbed of it
//! before it leaves the agent.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;

use bollard::Docker;
use bollard::container::LogOutput;
use bollard::exec::{CreateExecOptions, StartExecResults};
use futures_util::StreamExt;
use proto::{Event, GATED_MODEL_MESSAGE, ModelDownloadRequest, Stream};
use tokio::sync::mpsc;
use tracing::warn;

use crate::Ctx;
use crate::podman::{ContainerSpec, Podman};

/// Base image of the downloader; `hf` is installed into it at start.
pub const IMAGE: &str = "docker.io/library/python:3.13-slim";
/// Pinned so a release of the CLI cannot change what a download does. The
/// 2.x line ships `hf` without the old `[cli]` extra.
pub const HF_PACKAGE: &str = "huggingface_hub==2.4.0";
const MOUNT: &str = "/models";
const PROGRESS_INTERVAL: Duration = Duration::from_secs(3);
const MEMORY_BYTES: i64 = 4 << 30;
const TAIL_LINES: usize = 20;
const EXIT_POLLS: u32 = 50;

/// Runs `pip install` and then replaces itself with `hf download "$@"`, so the
/// request's values are arguments and never part of a shell script.
const SCRIPT: &str = "pip install --no-cache-dir -q --root-user-action=ignore \
	\"$HF_PACKAGE\" && exec hf download \"$@\"";

/// Runs a download, sending logs and progress on `tx` and exactly one terminal
/// event unless the client went away.
pub async fn run(ctx: Arc<Ctx>, req: ModelDownloadRequest, tx: mpsc::Sender<Event>) {
	let terminal = match execute(&ctx, &req, &tx).await {
		Ok(()) => Event::Done,
		Err(message) => Event::Failed {
			message: scrub(&message, req.token.as_deref()),
		},
	};
	let _ = tx.send(terminal).await;
}

/// Removes the container when the download ends or is cancelled.
struct Cleanup {
	podman: Podman,
	name: String,
}

impl Drop for Cleanup {
	fn drop(&mut self) {
		let (podman, name) = (self.podman.clone(), std::mem::take(&mut self.name));
		tokio::spawn(async move {
			if let Err(e) = podman.remove(&name).await {
				warn!(container = %name, error = %e, "removing downloader container failed");
			}
		});
	}
}

/// What can go in a Podman volume or container name, an HF repo id or a
/// revision. Keeps odd input out of argv and API paths.
pub(crate) fn valid_ref(s: &str, allow_slash: bool) -> bool {
	!s.is_empty()
		&& s.len() <= 255
		&& !s.starts_with(['-', '.', '/'])
		&& !s.contains("..")
		&& s.bytes().all(|b| {
			b.is_ascii_alphanumeric()
				|| matches!(b, b'_' | b'.' | b'-')
				|| (allow_slash && b == b'/')
		})
}

fn validate(req: &ModelDownloadRequest) -> Result<(), String> {
	if !valid_ref(&req.volume, false) {
		return Err(format!("invalid volume name {:?}", req.volume));
	}
	if !valid_ref(&req.repo, true) {
		return Err(format!("invalid repo {:?}", req.repo));
	}
	if !valid_ref(&req.revision, true) {
		return Err(format!("invalid revision {:?}", req.revision));
	}
	for pattern in &req.include {
		if pattern.is_empty() || pattern.starts_with('-') || pattern.chars().any(char::is_control) {
			return Err(format!("invalid include pattern {pattern:?}"));
		}
	}
	Ok(())
}

/// Arguments of `hf download`, after the program name.
fn hf_args(req: &ModelDownloadRequest) -> Vec<String> {
	let mut args = vec![
		req.repo.clone(),
		"--revision".to_owned(),
		req.revision.clone(),
	];
	for pattern in &req.include {
		args.push("--include".to_owned());
		args.push(pattern.clone());
	}
	args.push("--local-dir".to_owned());
	args.push(MOUNT.to_owned());
	args
}

fn container_spec(req: &ModelDownloadRequest, name: &str) -> ContainerSpec {
	let mut env = vec![
		("HF_PACKAGE".to_owned(), HF_PACKAGE.to_owned()),
		("HF_HUB_DISABLE_PROGRESS_BARS".to_owned(), "1".to_owned()),
		("HF_HUB_DISABLE_TELEMETRY".to_owned(), "1".to_owned()),
		("NO_COLOR".to_owned(), "1".to_owned()),
		("PIP_DISABLE_PIP_VERSION_CHECK".to_owned(), "1".to_owned()),
	];
	if let Some(token) = req.token.as_ref().filter(|t| !t.is_empty()) {
		env.push(("HF_TOKEN".to_owned(), token.clone()));
	}
	let mut command = vec![
		"sh".to_owned(),
		"-c".to_owned(),
		SCRIPT.to_owned(),
		"sh".to_owned(),
	];
	command.extend(hf_args(req));
	ContainerSpec {
		name: name.to_owned(),
		image: IMAGE.to_owned(),
		command: Some(command),
		env,
		labels: vec![("podkit.model-download".to_owned(), req.volume.clone())],
		binds: vec![format!("{}:{MOUNT}", req.volume)],
		memory_bytes: Some(MEMORY_BYTES),
		..Default::default()
	}
}

fn container_name(volume: &str) -> String {
	format!("podkit-modeldl-{volume}")
}

/// Replaces the token in `line`. Tokens are never printed by `hf`, but a
/// failing command line or a proxy error page could echo one.
fn scrub(line: &str, token: Option<&str>) -> String {
	match token.filter(|t| !t.is_empty()) {
		Some(token) => line.replace(token, "***"),
		None => line.to_owned(),
	}
}

/// Parses `du -sb` output: `<bytes>\t<path>`.
fn parse_du(output: &str) -> Option<u64> {
	output.split_whitespace().next()?.parse().ok()
}

/// Whether the output of a failed `hf download` says Hugging Face refused access.
fn looks_gated(tail: &str) -> bool {
	[
		"401 Client Error",
		"403 Client Error",
		"GatedRepoError",
		"Cannot access gated repo",
		"requires approval",
		"Access denied",
		"Invalid user token",
	]
	.iter()
	.any(|needle| tail.contains(needle))
}

fn failure_message(code: i64, tail: &VecDeque<String>) -> String {
	let text = tail
		.iter()
		.map(String::as_str)
		.collect::<Vec<_>>()
		.join("\n");
	if looks_gated(&text) {
		return GATED_MODEL_MESSAGE.to_owned();
	}
	if text.is_empty() {
		format!("hf download exited with code {code}")
	} else {
		format!("hf download exited with code {code}: {text}")
	}
}

async fn emit(tx: &mpsc::Sender<Event>, event: Event) -> Result<(), String> {
	tx.send(event)
		.await
		.map_err(|_| "client disconnected".to_owned())
}

/// Size in bytes of everything under the mount, via `du` inside the container.
async fn volume_bytes(docker: &Docker, container: &str) -> Option<u64> {
	let exec = docker
		.create_exec(
			container,
			CreateExecOptions::<String> {
				attach_stdout: Some(true),
				attach_stderr: Some(false),
				cmd: Some(vec!["du".to_owned(), "-sb".to_owned(), MOUNT.to_owned()]),
				..Default::default()
			},
		)
		.await
		.ok()?;
	let StartExecResults::Attached { mut output, .. } =
		docker.start_exec(&exec.id, None).await.ok()?
	else {
		return None;
	};
	let mut text = String::new();
	while let Some(Ok(chunk)) = output.next().await {
		if let LogOutput::StdOut { message } = chunk {
			text.push_str(&String::from_utf8_lossy(&message));
		}
	}
	parse_du(&text)
}

pub(crate) async fn ensure_image(
	podman: &Podman,
	image: &str,
	tx: &mpsc::Sender<Event>,
) -> Result<(), String> {
	if podman
		.inspect_image(image)
		.await
		.map_err(|e| format!("image check failed: {e}"))?
		.is_some()
	{
		return Ok(());
	}
	emit(
		tx,
		Event::Log {
			stream: Stream::System,
			line: format!("pulling {image}"),
		},
	)
	.await?;
	let mut pull = std::pin::pin!(podman.pull_image(image));
	while let Some(item) = pull.next().await {
		item.map_err(|e| format!("image pull failed: {e}"))?;
	}
	Ok(())
}

async fn execute(
	ctx: &Ctx,
	req: &ModelDownloadRequest,
	tx: &mpsc::Sender<Event>,
) -> Result<(), String> {
	validate(req)?;
	let podman = ctx.podman();
	let token = req.token.as_deref();

	ensure_image(&podman, IMAGE, tx).await?;
	podman
		.ensure_volume(&req.volume)
		.await
		.map_err(|e| format!("volume {} setup failed: {e}", req.volume))?;

	let name = container_name(&req.volume);
	// A container left by a crashed agent would hold the name.
	podman
		.remove(&name)
		.await
		.map_err(|e| format!("removing stale downloader failed: {e}"))?;
	podman
		.create_container(container_spec(req, &name))
		.await
		.map_err(|e| format!("downloader create failed: {e}"))?;
	let _cleanup = Cleanup {
		podman: podman.clone(),
		name: name.clone(),
	};
	podman
		.start(&name)
		.await
		.map_err(|e| format!("downloader start failed: {e}"))?;
	emit(
		tx,
		Event::Log {
			stream: Stream::System,
			line: format!(
				"downloading {}@{} into {}",
				req.repo, req.revision, req.volume
			),
		},
	)
	.await?;

	let mut logs = std::pin::pin!(podman.logs(&name, None, true));
	let mut ticker = tokio::time::interval(PROGRESS_INTERVAL);
	let mut tail: VecDeque<String> = VecDeque::new();
	loop {
		tokio::select! {
			item = logs.next() => {
				let Some(item) = item else { break };
				let output = item.map_err(|e| format!("reading downloader logs failed: {e}"))?;
				let stream = match output {
					LogOutput::StdErr { .. } => Stream::Stderr,
					_ => Stream::Stdout,
				};
				for line in String::from_utf8_lossy(output.as_ref()).lines() {
					let line = scrub(line, token);
					if line.trim().is_empty() {
						continue;
					}
					if tail.len() == TAIL_LINES {
						tail.pop_front();
					}
					tail.push_back(line.clone());
					emit(tx, Event::Log { stream, line }).await?;
				}
			}
			_ = ticker.tick() => {
				if let Some(bytes) = volume_bytes(&ctx.docker, &name).await {
					emit(tx, Event::Progress { bytes }).await?;
				}
			}
		}
	}

	let code = exit_code(&podman, &name).await?;
	if code != 0 {
		return Err(failure_message(code, &tail));
	}
	Ok(())
}

/// Exit code of the finished container. Podman can report it running briefly
/// after the log stream closes.
async fn exit_code(podman: &Podman, name: &str) -> Result<i64, String> {
	for _ in 0..EXIT_POLLS {
		let info = podman
			.inspect(name)
			.await
			.map_err(|e| format!("inspect downloader failed: {e}"))?
			.ok_or_else(|| "downloader container vanished".to_owned())?;
		let state = info.state.unwrap_or_default();
		if state.running != Some(true) {
			return Ok(state.exit_code.unwrap_or(-1));
		}
		tokio::time::sleep(Duration::from_millis(200)).await;
	}
	Err("downloader still running after its logs closed".to_owned())
}

#[cfg(test)]
mod tests {
	use super::*;

	type Mutation = fn(&mut ModelDownloadRequest);

	fn request() -> ModelDownloadRequest {
		ModelDownloadRequest {
			volume: "model-files-7".to_owned(),
			repo: "Qwen/Qwen3-4B-GGUF".to_owned(),
			revision: "main".to_owned(),
			include: vec!["*Q4_K_M.gguf".to_owned()],
			token: Some("hf_secrettoken123".to_owned()),
		}
	}

	#[test]
	fn token_goes_in_env_and_never_in_the_command() {
		let spec = container_spec(&request(), "podkit-modeldl-model-files-7");
		let command = spec.command.unwrap();
		assert!(command.iter().all(|a| !a.contains("hf_secrettoken123")));
		assert!(
			spec.env
				.contains(&("HF_TOKEN".to_owned(), "hf_secrettoken123".to_owned()))
		);
		assert_eq!(spec.binds, vec!["model-files-7:/models"]);
	}

	#[test]
	fn without_a_token_no_hf_token_is_set() {
		let mut req = request();
		req.token = None;
		let spec = container_spec(&req, "n");
		assert!(spec.env.iter().all(|(k, _)| k != "HF_TOKEN"));
	}

	#[test]
	fn command_runs_hf_download_with_the_request_as_arguments() {
		let command = container_spec(&request(), "n").command.unwrap();
		assert_eq!(&command[..2], ["sh", "-c"]);
		assert!(command[2].contains("exec hf download \"$@\""));
		assert_eq!(
			&command[4..],
			[
				"Qwen/Qwen3-4B-GGUF",
				"--revision",
				"main",
				"--include",
				"*Q4_K_M.gguf",
				"--local-dir",
				"/models"
			]
		);
	}

	#[test]
	fn debug_output_hides_the_token() {
		let shown = format!("{:?}", request());
		assert!(!shown.contains("hf_secrettoken123"));
		assert!(shown.contains("redacted"));
	}

	#[test]
	fn scrub_removes_the_token() {
		assert_eq!(
			scrub(
				"Bearer hf_secrettoken123 rejected",
				Some("hf_secrettoken123")
			),
			"Bearer *** rejected"
		);
		assert_eq!(scrub("plain", None), "plain");
		assert_eq!(scrub("plain", Some("")), "plain");
	}

	#[test]
	fn validation_table() {
		assert!(validate(&request()).is_ok());
		let cases: [(&str, Mutation); 7] = [
			("volume with slash", |r| r.volume = "a/b".to_owned()),
			("empty volume", |r| r.volume = String::new()),
			("repo option", |r| r.repo = "--help".to_owned()),
			("repo traversal", |r| r.repo = "a/../b".to_owned()),
			("revision space", |r| r.revision = "a b".to_owned()),
			("include option", |r| {
				r.include = vec!["--token=x".to_owned()];
			}),
			("include control", |r| r.include = vec!["a\nb".to_owned()]),
		];
		for (what, mutate) in cases {
			let mut req = request();
			mutate(&mut req);
			assert!(validate(&req).is_err(), "{what}");
		}
		let mut pr = request();
		pr.revision = "refs/pr/1".to_owned();
		assert!(validate(&pr).is_ok());
	}

	#[test]
	fn du_parsing() {
		assert_eq!(parse_du("1185376\t/models\n"), Some(1_185_376));
		assert_eq!(parse_du(""), None);
		assert_eq!(parse_du("du: cannot access"), None);
	}

	#[test]
	fn gated_failures_get_the_license_message() {
		let tail =
			VecDeque::from(["huggingface_hub.errors.GatedRepoError: 403 Client Error.".to_owned()]);
		assert_eq!(failure_message(1, &tail), GATED_MODEL_MESSAGE);
		let tail = VecDeque::from([
			"Error: Access denied. Model 'google/gemma-2-2b' requires approval.".to_owned(),
		]);
		assert_eq!(failure_message(1, &tail), GATED_MODEL_MESSAGE);
		let tail = VecDeque::from(["Disk quota exceeded".to_owned()]);
		assert_eq!(
			failure_message(1, &tail),
			"hf download exited with code 1: Disk quota exceeded"
		);
		assert_eq!(
			failure_message(2, &VecDeque::new()),
			"hf download exited with code 2"
		);
	}
}
