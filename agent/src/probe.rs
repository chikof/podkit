//! `POST /v1/probes`: one HTTP request to a container's port.
//!
//! With rootless Podman the host cannot reach container addresses, so the
//! request is made from a throwaway container on the `podkit` network. It
//! reuses the image of the model downloader, which is already on any server
//! that downloads weights. Values reach the script as arguments, never as
//! part of it.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use futures_util::StreamExt;
use proto::{ProbeRequest, ProbeResponse};

use crate::model_download::IMAGE;
use crate::podman::{ContainerSpec, Podman};

const MEMORY_BYTES: i64 = 128 << 20;
/// Longest a probe may take beyond its own request timeout (image pull, startup).
const SLACK: Duration = Duration::from_mins(2);
const MAX_TIMEOUT_S: u32 = 120;
static COUNTER: AtomicU64 = AtomicU64::new(0);

const SCRIPT: &str = r#"
import json, sys, urllib.error, urllib.request
method, url, timeout = sys.argv[1], sys.argv[2], float(sys.argv[3])
body = sys.argv[4].encode() if len(sys.argv) > 4 else None
headers = {"content-type": "application/json"} if body is not None else {}
request = urllib.request.Request(url, data=body, method=method, headers=headers)
try:
    with urllib.request.urlopen(request, timeout=timeout) as response:
        status, detail = response.status, response.read(512).decode("utf-8", "replace")
except urllib.error.HTTPError as e:
    status, detail = e.code, e.read(512).decode("utf-8", "replace")
except Exception as e:
    status, detail = None, str(e)
print(json.dumps({"status": status, "detail": detail}))
"#;

#[derive(serde::Deserialize)]
struct Output {
	status: Option<u16>,
	detail: String,
}

fn valid_host(host: &str) -> bool {
	!host.is_empty()
		&& host.len() <= 253
		&& host
			.bytes()
			.all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

/// Rejects requests that could do anything but ask a container's port a question.
///
/// # Errors
/// A message naming the bad field.
pub fn validate(req: &ProbeRequest) -> Result<(), String> {
	if !matches!(req.method.as_str(), "GET" | "POST") {
		return Err(format!("unsupported probe method {:?}", req.method));
	}
	if !valid_host(&req.host) {
		return Err(format!("invalid probe host {:?}", req.host));
	}
	if !req.path.starts_with('/') || req.path.chars().any(|c| c.is_control() || c == ' ') {
		return Err(format!("invalid probe path {:?}", req.path));
	}
	if req.port == 0 {
		return Err("invalid probe port 0".to_owned());
	}
	if req.timeout_s == 0 || req.timeout_s > MAX_TIMEOUT_S {
		return Err(format!(
			"probe timeout must be 1 to {MAX_TIMEOUT_S} seconds"
		));
	}
	Ok(())
}

/// Arguments after `python3 -c SCRIPT`.
#[must_use]
pub fn script_args(req: &ProbeRequest) -> Vec<String> {
	let mut args = vec![
		req.method.clone(),
		format!("http://{}:{}{}", req.host, req.port, req.path),
		req.timeout_s.to_string(),
	];
	if let Some(body) = &req.body {
		args.push(body.clone());
	}
	args
}

fn container_spec(req: &ProbeRequest, name: &str, network: &str) -> ContainerSpec {
	let mut command = vec!["python3".to_owned(), "-c".to_owned(), SCRIPT.to_owned()];
	command.extend(script_args(req));
	ContainerSpec {
		name: name.to_owned(),
		image: IMAGE.to_owned(),
		command: Some(command),
		network: network.to_owned(),
		labels: vec![("podkit.probe".to_owned(), req.container_id.clone())],
		memory_bytes: Some(MEMORY_BYTES),
		..Default::default()
	}
}

/// Runs the probe.
///
/// # Errors
/// The probe container could not be run; a refused or timed-out request is
/// not an error, it is `status: None`.
pub async fn run(podman: &Podman, req: &ProbeRequest) -> Result<ProbeResponse, String> {
	validate(req)?;
	let info = podman
		.inspect(&req.container_id)
		.await
		.map_err(|e| format!("inspect failed: {e}"))?
		.ok_or_else(|| format!("container {} not found", req.container_id))?;
	// The probe joins the network the container lives on: its environment's.
	let network = crate::networks::probe_network(
		info.network_settings
			.as_ref()
			.and_then(|n| n.networks.as_ref())
			.map(|n| n.keys().map(String::as_str).collect::<Vec<_>>())
			.unwrap_or_default(),
	);
	if info.state.and_then(|s| s.running) != Some(true) {
		return Ok(ProbeResponse {
			running: false,
			status: None,
			detail: "container is not running".to_owned(),
		});
	}

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
		"podkit-probe-{}-{}",
		std::process::id(),
		COUNTER.fetch_add(1, Ordering::Relaxed)
	);
	let result = tokio::time::timeout(
		Duration::from_secs(u64::from(req.timeout_s)) + SLACK,
		execute(podman, req, &name, &network),
	)
	.await
	.unwrap_or_else(|_| Err("probe container did not finish".to_owned()));
	if let Err(e) = podman.remove(&name).await {
		tracing::warn!(container = %name, error = %e, "removing probe container failed");
	}
	let output = result?;
	Ok(ProbeResponse {
		running: true,
		status: output.status,
		detail: output.detail,
	})
}

