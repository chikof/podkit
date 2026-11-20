//! Scheduled tasks: runs one shell command and streams its output.
//!
//! An exec runs in a container that is already up (the Podman exec API, the same one the
//! database backups use). Podman has no call to kill an exec, so the shell that runs it
//! writes its pid to a file first and a second exec kills that process and its children
//! when the run is cancelled or times out. A container task runs in a new container that
//! is removed afterwards, whatever happened.
//!
//! The server stops a run by dropping the connection. Everything that has to happen then
//! lives in a guard's `Drop`, because the handler's future is dropped without being polled
//! again.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use bollard::Docker;
use bollard::container::LogOutput;
use bollard::exec::{CreateExecOptions, StartExecResults};
use futures_util::StreamExt;
use proto::{CronRunRequest, CronTarget, Event, NetworkAttach, RegistryAuth, Stream, VolumeMount};
use tokio::sync::mpsc;

use crate::Ctx;
use crate::podman::{ContainerSpec, Podman};

/// Longest line sent on; the server clips again.
const MAX_LINE: usize = 8 * 1024;
/// How long to wait for Podman to report a finished container or exec.
const EXIT_WAIT: Duration = Duration::from_secs(30);

/// Runs the request and sends the terminal event.
pub async fn run(ctx: Arc<Ctx>, req: CronRunRequest, tx: mpsc::Sender<Event>) {
	let podman = ctx.podman();
	let result = match req.target {
		CronTarget::Exec { container, command } => {
			run_exec(&podman, req.run_id, &container, &command, &tx).await
		}
		CronTarget::Container {
			image,
			auth,
			command,
			env,
			attach,
			memory_bytes,
			cpu_cores,
			volumes,
		} => {
			let task = Container {
				run_id: req.run_id,
				image,
				auth,
				command,
				env,
				attach,
				memory_bytes,
				cpu_cores,
				volumes,
			};
			run_container(&podman, task, &tx).await
		}
	};
	let event = match result {
		Ok(code) => Event::Exited { code },
		Err(message) => Event::Failed { message },
	};
	let _ = tx.send(event).await;
}

/// Splits output into lines. A line ends at `\n`; one cut short by the end of the stream is
/// still a line.
#[derive(Default)]
struct Lines {
	out: Vec<u8>,
	err: Vec<u8>,
}

impl Lines {
	fn feed(&mut self, chunk: &LogOutput) -> Vec<(Stream, String)> {
		let (stream, buf) = match chunk {
			LogOutput::StdErr { .. } => (Stream::Stderr, &mut self.err),
			_ => (Stream::Stdout, &mut self.out),
		};
		buf.extend_from_slice(chunk.as_ref());
		let mut lines = Vec::new();
		while let Some(end) = buf.iter().position(|b| *b == b'\n') {
			let rest = buf.split_off(end + 1);
			let line = std::mem::replace(buf, rest);
			lines.push((stream, text(&line[..end])));
		}
		lines
	}

	fn finish(&mut self) -> Vec<(Stream, String)> {
		let mut lines = Vec::new();
		for (stream, buf) in [
			(Stream::Stdout, &mut self.out),
			(Stream::Stderr, &mut self.err),
		] {
			if !buf.is_empty() {
				lines.push((stream, text(buf)));
				buf.clear();
			}
		}
		lines
	}
}

fn text(bytes: &[u8]) -> String {
	let bytes = bytes.strip_suffix(b"\r").unwrap_or(bytes);
	let mut line = String::from_utf8_lossy(bytes).into_owned();
	if line.len() > MAX_LINE {
		let mut end = MAX_LINE;
		while !line.is_char_boundary(end) {
			end -= 1;
		}
		line.truncate(end);
	}
	line
}

async fn send(tx: &mpsc::Sender<Event>, lines: Vec<(Stream, String)>) -> Result<(), String> {
	for (stream, line) in lines {
		tx.send(Event::Log { stream, line })
			.await
			.map_err(|_| "the server stopped the run".to_owned())?;
	}
	Ok(())
}

async fn system(tx: &mpsc::Sender<Event>, line: impl Into<String>) -> Result<(), String> {
	send(tx, vec![(Stream::System, line.into())]).await
}

