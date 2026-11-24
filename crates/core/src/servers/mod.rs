//! Servers: podman hosts a team can deploy to. The local one is created with the team.

pub mod bootstrap;
pub mod hardware;
mod routes;
pub mod service;
pub mod sync;

pub use routes::router;

use base64::Engine;
use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD};
use sha2::{Digest, Sha256};

use crate::ids::{ServerId, SshKeyId, TeamId};

/// Connectivity state of a registered server.
#[derive(Debug, Clone, Copy, PartialEq, Eq, sqlx::Type, serde::Serialize, serde::Deserialize)]
#[sqlx(type_name = "varchar", rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum ServerStatus {
	/// Remote server registered, agent not yet installed.
	Pending,
	/// Agent reachable and ready to run containers.
	Active,
	/// Was active, most recent health check or bootstrap failed.
	Unreachable,
}

impl ServerStatus {
	/// Returns the lowercase string form used for storage and display.
	pub fn as_str(self) -> &'static str {
		match self {
			Self::Pending => "pending",
			Self::Active => "active",
			Self::Unreachable => "unreachable",
		}
	}
}

/// A podman host a team can deploy containers to.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Server {
	pub id: ServerId,
	pub team_id: TeamId,
	pub name: String,
	/// Hostname or IP podkit connects to.
	pub hostname: String,
	/// SSH port, unused for the local server.
	pub ssh_port: i32,
	/// SSH username. `None` iff `is_local`.
	pub ssh_user: Option<String>,
	/// The vault key podkit logs in with. `None` iff `is_local`.
	pub ssh_key_id: Option<SshKeyId>,
	/// Path to the podman socket on the target host.
	pub podman_socket_path: String,
	/// True for the podkit host itself; false for SSH-reached hosts.
	pub is_local: bool,
	pub status: ServerStatus,
	/// Pinned `known_hosts` line(s), scanned by the first bootstrap.
	pub host_key: Option<String>,
	/// The agent's unix socket on the remote host. `None` until bootstrapped.
	pub agent_socket_path: Option<String>,
	/// Version the installed agent reported at the last bootstrap.
	pub agent_version: Option<String>,
	/// `uname -m` of the remote host, as found at the last bootstrap.
	pub arch: Option<String>,
	/// Why the server is not `active`, when known.
	pub status_message: Option<String>,
	/// Latest hardware snapshot from the agent.
	pub hardware: Option<sqlx::types::Json<proto::Hardware>>,
	/// When `hardware` was taken.
	pub hardware_at: Option<time::OffsetDateTime>,
}

/// AMD GPUs below this are integrated parts that cannot run an engine.
const MIN_AMD_GPU_VRAM_MB: u64 = 2048;

/// The GPU vendors a container on this server can use, NVIDIA first: NVIDIA
/// needs a GPU and a `nvidia.com/gpu` CDI spec (`docs/spikes/gpu-cdi.md`), AMD
/// a discrete card. Empty before the first snapshot.
#[must_use]
pub fn gpu_vendors(server: &Server) -> Vec<proto::GpuVendor> {
	let Some(hardware) = server.hardware.as_ref().map(|h| &h.0) else {
		return Vec::new();
	};
	let mut vendors = Vec::new();
	if hardware.gpus.iter().any(|g| g.vendor == "nvidia")
		&& hardware.cdi.devices.iter().any(|k| k == "nvidia.com/gpu")
	{
		vendors.push(proto::GpuVendor::Nvidia);
	}
	if hardware
		.gpus
		.iter()
		.any(|g| g.vendor == "amd" && g.vram_mb >= MIN_AMD_GPU_VRAM_MB)
	{
		vendors.push(proto::GpuVendor::Amd);
	}
	vendors
}

/// Total VRAM, in MB, of the usable GPUs of `vendor`.
#[must_use]
pub fn vram_mb(server: &Server, vendor: proto::GpuVendor) -> u64 {
	let name = match vendor {
		proto::GpuVendor::Nvidia => "nvidia",
		proto::GpuVendor::Amd => "amd",
	};
	server.hardware.as_ref().map_or(0, |h| {
		h.gpus
			.iter()
			.filter(|g| {
				g.vendor == name
					&& (vendor == proto::GpuVendor::Nvidia || g.vram_mb >= MIN_AMD_GPU_VRAM_MB)
			})
			.map(|g| g.vram_mb)
			.sum()
	})
}

