//! What the sidecar is made of: names, the serve config, the agent's deploy request, and the
//! small parsers for what the tailscale CLI prints. Nothing here touches the database or the
//! network, so the shape of a sidecar is tested without a tailnet.

use proto::{ContainerOptions, DeploySpec, FileMount, Ingress, NetworkAttach, Source, Strategy};
use serde::Deserialize;

use crate::ids::ApplicationId;

/// The image of the sidecar. `stable` follows Tailscale's stable releases.
pub const IMAGE: &str = "docker.io/tailscale/tailscale:stable";
/// Where tailscaled keeps its state in the container; a volume is mounted here.
pub const STATE_DIR: &str = "/var/lib/tailscale";
/// Where the serve config is mounted.
pub const SERVE_PATH: &str = "/config/serve.json";
/// The `LocalAPI` socket the `tailscale` CLI in the container talks to.
pub const SOCKET: &str = "/tmp/tailscaled.sock";
/// The longest node name: a DNS label.
pub const MAX_HOSTNAME: usize = 63;
/// The most tags a team can set.
pub const MAX_TAGS: usize = 10;

/// The agent's service name for the sidecar of `app`. It starts with
/// [`proto::TAILSCALE_SERVICE_PREFIX`], which is what lets the agent run `tailscale` in it.
#[must_use]
pub fn service(app: ApplicationId) -> String {
	format!("{}app-{app}", proto::TAILSCALE_SERVICE_PREFIX)
}

/// The volume holding the node's identity. Losing it makes a new node.
#[must_use]
pub fn volume(app: ApplicationId) -> String {
	format!("{}-state", service(app))
}

/// The DNS alias the application answers to on its environment's network once it is exposed:
/// the sidecar proxies to it, because a container's own name changes with every deploy.
#[must_use]
pub fn target_alias(app: ApplicationId) -> String {
	format!("pk-app-{app}")
}

/// `tailscale` with the socket of the sidecar's daemon.
fn cli(args: &[&str]) -> Vec<String> {
	["tailscale", "--socket", SOCKET]
		.into_iter()
		.chain(args.iter().copied())
		.map(str::to_owned)
		.collect()
}

/// The command that reports the node's state as JSON.
#[must_use]
pub fn status_cmd() -> Vec<String> {
	cli(&["status", "--json"])
}

/// The command that removes the node from the tailnet.
#[must_use]
pub fn logout_cmd() -> Vec<String> {
	cli(&["logout"])
}

/// Whether `name` is a usable node name: a lowercase DNS label.
#[must_use]
pub fn valid_hostname(name: &str) -> bool {
	!name.is_empty()
		&& name.len() <= MAX_HOSTNAME
		&& !name.starts_with('-')
		&& !name.ends_with('-')
		&& name
			.bytes()
			.all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// A node name made from an application's slug: the slug, cut to a label.
#[must_use]
pub fn default_hostname(slug: &str) -> String {
	let mut name: String = slug
		.to_ascii_lowercase()
		.chars()
		.map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
		.collect();
	name.truncate(MAX_HOSTNAME);
	name.trim_matches('-').to_owned()
}

/// A control server URL: Headscale, or any server that speaks the Tailscale protocol.
/// `http://` is allowed because a homelab's Headscale is often reached on a private address.
/// Empty means the Tailscale service itself, so it is `None`.
///
/// # Errors
/// A message for the user when the text is not an `http(s)` URL with a host.
pub fn normalize_login_server(input: &str) -> Result<Option<String>, String> {
	let url = input.trim();
	if url.is_empty() {
		return Ok(None);
	}
	let invalid = || "the login server must be an http:// or https:// address".to_owned();
	if url.len() > 255 || url.chars().any(|c| c.is_whitespace() || c.is_control()) {
		return Err(invalid());
	}
	let (scheme, rest) = if let Some(rest) = url.strip_prefix("https://") {
		("https://", rest)
	} else if let Some(rest) = url.strip_prefix("http://") {
		("http://", rest)
	} else {
		return Err(invalid());
	};
	let rest = rest.trim_end_matches('/');
	let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));
	if authority.is_empty()
		|| authority.contains('@')
		|| path.contains(['?', '#'])
		|| authority.contains(['?', '#'])
		|| !authority
			.bytes()
			.all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b':' | b'[' | b']'))
	{
		return Err(invalid());
	}
	let path = if path.is_empty() {
		String::new()
	} else {
		format!("/{path}")
	};
	Ok(Some(format!("{scheme}{authority}{path}")))
}

/// Tags from free text: separated by commas, spaces or lines, each with or without the
/// `tag:` prefix, lowercase letters, digits and dashes. Duplicates collapse.
///
/// # Errors
/// A message for the user naming the tag that is not allowed.
pub fn parse_tags(input: &str) -> Result<Vec<String>, String> {
	let mut tags: Vec<String> = Vec::new();
	for raw in input
		.split([',', ' ', '\n', '\t', '\r'])
		.filter(|t| !t.is_empty())
	{
		let name = raw.trim().strip_prefix("tag:").unwrap_or(raw.trim());
		let ok = name.len() <= 50
			&& name.starts_with(|c: char| c.is_ascii_lowercase())
			&& name
				.bytes()
				.all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
		if !ok {
			return Err(format!(
				"{raw:?} is not a tag: use lowercase letters, digits and dashes, such as tag:podkit"
			));
		}
		let tag = format!("tag:{name}");
		if !tags.contains(&tag) {
			tags.push(tag);
		}
	}
	if tags.len() > MAX_TAGS {
		return Err(format!("at most {MAX_TAGS} tags"));
	}
	Ok(tags)
}

