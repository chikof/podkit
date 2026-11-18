//! The curated model catalog: models and the recipes (engine x quant x files)
//! that run them, plus the pure functions that say whether a recipe fits a
//! server's hardware.
//!
//! The data lives in `server/catalog/models.toml` and is embedded into the
//! binary. Revisions, byte sizes and memory estimates in it are filled in and
//! checked against Hugging Face by `scripts/catalog-verify.sh`; nothing here
//! touches the network except [`search`], which proxies the Hugging Face
//! search for the advanced mode.
//!
//! Routes live under `/api/v1/catalog` and `/api/v1/huggingface/search`.

pub mod fit;
mod routes;
pub mod search;
pub mod service;

#[cfg(test)]
mod api_tests;

use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

use crate::model_files::hf::Selection;
use crate::models::engines::{EngineKind, Format};

pub use fit::{Fit, fit, suggest};
pub use routes::{hf_router, router};

/// Context length the estimates in the catalog assume.
pub const CONTEXT: u32 = 8192;

const SOURCE: &str = include_str!("../../catalog/models.toml");

/// chiko's tier list. `S` is best; models without a tier are not ranked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Tier {
	S,
	A,
	B,
	C,
	D,
}

/// How big a model is, by total parameters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SizeClass {
	/// Under 8B.
	Tiny,
	/// 8B up to 16B.
	Small,
	/// 16B up to 40B.
	Medium,
	/// 40B and up.
	Large,
}

impl SizeClass {
	/// The class of a model with `params_b` billion parameters.
	#[cfg(test)]
	#[must_use]
	pub fn of(params_b: f64) -> Self {
		if params_b < 8.0 {
			Self::Tiny
		} else if params_b < 16.0 {
			Self::Small
		} else if params_b < 40.0 {
			Self::Medium
		} else {
			Self::Large
		}
	}
}

/// What a model is good at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tag {
	Coding,
	Reasoning,
	Vision,
}

/// A decode speed from the engine spike (`docs/spikes/engines.json`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Measured {
	/// The spike's variant name, like `llama-cuda`.
	pub variant: String,
	/// Streamed decode tokens per second.
	pub decode_tok_s: f64,
}

/// One way to run a model: an engine, a quantization and the files it needs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Recipe {
	/// Unique within the model.
	pub id: String,
	/// The preferred engine. GGUF recipes name `llama_cuda` and fall back to
	/// `llama_cpu` when the server has no usable GPU; see [`Fit::engine`].
	pub engine: EngineKind,
	/// Hugging Face repo.
	pub repo: String,
	/// A commit sha, so the files cannot change under the recorded size.
	pub revision: String,
	/// Globs of the files to download; with `files`, both empty means the whole repo.
	#[serde(default)]
	pub include: Vec<String>,
	/// Exact files to download.
	#[serde(default)]
	pub files: Vec<String>,
	/// What the engine loads inside the download; `.` is the directory itself.
	pub model_path: String,
	/// Weights format.
	pub format: Format,
	/// Quantization label as the repo spells it, like `Q4_K_M`.
	pub quant: String,
	/// Anything the user should know about where the files come from.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub note: Option<String>,
	/// Bytes of every selected file.
	pub total_bytes: u64,
	/// Estimated RAM for a CPU run, in MiB, at [`CONTEXT`] tokens.
	pub est_ram_mb: u64,
	/// Estimated VRAM for a GPU run, in MiB, at [`CONTEXT`] tokens.
	pub est_vram_mb: u64,
	/// True only for the tiny models the engine spike ran.
	pub verified: bool,
	/// Speeds the spike measured for exactly these files.
	#[serde(default)]
	pub measured: Vec<Measured>,
}

impl Recipe {
	/// The download this recipe needs.
	#[must_use]
	pub fn selection(&self) -> Selection {
		Selection {
			include: self.include.clone(),
			files: self.files.clone(),
		}
	}
}

/// A model of the catalog.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Model {
	/// Stable slug, like `qwen3-5-4b`.
	pub id: String,
	/// Display name.
	pub name: String,
	/// Model family, like `Qwen`.
	pub family: String,
	/// The upstream repo the parameters and license were read from.
	pub base_repo: String,
	/// Total parameters, in billions.
	pub params_b: f64,
	/// Parameters active per token for mixture-of-experts models, in billions.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub active_params_b: Option<f64>,
	/// License as Hugging Face reports it; `other` means a custom one.
	pub license: String,
	/// chiko's tier, when ranked.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub tier: Option<Tier>,
	/// Size class.
	pub size_class: SizeClass,
	/// What it is good at.
	pub tags: Vec<Tag>,
	/// The ways to run it.
	#[serde(rename(deserialize = "recipe"))]
	pub recipes: Vec<Recipe>,
}

