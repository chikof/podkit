//! A pass-through to the `podkit-firewall` helper (see `crates/firewall`). The helper runs as
//! root and listens on a unix socket only this agent's user may open; the agent carries one
//! request line from the control plane to it and brings the answer line back. It does not
//! read the request: the helper checks it and decides, and it is the only one that does.

use std::path::PathBuf;
use std::time::Duration;

use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::UnixStream;

/// Where the helper listens unless `PODKIT_FIREWALL_SOCKET` says otherwise.
pub const DEFAULT_SOCKET: &str = "/run/podkit-firewall/firewall.sock";
/// Longest answer carried back.
const MAX_ANSWER_BYTES: u64 = 512 * 1024;
/// An apply runs `nft`, which takes a moment on a big ruleset.
const TIMEOUT: Duration = Duration::from_secs(20);
/// What the control plane is told when there is no helper to ask.
const NOT_INSTALLED: &str = r#"{"kind":"not_installed"}"#;

fn socket() -> PathBuf {
	std::env::var_os("PODKIT_FIREWALL_SOCKET").map_or_else(|| DEFAULT_SOCKET.into(), Into::into)
}

/// Sends `request` (one JSON object) to the helper and returns its one-line answer.
///
/// # Errors
/// A message when the helper does not answer in time or breaks the conversation. A missing
/// helper is not an error: it is answered as `not_installed`.
pub async fn relay(request: &[u8]) -> Result<Vec<u8>, String> {
	relay_to(&socket(), request).await
}

async fn relay_to(path: &std::path::Path, request: &[u8]) -> Result<Vec<u8>, String> {
	let talk = async {
		let mut stream = match UnixStream::connect(path).await {
			Ok(s) => s,
			Err(e)
				if matches!(
					e.kind(),
					std::io::ErrorKind::NotFound
						| std::io::ErrorKind::ConnectionRefused
						| std::io::ErrorKind::PermissionDenied
				) =>
			{
				return Ok(NOT_INSTALLED.as_bytes().to_vec());
			}
			Err(e) => return Err(format!("cannot reach the firewall helper: {e}")),
		};
		stream
			.write_all(request)
			.await
			.map_err(|e| format!("cannot write to the firewall helper: {e}"))?;
		stream
			.write_all(b"\n")
			.await
			.map_err(|e| format!("cannot write to the firewall helper: {e}"))?;
		let mut answer = Vec::new();
		stream
			.take(MAX_ANSWER_BYTES)
			.read_to_end(&mut answer)
			.await
			.map_err(|e| format!("cannot read the firewall helper's answer: {e}"))?;
		Ok(answer)
	};
	tokio::time::timeout(TIMEOUT, talk)
		.await
		.map_err(|_| "the firewall helper did not answer in time".to_owned())?
}

#[cfg(test)]
mod tests {
	use super::*;

	#[tokio::test]
	async fn a_missing_helper_is_answered_as_not_installed() {
		let dir = tempfile::tempdir().unwrap();
		let answer = relay_to(&dir.path().join("none.sock"), b"{}")
			.await
			.unwrap();
		assert_eq!(answer, NOT_INSTALLED.as_bytes());
	}

	#[tokio::test]
	async fn a_request_goes_through_and_the_answer_comes_back() {
		let dir = tempfile::tempdir().unwrap();
		let path = dir.path().join("fw.sock");
		let listener = tokio::net::UnixListener::bind(&path).unwrap();
		tokio::spawn(async move {
			let (mut s, _) = listener.accept().await.unwrap();
			let mut buf = vec![0u8; 64];
			let n = s.read(&mut buf).await.unwrap();
			assert_eq!(&buf[..n], b"{\"op\":\"status\"}\n");
			s.write_all(b"{\"kind\":\"done\"}\n").await.unwrap();
		});
		let answer = relay_to(&path, br#"{"op":"status"}"#).await.unwrap();
		assert_eq!(answer, b"{\"kind\":\"done\"}\n");
	}
}
