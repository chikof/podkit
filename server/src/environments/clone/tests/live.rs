//! The real thing: the real agent, Podman, a Garage container and the job dispatcher.
//!
//! Clones an environment that has an application and a Postgres **with data**: the
//! source database gets 500 rows, the clone is made with the data and deploy options, and
//! the cloned database must hold the same 500 rows while both applications answer on
//! their own URLs.
//!
//! NOT RUN by the author: the machine it was written on has no Podman. Run it on a
//! machine with a rootless `podman.sock`, the network (it pulls alpine, garage, postgres
//! and traefik/whoami) and no podkit ingress of its own on the ports below:
//! `cargo test -p server --all-features -- --ignored clone_env_live`
//!
//! Garage as in `volume_backups::tests::live` (v2.3 or newer; single-node start with a
//! default key and bucket from the environment, path-style requests):
//! <https://garagehq.deuxfleurs.fr/documentation/quick-start/>

use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use agent::Ctx;
use agent::podman::{Podman, local_socket_path};
use agent::supervisor::Supervisor;
use sqlx::PgPool;

use super::{Fx, input};
use crate::applications::public_hostname;
use crate::databases::service::CreateInput;
use crate::databases::{self, DatabaseStatus, EngineKind};
use crate::deployments;
use crate::environments::clone::{CloneStatus, StepStatus, find_by_id};
use crate::ids::{ApplicationId, DatabaseId, ServerId};

const GARAGE: &str = "docker.io/dxflrs/garage:v2.5.0";
const ACCESS_KEY: &str = "GK0123456789abcdef01234567";
const SECRET_KEY: &str = "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff";
const BUCKET: &str = "podkit-clone-env";
const TASK: &str = "t926bcc9e";

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

/// Removes by name what the test made, whatever happened. Container ids of the
/// databases and deployments are added as they appear.
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

async fn database_status(f: &Fx, id: DatabaseId) -> DatabaseStatus {
	databases::find_by_id(&f.db, id)
		.await
		.unwrap()
		.unwrap()
		.status
}

async fn wait_database(f: &Fx, id: DatabaseId) -> String {
	for _ in 0..180 {
		let row = databases::find_by_id(&f.db, id).await.unwrap().unwrap();
		match row.status {
			DatabaseStatus::Running => return row.container_id.unwrap(),
			DatabaseStatus::Failed => panic!("database failed: {:?}", row.status_message),
			_ => tokio::time::sleep(Duration::from_secs(2)).await,
		}
	}
	panic!("database did not start: {:?}", database_status(f, id).await);
}

/// What `sql` returns from the database's own `psql`.
fn psql(container: &str, sql: &str) -> String {
	podman(&[
		"exec", container, "psql", "-U", "podkit", "-d", "podkit", "-tA", "-c", sql,
	])
	.unwrap()
	.trim()
	.to_owned()
}

