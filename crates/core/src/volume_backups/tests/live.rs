//! The real thing: the real agent, Podman and a Garage container.
//!
//! Writes a file into a volume, backs the volume up, deletes the file, restores, and
//! checks the file is back with the same sha256.
//!
//! Needs a rootless `podman.sock` and the network (it pulls `alpine` and `garage`).
//! `cargo test -p podkit-core -p podkit-ui --all-features -- --ignored volume_backup_live`
//!
//! Garage: single-node start with a default key and bucket from the environment,
//! <https://garagehq.deuxfleurs.fr/documentation/quick-start/> (needs v2.3 or newer);
//! the config keys are from the same page. Garage serves path-style requests unless a
//! `root_domain` is set, and its region is whatever `s3_region` says.

use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use agent::Ctx;
use agent::podman::{Podman, local_socket_path};
use agent::supervisor::Supervisor;
use serde_json::json;
use sqlx::PgPool;

use super::Fx;
use crate::jobs::{self, JobKind};

const GARAGE: &str = "docker.io/dxflrs/garage:v2.5.0";
const IMAGE: &str = "docker.io/library/alpine:3.21";
const ACCESS_KEY: &str = "GK0123456789abcdef01234567";
const SECRET_KEY: &str = "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff";
const BUCKET: &str = "podkit-volume-backups";
const TASK: &str = "t27ec69b4";

/// `podman` with `args`, its stdout on success.
fn podman(args: &[&str]) -> Result<String, String> {
	let path = format!(
		"{}:/run/current-system/sw/bin",
		std::env::var("PATH").unwrap_or_default()
	);
	let out = Command::new("podman")
		.args(args)
		.env("PATH", path)
		.output()
		.map_err(|e| format!("podman {args:?}: {e}"))?;
	if out.status.success() {
		Ok(String::from_utf8_lossy(&out.stdout).into_owned())
	} else {
		Err(format!(
			"podman {args:?}: {}",
			String::from_utf8_lossy(&out.stderr)
		))
	}
}

/// Removes what the test made, by name, whatever happened.
struct Cleanup {
	containers: Vec<String>,
	volumes: Vec<String>,
}

impl Drop for Cleanup {
	fn drop(&mut self) {
		for name in &self.containers {
			let _ = podman(&["rm", "-f", "-t", "0", name]);
		}
		for name in &self.volumes {
			let _ = podman(&["volume", "rm", "-f", name]);
		}
	}
}

/// What `script` prints in a throwaway container with the volume at `/data`.
fn in_volume(volume: &str, script: &str) -> String {
	podman(&[
		"run",
		"--rm",
		"--name",
		&format!("pk-{TASK}-sh"),
		"-v",
		&format!("{volume}:/data"),
		IMAGE,
		"sh",
		"-c",
		script,
	])
	.unwrap()
}

