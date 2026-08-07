//! Proves the actual ingress mechanism: Traefik discovers an app container
//! via docker-provider labels on the same podman.sock we already talk to,
//! and correctly proxies HTTP to it over the shared `podkit` network, with
//! no host port published on the app container itself.
//!
//! Binds Traefik to a non-privileged test port. Rootless podman on most
//! hosts, including this one, can't bind :80 without either
//! `net.ipv4.ip_unprivileged_port_start<=80` or rootful podman, so this test
//! exercises the identical code path via `ensure_traefik`'s port param.
//!
//! Run explicitly: `cargo test -p runtime --test traefik -- --ignored`.

use podkit_core::domain::runtime::container_runtime::ContainerRuntime;
use podkit_core::domain::runtime::entity::{BuildSpec, ContainerId, ContainerSpec, ImageRef};
use runtime::PodmanRuntime;

const TEST_INGRESS_PORT: u16 = 18080;

fn make_tar(dockerfile: &[u8]) -> Vec<u8> {
	let mut tar = tar::Builder::new(Vec::new());
	let mut header = tar::Header::new_gnu();
	header.set_path("Dockerfile").unwrap();
	header.set_size(dockerfile.len() as u64);
	header.set_mode(0o644);
	header.set_cksum();
	tar.append(&header, dockerfile).unwrap();
	tar.into_inner().unwrap()
}

#[tokio::test]
#[ignore = "requires a live podman.sock and binds a local test port"]
async fn traefik_routes_to_app_container_via_docker_labels() {
	let rt = PodmanRuntime::connect_local_default().expect("connect to podman.sock");
	let socket_path = runtime::local_socket_path();

	// clean slate
	let _ = rt
		.remove_container(&ContainerId("podkit-traefik".to_string()), true)
		.await;
	let _ = rt
		.remove_container(&ContainerId("podkit-ingress-test-app".to_string()), true)
		.await;

	runtime::ensure_traefik(
		&rt,
		&socket_path,
		TEST_INGRESS_PORT,
		TEST_INGRESS_PORT + 1,
		None,
	)
	.await
	.expect("ensure_traefik");

	// a tiny http server baked into the image so we don't depend on any
	// app-level framework, just busybox httpd serving a static marker file.
	let dockerfile = b"FROM docker.io/library/busybox:1.36\n\
		RUN mkdir -p /www && echo hello-from-ingress-test > /www/index.html\n\
		CMD [\"httpd\", \"-f\", \"-p\", \"8080\", \"-h\", \"/www\"]\n";

	rt.build_image(BuildSpec {
		tag: ImageRef("podkit-ingress-test:latest".to_string()),
		dockerfile_path: "Dockerfile".to_string(),
		context_tar: make_tar(dockerfile),
	})
	.await
	.expect("build test app image");

	let hostname = "ingress-test.127-0-0-1.sslip.io";
	let labels = runtime::routing_labels("ingress-test", hostname, 8080);

	let id = rt
		.create_container(ContainerSpec {
			name: "podkit-ingress-test-app".to_string(),
			image: ImageRef("podkit-ingress-test:latest".to_string()),
			command: None,
			env: vec![],
			ports: vec![], // no host publish, reached only via Traefik + the shared network
			networks: vec![runtime::traefik::NETWORK.to_string()],
			labels,
			binds: vec![],
			resource_limits: podkit_core::domain::runtime::entity::ResourceLimits::default(),
			restart_policy: podkit_core::domain::runtime::entity::RestartPolicy::Never,
		})
		.await
		.expect("create app container");
	rt.start_container(&id).await.expect("start app container");

	// Traefik's docker provider polls periodically, so give it a moment to
	// pick up the new container before asserting on routing.
	let client = reqwest::Client::new();
	let mut body = None;
	for _ in 0..20 {
		tokio::time::sleep(std::time::Duration::from_millis(500)).await;
		let resp = client
			.get(format!("http://127.0.0.1:{TEST_INGRESS_PORT}/"))
			.header("Host", hostname)
			.send()
			.await;
		if let Ok(resp) = resp
			&& resp.status().is_success()
		{
			body = Some(resp.text().await.unwrap());
			break;
		}
	}

	let body = body.expect("Traefik never routed to the app container in time");
	assert!(
		body.contains("hello-from-ingress-test"),
		"expected app marker in response body, got: {body:?}"
	);

	// cross-check: a request for a hostname with no matching router should
	// NOT hit our app (proves this isn't just "any request reaches it").
	let resp = client
		.get(format!("http://127.0.0.1:{TEST_INGRESS_PORT}/"))
		.header("Host", "unrelated.example.com")
		.send()
		.await
		.expect("request with unmatched Host");
	assert_ne!(
		resp.status().as_u16(),
		200,
		"unmatched Host must not route to the app"
	);

	rt.remove_container(&id, true)
		.await
		.expect("cleanup app container");
	rt.remove_container(&ContainerId("podkit-traefik".to_string()), true)
		.await
		.expect("cleanup traefik container");
}

