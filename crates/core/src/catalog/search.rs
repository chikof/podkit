//! Advanced mode: any Hugging Face GGUF repo, run through the same [`fit`].
//!
//! The search is proxied to `{endpoint}/api/models?search=…&filter=gguf&sort=downloads`
//! and each hit's file tree is listed for sizes. The tree is grouped into one
//! variant per quantization (sharded files count as one).

use std::collections::BTreeMap;

use proto::Hardware;
use serde::{Deserialize, Serialize};

use super::{CONTEXT, Fit, Recipe, fit};
use crate::error::{AppResult, ServerError};
use crate::model_files::hf::{self, RepoFile, Selection};
use crate::models::engines::{EngineKind, Format};
use crate::models::service::estimate;
use crate::state::AppState;

const REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
/// Most repos one search returns.
pub const MAX_LIMIT: usize = 20;
/// Repos returned when `limit` is absent.
pub const DEFAULT_LIMIT: usize = 8;

/// One Hugging Face repo that has GGUF files.
#[derive(Debug, Serialize, PartialEq)]
pub struct Hit {
	/// `owner/name`.
	pub repo: String,
	/// Downloads, as Hugging Face counts them.
	pub downloads: u64,
	/// Likes.
	pub likes: u64,
	/// One entry per quantization, smallest first.
	pub variants: Vec<Variant>,
	/// Why the files could not be listed (gated, private or unreachable).
	#[serde(skip_serializing_if = "Option::is_none")]
	pub error: Option<String>,
}

/// One quantization of a repo.
#[derive(Debug, Serialize, PartialEq)]
pub struct Variant {
	/// Quantization label, like `Q4_K_M` or `UD-Q4_K_XL`.
	pub quant: String,
	/// What the engine loads: the file, or the first shard.
	pub model_path: String,
	/// Number of files (shards).
	pub shards: usize,
	/// Bytes of all of them.
	pub total_bytes: u64,
	/// A download selection that fetches exactly these files.
	pub selection: Selection,
	/// Estimated memory, in MiB.
	pub est_mb: u64,
	/// How it fits the asked server; absent without one.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub fit: Option<Fit>,
}

#[derive(Deserialize)]
struct ListEntry {
	id: String,
	#[serde(default)]
	downloads: u64,
	#[serde(default)]
	likes: u64,
}

/// Searches Hugging Face and sizes every hit.
///
/// # Errors
/// `HuggingFaceUnavailable` when the search itself fails. A repo whose files
/// cannot be listed is reported in its own `error` instead.
pub async fn search(
	s: &AppState,
	query: &str,
	limit: usize,
	hardware: Option<&Hardware>,
) -> AppResult<Vec<Hit>> {
	let base = s.config.hf_endpoint.trim_end_matches('/');
	let url = url::Url::parse_with_params(
		&format!("{base}/api/models"),
		[
			("search", query),
			("filter", "gguf"),
			("sort", "downloads"),
			("direction", "-1"),
			("limit", limit.to_string().as_str()),
		],
	)
	.map_err(|_| ServerError::HuggingFaceUnavailable)?;
	let response = s
		.http
		.get(url)
		.timeout(REQUEST_TIMEOUT)
		.send()
		.await
		.map_err(|e| {
			tracing::warn!("hugging face search failed: {}", e.without_url());
			ServerError::HuggingFaceUnavailable
		})?;
	if !response.status().is_success() {
		tracing::warn!(status = %response.status(), "hugging face search answered with an error");
		return Err(ServerError::HuggingFaceUnavailable);
	}
	let body = response.bytes().await.map_err(|e| {
		tracing::warn!("hugging face search failed: {}", e.without_url());
		ServerError::HuggingFaceUnavailable
	})?;
	let found: Vec<ListEntry> = serde_json::from_slice(&body).map_err(|e| {
		tracing::warn!("hugging face search is not valid: {e}");
		ServerError::HuggingFaceUnavailable
	})?;
	let found: Vec<ListEntry> = found
		.into_iter()
		.filter(|e| hf::valid_repo(&e.id))
		.take(limit)
		.collect();
	let hits = found.iter().map(|entry| async {
		let listing = hf::plan(
			&s.http,
			&s.config.hf_endpoint,
			None,
			&entry.id,
			"main",
			&Selection::default(),
		)
		.await;
		let (variants, error) = match listing {
			Ok(plan) => (variants(&entry.id, &plan.files, hardware), None),
			Err(e) => (Vec::new(), Some(e.to_string())),
		};
		Hit {
			repo: entry.id.clone(),
			downloads: entry.downloads,
			likes: entry.likes,
			variants,
			error,
		}
	});
	Ok(futures_util::future::join_all(hits).await)
}

