//! The Hugging Face side of model downloads: which files a selection covers,
//! how big they are, and whether they fit on the server.
//!
//! The listing comes from `GET {endpoint}/api/models/{repo}/tree/{rev}?recursive=true`.
//! The endpoint is configurable so tests run against a local stub.

use std::time::Duration;

use proto::GATED_MODEL_MESSAGE;
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};

use crate::error::ServerError;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// The listing is paged (1000 entries a page); this bounds a runaway `Link` chain.
const MAX_PAGES: usize = 500;
/// Free space kept beyond the download itself, as a percentage of its size.
const HEADROOM_PERCENT: u64 = 10;

/// Which files of a repo to download: files matching an `include` glob, plus
/// the explicitly listed `files`. Empty means the whole repo.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Selection {
	#[serde(default)]
	pub include: Vec<String>,
	#[serde(default)]
	pub files: Vec<String>,
}

impl Selection {
	/// Patterns for `hf download --include`. Explicit files are exact patterns:
	/// [`Selection::validate`] keeps glob characters out of them.
	#[must_use]
	pub fn patterns(&self) -> Vec<String> {
		self.include.iter().chain(&self.files).cloned().collect()
	}

	#[must_use]
	pub fn matches(&self, path: &str) -> bool {
		let patterns = self.patterns();
		patterns.is_empty() || patterns.iter().any(|p| glob_match(p, path))
	}

	/// # Errors
	/// `Validation` for empty, oversized, option-like or unsupported patterns.
	pub fn validate(&self) -> Result<(), ServerError> {
		let bad =
			|what: &str, value: &str| ServerError::Validation(format!("invalid {what} {value:?}"));
		if self.include.len() + self.files.len() > 50 {
			return Err(ServerError::Validation(
				"a selection holds at most 50 patterns and files".to_owned(),
			));
		}
		for pattern in &self.include {
			if !plain(pattern) || pattern.contains('[') {
				return Err(bad("include pattern", pattern));
			}
		}
		for file in &self.files {
			if !plain(file) || file.contains(['*', '?', '[']) || file.starts_with('/') {
				return Err(bad("file", file));
			}
		}
		Ok(())
	}
}

/// Non-empty, bounded, printable, and not something `hf` would read as an option.
fn plain(s: &str) -> bool {
	!s.is_empty() && s.len() <= 255 && !s.starts_with('-') && !s.chars().any(char::is_control)
}

/// Shell-style match where `*` also crosses `/`, like Python's `fnmatch`
/// (which `hf download --include` uses). Supports `*` and `?`.
#[must_use]
pub fn glob_match(pattern: &str, text: &str) -> bool {
	let (p, t): (Vec<char>, Vec<char>) = (pattern.chars().collect(), text.chars().collect());
	let (mut pi, mut ti) = (0, 0);
	let mut backtrack: Option<(usize, usize)> = None;
	while ti < t.len() {
		if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) && p[pi] != '*' {
			pi += 1;
			ti += 1;
		} else if pi < p.len() && p[pi] == '*' {
			backtrack = Some((pi, ti));
			pi += 1;
		} else if let Some((star, matched)) = backtrack {
			pi = star + 1;
			ti = matched + 1;
			backtrack = Some((star, matched + 1));
		} else {
			return false;
		}
	}
	p[pi..].iter().all(|&c| c == '*')
}

/// `owner/name` or a bare name, as Hugging Face spells repo ids.
#[must_use]
pub fn valid_repo(repo: &str) -> bool {
	let part = |s: &str| {
		!s.is_empty()
			&& s.len() <= 96
			&& !s.starts_with(['-', '.'])
			&& s.bytes()
				.all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'))
	};
	match repo.split_once('/') {
		Some((owner, name)) => part(owner) && part(name) && !name.contains('/'),
		None => part(repo),
	}
}

/// A branch, tag, commit or `refs/...` ref.
#[must_use]
pub fn valid_revision(revision: &str) -> bool {
	!revision.is_empty()
		&& revision.len() <= 255
		&& !revision.starts_with(['-', '/', '.'])
		&& !revision.ends_with('/')
		&& !revision.contains("..")
		&& !revision.contains("//")
		&& revision
			.bytes()
			.all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-' | b'/'))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoFile {
	pub path: String,
	pub size: u64,
}

