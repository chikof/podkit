//! Proves the actual ingress mechanism: Traefik discovers an app container
//! via docker-provider labels on the same podman.sock we already talk to,
//! and correctly proxies HTTP to it over the shared `podkit` network, with
//! no host port published on the app container itself.
//!
//! Binds Traefik to a non-privileged test port. Rootless podman on most
//! hosts can't bind :80 without either `net.ipv4.ip_unprivileged_port_start<=80`
//! or rootful podman, so this test exercises the identical code path via the
//! ingress ports.
//!
//! Run explicitly: `cargo test -p agent --test traefik -- --ignored`.

mod common;

use agent::ingress::{self, NETWORK, TRAEFIK_CONTAINER_NAME};
use agent::podman::{ContainerSpec, Podman, local_socket_path};
use proto::Ingress;

const TEST_INGRESS_PORT: u16 = 18080;

fn connect() -> Podman {
	// reqwest is built with `rustls-no-provider`.
	let _ = rustls::crypto::ring::default_provider().install_default();
	Podman::connect(&local_socket_path()).expect("connect to podman.sock")
}

async fn build_busybox_app(podman: &Podman, tag: &str, marker: &str) {
	let dir = tempfile::tempdir().unwrap();
	let dockerfile = format!(
		"FROM docker.io/library/busybox:1.36\n\
		RUN mkdir -p /www && echo {marker} > /www/index.html\n\
		CMD [\"httpd\", \"-f\", \"-p\", \"8080\", \"-h\", \"/www\"]\n"
	);
	let context = common::dockerfile_tar(dir.path(), dockerfile.as_bytes());
	common::build(podman, tag, "Dockerfile", &context).await;
}

async fn get_body(port: u16, host: &str) -> Option<String> {
	let client = reqwest::Client::new();
	for _ in 0..20 {
		tokio::time::sleep(std::time::Duration::from_millis(500)).await;
		let resp = client
			.get(format!("http://127.0.0.1:{port}/"))
			.header("Host", host)
			.send()
			.await;
		if let Ok(resp) = resp
			&& resp.status().is_success()
		{
			return Some(resp.text().await.unwrap());
		}
	}
	None
}

#[tokio::test]
#[ignore = "requires a live podman.sock and binds a local test port"]
async fn traefik_routes_to_app_container_via_docker_labels() {
	let podman = connect();
	let socket_path = local_socket_path();
	let data = tempfile::tempdir().unwrap();

	podman.remove(TRAEFIK_CONTAINER_NAME).await.unwrap();
	podman.remove("podkit-ingress-test-1").await.unwrap();

	let config = Ingress {
		http_port: TEST_INGRESS_PORT,
		https_port: TEST_INGRESS_PORT + 1,
		acme_email: None,
	};
	Box::pin(ingress::ensure_traefik(
		&podman,
		&config,
		&socket_path,
		data.path(),
	))
	.await
	.expect("ensure_traefik");

	build_busybox_app(
		&podman,
		"podkit-ingress-test:latest",
		"hello-from-ingress-test",
	)
	.await;

	let hostname = "ingress-test.127-0-0-1.sslip.io";
	podman
		.create_container(ContainerSpec {
			name: "podkit-ingress-test-1".to_string(),
			image: "podkit-ingress-test:latest".to_string(),
			// no host publish, reached only via Traefik + the shared network
			network: NETWORK.to_string(),
			labels: ingress::routing_labels("ingress-test", &[hostname.to_string()], 8080),
			..Default::default()
		})
		.await
		.expect("create app container");
	podman
		.start("podkit-ingress-test-1")
		.await
		.expect("start app container");

	let body = get_body(TEST_INGRESS_PORT, hostname)
		.await
		.expect("Traefik never routed to the app container in time");
	assert!(body.contains("hello-from-ingress-test"), "got: {body:?}");

	// a Host with no matching router must not reach the app
	let resp = reqwest::Client::new()
		.get(format!("http://127.0.0.1:{TEST_INGRESS_PORT}/"))
		.header("Host", "unrelated.example.com")
		.send()
		.await
		.expect("request with unmatched Host");
	assert_ne!(resp.status().as_u16(), 200);

	podman.remove("podkit-ingress-test-1").await.unwrap();
	podman.remove(TRAEFIK_CONTAINER_NAME).await.unwrap();
}