/// Whether `path` names a `.gguf` file.
fn is_gguf(path: &str) -> bool {
	std::path::Path::new(path)
		.extension()
		.is_some_and(|e| e.eq_ignore_ascii_case("gguf"))
}

/// Files that sit next to a model but are not one.
fn is_auxiliary(path: &str) -> bool {
	let name = path.rsplit('/').next().unwrap_or(path).to_ascii_lowercase();
	["mmproj", "mtp-", "dflash", "eagle", "imatrix"]
		.iter()
		.any(|marker| name.contains(marker))
}

/// `(stem, index, count)` of `name-00001-of-00004.gguf`.
fn shard(path: &str) -> Option<(&str, u32, u32)> {
	let stem = path.strip_suffix(".gguf")?;
	let (rest, count) = stem.rsplit_once("-of-")?;
	let (stem, index) = rest.rsplit_once('-')?;
	if count.len() != 5 || index.len() != 5 {
		return None;
	}
	Some((stem, index.parse().ok()?, count.parse().ok()?))
}

fn is_quant(token: &str) -> bool {
	let t = token.to_ascii_uppercase();
	if matches!(t.as_str(), "BF16" | "F16" | "F32" | "MXFP4" | "MXFP4_MOE") {
		return true;
	}
	let t = t.strip_prefix('I').unwrap_or(&t);
	let mut parts = t.split('_');
	let head = parts.next().unwrap_or_default();
	head.len() == 2
		&& head.starts_with('Q')
		&& head.as_bytes()[1].is_ascii_digit()
		&& parts.all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_alphanumeric()))
}

/// The quantization named in a file name, `UD-` prefix included.
fn quant_of(stem: &str) -> String {
	let name = stem.rsplit('/').next().unwrap_or(stem);
	let tokens: Vec<&str> = name.split(['-', '.']).collect();
	for (i, token) in tokens.iter().enumerate() {
		if is_quant(token) {
			let ud = i > 0 && tokens[i - 1].eq_ignore_ascii_case("ud");
			return if ud {
				format!("UD-{token}")
			} else {
				(*token).to_owned()
			};
		}
	}
	name.to_owned()
}