// ----- exec -----------------------------------------------------------------------------

/// Kills the process tree whose pid is in the pid file, from the leaves up. It walks
/// `/proc/<pid>/task/<pid>/children`, which needs no `ps` in the image.
const KILL_SCRIPT: &str = "p=$(cat \"$1\" 2>/dev/null) || exit 0; \
	k() { for c in $(cat /proc/$1/task/$1/children 2>/dev/null); do k $c; done; kill -KILL $1 2>/dev/null; }; \
	k \"$p\"; rm -f \"$1\"";

/// Writes the shell's pid, then becomes the command's shell, so the pid is the command's.
const WRAP_SCRIPT: &str = "echo $$ > \"$1\" 2>/dev/null; exec sh -c \"$2\"";

struct ExecGuard {
	docker: Docker,
	container: String,
	pid_file: String,
	armed: bool,
}

impl Drop for ExecGuard {
	fn drop(&mut self) {
		if !self.armed {
			return;
		}
		let docker = self.docker.clone();
		let container = std::mem::take(&mut self.container);
		let pid_file = std::mem::take(&mut self.pid_file);
		tokio::spawn(async move {
			if let Err(e) = quiet_exec(
				&docker,
				&container,
				vec![
					"sh".to_owned(),
					"-c".to_owned(),
					KILL_SCRIPT.to_owned(),
					"sh".to_owned(),
					pid_file,
				],
			)
			.await
			{
				tracing::warn!(%container, error = %e, "killing a cancelled cron exec failed");
			}
		});
	}
}

/// Runs a command in `container` and waits for it, discarding its output.
async fn quiet_exec(docker: &Docker, container: &str, cmd: Vec<String>) -> Result<(), String> {
	let created = docker
		.create_exec(
			container,
			CreateExecOptions::<String> {
				attach_stdout: Some(true),
				attach_stderr: Some(true),
				cmd: Some(cmd),
				..Default::default()
			},
		)
		.await
		.map_err(|e| format!("create exec in {container}: {e}"))?;
	match docker.start_exec(&created.id, None).await {
		Ok(StartExecResults::Attached { mut output, .. }) => {
			while output.next().await.is_some() {}
			Ok(())
		}
		Ok(StartExecResults::Detached) => Ok(()),
		Err(e) => Err(format!("start exec in {container}: {e}")),
	}
}

async fn run_exec(
	podman: &Podman,
	run_id: i64,
	container: &str,
	command: &str,
	tx: &mpsc::Sender<Event>,
) -> Result<i64, String> {
	let info = podman
		.inspect(container)
		.await
		.map_err(|e| format!("inspect {container}: {e}"))?
		.ok_or_else(|| format!("container {container} not found"))?;
	if info.state.and_then(|s| s.running) != Some(true) {
		return Err("the application's container is not running".to_owned());
	}

	let docker = podman.docker();
	let pid_file = format!("/tmp/.podkit-cron-{run_id}.pid");
	let created = docker
		.create_exec(
			container,
			CreateExecOptions::<String> {
				attach_stdout: Some(true),
				attach_stderr: Some(true),
				cmd: Some(vec![
					"sh".to_owned(),
					"-c".to_owned(),
					WRAP_SCRIPT.to_owned(),
					"sh".to_owned(),
					pid_file.clone(),
					command.to_owned(),
				]),
				..Default::default()
			},
		)
		.await
		.map_err(|e| format!("create exec in {container}: {e}"))?;
	let mut guard = ExecGuard {
		docker: docker.clone(),
		container: container.to_owned(),
		pid_file,
		armed: true,
	};
	let mut output = match docker.start_exec(&created.id, None).await {
		Ok(StartExecResults::Attached { output, .. }) => output,
		Ok(StartExecResults::Detached) => return Err("exec started detached".to_owned()),
		Err(e) => return Err(format!("start exec in {container}: {e}")),
	};

	let mut lines = Lines::default();
	while let Some(item) = output.next().await {
		let chunk = item.map_err(|e| format!("exec stream: {e}"))?;
		send(tx, lines.feed(&chunk)).await?;
	}
	send(tx, lines.finish()).await?;

	let code = exit_code(docker, &created.id).await;
	// Done: nothing left to kill. Only the pid file is.
	guard.armed = false;
	let _ = quiet_exec(
		docker,
		container,
		vec![
			"rm".to_owned(),
			"-f".to_owned(),
			std::mem::take(&mut guard.pid_file),
		],
	)
	.await;
	code
}