/// Proves Traefik actually accepts and starts cleanly with ACME configured,
/// and that plain-HTTP routing still works alongside it (regression). What
/// this can't prove in a sandbox with no public IP/DNS is a *real* Let's
/// Encrypt HTTP-01 challenge succeeding, since that requires the challenge
/// port reachable from the internet, which loopback isn't.
#[tokio::test]
#[ignore = "requires a live podman.sock and binds local test ports"]
async fn traefik_starts_cleanly_with_acme_configured() {
	let rt = PodmanRuntime::connect_local_default().expect("connect to podman.sock");
	let socket_path = runtime::local_socket_path();
	let ingress_port = TEST_INGRESS_PORT + 10;
	let tls_port = TEST_INGRESS_PORT + 11;

	let _ = rt
		.remove_container(&ContainerId("podkit-traefik".to_string()), true)
		.await;
	let _ = rt
		.remove_container(&ContainerId("podkit-acme-test-app".to_string()), true)
		.await;

	let acme = runtime::AcmeConfig {
		email: "test@podkit.dev",
	};
	runtime::ensure_traefik(&rt, &socket_path, ingress_port, tls_port, Some(&acme))
		.await
		.expect("ensure_traefik with acme");

	tokio::time::sleep(std::time::Duration::from_millis(1500)).await;

	let traefik_id = ContainerId("podkit-traefik".to_string());
	let status = rt.inspect(&traefik_id).await.expect("inspect traefik");
	assert_eq!(
		status.state,
		podkit_core::domain::runtime::entity::ContainerState::Running,
		"traefik did not stay running with acme configured, bad cli flags would crash it immediately"
	);

	let logs = rt
		.logs(
			&traefik_id,
			podkit_core::domain::runtime::entity::LogsQuery::default(),
		)
		.await
		.unwrap_or_default()
		.join("\n");
	assert!(
		!logs.to_lowercase().contains("error initializing"),
		"traefik logged an acme/resolver init error: {logs}"
	);

	// Regression: plain HTTP routing (no cert needed) still works with ACME
	// configured alongside it.
	let dockerfile = b"FROM docker.io/library/busybox:1.36\n\
		RUN mkdir -p /www && echo acme-sibling-ok > /www/index.html\n\
		CMD [\"httpd\", \"-f\", \"-p\", \"8080\", \"-h\", \"/www\"]\n";
	rt.build_image(BuildSpec {
		tag: ImageRef("podkit-acme-test:latest".to_string()),
		dockerfile_path: "Dockerfile".to_string(),
		context_tar: make_tar(dockerfile),
	})
	.await
	.expect("build test app image");

	let hostname = "acme-test.127-0-0-1.sslip.io";
	let mut labels = runtime::routing_labels("acme-test", hostname, 8080);
	labels.extend(runtime::secure_routing_labels(
		"acme-test",
		&["acme-test.example.invalid".to_string()],
	));

	let id = rt
		.create_container(ContainerSpec {
			name: "podkit-acme-test-app".to_string(),
			image: ImageRef("podkit-acme-test:latest".to_string()),
			command: None,
			env: vec![],
			ports: vec![],
			networks: vec![runtime::traefik::NETWORK.to_string()],
			labels,
			binds: vec![],
			resource_limits: podkit_core::domain::runtime::entity::ResourceLimits::default(),
			restart_policy: podkit_core::domain::runtime::entity::RestartPolicy::Never,
		})
		.await
		.expect("create app container");
	rt.start_container(&id).await.expect("start app container");

	let client = reqwest::Client::new();
	let mut body = None;
	for _ in 0..20 {
		tokio::time::sleep(std::time::Duration::from_millis(500)).await;
		let resp = client
			.get(format!("http://127.0.0.1:{ingress_port}/"))
			.header("Host", hostname)
			.send()
			.await;
		if let Ok(resp) = resp
			&& resp.status().is_success()
		{
			body = Some(resp.text().await.unwrap());
			break;
		}
	}
	let body = body.expect("plain HTTP router never came up alongside ACME config");
	assert!(body.contains("acme-sibling-ok"), "got: {body:?}");

	rt.remove_container(&id, true)
		.await
		.expect("cleanup app container");
	rt.remove_container(&traefik_id, true)
		.await
		.expect("cleanup traefik container");
}