/// Groups GGUF files into one variant per quantization and rates each.
#[must_use]
pub fn variants(repo: &str, files: &[RepoFile], hardware: Option<&Hardware>) -> Vec<Variant> {
	// (directory-qualified stem) -> (first path, shard paths, bytes, shard count)
	let mut groups: BTreeMap<String, (String, Vec<String>, u64)> = BTreeMap::new();
	for f in files {
		if !is_gguf(&f.path) || is_auxiliary(&f.path) {
			continue;
		}
		let (key, first) = match shard(&f.path) {
			Some((stem, index, _)) => (stem.to_owned(), index == 1),
			None => (f.path.trim_end_matches(".gguf").to_owned(), true),
		};
		let entry = groups.entry(key).or_default();
		if first {
			entry.0.clone_from(&f.path);
		}
		entry.1.push(f.path.clone());
		entry.2 += f.size;
	}
	let mut out: Vec<Variant> = groups
		.into_iter()
		.filter(|(_, (first, _, _))| !first.is_empty())
		.map(|(key, (first, paths, total_bytes))| {
			let selection = if paths.len() == 1 {
				Selection {
					include: Vec::new(),
					files: paths.clone(),
				}
			} else {
				// `?` matches one character: the shard number of an exact count.
				let count = shard(&first).map_or(0, |(_, _, n)| n);
				Selection {
					include: vec![format!("{key}-?????-of-{count:05}.gguf")],
					files: Vec::new(),
				}
			};
			let est_mb = estimate(total_bytes, CONTEXT).total_mb();
			let quant = quant_of(&key);
			let rated = hardware.map(|hw| {
				fit(
					hw,
					&Recipe {
						id: quant.to_ascii_lowercase(),
						engine: EngineKind::LlamaCuda,
						repo: repo.to_owned(),
						revision: "main".to_owned(),
						include: selection.include.clone(),
						files: selection.files.clone(),
						model_path: first.clone(),
						format: Format::Gguf,
						quant: quant.clone(),
						note: None,
						total_bytes,
						est_ram_mb: est_mb,
						est_vram_mb: est_mb,
						verified: false,
						measured: Vec::new(),
					},
				)
			});
			Variant {
				quant,
				model_path: first,
				shards: paths.len(),
				total_bytes,
				selection,
				est_mb,
				fit: rated,
			}
		})
		.collect();
	out.sort_by(|a, b| {
		a.total_bytes
			.cmp(&b.total_bytes)
			.then(a.model_path.cmp(&b.model_path))
	});
	out
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::catalog::fit::FitKind;
	use crate::catalog::fit::fixtures::{chikos_box, cpu_512gb};

	fn f(path: &str, size: u64) -> RepoFile {
		RepoFile {
			path: path.to_owned(),
			size,
		}
	}

	#[test]
	fn quantizations_are_read_from_file_names() {
		for (name, quant) in [
			("Qwen3-4B-Q4_K_M.gguf", "Q4_K_M"),
			("gemma-4-31B-it-UD-IQ2_XXS.gguf", "UD-IQ2_XXS"),
			("gpt-oss-20b-MXFP4.gguf", "MXFP4"),
			("Phi-4-mini-instruct.Q8_0.gguf", "Q8_0"),
			("model-BF16.gguf", "BF16"),
			("Qwen3.5-397B-A17B-MXFP4_MOE.gguf", "MXFP4_MOE"),
			("UD-Q4_K_XL/Kimi-K3-UD-Q4_K_XL", "UD-Q4_K_XL"),
			("plain.gguf", "plain.gguf"),
		] {
			assert_eq!(
				quant_of(name.trim_end_matches(".gguf")),
				quant.trim_end_matches(".gguf")
			);
		}
	}

	#[test]
	fn shards_are_one_variant_and_extras_are_skipped() {
		let files = [
			f("README.md", 10),
			f("m-Q4_K_M.gguf", 4_000),
			f("m-Q8_0.gguf", 8_000),
			f("mmproj-F16.gguf", 900),
			f("MTP/mtp-m-Q8_0.gguf", 700),
			f("UD-Q2/m-UD-Q2-00002-of-00002.gguf", 5_000),
			f("UD-Q2/m-UD-Q2-00001-of-00002.gguf", 6_000),
		];
		let v = variants("a/b", &files, None);
		let quants: Vec<_> = v
			.iter()
			.map(|v| (v.quant.as_str(), v.total_bytes, v.shards))
			.collect();
		assert_eq!(
			quants,
			[
				("Q4_K_M", 4_000, 1),
				("Q8_0", 8_000, 1),
				("UD-Q2", 11_000, 2)
			]
		);
		let sharded = &v[2];
		assert_eq!(sharded.model_path, "UD-Q2/m-UD-Q2-00001-of-00002.gguf");
		assert_eq!(
			sharded.selection.include,
			["UD-Q2/m-UD-Q2-?????-of-00002.gguf"]
		);
		assert!(sharded.selection.validate().is_ok());
		assert!(
			sharded
				.selection
				.matches("UD-Q2/m-UD-Q2-00002-of-00002.gguf")
		);
		assert!(
			!sharded
				.selection
				.matches("UD-Q2/m-UD-Q2-00001-of-00003.gguf")
		);
		assert_eq!(v[0].selection.files, ["m-Q4_K_M.gguf"]);
		assert!(v.iter().all(|v| v.fit.is_none()));
	}

	#[test]
	fn variants_are_rated_by_the_same_fit() {
		let files = [
			f("tiny-Q4_K_M.gguf", 2_500_000_000),
			f("huge-Q8_0.gguf", 600_000_000_000),
		];
		let on_box = variants("a/b", &files, Some(&chikos_box()));
		assert_eq!(on_box[0].fit.as_ref().unwrap().kind, FitKind::GpuFull);
		assert_eq!(on_box[1].fit.as_ref().unwrap().kind, FitKind::DoesNotFit);
		let on_cpu = variants("a/b", &files, Some(&cpu_512gb()));
		assert_eq!(on_cpu[0].fit.as_ref().unwrap().kind, FitKind::CpuOnly);
		assert_eq!(
			on_cpu[0].est_mb,
			estimate(2_500_000_000, CONTEXT).total_mb()
		);
	}
}