/// Why `vendor` cannot be used on this server, for an error message.
#[must_use]
pub fn gpu_problem(server: &Server, vendor: proto::GpuVendor) -> String {
	let Some(hardware) = server.hardware.as_ref().map(|h| &h.0) else {
		return format!("{} has not reported its hardware yet", server.name);
	};
	match vendor {
		proto::GpuVendor::Nvidia if hardware.gpus.iter().any(|g| g.vendor == "nvidia") => format!(
			"{} has an NVIDIA GPU but no CDI spec for it (install nvidia-container-toolkit and \
			generate one, see docs/spikes/gpu-cdi.md)",
			server.name
		),
		proto::GpuVendor::Nvidia => format!("{} has no NVIDIA GPU", server.name),
		proto::GpuVendor::Amd => format!("{} has no discrete AMD GPU", server.name),
	}
}

/// Whether the server's latest inventory shows a GPU a container can use.
/// `false` before the first snapshot.
#[must_use]
pub fn gpu_capable(server: &Server) -> bool {
	!gpu_vendors(server).is_empty()
}

impl Server {
	/// Builds a remote server pending its first bootstrap.
	#[allow(clippy::too_many_arguments)]
	pub fn new_remote(
		team_id: TeamId,
		name: String,
		hostname: String,
		ssh_port: i32,
		ssh_user: String,
		ssh_key_id: SshKeyId,
		podman_socket_path: String,
	) -> Self {
		Self {
			id: ServerId::generate(),
			team_id,
			name,
			hostname,
			ssh_port,
			ssh_user: Some(ssh_user),
			ssh_key_id: Some(ssh_key_id),
			podman_socket_path,
			is_local: false,
			status: ServerStatus::Pending,
			host_key: None,
			agent_socket_path: None,
			agent_version: None,
			arch: None,
			status_message: None,
			hardware: None,
			hardware_at: None,
		}
	}
}

/// Resolves the current user's default rootless podman socket path
/// (`$XDG_RUNTIME_DIR/podman/podman.sock`, falling back to
/// `/run/user/<uid>/podman/podman.sock`). Does not verify the socket exists.
pub fn local_podman_socket() -> String {
	match std::env::var("XDG_RUNTIME_DIR").ok() {
		Some(dir) => format!("{dir}/podman/podman.sock"),
		None => format!("/run/user/{}/podman/podman.sock", getuid()),
	}
}

fn getuid() -> u32 {
	unsafe extern "C" {
		fn getuid() -> u32;
	}
	unsafe { getuid() }
}

/// `SHA256:<unpadded base64>` of the pinned key blob, as `ssh-keygen -lf` prints
/// it. Prefers the `ssh-ed25519` line, else the first key line. `None` when no
/// line holds a decodable key.
pub fn host_key_fingerprint(host_key: &str) -> Option<String> {
	let lines = || {
		host_key
			.lines()
			.map(str::trim)
			.filter(|l| !l.is_empty() && !l.starts_with('#'))
	};
	let line = lines()
		.find(|l| l.split_whitespace().any(|t| t == "ssh-ed25519"))
		.or_else(|| lines().next())?;
	let mut tokens = line.split_whitespace();
	tokens.find(|t| t.starts_with("ssh-") || t.starts_with("ecdsa-") || t.starts_with("sk-"))?;
	let blob = STANDARD.decode(tokens.next()?).ok()?;
	Some(format!(
		"SHA256:{}",
		STANDARD_NO_PAD.encode(Sha256::digest(blob))
	))
}

pub async fn insert(db: impl sqlx::PgExecutor<'_>, server: &Server) -> Result<(), sqlx::Error> {
	sqlx::query!(
		r#"
			INSERT INTO servers
				(id, team_id, name, hostname, ssh_port, ssh_user, ssh_key_id, podman_socket_path, is_local, status)
			VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
		"#,
		server.id as ServerId,
		server.team_id as TeamId,
		server.name,
		server.hostname,
		server.ssh_port,
		server.ssh_user,
		server.ssh_key_id as Option<SshKeyId>,
		server.podman_socket_path,
		server.is_local,
		server.status.as_str()
	)
	.execute(db)
	.await?;
	Ok(())
}

pub async fn find(
	db: impl sqlx::PgExecutor<'_>,
	id: ServerId,
) -> Result<Option<Server>, sqlx::Error> {
	sqlx::query_as!(
		Server,
		r#"
			SELECT id AS "id: ServerId", team_id AS "team_id: TeamId", name, hostname, ssh_port,
				ssh_user, ssh_key_id AS "ssh_key_id: SshKeyId", podman_socket_path, is_local,
				status AS "status: ServerStatus", host_key, agent_socket_path, agent_version,
				arch, status_message,
				hardware AS "hardware?: sqlx::types::Json<proto::Hardware>", hardware_at
			FROM servers
			WHERE id = $1
		"#,
		id as ServerId
	)
	.fetch_optional(db)
	.await
}