/// The files a selection covers and their combined size.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
	pub files: Vec<RepoFile>,
	pub total_bytes: u64,
}

#[derive(Deserialize)]
struct TreeEntry {
	#[serde(rename = "type")]
	kind: String,
	path: String,
	#[serde(default)]
	size: u64,
	lfs: Option<Lfs>,
}

#[derive(Deserialize)]
struct Lfs {
	size: u64,
}

/// Lists the repo at `revision`, applies `selection` and sums the sizes.
///
/// # Errors
/// `Validation` when Hugging Face refuses access (gated or private repo) or
/// does not know the repo or revision, or when the selection matches nothing;
/// `HuggingFaceUnavailable` when it cannot be reached or answers badly.
pub async fn plan(
	http: &reqwest::Client,
	endpoint: &str,
	token: Option<&str>,
	repo: &str,
	revision: &str,
	selection: &Selection,
) -> Result<Plan, ServerError> {
	let entries = list_tree(http, endpoint, token, repo, revision).await?;
	let files: Vec<RepoFile> = entries
		.into_iter()
		.filter(|e| e.kind == "file" && selection.matches(&e.path))
		.map(|e| RepoFile {
			size: e.lfs.map_or(e.size, |l| l.size),
			path: e.path,
		})
		.collect();

	if let Some(missing) = selection
		.files
		.iter()
		.find(|f| !files.iter().any(|file| &file.path == *f))
	{
		return Err(ServerError::Validation(format!(
			"file {missing:?} does not exist in the repository"
		)));
	}
	if files.is_empty() {
		return Err(ServerError::Validation(
			"no files in the repository match the selection".to_owned(),
		));
	}
	let total_bytes = files
		.iter()
		.try_fold(0u64, |sum, f| sum.checked_add(f.size))
		.ok_or_else(|| ServerError::Validation("the selection is implausibly large".to_owned()))?;
	Ok(Plan { files, total_bytes })
}

async fn list_tree(
	http: &reqwest::Client,
	endpoint: &str,
	token: Option<&str>,
	repo: &str,
	revision: &str,
) -> Result<Vec<TreeEntry>, ServerError> {
	let base = endpoint.trim_end_matches('/');
	let mut url = format!(
		"{base}/api/models/{repo}/tree/{}?recursive=true",
		revision.replace('/', "%2F")
	);
	let mut entries = Vec::new();
	for _ in 0..MAX_PAGES {
		let mut request = http.get(&url).timeout(REQUEST_TIMEOUT);
		if let Some(token) = token {
			request = request.bearer_auth(token);
		}
		let response = request.send().await.map_err(|e| {
			tracing::warn!("hugging face request failed: {}", e.without_url());
			ServerError::HuggingFaceUnavailable
		})?;
		match response.status() {
			StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
				return Err(ServerError::Validation(GATED_MODEL_MESSAGE.to_owned()));
			}
			StatusCode::NOT_FOUND => {
				return Err(ServerError::Validation(
					"repository or revision not found on Hugging Face".to_owned(),
				));
			}
			status if !status.is_success() => {
				tracing::warn!(%status, "hugging face answered with an error");
				return Err(ServerError::HuggingFaceUnavailable);
			}
			_ => {}
		}
		let next = response
			.headers()
			.get(reqwest::header::LINK)
			.and_then(|v| v.to_str().ok())
			.and_then(next_link);
		let body = response.bytes().await.map_err(|e| {
			tracing::warn!("hugging face listing failed: {}", e.without_url());
			ServerError::HuggingFaceUnavailable
		})?;
		let page: Vec<TreeEntry> = serde_json::from_slice(&body).map_err(|e| {
			tracing::warn!("hugging face listing is not valid: {e}");
			ServerError::HuggingFaceUnavailable
		})?;
		entries.extend(page);
		match next {
			// The token is only ever sent to the configured endpoint.
			Some(next) if next.starts_with(base) => url = next,
			Some(_) | None => return Ok(entries),
		}
	}
	Err(ServerError::HuggingFaceUnavailable)
}

