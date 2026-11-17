//! Downloads a real file from Hugging Face with a real downloader container.
//! Needs a rootless podman.sock and network access (it pulls `python:3.13-slim`
//! and installs `huggingface_hub` into it).
//! Run explicitly: `cargo test -p agent --test model_download -- --ignored`.

use std::sync::Arc;
use std::time::Duration;

use agent::Ctx;
use agent::model_download;
use agent::podman::{ContainerSpec, Podman, local_socket_path};
use agent::supervisor::Supervisor;
use futures_util::StreamExt;
use proto::{Event, GATED_MODEL_MESSAGE, ModelDownloadRequest};
use tokio::sync::mpsc;

/// `ggml-org/tiny-llamas` holds 1,185,376 bytes in `stories260K.gguf`.
const REPO: &str = "ggml-org/tiny-llamas";
const FILE: &str = "stories260K.gguf";
const FILE_BYTES: u64 = 1_185_376;

async fn ctx() -> Arc<Ctx> {
	let _ = rustls::crypto::ring::default_provider().install_default();
	let socket = local_socket_path();
	let podman = Podman::connect(&socket).expect("connect to podman.sock");
	let data_dir = tempfile::tempdir().unwrap().keep();
	Arc::new(Ctx {
		docker: podman.docker().clone(),
		supervisor: Supervisor::start(podman.clone(), &data_dir).await,
		data_dir,
		podman_socket: socket,
		http: reqwest::Client::new(),
	})
}

async fn download(ctx: &Arc<Ctx>, req: ModelDownloadRequest) -> Vec<Event> {
	let (tx, mut rx) = mpsc::channel(256);
	tokio::spawn(model_download::run(ctx.clone(), req, tx));
	let mut events = Vec::new();
	while let Some(event) = rx.recv().await {
		let terminal = event.is_terminal();
		events.push(event);
		if terminal {
			break;
		}
	}
	events
}

/// `du -sb` of the volume, from a throwaway container.
async fn volume_bytes(podman: &Podman, volume: &str) -> u64 {
	let name = format!("{volume}-du");
	podman.remove(&name).await.unwrap();
	podman
		.create_container(ContainerSpec {
			name: name.clone(),
			image: model_download::IMAGE.to_owned(),
			command: Some(vec![
				"sh".to_owned(),
				"-c".to_owned(),
				format!("du -sb /models/{FILE}"),
			]),
			binds: vec![format!("{volume}:/models")],
			..Default::default()
		})
		.await
		.unwrap();
	podman.start(&name).await.unwrap();
	tokio::time::sleep(Duration::from_secs(2)).await;
	let mut out = String::new();
	let mut logs = std::pin::pin!(podman.logs(&name, None, false));
	while let Some(chunk) = logs.next().await {
		out.push_str(&chunk.unwrap().to_string());
	}
	podman.remove(&name).await.unwrap();
	out.split_whitespace().next().unwrap().parse().unwrap()
}

fn request(volume: &str, token: Option<&str>) -> ModelDownloadRequest {
	ModelDownloadRequest {
		volume: volume.to_owned(),
		repo: REPO.to_owned(),
		revision: "main".to_owned(),
		include: vec![FILE.to_owned()],
		token: token.map(str::to_owned),
	}
}

#[tokio::test]
#[ignore = "requires a live podman.sock and network access"]
async fn downloads_one_file_resumes_and_removes_its_container() {
	let ctx = ctx().await;
	let podman = ctx.podman();
	let volume = "podkit-test-model-files";
	let _ = podman.remove_volume(volume).await;

	let events = download(&ctx, request(volume, None)).await;
	assert_eq!(events.last(), Some(&Event::Done), "{events:#?}");
	assert_eq!(volume_bytes(&podman, volume).await, FILE_BYTES);

	// Running it again over the same volume is the resume path: it succeeds
	// without downloading the file a second time.
	let events = download(&ctx, request(volume, None)).await;
	assert_eq!(events.last(), Some(&Event::Done), "{events:#?}");
	assert_eq!(volume_bytes(&podman, volume).await, FILE_BYTES);

	// The throwaway container is gone once the download ends.
	tokio::time::sleep(Duration::from_millis(500)).await;
	assert!(
		podman
			.inspect(&format!("podkit-modeldl-{volume}"))
			.await
			.unwrap()
			.is_none()
	);

	podman.remove_volume(volume).await.unwrap();
}

/// Hugging Face lists a gated repo's files to anyone but refuses the download
/// without a token that has accepted the license, so this is the gated path.
#[tokio::test]
#[ignore = "requires a live podman.sock and network access"]
async fn a_gated_repo_gets_the_license_message_and_the_token_stays_secret() {
	let ctx = ctx().await;
	let podman = ctx.podman();
	let volume = "podkit-test-model-files-gated";
	let token = "hf_thisIsNotARealTokenAtAll0123456789";

	for token in [None, Some(token)] {
		let events = download(
			&ctx,
			ModelDownloadRequest {
				volume: volume.to_owned(),
				repo: "google/gemma-2-2b".to_owned(),
				revision: "main".to_owned(),
				include: vec!["config.json".to_owned()],
				token: token.map(str::to_owned),
			},
		)
		.await;
		let Some(Event::Failed { message }) = events.last() else {
			panic!("expected a failure: {events:#?}");
		};
		assert_eq!(message, GATED_MODEL_MESSAGE, "{events:#?}");
		assert!(
			!format!("{events:?}").contains("hf_thisIsNot"),
			"token leaked into the stream: {events:#?}"
		);
	}
	let _ = podman.remove_volume(volume).await;
}
