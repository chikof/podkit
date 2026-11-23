//! The web terminal: an interactive shell in a running container, over one WebSocket.
//!
//! The agent creates a TTY exec (`create_exec` with `tty` and stdin attached), starts it
//! attached (`start_exec`) and copies bytes both ways. The wire format is in
//! [`proto::TerminalControl`] and [`proto::TerminalNotice`]. Podman's API for this:
//! <https://docs.podman.io/en/latest/_static/api.html#tag/exec-(compat)> (compat
//! `POST /containers/{id}/exec`, `POST /exec/{id}/start`, `POST /exec/{id}/resize`).
//!
//! Closing the socket ends the session: stdin is closed, which makes an interactive shell
//! exit on its own.

use std::time::Duration;

use axum::extract::ws::{CloseFrame, Message, WebSocket};
use bollard::container::LogOutput;
use bollard::exec::{CreateExecOptions, ResizeExecOptions, StartExecOptions, StartExecResults};
use futures_util::StreamExt;
use proto::{TERMINAL_MAX_COLS, TERMINAL_MAX_ROWS, TerminalControl, TerminalNotice};
use tokio::io::AsyncWriteExt;
use tracing::{debug, warn};

use crate::podman::{Podman, PodmanError};

/// Picks `bash` when the image has it, else `sh`, and replaces itself with the shell so
/// the exec's process is the shell.
const PICK_SHELL: &str = "if command -v bash >/dev/null 2>&1; then exec bash; else exec sh; fi";

/// The window size to use: `0` or absent falls back to 80x24, big values are clamped.
#[must_use]
pub fn clamp_size(cols: Option<u16>, rows: Option<u16>) -> (u16, u16) {
	let fit =
		|v: Option<u16>, default: u16, max: u16| v.filter(|v| *v > 0).unwrap_or(default).min(max);
	(
		fit(cols, 80, TERMINAL_MAX_COLS),
		fit(rows, 24, TERMINAL_MAX_ROWS),
	)
}

/// Creates the TTY exec in `container`; returns its id. Nothing runs until [`serve`].
///
/// # Errors
/// The container is missing (404) or not running (409), or Podman is unreachable.
pub async fn prepare(podman: &Podman, container: &str) -> Result<String, PodmanError> {
	let created = podman
		.docker()
		.create_exec(
			container,
			CreateExecOptions::<String> {
				attach_stdin: Some(true),
				attach_stdout: Some(true),
				attach_stderr: Some(true),
				tty: Some(true),
				env: Some(vec!["TERM=xterm-256color".to_owned()]),
				cmd: Some(vec![
					"sh".to_owned(),
					"-c".to_owned(),
					PICK_SHELL.to_owned(),
				]),
				..Default::default()
			},
		)
		.await?;
	Ok(created.id)
}

async fn notice(socket: &mut WebSocket, notice: &TerminalNotice) {
	if let Ok(text) = serde_json::to_string(notice) {
		let _ = socket.send(Message::Text(text.into())).await;
	}
}

/// Runs the session until the shell or the socket ends.
pub async fn serve(podman: Podman, exec_id: String, size: (u16, u16), mut socket: WebSocket) {
	let started = podman
		.docker()
		.start_exec(
			&exec_id,
			Some(StartExecOptions {
				detach: false,
				tty: true,
				output_capacity: None,
			}),
		)
		.await;
	let (mut output, mut input) = match started {
		Ok(StartExecResults::Attached { output, input }) => (output, input),
		Ok(StartExecResults::Detached) => return,
		Err(e) => {
			warn!(error = %e, "terminal: start_exec failed");
			notice(
				&mut socket,
				&TerminalNotice::Error {
					message: "the shell could not start".to_owned(),
				},
			)
			.await;
			return;
		}
	};
	resize(&podman, &exec_id, size.0, size.1).await;

	let mut shell_ended = false;
	loop {
		tokio::select! {
			item = output.next() => match item {
				Some(Ok(LogOutput::Console { message } | LogOutput::StdOut { message } | LogOutput::StdErr { message })) => {
					if socket.send(Message::Binary(message)).await.is_err() {
						break;
					}
				}
				Some(Ok(_)) => {}
				Some(Err(e)) => {
					debug!(error = %e, "terminal: output closed");
					shell_ended = true;
					break;
				}
				None => {
					shell_ended = true;
					break;
				}
			},
			frame = socket.recv() => match frame {
				Some(Ok(Message::Binary(bytes))) => {
					if input.write_all(&bytes).await.is_err() || input.flush().await.is_err() {
						shell_ended = true;
						break;
					}
				}
				Some(Ok(Message::Text(text))) => {
					if let Ok(TerminalControl::Resize { cols, rows }) = serde_json::from_str(&text) {
						let (cols, rows) = clamp_size(Some(cols), Some(rows));
						resize(&podman, &exec_id, cols, rows).await;
					}
				}
				Some(Ok(Message::Ping(_) | Message::Pong(_))) => {}
				Some(Ok(Message::Close(_)) | Err(_)) | None => break,
			},
		}
	}
	drop(input);

	if shell_ended {
		let code = exit_code(&podman, &exec_id).await;
		notice(&mut socket, &TerminalNotice::Exit { code }).await;
		let _ = socket
			.send(Message::Close(Some(CloseFrame {
				code: 1000,
				reason: "shell ended".into(),
			})))
			.await;
	}
}

async fn resize(podman: &Podman, exec_id: &str, cols: u16, rows: u16) {
	let result = podman
		.docker()
		.resize_exec(
			exec_id,
			ResizeExecOptions {
				height: rows,
				width: cols,
			},
		)
		.await;
	if let Err(e) = result {
		debug!(error = %e, "terminal: resize failed");
	}
}

/// Podman can report the exec as running for a moment after its output closed.
async fn exit_code(podman: &Podman, exec_id: &str) -> Option<i64> {
	for _ in 0..20 {
		let info = podman.docker().inspect_exec(exec_id).await.ok()?;
		if info.running != Some(true) {
			return info.exit_code;
		}
		tokio::time::sleep(Duration::from_millis(50)).await;
	}
	None
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn size_defaults_and_clamps() {
		assert_eq!(clamp_size(None, None), (80, 24));
		assert_eq!(clamp_size(Some(0), Some(0)), (80, 24));
		assert_eq!(clamp_size(Some(120), Some(40)), (120, 40));
		assert_eq!(clamp_size(Some(u16::MAX), Some(u16::MAX)), (500, 200));
	}
}