pub async fn list_by_team(
	db: impl sqlx::PgExecutor<'_>,
	team: TeamId,
) -> Result<Vec<Server>, sqlx::Error> {
	sqlx::query_as!(
		Server,
		r#"
			SELECT id AS "id: ServerId", team_id AS "team_id: TeamId", name, hostname, ssh_port,
				ssh_user, ssh_key_id AS "ssh_key_id: SshKeyId", podman_socket_path, is_local,
				status AS "status: ServerStatus", host_key, agent_socket_path, agent_version,
				arch, status_message,
				hardware AS "hardware?: sqlx::types::Json<proto::Hardware>", hardware_at
			FROM servers
			WHERE team_id = $1
		"#,
		team as TeamId
	)
	.fetch_all(db)
	.await
}

/// Servers the status sync checks: every active one plus all local ones.
pub async fn list_for_sync(db: impl sqlx::PgExecutor<'_>) -> Result<Vec<Server>, sqlx::Error> {
	sqlx::query_as!(
		Server,
		r#"
			SELECT id AS "id: ServerId", team_id AS "team_id: TeamId", name, hostname, ssh_port,
				ssh_user, ssh_key_id AS "ssh_key_id: SshKeyId", podman_socket_path, is_local,
				status AS "status: ServerStatus", host_key, agent_socket_path, agent_version,
				arch, status_message,
				hardware AS "hardware?: sqlx::types::Json<proto::Hardware>", hardware_at
			FROM servers
			WHERE status = 'active' OR is_local OR (status = 'unreachable' AND agent_version IS NOT NULL)
		"#
	)
	.fetch_all(db)
	.await
}

pub async fn set_host_key(
	db: impl sqlx::PgExecutor<'_>,
	id: ServerId,
	host_key: Option<&str>,
) -> Result<(), sqlx::Error> {
	sqlx::query!(
		"UPDATE servers SET host_key = $2 WHERE id = $1",
		id as ServerId,
		host_key
	)
	.execute(db)
	.await?;
	Ok(())
}

pub async fn set_agent_socket_path(
	db: impl sqlx::PgExecutor<'_>,
	id: ServerId,
	path: &str,
) -> Result<(), sqlx::Error> {
	sqlx::query!(
		"UPDATE servers SET agent_socket_path = $2 WHERE id = $1",
		id as ServerId,
		path
	)
	.execute(db)
	.await?;
	Ok(())
}

/// Records a finished bootstrap: the agent answered with the expected version.
pub async fn mark_bootstrapped(
	db: impl sqlx::PgExecutor<'_>,
	id: ServerId,
	agent_version: &str,
	arch: &str,
) -> Result<(), sqlx::Error> {
	sqlx::query!(
		"UPDATE servers SET status = 'active', agent_version = $2, arch = $3, status_message = NULL WHERE id = $1",
		id as ServerId,
		agent_version,
		arch
	)
	.execute(db)
	.await?;
	Ok(())
}

/// Writes only when something changes, so the 30 s sync does not churn rows.
pub async fn mark_unreachable(
	db: impl sqlx::PgExecutor<'_>,
	id: ServerId,
	message: &str,
) -> Result<(), sqlx::Error> {
	sqlx::query!(
		r#"
			UPDATE servers SET status = 'unreachable', status_message = $2
			WHERE id = $1 AND (status <> 'unreachable' OR status_message IS DISTINCT FROM $2)
		"#,
		id as ServerId,
		message
	)
	.execute(db)
	.await?;
	Ok(())
}

pub async fn mark_active(db: impl sqlx::PgExecutor<'_>, id: ServerId) -> Result<(), sqlx::Error> {
	sqlx::query!(
		r#"
			UPDATE servers SET status = 'active', status_message = NULL
			WHERE id = $1 AND (status <> 'active' OR status_message IS NOT NULL)
		"#,
		id as ServerId
	)
	.execute(db)
	.await?;
	Ok(())
}

/// Longest Podman version kept (the column's width); a longer one is cut.
const PODMAN_VERSION_MAX: usize = 40;