impl Model {
	/// The recipe called `id`.
	#[cfg(test)]
	#[must_use]
	pub fn recipe(&self, id: &str) -> Option<&Recipe> {
		self.recipes.iter().find(|r| r.id == id)
	}
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Catalog {
	model: Vec<Model>,
}

/// Every model, in file order.
///
/// # Panics
/// When the embedded file is malformed; `catalog::tests` keeps it valid.
#[must_use]
pub fn models() -> &'static [Model] {
	static CATALOG: OnceLock<Vec<Model>> = OnceLock::new();
	CATALOG.get_or_init(|| {
		parse(SOURCE)
			.expect("server/catalog/models.toml is valid")
			.model
	})
}

/// The model called `id`.
#[must_use]
pub fn model(id: &str) -> Option<&'static Model> {
	models().iter().find(|m| m.id == id)
}

fn parse(text: &str) -> Result<Catalog, toml::de::Error> {
	toml::from_str(text)
}

#[cfg(test)]
mod tests {
	use std::collections::HashSet;

	use super::*;
	use crate::models::service::estimate;

	#[test]
	fn the_embedded_file_parses() {
		assert!(!models().is_empty());
	}

	#[test]
	fn unknown_fields_and_bad_values_are_refused() {
		let good = r#"
			[[model]]
			id = "m"
			name = "M"
			family = "F"
			base_repo = "a/b"
			params_b = 1.0
			license = "mit"
			size_class = "Tiny"
			tags = []
			[[model.recipe]]
			id = "r"
			engine = "llama_cuda"
			repo = "a/b"
			revision = "main"
			model_path = "x.gguf"
			format = "gguf"
			quant = "Q4_K_M"
			total_bytes = 1
			est_ram_mb = 1
			est_vram_mb = 1
			verified = false
		"#;
		assert!(parse(good).is_ok());
		for (from, to) in [
			("tags = []", "tags = []\nsurprise = 1"),
			("tags = []", "tags = [\"magic\"]"),
			("engine = \"llama_cuda\"", "engine = \"tensorrt\""),
			("format = \"gguf\"", "format = \"onnx\""),
			("size_class = \"Tiny\"", "size_class = \"Huge\""),
			("total_bytes = 1", ""),
		] {
			assert!(parse(&good.replace(from, to)).is_err(), "{from} -> {to}");
		}
	}

	#[test]
	fn there_are_at_least_fifteen_models_and_each_has_a_recipe() {
		let resolved = models().iter().filter(|m| !m.recipes.is_empty()).count();
		assert!(resolved >= 15, "{resolved} models with a recipe");
		assert_eq!(resolved, models().len());
	}

	#[test]
	fn ids_are_unique_and_slugs() {
		let mut seen = HashSet::new();
		for m in models() {
			assert!(seen.insert(&m.id), "duplicate model {}", m.id);
			for id in std::iter::once(&m.id).chain(m.recipes.iter().map(|r| &r.id)) {
				assert!(
					!id.is_empty()
						&& id
							.bytes()
							.all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'),
					"{id:?} is not a slug"
				);
			}
			let mut recipes = HashSet::new();
			for r in &m.recipes {
				assert!(recipes.insert(&r.id), "duplicate recipe {}/{}", m.id, r.id);
			}
		}
	}

	#[test]
	fn the_requested_and_tiered_models_are_present_with_their_tier() {
		let tiers: &[(&str, Option<Tier>)] = &[
			("qwen3-8-flash-next", None),
			("deepseek-v4-1-flash", None),
			("kimi-k3", Some(Tier::S)),
			("glm-5-3", Some(Tier::S)),
			("mimo-v2-6-pro", Some(Tier::S)),
			("deepseek-v4-pro", Some(Tier::S)),
			("qwen3-5-397b", Some(Tier::S)),
			("deepseek-v4-flash", Some(Tier::A)),
			("glm-5-3-flash", Some(Tier::A)),
			("qwen3-8-27b", Some(Tier::A)),
			("qwen3-6-27b", Some(Tier::A)),
			("qwen3-6-35b-a3b", Some(Tier::A)),
			("gemma-4-31b", Some(Tier::A)),
			("gpt-oss-120b", Some(Tier::A)),
			("qwen3-coder-next", Some(Tier::A)),
			("qwen3-5-9b", Some(Tier::A)),
			("qwen3-5-4b", Some(Tier::A)),
			("gpt-oss-20b", Some(Tier::B)),
			("gemma-4-12b", Some(Tier::B)),
			("nemotron-3-nano", Some(Tier::B)),
			("mistral-small-4", Some(Tier::B)),
			("phi-4-mini", Some(Tier::D)),
		];
		for (id, tier) in tiers {
			let m = model(id).unwrap_or_else(|| panic!("{id} is missing"));
			assert_eq!(m.tier, *tier, "{id}");
		}
	}

