//! The logo registry: SVGs under `crates/core/static/logos` embedded in the
//! binary, served at a URL that carries the file's hash so browsers can keep
//! them for a year, plus [`for_image`] to guess one from an image name.
//!
//! The files come from homarr-labs/dashboard-icons (Apache-2.0) and
//! simple-icons (CC0); `NOTICE` and `crates/core/static/logos/SOURCES.tsv` say
//! which and from where.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use axum::Router;
use axum::extract::Path;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;

use crate::state::AppState;
use crate::templates::embed::LOGOS;
use crate::templates::{Loaded, logo_type, short_hash};

/// A logo of the registry.
struct Entry {
	media: &'static str,
	ext: &'static str,
	bytes: &'static [u8],
	hash: String,
}

fn entries() -> &'static BTreeMap<String, Entry> {
	static ENTRIES: OnceLock<BTreeMap<String, Entry>> = OnceLock::new();
	ENTRIES.get_or_init(|| {
		LOGOS
			.iter()
			.filter_map(|(file, bytes)| {
				let (stem, ext) = file.rsplit_once('.')?;
				let media = logo_type(file)?;
				Some((
					stem.to_string(),
					Entry {
						media,
						ext: if ext == "svg" { "svg" } else { "png" },
						bytes,
						hash: short_hash(bytes),
					},
				))
			})
			.collect()
	})
}

/// Whether the registry has a logo called `name`.
#[must_use]
#[cfg(test)]
pub fn has(name: &str) -> bool {
	entries().contains_key(name)
}

/// The URL of registry logo `name`, with its hash.
#[must_use]
pub fn url(name: &str) -> Option<String> {
	let e = entries().get(name)?;
	Some(format!("/static/logos/{name}.{}.{}", e.hash, e.ext))
}

/// The URL of a template's logo: its own file, or the registry logo it names.
#[must_use]
pub fn template_url(l: &Loaded) -> String {
	if let Some(name) = l.template.logo.strip_prefix('@') {
		return url(name).unwrap_or_default();
	}
	let ext = if l.logo.media == "image/svg+xml" {
		"svg"
	} else {
		"png"
	};
	format!("/static/logos/t-{}.{}.{ext}", l.template.id, l.logo.hash)
}

/// Image names (the last path segment, without a tag) that are called
/// something else in the registry.
#[allow(
	dead_code,
	reason = "the gallery and the resource cards of the UI cards call for_image"
)]
const ALIASES: &[(&str, &str)] = &[
	("pgvector", "postgresql"),
	("postgis", "postgresql"),
	("timescaledb", "postgresql"),
	("mongo", "mongodb"),
	("mongo-express", "mongodb"),
	("mysql-server", "mysql"),
	("percona", "mysql"),
	("redis-stack", "redis"),
	("redis-stack-server", "redis"),
	("clickhouse-server", "clickhouse"),
	("couchdb", "couchdb"),
	("cassandra", "cassandra"),
	("rabbitmq-management", "rabbitmq"),
	("httpd", "apache"),
	("golang", "go"),
	("node", "nodejs"),
	("openjdk", "java"),
	("eclipse-temurin", "java"),
	("temurin", "java"),
	("amazoncorretto", "java"),
	("php-fpm", "php"),
	("ruby", "ruby"),
	("alpine", "alpinelinux"),
	("dotnet", "dotnet"),
	("aspnet", "dotnet"),
	("sdk", "dotnet"),
	("llama.cpp", "huggingface"),
	("llama-cpp", "huggingface"),
	("text-generation-inference", "huggingface"),
	("text-embeddings-inference", "huggingface"),
	("ghcr.io/open-webui/open-webui", "open-webui"),
	("docker", "docker"),
	("dind", "docker"),
	("kubectl", "kubernetes"),
	("k3s", "kubernetes"),
	("gitea", "gitea"),
	("act_runner", "gitea"),
	("elasticsearch", "elasticsearch"),
	("kibana", "elasticsearch"),
	("logstash", "elasticsearch"),
	("kafka", "kafka"),
	("cp-kafka", "kafka"),
	("bitnami-minio", "minio"),
	("qwen", "qwen"),
	("deepseek", "deepseek"),
	("mistral", "mistral"),
	("gemma", "google"),
	("llama", "meta"),
];

/// A registry logo for an image reference such as `postgres:18`,
/// `docker.io/library/redis:7-alpine` or `ghcr.io/ggml-org/llama.cpp:server`;
/// `None` when nothing matches.
#[must_use]
pub fn for_image(image: &str) -> Option<String> {
	name_for_image(image).and_then(url)
}