/// Stores what the agent's health check said about Podman. Writes only on a change, so a
/// sweep of steady servers costs no row versions.
///
/// # Errors
/// The database rejects the write.
pub async fn record_podman(
	db: impl sqlx::PgExecutor<'_>,
	id: ServerId,
	health: &proto::Health,
) -> Result<(), sqlx::Error> {
	let version: String = health
		.podman_version
		.chars()
		.take(PODMAN_VERSION_MAX)
		.collect();
	let version = Some(version).filter(|v| !v.is_empty());
	sqlx::query!(
		r#"
			UPDATE servers SET podman_version = $2, podman_rootless = $3
			WHERE id = $1
				AND (podman_version IS DISTINCT FROM $2 OR podman_rootless IS DISTINCT FROM $3)
		"#,
		id as ServerId,
		version,
		health.rootless,
	)
	.execute(db)
	.await?;
	Ok(())
}

/// What the sidebar's agent card says about a team's servers: how many there are, how many
/// answer, and what the answering ones run. Read from the columns the status sync keeps, so
/// it never reaches an agent.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AgentSummary {
	/// Every server of the team.
	pub total: i64,
	/// Those whose agent answered the last health check.
	pub connected: i64,
	/// Distinct Podman versions the connected servers reported, sorted.
	pub podman_versions: Vec<String>,
	/// Connected servers whose Podman said it runs rootless.
	pub rootless: i64,
	/// Connected servers whose Podman said it runs as root.
	pub rootful: i64,
}

impl AgentSummary {
	/// `rootless`, `rootful` or `mixed`; `None` while no agent said (an older agent does not).
	#[must_use]
	pub fn mode(&self) -> Option<&'static str> {
		match (self.rootless > 0, self.rootful > 0) {
			(true, false) => Some("rootless"),
			(false, true) => Some("rootful"),
			(true, true) => Some("mixed"),
			(false, false) => None,
		}
	}

	/// `Podman 5.4.0`, or the range when the servers differ; `None` before any report.
	#[must_use]
	pub fn podman_label(&self) -> Option<String> {
		match self.podman_versions.as_slice() {
			[] => None,
			[one] => Some(format!("Podman {one}")),
			[first, .., last] => Some(format!("Podman {first} to {last}")),
		}
	}
}

/// Orders `5.10.0` after `5.4.0`: the numeric parts compare as numbers, the rest as text.
fn version_key(version: &str) -> Vec<(u64, String)> {
	version
		.split(['.', '-', '+'])
		.map(|part| (part.parse().unwrap_or(u64::MAX), part.to_owned()))
		.collect()
}

/// The team's [`AgentSummary`] in one aggregate query over its servers.
///
/// # Errors
/// The database rejects the read.
pub async fn agent_summary(
	db: impl sqlx::PgExecutor<'_>,
	team: TeamId,
) -> Result<AgentSummary, sqlx::Error> {
	let row = sqlx::query!(
		r#"
			SELECT COUNT(*) AS "total!",
				COUNT(*) FILTER (WHERE status = 'active') AS "connected!",
				COUNT(*) FILTER (WHERE status = 'active' AND podman_rootless) AS "rootless!",
				COUNT(*) FILTER (WHERE status = 'active' AND NOT podman_rootless) AS "rootful!",
				COALESCE(
					ARRAY_AGG(DISTINCT podman_version ORDER BY podman_version)
						FILTER (WHERE status = 'active' AND podman_version IS NOT NULL),
					'{}'
				) AS "versions!: Vec<String>"
			FROM servers
			WHERE team_id = $1
		"#,
		team as TeamId
	)
	.fetch_one(db)
	.await?;
	let mut versions = row.versions;
	versions.sort_by_cached_key(|v| version_key(v));
	Ok(AgentSummary {
		total: row.total,
		connected: row.connected,
		podman_versions: versions,
		rootless: row.rootless,
		rootful: row.rootful,
	})
}