	#[test]
	fn size_class_follows_the_parameter_count() {
		for m in models() {
			assert_eq!(m.size_class, SizeClass::of(m.params_b), "{}", m.id);
		}
		assert_eq!(SizeClass::of(7.99), SizeClass::Tiny);
		assert_eq!(SizeClass::of(8.0), SizeClass::Small);
		assert_eq!(SizeClass::of(16.0), SizeClass::Medium);
		assert_eq!(SizeClass::of(40.0), SizeClass::Large);
	}

	#[test]
	fn recipes_are_pinned_sized_and_estimated_like_the_server_does() {
		for m in models() {
			assert!(m.active_params_b.is_none_or(|a| a <= m.params_b));
			for r in &m.recipes {
				let tag = format!("{}/{}", m.id, r.id);
				assert_eq!(r.revision.len(), 40, "{tag}: revision is not a commit sha");
				assert!(r.revision.bytes().all(|b| b.is_ascii_hexdigit()), "{tag}");
				assert!(r.total_bytes > 0, "{tag}");
				let est = estimate(r.total_bytes, CONTEXT).total_mb();
				assert_eq!(r.est_ram_mb, est, "{tag}: est_ram_mb drifted");
				assert_eq!(r.est_vram_mb, est, "{tag}: est_vram_mb drifted");
				assert!(crate::model_files::hf::valid_repo(&r.repo), "{tag}");
				r.selection().validate().unwrap();
				assert!(!r.model_path.starts_with('/'), "{tag}");
				assert!(!r.model_path.contains(".."), "{tag}");
			}
		}
	}

	#[test]
	fn formats_match_what_the_engine_serves() {
		for m in models() {
			for r in &m.recipes {
				let spec = crate::models::engines::spec(r.engine);
				assert!(
					spec.formats.contains(&r.format),
					"{}/{}: {:?} cannot serve {:?}",
					m.id,
					r.id,
					r.engine,
					r.format
				);
				match r.format {
					Format::Gguf => assert!(
						std::path::Path::new(&r.model_path)
							.extension()
							.is_some_and(|e| e == "gguf"),
						"{}/{}",
						m.id,
						r.id
					),
					Format::Safetensors => assert_eq!(r.model_path, ".", "{}/{}", m.id, r.id),
					Format::Colibri => {}
				}
			}
		}
	}

	/// Sharded GGUFs name their first shard; the engine finds the rest.
	#[test]
	fn sharded_downloads_load_the_first_shard() {
		for m in models() {
			for r in &m.recipes {
				if let Some(n) = r.model_path.rsplit_once("-of-") {
					assert!(n.0.ends_with("-00001"), "{}/{}", m.id, r.id);
				}
			}
		}
	}

	#[test]
	fn only_spike_models_are_verified_and_speeds_come_from_the_spike() {
		let spikes: Vec<serde_json::Value> =
			serde_json::from_str(include_str!("../../../docs/spikes/engines.json")).unwrap();
		let mut verified = 0;
		for m in models() {
			for r in &m.recipes {
				let tag = format!("{}/{}", m.id, r.id);
				if r.verified {
					verified += 1;
					assert!(
						!r.measured.is_empty(),
						"{tag}: verified without a measurement"
					);
				} else {
					assert!(
						r.measured.is_empty(),
						"{tag}: speeds on an unverified recipe"
					);
				}
				for speed in &r.measured {
					let file = r.model_path.rsplit('/').next().unwrap();
					let dir_name = r.repo.rsplit('/').next().unwrap();
					let hit = spikes.iter().find(|s| {
						s["engine"] == speed.variant.as_str()
							&& s["model"].as_str().is_some_and(|p| {
								p.ends_with(&format!("/{file}"))
									|| (r.model_path == "." && p.ends_with(&format!("/{dir_name}")))
							})
					});
					let hit =
						hit.unwrap_or_else(|| panic!("{tag}: no spike run {}", speed.variant));
					assert!(
						(hit["decode_tok_s"].as_f64().unwrap() - speed.decode_tok_s).abs() < 1e-9,
						"{tag}: {} differs from engines.json",
						speed.variant
					);
				}
			}
		}
		assert!(verified >= 3, "{verified} verified recipes");
		// The big models are data only.
		for m in models().iter().filter(|m| m.params_b > 8.0) {
			assert!(m.recipes.iter().all(|r| !r.verified), "{}", m.id);
		}
	}
}