#[sqlx::test]
#[ignore = "needs a live podman.sock, the network and free local ports"]
async fn clone_env_live_copies_a_postgres_with_its_rows_and_both_apps_answer(db: PgPool) {
	let _ = rustls::crypto::ring::default_provider().install_default();
	let garage = format!("pk-{TASK}-garage");
	let base_port = 20_000 + u16::from_str_radix(&TASK[TASK.len() - 3..], 16).unwrap() % 1000;
	let (s3_port, ingress_port) = (base_port, base_port + 1);
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

	let mut f = Fx::empty(db).await;
	let mut config_copy = (*f.state.config).clone();
	config_copy.ingress_port = ingress_port;
	config_copy.https_port = ingress_port + 1;
	f.state.config = Arc::new(config_copy);
	let mut cleanup = Cleanup {
		containers: vec![garage.clone()],
		volumes: Vec::new(),
	};

	// Garage, with the key and bucket made at start.
	let _ = podman(&["rm", "-f", "-t", "0", &garage]);
	podman(&[
		"run",
		"-d",
		"--name",
		&garage,
		"-p",
		&format!("127.0.0.1:{s3_port}:3900"),
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
		if http
			.get(format!("http://127.0.0.1:{s3_port}/"))
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
	let target = f.backup_target().await;
	sqlx::query(
		"UPDATE backup_targets SET endpoint = $2, region = 'garage', bucket = $3,
			access_key_id = $4, secret_access_key = $5, path_style = TRUE WHERE id = $1",
	)
	.bind(target)
	.bind(format!("http://127.0.0.1:{s3_port}"))
	.bind(BUCKET)
	.bind(ACCESS_KEY)
	.bind(f.enc(SECRET_KEY))
	.execute(&f.db)
	.await
	.unwrap();

	// The real agent on the socket the control plane's local server uses, and the real
	// dispatcher, so every job the clone queues runs.
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
	tokio::spawn({
		let router = agent::routes::router(ctx);
		async move { axum::serve(listener, router).await.unwrap() }
	});
	tokio::spawn(crate::jobs::worker::run_dispatcher(
		f.state.clone(),
		f.state.shutdown.clone(),
	));

	// The source: a Postgres with 500 rows and an application.
	let source_db = databases::service::create(
		&f.state,
		f.user,
		f.team_id(),
		f.project_id(),
		CreateInput {
			name: "orders".to_owned(),
			engine: EngineKind::Postgres,
			version: None,
			server_id: ServerId(f.server),
			public_port: None,
			memory_limit_mb: None,
			cpu_limit: None,
			environment_id: Some(f.source),
		},
	)
	.await
	.unwrap();
	cleanup
		.volumes
		.push(format!("podkit-db-{}-data", source_db.id));
	let source_container = wait_database(&f, source_db.id).await;
	cleanup.containers.push(source_container.clone());
	psql(
		&source_container,
		"CREATE TABLE orders (n int); INSERT INTO orders SELECT generate_series(1, 500)",
	);

	sqlx::query(
		"INSERT INTO applications
			(id, project_id, environment_id, server_id, name, slug, source_kind, image, container_port)
		VALUES (5001, $1, $2, $3, 'Whoami', 'whoami', 'image', 'docker.io/traefik/whoami:v1.10', 80)",
	)
	.bind(f.project)
	.bind(f.source)
	.bind(f.server)
	.execute(&f.db)
	.await
	.unwrap();

	// The clone: data and deploy.
	let mut i = input("Staging", "staging");
	i.options.data = true;
	i.options.deploy = true;
	i.options.backup_target_id = Some(target);
	let clone = f.clone(i).await.unwrap();
	let mut done = None;
	for _ in 0..450 {
		let row = find_by_id(&f.db, clone.id).await.unwrap().unwrap();
		if row.status.is_final() {
			done = Some(row);
			break;
		}
		tokio::time::sleep(Duration::from_secs(2)).await;
	}
	let done = done.expect("the clone did not finish in 15 minutes");
	assert_eq!(done.status, CloneStatus::Succeeded, "{:?}", done.report.0);
	assert!(done.report.0.iter().all(|s| s.status == StepStatus::Ok));

	// The cloned database has the rows.
	let cloned = databases::list_by_environment(&f.db, done.environment_id.unwrap())
		.await
		.unwrap()
		.remove(0);
	let cloned_container = cloned.container_id.clone().unwrap();
	cleanup.containers.push(cloned_container.clone());
	cleanup
		.volumes
		.push(format!("podkit-db-{}-data", cloned.id));
	assert_eq!(
		psql(&cloned_container, "SELECT count(*) FROM orders"),
		"500"
	);
	assert_eq!(
		psql(&source_container, "SELECT count(*) FROM orders"),
		"500",
		"the source is untouched"
	);

	// Both applications answer on their own URLs. The source was never deployed: deploy
	// it now, as the person would.
	let source_app = ApplicationId(5001);
	let source = crate::applications::find_by_id(&f.db, source_app)
		.await
		.unwrap()
		.unwrap();
	deployments::queue(&f.state, &source, Some(f.user), None)
		.await
		.unwrap();
	let server = crate::scope::server(&f.db, f.team_id(), ServerId(f.server))
		.await
		.unwrap();
	let cloned_app = crate::applications::list_by_environment(&f.db, done.environment_id.unwrap())
		.await
		.unwrap()
		.remove(0);
	for app in [&source, &cloned_app] {
		let host = public_hostname(&app.slug, &server);
		let mut answered = false;
		for _ in 0..90 {
			let reply = http
				.get(format!("http://127.0.0.1:{ingress_port}/"))
				.header("Host", &host)
				.send()
				.await;
			if let Ok(reply) = reply
				&& reply.status().is_success()
				&& reply.text().await.is_ok_and(|b| b.contains("Hostname:"))
			{
				answered = true;
				break;
			}
			tokio::time::sleep(Duration::from_secs(1)).await;
		}
		assert!(answered, "{host} did not answer");
	}
	// What the deployments started goes with the test.
	let running: Vec<String> =
		sqlx::query_scalar("SELECT container_id FROM deployments WHERE container_id IS NOT NULL")
			.fetch_all(&f.db)
			.await
			.unwrap();
	cleanup.containers.extend(running);
}