#[sqlx::test]
#[ignore = "needs a live podman.sock, the network and a free local port"]
async fn volume_backup_live_restores_a_file_with_the_same_sha256(db: PgPool) {
	let _ = rustls::crypto::ring::default_provider().install_default();
	let garage = format!("pk-{TASK}-garage");
	let port = 20_000 + u16::from_str_radix(&TASK[TASK.len() - 3..], 16).unwrap() % 1000;
	let dir = tempfile::tempdir().unwrap();
	let config = dir.path().join("garage.toml");
	std::fs::write(
		&config,
		format!(
			"metadata_dir = \"/tmp/meta\"\ndata_dir = \"/tmp/data\"\ndb_engine = \"sqlite\"\n\
			replication_factor = 1\nrpc_bind_addr = \"[::]:3901\"\nrpc_secret = \"{SECRET_KEY}\"\n\n\
			[s3_api]\ns3_region = \"garage\"\napi_bind_addr = \"[::]:3900\"\n"
		),
	)
	.unwrap();

	let f = Fx::build(db, false).await;
	let volume = f.volume_name.clone();
	let _cleanup = Cleanup {
		containers: vec![garage.clone(), format!("pk-{TASK}-sh")],
		volumes: vec![volume.clone()],
	};

	// Garage, with the key and bucket made at start.
	let _ = podman(&["rm", "-f", "-t", "0", &garage]);
	podman(&[
		"run",
		"-d",
		"--name",
		&garage,
		"-p",
		&format!("127.0.0.1:{port}:3900"),
		"-v",
		&format!("{}:/etc/garage.toml:ro,z", config.display()),
		"-e",
		&format!("GARAGE_DEFAULT_ACCESS_KEY={ACCESS_KEY}"),
		"-e",
		&format!("GARAGE_DEFAULT_SECRET_KEY={SECRET_KEY}"),
		"-e",
		&format!("GARAGE_DEFAULT_BUCKET={BUCKET}"),
		GARAGE,
		"/garage",
		"server",
		"--single-node",
		"--default-bucket",
	])
	.unwrap();
	let http = reqwest::Client::new();
	let mut ready = false;
	for _ in 0..60 {
		// Any answer at all means the S3 api is listening.
		if http
			.get(format!("http://127.0.0.1:{port}/"))
			.send()
			.await
			.is_ok()
		{
			ready = true;
			break;
		}
		tokio::time::sleep(Duration::from_secs(1)).await;
	}
	assert!(
		ready,
		"garage did not come up: {:?}",
		podman(&["logs", &garage])
	);
	tokio::time::sleep(Duration::from_secs(3)).await;

	// The destination, with the keys encrypted like any other.
	let secret = f.state.secrets.encrypt(SECRET_KEY).unwrap();
	sqlx::query(
		"UPDATE backup_targets SET endpoint = $2, region = 'garage', bucket = $3,
			access_key_id = $4, secret_access_key = $5, path_style = TRUE WHERE id = $1",
	)
	.bind(f.target)
	.bind(format!("http://127.0.0.1:{port}"))
	.bind(BUCKET)
	.bind(ACCESS_KEY)
	.bind(secret)
	.execute(&f.db)
	.await
	.unwrap();

	// The real agent, on the socket the control plane's local server uses.
	let socket = local_socket_path();
	let podman_client = Podman::connect(&socket).expect("connect to podman.sock");
	let data_dir = dir.path().join("agent");
	std::fs::create_dir_all(&data_dir).unwrap();
	let ctx = Arc::new(Ctx {
		docker: podman_client.docker().clone(),
		supervisor: Supervisor::start(podman_client.clone(), &data_dir).await,
		data_dir,
		podman_socket: socket,
		http: reqwest::Client::new(),
	});
	let listener = tokio::net::UnixListener::bind(&f.state.config.local_agent_socket).unwrap();
	let router = agent::routes::router(ctx);
	tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });

	// A volume with files in it, one big enough to be more than a few tar blocks.
	podman(&["volume", "create", &volume]).unwrap();
	in_volume(
		&volume,
		"mkdir -p /data/sub && head -c 3000000 /dev/urandom > /data/sub/blob.bin && echo hello > /data/hello.txt",
	);
	let sum = |path: &str| in_volume(&volume, &format!("sha256sum {path}"));
	let before = sum("/data/sub/blob.bin");

	f.configure(json!({ "consistency": "live" })).await;
	let (backup, result) = f.back_up().await;
	result.unwrap();
	let (status, error, _, entries) = f.backup_row(backup).await;
	assert_eq!(status, "succeeded", "{error:?}");
	assert!(entries.unwrap() >= 3, "{entries:?}");

	// Lose the data, then get it back.
	in_volume(
		&volume,
		"rm -rf /data/sub /data/hello.txt && echo stray > /data/stray.txt",
	);
	assert!(in_volume(&volume, "ls /data").contains("stray.txt"));

	let (s, restore) = f
		.restore(backup, json!({ "mode": "same", "confirm": "/data" }))
		.await;
	assert_eq!(s, axum::http::StatusCode::ACCEPTED, "{restore}");
	let id = restore["id"].as_i64().unwrap();
	let job = jobs::latest_for(&f.db, JobKind::VolumeRestore, id)
		.await
		.unwrap()
		.unwrap();
	f.run(job).await.unwrap();
	assert_eq!(f.restore_status(id).await, ("succeeded".to_owned(), None));

	assert_eq!(
		sum("/data/sub/blob.bin"),
		before,
		"the file is back with the same sha256"
	);
	assert_eq!(in_volume(&volume, "cat /data/hello.txt").trim(), "hello");
	let listing = in_volume(&volume, "ls /data");
	assert!(
		!listing.contains("stray.txt"),
		"a restore replaces the volume: {listing}"
	);
}
