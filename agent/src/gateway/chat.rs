//! `POST /v1/gateway/chat`: one streaming chat completion through the gateway.
//!
//! The gateway only listens on the `podkit` network and, with rootless Podman,
//! the host cannot reach container addresses, so the request is made from a
//! throwaway container (the image the probes use) and what it prints is sent
//! back line by line. The key reaches the container as an environment
//! variable and the body as an argument, never as part of the script.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use futures_util::StreamExt;
use proto::{Event, GATEWAY_HOST, GATEWAY_PORT, GatewayChatRequest, Stream};
use tokio::sync::mpsc;

use crate::ingress::NETWORK;
use crate::model_download::IMAGE;
use crate::podman::{ContainerSpec, Podman};

const MEMORY_BYTES: i64 = 128 << 20;
const MAX_BODY_BYTES: usize = 256 << 10;
const MAX_TIMEOUT_S: u32 = 600;
/// Image pull and container start, beyond the request's own timeout.
const SLACK: Duration = Duration::from_mins(2);
const KEY_ENV: &str = "PODKIT_GATEWAY_KEY";
static COUNTER: AtomicU64 = AtomicU64::new(0);

/// Retries a 401 or a refused connection for a while: the first key of a
/// server makes the agent start the gateway and the sidecar reads its table a
/// moment after it is written.
const SCRIPT: &str = r#"
import json, os, sys, time, urllib.error, urllib.request
url, timeout, body = sys.argv[1], float(sys.argv[2]), sys.argv[3].encode()
headers = {"content-type": "application/json", "authorization": "Bearer " + os.environ["PODKIT_GATEWAY_KEY"]}
request = urllib.request.Request(url, data=body, method="POST", headers=headers)
def emit(value):
    print(json.dumps(value), flush=True)
deadline = time.time() + 25
while True:
    try:
        response = urllib.request.urlopen(request, timeout=timeout)
        break
    except urllib.error.HTTPError as e:
        if e.code == 401 and time.time() < deadline:
            time.sleep(0.5)
            continue
        emit({"error": "HTTP %d: %s" % (e.code, e.read(600).decode("utf-8", "replace"))})
        sys.exit(0)
    except Exception as e:
        if time.time() < deadline:
            time.sleep(0.5)
            continue
        emit({"error": str(e)})
        sys.exit(0)
with response:
    for raw in response:
        line = raw.decode("utf-8", "replace").strip()
        if line.startswith("data:"):
            emit({"data": line[5:].strip()})
emit({"end": True})
"#;

/// Rejects requests that could do anything but chat.
///
/// # Errors
/// A message naming the bad field.
pub fn validate(req: &GatewayChatRequest) -> Result<(), String> {
	if req.key.is_empty() || req.key.len() > 200 || req.key.chars().any(char::is_control) {
		return Err("invalid gateway key".to_owned());
	}
	if req.body.is_empty() || req.body.len() > MAX_BODY_BYTES {
		return Err(format!("body must be 1 to {MAX_BODY_BYTES} bytes"));
	}
	if serde_json::from_str::<serde_json::Value>(&req.body).is_err() {
		return Err("body is not JSON".to_owned());
	}
	if req.timeout_s == 0 || req.timeout_s > MAX_TIMEOUT_S {
		return Err(format!("timeout must be 1 to {MAX_TIMEOUT_S} seconds"));
	}
	Ok(())
}

fn container_spec(req: &GatewayChatRequest, name: &str) -> ContainerSpec {
	let url = format!("http://{GATEWAY_HOST}:{GATEWAY_PORT}/v1/chat/completions");
	ContainerSpec {
		name: name.to_owned(),
		image: IMAGE.to_owned(),
		command: Some(vec![
			"python3".to_owned(),
			"-u".to_owned(),
			"-c".to_owned(),
			SCRIPT.to_owned(),
			url,
			req.timeout_s.to_string(),
			req.body.clone(),
		]),
		env: vec![(KEY_ENV.to_owned(), req.key.clone())],
		network: NETWORK.to_owned(),
		labels: vec![("podkit.gateway-chat".to_owned(), "1".to_owned())],
		memory_bytes: Some(MEMORY_BYTES),
		..Default::default()
	}
}

/// Removes the container when the stream ends or the client goes away.
struct Cleanup {
	podman: Podman,
	name: String,
}