/// Podman can report `running` for a moment after the output closes.
async fn exit_code(docker: &Docker, exec_id: &str) -> Result<i64, String> {
	let deadline = tokio::time::Instant::now() + EXIT_WAIT;
	loop {
		let info = docker
			.inspect_exec(exec_id)
			.await
			.map_err(|e| format!("inspect exec: {e}"))?;
		if info.running != Some(true) {
			return info
				.exit_code
				.ok_or_else(|| "exec exit code unavailable".to_owned());
		}
		if tokio::time::Instant::now() >= deadline {
			return Err("exec still running after its output closed".to_owned());
		}
		tokio::time::sleep(Duration::from_millis(100)).await;
	}
}

// ----- container ------------------------------------------------------------------------

struct Container {
	run_id: i64,
	image: String,
	auth: Option<RegistryAuth>,
	command: String,
	env: Vec<(String, String)>,
	attach: Vec<NetworkAttach>,
	memory_bytes: Option<i64>,
	cpu_cores: Option<f64>,
	volumes: Vec<VolumeMount>,
}

/// Removes the container however the run ends.
struct ContainerGuard {
	podman: Podman,
	name: String,
}

impl Drop for ContainerGuard {
	fn drop(&mut self) {
		let podman = self.podman.clone();
		let name = std::mem::take(&mut self.name);
		tokio::spawn(async move {
			if let Err(e) = podman.remove(&name).await {
				tracing::warn!(container = %name, error = %e, "removing a cron container failed");
			}
		});
	}
}

fn validate(task: &Container) -> Result<(), String> {
	if task.image.is_empty()
		|| task.image.starts_with('-')
		|| task.image.contains(char::is_whitespace)
	{
		return Err("invalid image reference".to_owned());
	}
	for a in &task.attach {
		if !crate::networks::valid_member_network(&a.name) {
			return Err(format!("invalid network name {:?}", a.name));
		}
	}
	let mut seen = HashSet::new();
	for (key, _) in &task.env {
		if key.is_empty() || key.contains('=') || !seen.insert(key) {
			return Err(format!("invalid or repeated environment variable {key:?}"));
		}
	}
	for v in &task.volumes {
		if v.name.is_empty() || v.name.contains(':') || !v.path.starts_with('/') {
			return Err(format!("invalid volume {:?}", v.name));
		}
	}
	Ok(())
}