/// The `rel="next"` target of a `Link` header.
fn next_link(header: &str) -> Option<String> {
	header.split(',').find_map(|part| {
		let (target, params) = part.split_once(';')?;
		params.contains("rel=\"next\"").then(|| {
			target
				.trim()
				.trim_start_matches('<')
				.trim_end_matches('>')
				.to_owned()
		})
	})
}

/// Bytes a download of `needed` bytes must find free: the download plus 10%.
#[must_use]
pub fn with_headroom(needed: u64) -> u64 {
	needed.saturating_add(needed.div_ceil(100).saturating_mul(HEADROOM_PERCENT))
}

/// # Errors
/// `Validation` when `needed` plus headroom does not fit in `free`.
pub fn check_fit(needed: u64, free: u64) -> Result<(), ServerError> {
	let required = with_headroom(needed);
	if required <= free {
		return Ok(());
	}
	Err(ServerError::Validation(format!(
		"not enough disk space: the download needs {} (including {HEADROOM_PERCENT}% headroom) \
		but the server has {} free",
		human(required),
		human(free)
	)))
}

#[allow(clippy::cast_precision_loss, reason = "display only")]
fn human(bytes: u64) -> String {
	const GIB: f64 = (1u64 << 30) as f64;
	const MIB: f64 = (1u64 << 20) as f64;
	let b = bytes as f64;
	if b >= GIB {
		format!("{:.1} GiB", b / GIB)
	} else {
		format!("{:.1} MiB", b / MIB)
	}
}

#[cfg(test)]
pub mod tests {
	use std::net::SocketAddr;

	use axum::Router;
	use std::collections::HashMap;

	use axum::extract::{Path, Query};
	use axum::http::{HeaderMap, StatusCode, header};
	use axum::response::IntoResponse;
	use axum::routing::get;
	use serde_json::{Value, json};

	use super::*;

	/// Token the stub accepts for `acme/gated`.
	pub const GOOD_TOKEN: &str = "hf_goodtoken";

	fn file(path: &str, size: u64) -> Value {
		json!({ "type": "file", "path": path, "size": size })
	}

	fn lfs(path: &str, size: u64) -> Value {
		// The pointer size is what `size` would show for a git file; lfs.size is the real one.
		json!({ "type": "file", "path": path, "size": 133, "lfs": { "oid": "x", "size": size } })
	}

	async fn tree(
		Path((owner, name, rev)): Path<(String, String, String)>,
		Query(query): Query<HashMap<String, String>>,
		headers: HeaderMap,
	) -> axum::response::Response {
		let header_text = |name| headers.get(name).and_then(|v| v.to_str().ok());
		let bearer = header_text(header::AUTHORIZATION).and_then(|v| v.strip_prefix("Bearer "));
		match format!("{owner}/{name}").as_str() {
			"acme/open" if rev == "main" => axum::Json(json!([
				{ "type": "directory", "path": "onnx", "size": 0 },
				file("config.json", 1_000),
				lfs("model-Q4_K_M.gguf", 2_000_000_000),
				lfs("model-Q8_0.gguf", 4_000_000_000),
				lfs("onnx/model.onnx", 500),
			]))
			.into_response(),
			"acme/open" if rev == "refs/pr/1" => {
				axum::Json(json!([file("config.json", 7)])).into_response()
			}
			"acme/paged" if query.contains_key("cursor") => {
				axum::Json(json!([file("b.bin", 20)])).into_response()
			}
			"acme/paged" => {
				let host = header_text(header::HOST).unwrap_or_default();
				let next = format!(
					"<http://{host}/api/models/acme/paged/tree/main?recursive=true&cursor=2>; rel=\"next\""
				);
				(
					[(header::LINK, next)],
					axum::Json(json!([file("a.bin", 10)])),
				)
					.into_response()
			}
			"acme/gated" if bearer == Some(GOOD_TOKEN) => {
				axum::Json(json!([lfs("w.gguf", 3_000)])).into_response()
			}
			"acme/gated" => StatusCode::UNAUTHORIZED.into_response(),
			"acme/forbidden" => StatusCode::FORBIDDEN.into_response(),
			"acme/broken" => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
			_ => StatusCode::NOT_FOUND.into_response(),
		}
	}