pub async fn delete(
	db: impl sqlx::PgExecutor<'_>,
	team: TeamId,
	id: ServerId,
) -> Result<(), sqlx::Error> {
	sqlx::query!(
		"DELETE FROM servers WHERE id = $1 AND team_id = $2",
		id as ServerId,
		team as TeamId
	)
	.execute(db)
	.await?;
	Ok(())
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::ids::TeamId;
	use proto::{Cdi, Cpu, Gpu, Hardware, Memory};

	fn server_with(gpus: &[(&str, u64)], cdi: &[&str]) -> Server {
		let mut server = Server::new_remote(
			TeamId(1),
			"s".to_string(),
			"h".to_string(),
			22,
			"root".to_string(),
			crate::ids::SshKeyId(1),
			"/run/podman.sock".to_string(),
		);
		server.hardware = Some(sqlx::types::Json(Hardware {
			cpu: Cpu {
				model: "c".to_string(),
				threads: 1,
			},
			memory: Memory {
				total_mb: 1,
				available_mb: 1,
			},
			disks: vec![],
			gpus: gpus
				.iter()
				.map(|(vendor, vram_mb)| Gpu {
					vendor: (*vendor).to_string(),
					name: "g".to_string(),
					vram_mb: *vram_mb,
					vram_used_mb: None,
				})
				.collect(),
			cdi: Cdi {
				available: !cdi.is_empty(),
				devices: cdi.iter().map(|k| (*k).to_string()).collect(),
			},
		}));
		server
	}

	#[test]
	fn gpu_capable_follows_the_inventory() {
		let mut none = server_with(&[], &[]);
		none.hardware = None;
		assert!(!gpu_capable(&none));
		assert!(gpu_capable(&server_with(
			&[("nvidia", 12288)],
			&["nvidia.com/gpu"]
		)));
		// The GPU is there but the toolkit's CDI spec is not.
		assert!(!gpu_capable(&server_with(&[("nvidia", 12288)], &[])));
		// A spec without a GPU.
		assert!(!gpu_capable(&server_with(&[], &["nvidia.com/gpu"])));
		// Integrated AMD graphics do not count; a discrete card does.
		assert!(!gpu_capable(&server_with(&[("amd", 512)], &[])));
		assert!(gpu_capable(&server_with(&[("amd", 16384)], &[])));
	}

	const ED25519: &str = "AAAAC3NzaC1lZDI1NTE5AAAAIJchJVs7z1tmicd4SpMN5UdQGOeEF7c+3zkUwGLv44BI";
	const ED25519_FP: &str = "SHA256:ywBw9jYLna1cd5Li260999IYfpxB/BrS/G6GhT1Xkwg";
	const ECDSA: &str = "AAAAE2VjZHNhLXNoYTItbmlzdHAyNTYAAAAIbmlzdHAyNTYAAABBBC0zD4CZuHNU8aoHB/3WyovTEJm6iAtrq5ekzPOGCImXBltH3T+gigNYNhKvdeuC4OKk0eY2ttw9/S86rp3vSbg=";
	const ECDSA_FP: &str = "SHA256:FqUVfbXpKVRSXQGtouPKGojmEQVz6t/VNKnlHfvejQ0";

	#[test]
	fn fingerprint_matches_ssh_keygen() {
		let line = format!("10.0.0.5 ssh-ed25519 {ED25519}");
		assert_eq!(host_key_fingerprint(&line).as_deref(), Some(ED25519_FP));
	}

	#[test]
	fn fingerprint_prefers_ed25519_over_earlier_lines() {
		let keys = format!(
			"# 10.0.0.5:22 SSH-2.0-OpenSSH_9.6\n10.0.0.5 ecdsa-sha2-nistp256 {ECDSA}\n10.0.0.5 ssh-ed25519 {ED25519}"
		);
		assert_eq!(host_key_fingerprint(&keys).as_deref(), Some(ED25519_FP));
	}

	#[test]
	fn fingerprint_falls_back_to_first_key_line() {
		let keys = format!("# banner\n[10.0.0.5]:2222 ecdsa-sha2-nistp256 {ECDSA}");
		assert_eq!(host_key_fingerprint(&keys).as_deref(), Some(ECDSA_FP));
	}

	#[test]
	fn fingerprint_rejects_garbage() {
		assert_eq!(host_key_fingerprint(""), None);
		assert_eq!(host_key_fingerprint("host ssh-ed25519 !!!notbase64"), None);
		assert_eq!(host_key_fingerprint("host"), None);
	}

	#[test]
	fn agent_summary_names_the_podman_versions_and_the_mode() {
		let mut summary = AgentSummary::default();
		assert_eq!((summary.podman_label(), summary.mode()), (None, None));
		summary.podman_versions = vec!["5.4.0".into()];
		summary.rootless = 2;
		assert_eq!(summary.podman_label().as_deref(), Some("Podman 5.4.0"));
		assert_eq!(summary.mode(), Some("rootless"));
		summary.podman_versions = vec!["4.9.3".into(), "5.0.1".into(), "5.4.0".into()];
		summary.rootful = 1;
		assert_eq!(
			summary.podman_label().as_deref(),
			Some("Podman 4.9.3 to 5.4.0")
		);
		assert_eq!(summary.mode(), Some("mixed"));
		summary.rootless = 0;
		assert_eq!(summary.mode(), Some("rootful"));
	}

	#[test]
	fn versions_sort_by_their_numbers() {
		let mut versions = vec!["5.10.0", "4.9.3", "5.4.0"];
		versions.sort_by_cached_key(|v| version_key(v));
		assert_eq!(versions, ["4.9.3", "5.4.0", "5.10.0"]);
	}
}