async fn run_container(
	podman: &Podman,
	task: Container,
	tx: &mpsc::Sender<Event>,
) -> Result<i64, String> {
	validate(&task)?;
	if podman
		.inspect_image(&task.image)
		.await
		.map_err(|e| format!("image check failed: {e}"))?
		.is_none()
	{
		system(tx, format!("pulling {}", task.image)).await?;
		let mut seen = HashSet::new();
		let mut progress = std::pin::pin!(podman.pull_image_as(&task.image, task.auth.as_ref()));
		while let Some(info) = progress.next().await {
			let info = info.map_err(|e| format!("image pull failed: {e}"))?;
			if let Some(status) = info.status
				&& seen.insert(status.clone())
			{
				system(tx, status).await?;
			}
		}
	}
	for a in &task.attach {
		podman
			.create_network(&a.name, a.internal)
			.await
			.map_err(|e| format!("network {} setup failed: {e}", a.name))?;
	}

	let name = format!("podkit-cron-{}", task.run_id);
	// A leftover of a run the agent lost track of.
	podman
		.remove(&name)
		.await
		.map_err(|e| format!("removing a stale container failed: {e}"))?;
	let mut networks = task.attach.iter();
	let first = networks.next().map(|a| a.name.clone()).unwrap_or_default();
	let spec = ContainerSpec {
		name: name.clone(),
		image: task.image.clone(),
		command: Some(vec![task.command.clone()]),
		entrypoint: Some(vec!["/bin/sh".to_owned(), "-c".to_owned()]),
		env: task.env.clone(),
		network: first,
		// No aliases: this container must not answer to the application's name.
		extra_networks: networks.map(|a| (a.name.clone(), Vec::new())).collect(),
		labels: vec![("podkit.cron".to_owned(), task.run_id.to_string())],
		binds: task
			.volumes
			.iter()
			.map(|v| {
				let mode = if v.read_only { ":ro" } else { "" };
				format!("{}:{}{mode}", v.name, v.path)
			})
			.collect(),
		memory_bytes: task.memory_bytes,
		cpu_cores: task.cpu_cores,
		..Default::default()
	};
	let _guard = ContainerGuard {
		podman: podman.clone(),
		name: name.clone(),
	};
	podman
		.create_container(spec)
		.await
		.map_err(|e| format!("creating the container failed: {e}"))?;
	podman
		.start(&name)
		.await
		.map_err(|e| format!("starting the container failed: {e}"))?;

	let mut lines = Lines::default();
	let mut logs = std::pin::pin!(podman.logs(&name, None, true));
	while let Some(item) = logs.next().await {
		let chunk = item.map_err(|e| format!("reading the container's output failed: {e}"))?;
		send(tx, lines.feed(&chunk)).await?;
	}
	send(tx, lines.finish()).await?;

	let deadline = tokio::time::Instant::now() + EXIT_WAIT;
	loop {
		let state = podman
			.inspect(&name)
			.await
			.map_err(|e| format!("inspect failed: {e}"))?
			.and_then(|i| i.state)
			.ok_or_else(|| "the container disappeared".to_owned())?;
		if state.running != Some(true) {
			return state
				.exit_code
				.ok_or_else(|| "the container's exit code is unavailable".to_owned());
		}
		if tokio::time::Instant::now() >= deadline {
			return Err("the container kept running after its output closed".to_owned());
		}
		tokio::time::sleep(Duration::from_millis(100)).await;
	}
}

#[cfg(test)]
mod tests {
	use bytes::Bytes;

	use super::*;

	fn out(s: &str) -> LogOutput {
		LogOutput::StdOut {
			message: Bytes::from(s.to_owned()),
		}
	}

	fn err(s: &str) -> LogOutput {
		LogOutput::StdErr {
			message: Bytes::from(s.to_owned()),
		}
	}

	#[test]
	fn lines_are_split_across_chunks_and_streams() {
		let mut lines = Lines::default();
		assert_eq!(
			lines.feed(&out("one\ntw")),
			[(Stream::Stdout, "one".to_owned())]
		);
		assert_eq!(
			lines.feed(&err("bad\r\n")),
			[(Stream::Stderr, "bad".to_owned())]
		);
		assert_eq!(
			lines.feed(&out("o\nthree")),
			[(Stream::Stdout, "two".to_owned())]
		);
		assert_eq!(lines.finish(), [(Stream::Stdout, "three".to_owned())]);
		assert!(lines.finish().is_empty());
	}

	#[test]
	fn a_long_line_is_cut_on_a_character_boundary() {
		let long = "é".repeat(MAX_LINE);
		let cut = text(long.as_bytes());
		assert!(cut.len() <= MAX_LINE);
		assert!(cut.chars().all(|c| c == 'é'));
	}

	#[test]
	fn a_container_task_is_validated() {
		let task = |edit: fn(&mut Container)| {
			let mut task = Container {
				run_id: 1,
				image: "busybox".to_owned(),
				auth: None,
				command: "date".to_owned(),
				env: vec![("A".to_owned(), "1".to_owned())],
				attach: vec![NetworkAttach {
					name: "podkit-env-1".to_owned(),
					aliases: Vec::new(),
					internal: false,
				}],
				memory_bytes: None,
				cpu_cores: None,
				volumes: Vec::new(),
			};
			edit(&mut task);
			validate(&task)
		};
		assert!(task(|_| {}).is_ok());
		assert!(task(|t| t.image = "-v".to_owned()).is_err());
		assert!(task(|t| t.attach[0].name = "podkit".to_owned()).is_err());
		assert!(task(|t| t.env.push(("A".to_owned(), "2".to_owned()))).is_err());
		assert!(task(|t| t.env[0].0 = "A=B".to_owned()).is_err());
	}
}