	/// A local stand-in for the Hugging Face hub. Returns its base URL.
	pub async fn stub() -> String {
		let app = Router::new().route("/api/models/{owner}/{name}/tree/{rev}", get(tree));
		let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
		let addr: SocketAddr = listener.local_addr().unwrap();
		tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
		format!("http://{addr}")
	}

	async fn run(
		repo: &str,
		revision: &str,
		token: Option<&str>,
		selection: &Selection,
	) -> Result<Plan, ServerError> {
		let endpoint = stub().await;
		plan(&client(), &endpoint, token, repo, revision, selection).await
	}

	/// A client for the stub; `state()` installs the TLS provider reqwest needs.
	fn client() -> reqwest::Client {
		let _ = rustls::crypto::ring::default_provider().install_default();
		reqwest::Client::new()
	}

	fn include(patterns: &[&str]) -> Selection {
		Selection {
			include: patterns.iter().map(|p| (*p).to_owned()).collect(),
			files: Vec::new(),
		}
	}

	#[tokio::test]
	async fn whole_repo_sums_files_and_uses_lfs_sizes() {
		let plan = run("acme/open", "main", None, &Selection::default())
			.await
			.unwrap();
		assert_eq!(plan.files.len(), 4, "directories are not files");
		assert_eq!(
			plan.total_bytes,
			1_000 + 2_000_000_000 + 4_000_000_000 + 500
		);
	}

	#[tokio::test]
	async fn include_glob_selects_matching_files() {
		let plan = run("acme/open", "main", None, &include(&["*Q4_K_M.gguf"]))
			.await
			.unwrap();
		assert_eq!(
			plan.files,
			vec![RepoFile {
				path: "model-Q4_K_M.gguf".to_owned(),
				size: 2_000_000_000
			}]
		);
		assert_eq!(plan.total_bytes, 2_000_000_000);
	}

	#[tokio::test]
	async fn explicit_files_and_globs_combine() {
		let selection = Selection {
			include: vec!["*.json".to_owned()],
			files: vec!["model-Q8_0.gguf".to_owned()],
		};
		let plan = run("acme/open", "main", None, &selection).await.unwrap();
		assert_eq!(plan.total_bytes, 1_000 + 4_000_000_000);
	}

	#[tokio::test]
	async fn missing_explicit_file_and_empty_matches_are_refused() {
		let selection = Selection {
			include: Vec::new(),
			files: vec!["nope.gguf".to_owned(), "config.json".to_owned()],
		};
		let err = run("acme/open", "main", None, &selection)
			.await
			.unwrap_err();
		assert!(err.to_string().contains("nope.gguf"), "{err}");

		let err = run("acme/open", "main", None, &include(&["*.safetensors"]))
			.await
			.unwrap_err();
		assert!(err.to_string().contains("no files"), "{err}");
	}

	#[tokio::test]
	async fn gated_repo_needs_a_token() {
		for repo in ["acme/gated", "acme/forbidden"] {
			let err = run(repo, "main", None, &Selection::default())
				.await
				.unwrap_err();
			assert!(matches!(&err, ServerError::Validation(m) if m == GATED_MODEL_MESSAGE));
		}
		let err = run("acme/gated", "main", Some("wrong"), &Selection::default())
			.await
			.unwrap_err();
		assert_eq!(err.to_string(), GATED_MODEL_MESSAGE);

		let plan = run(
			"acme/gated",
			"main",
			Some(GOOD_TOKEN),
			&Selection::default(),
		)
		.await
		.unwrap();
		assert_eq!(plan.total_bytes, 3_000);
	}

