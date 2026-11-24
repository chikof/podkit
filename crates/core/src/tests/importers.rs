//! What the importer tests share with the tests of the import pages: the recorded responses
//! of each platform under `tests/fixtures/importers/`, and a stub HTTP server that serves
//! them, so the fetching is tested as well as the mapping.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use axum::Router;
use axum::body::Body;
use axum::extract::State;
use axum::http::{Request, StatusCode};
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

use crate::importers::{Snapshot, Source};

pub const COOLIFY: &str = include_str!("../../tests/fixtures/importers/coolify.json");
pub const DOKPLOY: &str = include_str!("../../tests/fixtures/importers/dokploy.json");
pub const CAPROVER: &str = include_str!("../../tests/fixtures/importers/caprover.json");

/// The responses of a fixture, without its `_provenance` notes.
pub fn fixture(text: &str) -> Snapshot {
	let all: BTreeMap<String, Value> = serde_json::from_str(text).unwrap();
	Snapshot(
		all.into_iter()
			.filter(|(k, _)| !k.starts_with('_'))
			.collect(),
	)
}

/// What the stub of a platform expects and serves.
pub struct Stub {
	source: Source,
	responses: BTreeMap<String, Value>,
	/// `(header, value)` every request must carry.
	auth: (&'static str, String),
	/// Requests that were not a `GET` (CapRover's login is the one allowed `POST`).
	pub writes: AtomicUsize,
	pub requests: AtomicUsize,
}

async fn handle(State(stub): State<Arc<Stub>>, req: Request<Body>) -> Response {
	stub.requests.fetch_add(1, Ordering::Relaxed);
	let path = req
		.uri()
		.path_and_query()
		.map_or_else(String::new, ToString::to_string);
	let headers = req.headers().clone();
	let method = req.method().clone();
	let header = |name: &str| {
		headers
			.get(name)
			.and_then(|v| v.to_str().ok())
			.unwrap_or_default()
			.to_owned()
	};
	if stub.source == Source::Caprover && path == "/api/v2/login" {
		assert_eq!(header("x-namespace"), "captain");
		let body = axum::body::to_bytes(req.into_body(), 4096).await.unwrap();
		let sent: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
		return if sent["password"] == "right-password" {
			axum::Json(
				json!({ "status": 100, "description": "Login succeeded", "data": { "token": "cap-session" } }),
			)
			.into_response()
		} else {
			axum::Json(json!({ "status": 1106, "description": "Wrong password" })).into_response()
		};
	}
	if method != axum::http::Method::GET {
		stub.writes.fetch_add(1, Ordering::Relaxed);
		return StatusCode::METHOD_NOT_ALLOWED.into_response();
	}
	if header(stub.auth.0) != stub.auth.1 {
		return (StatusCode::UNAUTHORIZED, "token tok-in-error-body").into_response();
	}
	match stub.responses.get(&path) {
		Some(v) => axum::Json(v.clone()).into_response(),
		None => StatusCode::NOT_FOUND.into_response(),
	}
}

pub async fn serve(source: Source, fixture_text: &str) -> (String, Arc<Stub>) {
	let auth = match source {
		Source::Coolify => ("authorization", "Bearer right-token".to_owned()),
		Source::Dokploy => ("x-api-key", "right-token".to_owned()),
		Source::Caprover => ("x-captain-auth", "cap-session".to_owned()),
	};
	let stub = Arc::new(Stub {
		source,
		responses: fixture(fixture_text).0,
		auth,
		writes: AtomicUsize::new(0),
		requests: AtomicUsize::new(0),
	});
	let router = Router::new().fallback(handle).with_state(stub.clone());
	let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
	let url = format!("http://{}", listener.local_addr().unwrap());
	tokio::spawn(async move {
		axum::serve(listener, router).await.unwrap();
	});
	(url, stub)
}