fn name_for_image(image: &str) -> Option<&'static str> {
	let image = image.trim().to_ascii_lowercase();
	let image = image.split('@').next().unwrap_or_default();
	// Drop the tag: a colon after the last slash.
	let last_slash = image.rfind('/').map_or(0, |i| i + 1);
	let image = match image[last_slash..].find(':') {
		Some(i) => &image[..last_slash + i],
		None => image,
	};
	let segments: Vec<&str> = image.split('/').filter(|s| !s.is_empty()).collect();
	let base = *segments.last()?;
	let reg = entries();
	let is = |name: &str| reg.get_key_value(name).map(|(k, _)| k.as_str());

	// The whole path first, then the last segment (or its alias), then the
	// part before a variant suffix (`postgres-alpine`, `redis_stack`).
	let resolve = |n: &str| {
		is(n).or_else(|| {
			ALIASES
				.iter()
				.find(|(from, _)| *from == n)
				.and_then(|(_, to)| is(to))
		})
	};
	let full = segments.join("/");
	for (from, to) in ALIASES {
		if (full == *from || full.ends_with(&format!("/{from}")))
			&& let Some(name) = is(to)
		{
			return Some(name);
		}
	}
	if let Some(name) = resolve(base) {
		return Some(name);
	}
	for sep in ['-', '_', '.'] {
		if let Some((head, _)) = base.split_once(sep)
			&& let Some(name) = resolve(head)
		{
			return Some(name);
		}
	}
	// Known organisations: `qwen/...`, `deepseek-ai/...`.
	for org in &segments[..segments.len() - 1] {
		let org = org.trim_end_matches("-ai").trim_end_matches("-labs");
		if let Some(name) = is(org) {
			return Some(name);
		}
	}
	None
}

/// Serves `/static/logos/<name>.<hash>.<ext>` with a year of caching: the
/// hash is in the name, so a changed file has a new URL.
pub fn router() -> Router<AppState> {
	Router::new().route("/static/logos/{file}", get(serve))
}

/// A response for `bytes` that browsers may keep for a year.
fn immutable(
	media: &'static str,
	hash: &str,
	bytes: impl Into<axum::body::Body>,
	svg: bool,
) -> Response {
	let bytes: axum::body::Body = bytes.into();
	let mut res = (StatusCode::OK, bytes).into_response();
	let h = res.headers_mut();
	h.insert(header::CONTENT_TYPE, HeaderValue::from_static(media));
	h.insert(
		header::CACHE_CONTROL,
		HeaderValue::from_static("public, max-age=31536000, immutable"),
	);
	h.insert(
		header::X_CONTENT_TYPE_OPTIONS,
		HeaderValue::from_static("nosniff"),
	);
	if let Ok(etag) = HeaderValue::from_str(&format!("\"{hash}\"")) {
		h.insert(header::ETAG, etag);
	}
	if svg {
		// Opened on its own, an SVG may not run anything or load anything.
		h.insert(
			header::CONTENT_SECURITY_POLICY,
			HeaderValue::from_static("default-src 'none'; style-src 'unsafe-inline'; sandbox"),
		);
	}
	res
}

async fn serve(Path(file): Path<String>) -> Response {
	let mut parts = file.rsplitn(3, '.');
	let (Some(ext), Some(hash), Some(name)) = (parts.next(), parts.next(), parts.next()) else {
		return StatusCode::NOT_FOUND.into_response();
	};
	if let Some(e) = entries().get(name)
		&& e.hash == hash
		&& e.ext == ext
	{
		return immutable(e.media, &e.hash, e.bytes, e.ext == "svg");
	}
	if let Some(id) = name.strip_prefix("t-")
		&& let Some(l) = crate::templates::find(id)
		&& l.logo.hash == hash
		&& let Some(path) = &l.logo.file
	{
		let svg = l.logo.media == "image/svg+xml";
		let want = if svg { "svg" } else { "png" };
		if want == ext
			&& let Some(bytes) = file_with_hash(path, hash).await
		{
			return immutable(l.logo.media, &l.logo.hash, bytes, svg);
		}
	}
	StatusCode::NOT_FOUND.into_response()
}

/// The file at `path` when its content has `hash`. A refresh puts the next catalog at the
/// same path, so the file can be another one than the address names: it is cached for a
/// year as whatever is sent, and so is sent only when it is the one asked for.
async fn file_with_hash(path: &std::path::Path, hash: &str) -> Option<Vec<u8>> {
	let bytes = tokio::fs::read(path).await.ok()?;
	(crate::templates::short_hash(&bytes) == hash).then_some(bytes)
}

#[cfg(test)]
#[tokio::test]
async fn a_logo_file_replaced_since_it_was_read_is_not_sent_under_the_old_hash() {
	let dir = tempfile::tempdir().unwrap();
	let path = dir.path().join("logo.svg");
	std::fs::write(&path, "<svg>old</svg>").unwrap();
	let hash = crate::templates::short_hash(b"<svg>old</svg>");
	assert_eq!(
		file_with_hash(&path, &hash).await.unwrap(),
		b"<svg>old</svg>"
	);
	std::fs::write(&path, "<svg>new</svg>").unwrap();
	assert_eq!(file_with_hash(&path, &hash).await, None);
	std::fs::remove_file(&path).unwrap();
	assert_eq!(file_with_hash(&path, &hash).await, None);
}