	#[tokio::test]
	async fn unknown_repo_is_a_validation_error_and_server_errors_are_upstream() {
		let err = run("acme/none", "main", None, &Selection::default())
			.await
			.unwrap_err();
		assert!(matches!(err, ServerError::Validation(m) if m.contains("not found")));
		let err = run("acme/broken", "main", None, &Selection::default())
			.await
			.unwrap_err();
		assert!(matches!(err, ServerError::HuggingFaceUnavailable));
		// Nothing listens on port 1.
		let err = plan(
			&client(),
			"http://127.0.0.1:1",
			None,
			"acme/open",
			"main",
			&Selection::default(),
		)
		.await
		.unwrap_err();
		assert!(matches!(err, ServerError::HuggingFaceUnavailable));
	}

	#[tokio::test]
	async fn slashes_in_revisions_are_encoded() {
		let plan = run("acme/open", "refs/pr/1", None, &Selection::default())
			.await
			.unwrap();
		assert_eq!(plan.total_bytes, 7);
	}

	#[tokio::test]
	async fn paged_listings_are_followed() {
		let plan = run("acme/paged", "main", None, &Selection::default())
			.await
			.unwrap();
		assert_eq!(plan.total_bytes, 30);
	}

	#[test]
	fn glob_table() {
		let cases = [
			("*.gguf", "a.gguf", true),
			("*.gguf", "dir/a.gguf", true),
			("*Q4_K_M.gguf", "x-Q4_K_M.gguf", true),
			("*Q4_K_M.gguf", "x-Q8_0.gguf", false),
			("a?c", "abc", true),
			("a?c", "ac", false),
			("config.json", "config.json", true),
			("config.json", "config.jsonx", false),
			("*", "anything/at/all", true),
			("a*b*c", "aXbYc", true),
			("a*b*c", "aXbY", false),
			("**", "", true),
			("", "", true),
			("", "a", false),
		];
		for (pattern, text, expected) in cases {
			assert_eq!(glob_match(pattern, text), expected, "{pattern} vs {text}");
		}
	}

	#[test]
	fn repo_and_revision_validation() {
		for ok in ["Qwen/Qwen3-4B-GGUF", "gpt2", "a_b/c.d"] {
			assert!(valid_repo(ok), "{ok}");
		}
		for bad in ["", "a/b/c", "../x", "-x/y", "a/ b", "a/b?x=1", "a//b", "/a"] {
			assert!(!valid_repo(bad), "{bad}");
		}
		for ok in ["main", "v1.0", "refs/pr/1", &"a".repeat(40)] {
			assert!(valid_revision(ok), "{ok}");
		}
		for bad in [
			"", "..", "a/../b", "-x", "a b", "a?b", "a#b", "/main", "main/",
		] {
			assert!(!valid_revision(bad), "{bad}");
		}
	}

	#[test]
	fn selection_validation() {
		assert!(include(&["*.gguf", "config.json"]).validate().is_ok());
		for bad in [
			include(&[""]),
			include(&["--token=x"]),
			include(&["[ab].bin"]),
			Selection {
				include: vec![],
				files: vec!["*.gguf".to_owned()],
			},
			Selection {
				include: vec![],
				files: vec!["/etc/passwd".to_owned()],
			},
			include(&["a\nb"]),
		] {
			assert!(bad.validate().is_err(), "{bad:?}");
		}
		let many = Selection {
			include: (0..51).map(|i| format!("f{i}")).collect(),
			files: Vec::new(),
		};
		assert!(many.validate().is_err());
	}

	#[test]
	fn headroom_is_ten_percent_and_decides_fit() {
		assert_eq!(with_headroom(1000), 1100);
		assert_eq!(with_headroom(0), 0);
		assert!(check_fit(1000, 1100).is_ok());
		let err = check_fit(1000, 1099).unwrap_err();
		assert!(err.to_string().contains("not enough disk space"), "{err}");
		assert_eq!(with_headroom(u64::MAX), u64::MAX);
	}

	#[test]
	fn link_header_parsing() {
		assert_eq!(
			next_link("<http://h/a?c=1>; rel=\"next\", <http://h/a>; rel=\"prev\""),
			Some("http://h/a?c=1".to_owned())
		);
		assert_eq!(next_link("<http://h/a>; rel=\"prev\""), None);
	}
}