/// Proves Traefik accepts and starts cleanly with ACME configured, and that
/// plain-HTTP routing still works alongside it. A real Let's Encrypt HTTP-01
/// challenge cannot succeed on loopback, so that part is out of reach here.
#[tokio::test]
#[ignore = "requires a live podman.sock and binds local test ports"]
async fn traefik_starts_cleanly_with_acme_configured() {
	let podman = connect();
	let socket_path = local_socket_path();
	let data = tempfile::tempdir().unwrap();
	let ingress_port = TEST_INGRESS_PORT + 10;

	podman.remove(TRAEFIK_CONTAINER_NAME).await.unwrap();
	podman.remove("podkit-acme-test-1").await.unwrap();

	let config = Ingress {
		http_port: ingress_port,
		https_port: TEST_INGRESS_PORT + 11,
		acme_email: Some("test@podkit.dev".to_string()),
	};
	Box::pin(ingress::ensure_traefik(
		&podman,
		&config,
		&socket_path,
		data.path(),
	))
	.await
	.expect("ensure_traefik with acme");

	tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
	let running = podman
		.inspect(TRAEFIK_CONTAINER_NAME)
		.await
		.unwrap()
		.and_then(|i| i.state)
		.and_then(|s| s.running);
	assert_eq!(
		running,
		Some(true),
		"traefik did not stay running with acme configured, bad cli flags would crash it immediately"
	);

	build_busybox_app(&podman, "podkit-acme-test:latest", "acme-sibling-ok").await;

	let hostname = "acme-test.127-0-0-1.sslip.io";
	let mut labels = ingress::routing_labels("acme-test", &[hostname.to_string()], 8080);
	labels.extend(ingress::secure_routing_labels(
		"acme-test",
		&["acme-test.example.invalid".to_string()],
	));
	podman
		.create_container(ContainerSpec {
			name: "podkit-acme-test-1".to_string(),
			image: "podkit-acme-test:latest".to_string(),
			network: NETWORK.to_string(),
			labels,
			..Default::default()
		})
		.await
		.expect("create app container");
	podman
		.start("podkit-acme-test-1")
		.await
		.expect("start app container");

	let body = get_body(ingress_port, hostname)
		.await
		.expect("plain HTTP router never came up alongside ACME config");
	assert!(body.contains("acme-sibling-ok"), "got: {body:?}");

	podman.remove("podkit-acme-test-1").await.unwrap();
	podman.remove(TRAEFIK_CONTAINER_NAME).await.unwrap();
}

/// A running Traefik is kept while its config hash matches and replaced when it changes.
#[tokio::test]
#[ignore = "requires a live podman.sock and binds local test ports"]
async fn traefik_is_recreated_when_config_changes() {
	let podman = connect();
	let socket_path = local_socket_path();
	let data = tempfile::tempdir().unwrap();
	podman.remove(TRAEFIK_CONTAINER_NAME).await.unwrap();

	let id_of = |podman: Podman| async move {
		podman
			.inspect(TRAEFIK_CONTAINER_NAME)
			.await
			.unwrap()
			.and_then(|i| i.id)
			.expect("traefik exists")
	};

	let first = Ingress {
		http_port: TEST_INGRESS_PORT + 20,
		https_port: TEST_INGRESS_PORT + 21,
		acme_email: None,
	};
	Box::pin(ingress::ensure_traefik(
		&podman,
		&first,
		&socket_path,
		data.path(),
	))
	.await
	.unwrap();
	let id = id_of(podman.clone()).await;

	Box::pin(ingress::ensure_traefik(
		&podman,
		&first,
		&socket_path,
		data.path(),
	))
	.await
	.unwrap();
	assert_eq!(
		id,
		id_of(podman.clone()).await,
		"same config must keep the container"
	);

	let second = Ingress {
		http_port: TEST_INGRESS_PORT + 22,
		..first
	};
	Box::pin(ingress::ensure_traefik(
		&podman,
		&second,
		&socket_path,
		data.path(),
	))
	.await
	.unwrap();
	assert_ne!(
		id,
		id_of(podman.clone()).await,
		"changed config must recreate it"
	);

	podman.remove(TRAEFIK_CONTAINER_NAME).await.unwrap();
}