/// A tailscale auth key: one token without spaces. Tailscale keys start with `tskey-`;
/// Headscale's pre-auth keys are plain hex, so the prefix is not required.
///
/// # Errors
/// A message for the user.
pub fn check_auth_key(key: &str) -> Result<&str, String> {
	let key = key.trim();
	if key.is_empty() || key.len() > 512 || key.chars().any(|c| c.is_control() || c.is_whitespace())
	{
		return Err("that is not an auth key".to_owned());
	}
	Ok(key)
}

/// The `tailscale serve` config: HTTPS on 443 of the node's own name, proxied to the
/// application. `${TS_CERT_DOMAIN}` is replaced by containerboot with the node's full name.
#[must_use]
pub fn serve_config(target: &str, port: u16) -> String {
	serde_json::json!({
		"TCP": { "443": { "HTTPS": true } },
		"Web": {
			"${TS_CERT_DOMAIN}:443": {
				"Handlers": { "/": { "Proxy": format!("http://{target}:{port}") } }
			}
		}
	})
	.to_string()
}

/// Everything a sidecar's deploy request depends on.
pub struct Sidecar<'a> {
	pub application: ApplicationId,
	pub hostname: &'a str,
	/// The port the application listens on.
	pub container_port: u16,
	pub auth_key: &'a str,
	pub login_server: Option<&'a str>,
	pub tags: &'a [String],
	/// The application's environment network.
	pub network: NetworkAttach,
	pub ingress: Ingress,
	/// The job that starts it: the container name suffix.
	pub instance_id: i64,
}

/// The agent's deploy request for a sidecar: the image, the node's identity on a volume, the
/// serve config as a file, and the application's network. Userspace networking needs no
/// capabilities and no `/dev/net/tun`, so the container is as unprivileged as any other.
#[must_use]
pub fn build_spec(s: &Sidecar<'_>) -> DeploySpec {
	let mut env = vec![
		("TS_AUTHKEY".to_owned(), s.auth_key.to_owned()),
		("TS_HOSTNAME".to_owned(), s.hostname.to_owned()),
		("TS_STATE_DIR".to_owned(), STATE_DIR.to_owned()),
		("TS_SOCKET".to_owned(), SOCKET.to_owned()),
		("TS_USERSPACE".to_owned(), "true".to_owned()),
		// Log in only when the state volume has no identity yet, so a key that was single-use
		// or has expired does not matter on a restart.
		("TS_AUTH_ONCE".to_owned(), "true".to_owned()),
		("TS_SERVE_CONFIG".to_owned(), SERVE_PATH.to_owned()),
	];
	let mut extra: Vec<String> = Vec::new();
	if let Some(server) = s.login_server {
		extra.push(format!("--login-server={server}"));
	}
	if !s.tags.is_empty() {
		extra.push(format!("--advertise-tags={}", s.tags.join(",")));
	}
	if !extra.is_empty() {
		env.push(("TS_EXTRA_ARGS".to_owned(), extra.join(" ")));
	}
	DeploySpec {
		service: service(s.application),
		instance_id: s.instance_id,
		source: Source::Image {
			reference: IMAGE.to_owned(),
			pull: true,
			auth: None,
		},
		env,
		command: None,
		container_port: None,
		routing: None,
		network_aliases: Vec::new(),
		volumes: vec![proto::VolumeMount {
			name: volume(s.application),
			path: STATE_DIR.to_owned(),
			read_only: false,
		}],
		published_ports: Vec::new(),
		memory_bytes: None,
		cpu_cores: None,
		// The state volume can be mounted by one container at a time.
		strategy: Strategy::Recreate,
		ingress: s.ingress.clone(),
		gpus: None,
		entrypoint: None,
		shm_bytes: None,
		stop_timeout_s: None,
		networks: Vec::new(),
		options: ContainerOptions {
			files: vec![FileMount {
				path: SERVE_PATH.to_owned(),
				content: serve_config(&target_alias(s.application), s.container_port),
			}],
			..ContainerOptions::default()
		},
		attach: vec![s.network.clone()],
		health: None,
	}
}

/// What `tailscale status --json` says about the node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Node {
	/// On the tailnet, under this name (no trailing dot).
	Running { dns_name: String },
	/// Waiting for a login: the auth key was not used yet, or was refused.
	NeedsLogin,
	/// Any other state, as Tailscale names it.
	Other(String),
}

#[derive(Deserialize)]
struct StatusJson {
	#[serde(rename = "BackendState", default)]
	backend_state: String,
	#[serde(rename = "Self")]
	own: Option<SelfJson>,
}

#[derive(Deserialize)]
struct SelfJson {
	#[serde(rename = "DNSName", default)]
	dns_name: String,
}

/// Reads the node's state from `tailscale status --json`.
#[must_use]
pub fn parse_status(output: &str) -> Option<Node> {
	let status: StatusJson = serde_json::from_str(output).ok()?;
	Some(match status.backend_state.as_str() {
		"Running" => {
			let name = status.own?.dns_name;
			let name = name.trim_end_matches('.');
			if name.is_empty() {
				Node::Other("Running".to_owned())
			} else {
				Node::Running {
					dns_name: name.to_owned(),
				}
			}
		}
		"NeedsLogin" => Node::NeedsLogin,
		other => Node::Other(other.to_owned()),
	})
}
