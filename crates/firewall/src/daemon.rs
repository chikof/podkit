//! The unix socket server and its one security check: who is on the other end.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{UnixListener, UnixStream};

use crate::engine::{Engine, Host};
use crate::protocol::{MAX_REQUEST_BYTES, Request, Response};

/// How long a client has to send its request.
const READ_TIMEOUT: Duration = Duration::from_secs(5);

/// Whether a peer with `peer_uid` may use the socket: only the user the helper was set up
/// for. Not root, not anyone else: the socket file's mode is a first line, this is the second.
#[must_use]
pub const fn authorized(peer_uid: u32, allowed_uid: u32) -> bool {
	peer_uid == allowed_uid
}

fn error(message: &str) -> Response {
	Response::Error {
		message: message.to_owned(),
	}
}

async fn read_request(stream: &mut UnixStream) -> Result<Request, &'static str> {
	let mut buf = Vec::new();
	let mut chunk = [0u8; 4096];
	loop {
		let n = tokio::time::timeout(READ_TIMEOUT, stream.read(&mut chunk))
			.await
			.map_err(|_| "request timed out")?
			.map_err(|_| "cannot read the request")?;
		if n == 0 {
			break;
		}
		buf.extend_from_slice(&chunk[..n]);
		if buf.len() > MAX_REQUEST_BYTES {
			return Err("request too large");
		}
		if buf.contains(&b'\n') {
			break;
		}
	}
	serde_json::from_slice(&buf).map_err(|_| "malformed request")
}

async fn serve_one<H: Host + 'static>(
	mut stream: UnixStream,
	engine: Arc<Mutex<Engine<H>>>,
	allowed_uid: u32,
) {
	let refused = match stream.peer_cred() {
		Ok(cred) if authorized(cred.uid(), allowed_uid) => false,
		Ok(cred) => {
			tracing::warn!(uid = cred.uid(), "refused a connection from the wrong user");
			true
		}
		Err(_) => true,
	};
	let response = if refused {
		// Take what the peer already sent without parsing it: closing a socket with unread
		// data resets the connection and the peer would lose the answer.
		let mut chunk = [0u8; 4096];
		let _ = tokio::time::timeout(Duration::from_secs(1), stream.read(&mut chunk)).await;
		error("not authorized")
	} else {
		match read_request(&mut stream).await {
			Ok(req) => {
				let handled = tokio::task::spawn_blocking(move || {
					let mut engine = engine
						.lock()
						.unwrap_or_else(std::sync::PoisonError::into_inner);
					engine.handle(req, Instant::now())
				})
				.await;
				handled.unwrap_or_else(|_| error("internal error"))
			}
			Err(message) => error(message),
		}
	};
	let mut line = serde_json::to_vec(&response).unwrap_or_default();
	line.push(b'\n');
	let _ = stream.write_all(&line).await;
}

/// Accepts connections until the listener fails. Each is one request and one answer.
pub async fn serve<H: Host + 'static>(
	listener: UnixListener,
	engine: Arc<Mutex<Engine<H>>>,
	allowed_uid: u32,
) {
	loop {
		match listener.accept().await {
			Ok((stream, _)) => {
				tokio::spawn(serve_one(stream, Arc::clone(&engine), allowed_uid));
			}
			Err(e) => {
				tracing::error!(error = %e, "accept failed");
				tokio::time::sleep(Duration::from_millis(200)).await;
			}
		}
	}
}

/// Checks every second whether an unconfirmed change ran out of time.
pub async fn run_timer<H: Host + 'static>(engine: Arc<Mutex<Engine<H>>>) {
	let mut tick = tokio::time::interval(Duration::from_secs(1));
	loop {
		tick.tick().await;
		let engine = Arc::clone(&engine);
		let _ = tokio::task::spawn_blocking(move || {
			engine
				.lock()
				.unwrap_or_else(std::sync::PoisonError::into_inner)
				.tick(Instant::now());
		})
		.await;
	}
}

#[cfg(test)]
mod tests {
	use std::net::IpAddr;
	use std::os::unix::fs::MetadataExt as _;

	use super::*;
	use crate::engine::Active;
	use crate::protocol::StatusInfo;

	struct Nothing;

	impl Host for Nothing {
		fn nft_apply(&self, _: &str) -> Result<(), String> {
			Ok(())
		}
		fn ssh_peers(&self, _: u16) -> Vec<IpAddr> {
			Vec::new()
		}
		fn load(&self) -> Option<Active> {
			None
		}
		fn save(&self, _: Option<&Active>) -> Result<(), String> {
			Ok(())
		}
	}

	fn my_uid() -> u32 {
		std::fs::metadata("/proc/self").unwrap().uid()
	}

	async fn ask(path: &std::path::Path, req: &Request) -> Response {
		let mut s = UnixStream::connect(path).await.unwrap();
		let mut line = serde_json::to_vec(req).unwrap();
		line.push(b'\n');
		s.write_all(&line).await.unwrap();
		let mut out = String::new();
		s.read_to_string(&mut out).await.unwrap();
		serde_json::from_str(&out).unwrap()
	}

	fn start(allowed: u32) -> (tempfile::TempDir, std::path::PathBuf) {
		let dir = tempfile::tempdir().unwrap();
		let path = dir.path().join("fw.sock");
		let listener = UnixListener::bind(&path).unwrap();
		let engine = Arc::new(Mutex::new(Engine::new(Nothing)));
		tokio::spawn(serve(listener, engine, allowed));
		(dir, path)
	}

	#[test]
	fn only_the_configured_user_is_authorized() {
		assert!(authorized(1000, 1000));
		assert!(!authorized(1001, 1000));
		assert!(!authorized(0, 1000));
	}

	#[tokio::test]
	async fn right_uid_gets_an_answer() {
		let (_dir, path) = start(my_uid());
		let r = ask(&path, &Request::Status).await;
		assert_eq!(
			r,
			Response::Status(StatusInfo {
				enabled: false,
				policy: None,
				pending: None
			})
		);
	}

	#[tokio::test]
	async fn wrong_uid_is_refused_without_reading_the_request() {
		let (_dir, path) = start(my_uid().wrapping_add(1));
		let r = ask(&path, &Request::Disable).await;
		assert_eq!(r, error("not authorized"));
	}

	#[tokio::test]
	async fn garbage_is_an_error_not_a_crash() {
		let (_dir, path) = start(my_uid());
		let mut s = UnixStream::connect(&path).await.unwrap();
		s.write_all(b"{\"op\":\"nope\"}\n").await.unwrap();
		let mut out = String::new();
		s.read_to_string(&mut out).await.unwrap();
		let r: Response = serde_json::from_str(&out).unwrap();
		assert_eq!(r, error("malformed request"));
	}
}