impl Drop for Cleanup {
	fn drop(&mut self) {
		let (podman, name) = (self.podman.clone(), std::mem::take(&mut self.name));
		tokio::spawn(async move {
			if let Err(e) = podman.remove(&name).await {
				tracing::warn!(container = %name, error = %e, "removing chat container failed");
			}
		});
	}
}

/// Runs the chat, sending each printed line on `tx` and one terminal event
/// unless the client went away.
pub async fn run(podman: Podman, req: GatewayChatRequest, tx: mpsc::Sender<Event>) {
	let terminal = match execute(&podman, &req, &tx).await {
		Ok(()) => Event::Done,
		Err(message) => Event::Failed {
			message: message.replace(&req.key, "***"),
		},
	};
	let _ = tx.send(terminal).await;
}

async fn execute(
	podman: &Podman,
	req: &GatewayChatRequest,
	tx: &mpsc::Sender<Event>,
) -> Result<(), String> {
	validate(req)?;
	if podman
		.inspect_image(IMAGE)
		.await
		.map_err(|e| format!("image check failed: {e}"))?
		.is_none()
	{
		let mut pull = std::pin::pin!(podman.pull_image(IMAGE));
		while let Some(item) = pull.next().await {
			item.map_err(|e| format!("image pull failed: {e}"))?;
		}
	}
	let name = format!(
		"podkit-chat-{}-{}",
		std::process::id(),
		COUNTER.fetch_add(1, Ordering::Relaxed)
	);
	let _cleanup = Cleanup {
		podman: podman.clone(),
		name: name.clone(),
	};
	tokio::time::timeout(
		Duration::from_secs(u64::from(req.timeout_s)) + SLACK,
		forward(podman, req, &name, tx),
	)
	.await
	.unwrap_or_else(|_| Err("the chat container did not finish".to_owned()))
}

async fn forward(
	podman: &Podman,
	req: &GatewayChatRequest,
	name: &str,
	tx: &mpsc::Sender<Event>,
) -> Result<(), String> {
	podman
		.create_container(container_spec(req, name))
		.await
		.map_err(|e| format!("chat create failed: {e}"))?;
	podman
		.start(name)
		.await
		.map_err(|e| format!("chat start failed: {e}"))?;
	let mut pending = String::new();
	let mut logs = std::pin::pin!(podman.logs(name, None, true));
	while let Some(item) = logs.next().await {
		let chunk = item.map_err(|e| format!("reading chat output failed: {e}"))?;
		pending.push_str(&String::from_utf8_lossy(chunk.as_ref()));
		while let Some(end) = pending.find('\n') {
			let line: String = pending.drain(..=end).collect();
			let line = line.trim_end().to_owned();
			if !line.is_empty()
				&& tx
					.send(Event::Log {
						stream: Stream::Stdout,
						line,
					})
					.await
					.is_err()
			{
				return Ok(());
			}
		}
	}
	Ok(())
}

#[cfg(test)]
mod tests {
	use super::*;

	fn request() -> GatewayChatRequest {
		GatewayChatRequest {
			key: "pk-secret".to_owned(),
			body: r#"{"model":"a'b","messages":[]}"#.to_owned(),
			timeout_s: 60,
		}
	}

	#[test]
	fn the_key_is_environment_and_the_body_an_argument() {
		let spec = container_spec(&request(), "podkit-chat-x");
		let command = spec.command.unwrap();
		assert!(!command[3].contains("pk-secret") && !command[3].contains("a'b"));
		assert_eq!(command[5], "60");
		assert_eq!(command[6], request().body);
		assert!(command[4].starts_with("http://llm.podkit.internal:8088/"));
		assert_eq!(spec.env, [(KEY_ENV.to_owned(), "pk-secret".to_owned())]);
		assert_eq!(spec.network, "podkit");
		assert!(!format!("{:?}", request()).contains("pk-secret"));
	}

	#[test]
	fn bad_requests_are_refused() {
		assert!(validate(&request()).is_ok());
		for edit in [
			(|r: &mut GatewayChatRequest| r.key.clear()) as fn(&mut GatewayChatRequest),
			|r| r.key = "a\nb".to_owned(),
			|r| r.body = "not json".to_owned(),
			|r| r.body = " ".repeat(MAX_BODY_BYTES + 1),
			|r| r.timeout_s = 0,
			|r| r.timeout_s = 9999,
		] {
			let mut req = request();
			edit(&mut req);
			assert!(validate(&req).is_err(), "{req:?}");
		}
	}
}