async fn execute(
	podman: &Podman,
	req: &ProbeRequest,
	name: &str,
	network: &str,
) -> Result<Output, String> {
	podman
		.create_container(container_spec(req, name, network))
		.await
		.map_err(|e| format!("probe create failed: {e}"))?;
	podman
		.start(name)
		.await
		.map_err(|e| format!("probe start failed: {e}"))?;
	let mut text = String::new();
	let mut logs = std::pin::pin!(podman.logs(name, None, true));
	while let Some(item) = logs.next().await {
		let chunk = item.map_err(|e| format!("reading probe output failed: {e}"))?;
		text.push_str(&String::from_utf8_lossy(chunk.as_ref()));
	}
	text.lines()
		.rev()
		.find_map(|line| serde_json::from_str(line).ok())
		.ok_or_else(|| format!("probe printed no result: {}", text.trim()))
}

#[cfg(test)]
mod tests {
	use super::*;

	fn request() -> ProbeRequest {
		ProbeRequest {
			container_id: "abc".to_owned(),
			host: "model-qwen".to_owned(),
			port: 8080,
			method: "GET".to_owned(),
			path: "/health".to_owned(),
			body: None,
			timeout_s: 5,
		}
	}

	#[test]
	fn values_are_arguments_not_script() {
		let mut req = request();
		req.method = "POST".to_owned();
		req.body = Some(r#"{"model":"a'b"}"#.to_owned());
		assert_eq!(
			script_args(&req),
			[
				"POST",
				"http://model-qwen:8080/health",
				"5",
				r#"{"model":"a'b"}"#
			]
		);
		let spec = container_spec(&req, "podkit-probe-x", "podkit-env-4");
		assert_eq!(spec.network, "podkit-env-4");
		assert!(!spec.command.unwrap()[2].contains("model-qwen"));
	}

	#[test]
	fn bad_requests_are_refused() {
		assert!(validate(&request()).is_ok());
		for edit in [
			(|r: &mut ProbeRequest| r.method = "DELETE".to_owned()) as fn(&mut ProbeRequest),
			|r| r.host = "a b".to_owned(),
			|r| r.host = "evil/../x".to_owned(),
			|r| r.path = "health".to_owned(),
			|r| r.path = "/a b".to_owned(),
			|r| r.port = 0,
			|r| r.timeout_s = 0,
			|r| r.timeout_s = 999,
		] {
			let mut req = request();
			edit(&mut req);
			assert!(validate(&req).is_err(), "{req:?}");
		}
	}
}
