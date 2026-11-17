//! Whether a recipe runs on a server, and which recipe suits it best.
//!
//! Both functions are pure: they read the hardware snapshot the agent reported
//! and the sizes recorded in the recipe. The numbers are estimates meant to
//! catch a model that cannot fit, not to size one that barely does (the same
//! stance as `models::service::check_fit`).
//!
//! Rules, in order:
//! 1. The download plus 10% headroom must fit on the smallest reported disk.
//! 2. With a usable GPU (NVIDIA, or an AMD card of at least 2 GB), the whole
//!    estimate in VRAM is `GpuFull`. vLLM only gets 75% of VRAM, the
//!    `--gpu-memory-utilization` podkit sets.
//! 3. llama.cpp and Ollama can offload layers: when the rest fits in RAM it is
//!    `GpuPlusRam`.
//! 4. Otherwise an engine that runs on the CPU gets `CpuOnly` when the estimate
//!    fits in RAM.
//! 5. Anything else is `DoesNotFit`.
//!
//! RAM is the reported total minus a 2 GiB reserve for the host: free RAM moves
//! with whatever else runs on the server, total RAM does not.

use proto::{Gpu, GpuVendor, Hardware};
use serde::Serialize;

use super::{Model, Recipe};
use crate::model_files::hf;
use crate::models::engines::EngineKind;

const MIB: u64 = 1 << 20;
/// RAM left to the host, in MiB.
const RAM_RESERVE_MB: u64 = 2048;
/// Smallest AMD card worth running a model on; matches `servers::gpu_vendors`.
const MIN_AMD_VRAM_MB: u64 = 2048;
/// Share of VRAM vLLM may use.
const VLLM_VRAM_PERCENT: u64 = 75;

/// How a recipe fits a server.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FitKind {
	/// The whole model sits in GPU memory.
	GpuFull,
	/// Some layers sit in GPU memory, the rest in RAM.
	GpuPlusRam,
	/// Runs on the CPU from RAM.
	CpuOnly,
	/// Not on this server.
	DoesNotFit,
}

/// The verdict for one recipe on one server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Fit {
	/// The verdict.
	pub kind: FitKind,
	/// The engine to run it with on this server (a GPU recipe falls back to
	/// the CPU variant); `None` when it does not fit.
	pub engine: Option<EngineKind>,
	/// Why, with the numbers.
	pub explanation: String,
}

impl Fit {
	fn new(kind: FitKind, engine: Option<EngineKind>, explanation: String) -> Self {
		Self {
			kind,
			engine,
			explanation,
		}
	}
}

fn gpu_engine(engine: EngineKind, vendor: GpuVendor) -> Option<EngineKind> {
	use EngineKind::{Colibri, Custom, LlamaCpu, LlamaCuda, LlamaVulkan, Ollama, Vllm};
	match (engine, vendor) {
		(LlamaCpu | LlamaCuda | LlamaVulkan, GpuVendor::Nvidia) => Some(LlamaCuda),
		(LlamaCpu | LlamaCuda | LlamaVulkan, GpuVendor::Amd) => Some(LlamaVulkan),
		(Ollama, _) => Some(Ollama),
		(Vllm, GpuVendor::Nvidia) => Some(Vllm),
		(Colibri, GpuVendor::Nvidia) => Some(Colibri),
		(Vllm | Colibri | Custom, _) => None,
	}
}

fn cpu_engine(engine: EngineKind) -> Option<EngineKind> {
	use EngineKind::{Colibri, Custom, LlamaCpu, LlamaCuda, LlamaVulkan, Ollama, Vllm};
	match engine {
		LlamaCpu | LlamaCuda | LlamaVulkan => Some(LlamaCpu),
		Ollama => Some(Ollama),
		Colibri => Some(Colibri),
		Vllm | Custom => None,
	}
}

/// Engines that can keep some layers in RAM.
fn offloads(engine: EngineKind) -> bool {
	matches!(
		engine,
		EngineKind::LlamaCuda | EngineKind::LlamaVulkan | EngineKind::Ollama
	)
}

fn free_vram(gpu: &Gpu) -> u64 {
	gpu.vram_mb.saturating_sub(gpu.vram_used_mb.unwrap_or(0))
}

/// The GPU vendor with the most free memory, and that memory in MiB.
fn best_gpu(hardware: &Hardware) -> Option<(GpuVendor, u64)> {
	let sum = |vendor: &str| -> u64 {
		hardware
			.gpus
			.iter()
			.filter(|g| g.vendor == vendor && (vendor == "nvidia" || g.vram_mb >= MIN_AMD_VRAM_MB))
			.map(free_vram)
			.sum()
	};
	let (nvidia, amd) = (sum("nvidia"), sum("amd"));
	match (nvidia, amd) {
		(0, 0) => None,
		(n, a) if n >= a => Some((GpuVendor::Nvidia, n)),
		(_, a) => Some((GpuVendor::Amd, a)),
	}
}

fn gb(mb: u64) -> String {
	format!("{:.1} GB", mb as f64 * MIB as f64 / 1e9)
}

/// Whether `recipe` runs on `hardware`.
#[must_use]
pub fn fit(hardware: &Hardware, recipe: &Recipe) -> Fit {
	let need = recipe.est_ram_mb.max(recipe.est_vram_mb);
	if let Some(free_mb) = hardware.disks.iter().map(|d| d.free_mb).min() {
		let required = hf::with_headroom(recipe.total_bytes);
		if required > free_mb.saturating_mul(MIB) {
			return Fit::new(
				FitKind::DoesNotFit,
				None,
				format!(
					"The download needs {} of disk (including 10% headroom) but only {} is free.",
					gb(required.div_ceil(MIB)),
					gb(free_mb)
				),
			);
		}
	}
	let ram = hardware.memory.total_mb.saturating_sub(RAM_RESERVE_MB);
	let gpu = best_gpu(hardware).and_then(|(vendor, vram)| {
		gpu_engine(recipe.engine, vendor).map(|engine| (vendor, engine, vram))
	});
	if let Some((vendor, engine, vram)) = gpu {
		let budget = if engine == EngineKind::Vllm {
			vram * VLLM_VRAM_PERCENT / 100
		} else {
			vram
		};
		let note = passthrough_note(hardware, vendor);
		if recipe.est_vram_mb <= budget {
			return Fit::new(
				FitKind::GpuFull,
				Some(engine),
				format!(
					"Needs about {} and the GPU has {} for it.{note}",
					gb(recipe.est_vram_mb),
					gb(budget)
				),
			);
		}
		if offloads(engine) {
			let in_ram = recipe.est_ram_mb.saturating_sub(budget);
			if in_ram <= ram {
				return Fit::new(
					FitKind::GpuPlusRam,
					Some(engine),
					format!(
						"Needs about {}: {} fit on the GPU and about {} go to RAM ({} usable). \
						Slower than a full GPU run.{note}",
						gb(need),
						gb(budget),
						gb(in_ram),
						gb(ram)
					),
				);
			}
		}
	}
	if let Some(engine) = cpu_engine(recipe.engine)
		&& recipe.est_ram_mb <= ram
	{
		let why = if gpu.is_some() {
			"the GPU is too small for this engine to help"
		} else {
			"the server has no usable GPU"
		};
		return Fit::new(
			FitKind::CpuOnly,
			Some(engine),
			format!(
				"Needs about {} of RAM and {} is usable; runs on the CPU because {why}.",
				gb(recipe.est_ram_mb),
				gb(ram)
			),
		);
	}
	Fit::new(
		FitKind::DoesNotFit,
		None,
		no_fit(hardware, recipe, gpu, ram),
	)
}

fn no_fit(
	hardware: &Hardware,
	recipe: &Recipe,
	gpu: Option<(GpuVendor, EngineKind, u64)>,
	ram: u64,
) -> String {
	let need = gb(recipe.est_ram_mb.max(recipe.est_vram_mb));
	match (gpu, cpu_engine(recipe.engine)) {
		(Some((_, EngineKind::Vllm, vram)), _) => format!(
			"vLLM keeps the whole model on the GPU: it needs about {need} but may use {} of {} VRAM.",
			gb(vram * VLLM_VRAM_PERCENT / 100),
			gb(vram)
		),
		(Some((_, _, vram)), Some(_)) => format!(
			"Needs about {need} but the server has {} of GPU memory and {} of usable RAM.",
			gb(vram),
			gb(ram)
		),
		(None, Some(_)) => format!(
			"Needs about {need} of RAM but only {} is usable (no usable GPU).",
			gb(ram)
		),
		(Some((_, _, vram)), None) => format!(
			"Needs a GPU with about {need} of memory; the best one has {}.",
			gb(vram)
		),
		(None, None) => {
			let why = if hardware.gpus.is_empty() {
				"the server has no GPU"
			} else {
				"none of its GPUs works with this engine"
			};
			format!(
				"{} needs a GPU with about {need} of memory, but {why}.",
				crate::models::engines::spec(recipe.engine).label
			)
		}
	}
}

/// Says so when podkit cannot hand the GPU to a container yet.
fn passthrough_note(hardware: &Hardware, vendor: GpuVendor) -> &'static str {
	if vendor == GpuVendor::Nvidia && !hardware.cdi.devices.iter().any(|k| k == "nvidia.com/gpu") {
		" The server has no NVIDIA CDI spec yet, so containers cannot use the GPU until \
		nvidia-container-toolkit is set up (docs/spikes/gpu-cdi.md)."
	} else {
		""
	}
}

/// Every recipe of `model` with how it fits, best first: by verdict, then
/// verified recipes, then the largest download (the best quality that still
/// gets that verdict). Recipes that do not fit come last.
#[must_use]
pub fn suggest(hardware: &Hardware, model: &Model) -> Vec<(Recipe, Fit)> {
	let mut ranked: Vec<(Recipe, Fit)> = model
		.recipes
		.iter()
		.map(|r| (r.clone(), fit(hardware, r)))
		.collect();
	ranked.sort_by(|(a, fa), (b, fb)| {
		fa.kind
			.cmp(&fb.kind)
			.then(b.verified.cmp(&a.verified))
			.then(b.total_bytes.cmp(&a.total_bytes))
			.then(a.id.cmp(&b.id))
	});
	ranked
}

#[cfg(test)]
pub mod fixtures {
	use proto::{Cdi, Cpu, Disk, Gpu, Hardware, Memory};

	fn base(ram_mb: u64, free_disk_mb: u64, gpus: Vec<Gpu>) -> Hardware {
		let cdi = if gpus.iter().any(|g| g.vendor == "nvidia") {
			Cdi {
				available: true,
				devices: vec!["nvidia.com/gpu".to_owned()],
			}
		} else {
			Cdi::default()
		};
		Hardware {
			cpu: Cpu {
				model: "test cpu".to_owned(),
				threads: 16,
			},
			memory: Memory {
				total_mb: ram_mb,
				available_mb: ram_mb / 2,
			},
			disks: vec![Disk {
				path: "/var/lib/containers".to_owned(),
				free_mb: free_disk_mb,
				total_mb: free_disk_mb * 2,
			}],
			gpus,
			cdi,
		}
	}

	fn nvidia(name: &str, vram_mb: u64) -> Gpu {
		Gpu {
			vendor: "nvidia".to_owned(),
			name: name.to_owned(),
			vram_mb,
			vram_used_mb: None,
		}
	}

	/// chiko's workstation: RTX 5070 12 GB, 30 GB RAM, 159 GB free disk.
	pub fn chikos_box() -> Hardware {
		base(
			31_224,
			159 * 1000,
			vec![nvidia("NVIDIA GeForce RTX 5070", 12_227)],
		)
	}

	/// A 24 GB card with 64 GB RAM and plenty of disk.
	pub fn gpu_24gb() -> Hardware {
		base(
			65_536,
			2_000_000,
			vec![nvidia("NVIDIA GeForce RTX 4090", 24_564)],
		)
	}

	/// 512 GB RAM, no GPU, 4 TB of disk.
	pub fn cpu_512gb() -> Hardware {
		base(524_288, 4_000_000, Vec::new())
	}
}

#[cfg(test)]
mod tests {
	use proto::Gpu;

	use super::fixtures::{chikos_box, cpu_512gb, gpu_24gb};
	use super::*;
	use crate::catalog::model;

	fn recipe(model_id: &str, recipe_id: &str) -> &'static Recipe {
		model(model_id)
			.unwrap_or_else(|| panic!("no model {model_id}"))
			.recipe(recipe_id)
			.unwrap_or_else(|| panic!("no recipe {model_id}/{recipe_id}"))
	}

	fn kind(hw: &Hardware, model_id: &str, recipe_id: &str) -> FitKind {
		fit(hw, recipe(model_id, recipe_id)).kind
	}

	#[test]
	fn chikos_box_cannot_hold_deepseek_v4_1_flash_on_disk() {
		let hw = chikos_box();
		let flash = model("deepseek-v4-1-flash").unwrap();
		assert!(!flash.recipes.is_empty());
		for r in &flash.recipes {
			let f = fit(&hw, r);
			assert_eq!(f.kind, FitKind::DoesNotFit, "{}", r.id);
			assert!(f.explanation.contains("disk"), "{}", f.explanation);
			assert_eq!(f.engine, None);
		}
	}

	#[test]
	fn chikos_box_runs_qwen3_5_4b_on_the_gpu() {
		let hw = chikos_box();
		let f = fit(&hw, recipe("qwen3-5-4b", "q4-k-m"));
		assert_eq!(f.kind, FitKind::GpuFull, "{}", f.explanation);
		assert_eq!(f.engine, Some(EngineKind::LlamaCuda));
		assert_eq!(kind(&hw, "qwen3-5-4b", "q8-0"), FitKind::GpuFull);
	}

	#[test]
	fn chikos_box_splits_gpt_oss_20b_between_gpu_and_ram() {
		let hw = chikos_box();
		let r = recipe("gpt-oss-20b", "mxfp4");
		// 12.1 GB of weights plus context and buffers does not fit in 12 GB of VRAM.
		assert!(r.est_vram_mb > 12_227);
		let f = fit(&hw, r);
		assert_eq!(f.kind, FitKind::GpuPlusRam, "{}", f.explanation);
		assert!(f.explanation.contains("RAM"));
	}

	#[test]
	fn vllm_only_gets_three_quarters_of_vram() {
		// 9.3 GB of bf16 weights: the estimate is under 12 GB but over 75% of it.
		let r = recipe("qwen3-5-4b", "bf16");
		assert!(r.est_vram_mb < 12_227 && r.est_vram_mb > 12_227 * 3 / 4);
		let f = fit(&chikos_box(), r);
		assert_eq!(f.kind, FitKind::DoesNotFit);
		assert!(f.explanation.contains("vLLM"), "{}", f.explanation);
		// The spike's Qwen3-1.7B does fit.
		assert_eq!(kind(&chikos_box(), "qwen3-1-7b", "bf16"), FitKind::GpuFull);
	}

	#[test]
	fn the_big_models_do_not_fit_chikos_box() {
		let hw = chikos_box();
		for id in [
			"kimi-k3",
			"glm-5-3",
			"deepseek-v4-pro",
			"qwen3-5-397b",
			"gpt-oss-120b",
			"qwen3-8-flash-next",
		] {
			let m = model(id).unwrap();
			assert!(
				suggest(&hw, m)
					.iter()
					.all(|(_, f)| f.kind == FitKind::DoesNotFit),
				"{id} should not fit"
			);
		}
	}

	#[test]
	fn a_24gb_gpu_with_64gb_ram() {
		let hw = gpu_24gb();
		assert_eq!(kind(&hw, "qwen3-6-27b", "q4-k-m"), FitKind::GpuFull);
		assert_eq!(kind(&hw, "gemma-4-31b", "q4-k-m"), FitKind::GpuFull);
		// 22 GB of weights plus context overflows 24 GB of VRAM.
		assert_eq!(
			kind(&hw, "qwen3-6-35b-a3b", "ud-q4-k-m"),
			FitKind::GpuPlusRam
		);
		assert_eq!(kind(&hw, "gpt-oss-120b", "mxfp4"), FitKind::GpuPlusRam);
		assert_eq!(kind(&hw, "qwen3-coder-next", "q4-k-m"), FitKind::GpuPlusRam);
		// Too big for 24 GB of VRAM and 62 GB of usable RAM together.
		assert_eq!(kind(&hw, "qwen3-5-397b", "ud-iq2-xxs"), FitKind::DoesNotFit);
	}

	#[test]
	fn a_512gb_cpu_server_runs_big_models_on_the_cpu_only() {
		let hw = cpu_512gb();
		let f = fit(&hw, recipe("qwen3-5-397b", "q4-k-m"));
		assert_eq!(f.kind, FitKind::CpuOnly, "{}", f.explanation);
		assert_eq!(f.engine, Some(EngineKind::LlamaCpu));
		assert_eq!(
			kind(&hw, "deepseek-v4-1-flash", "ud-iq2-xxs"),
			FitKind::CpuOnly
		);
		assert_eq!(kind(&hw, "qwen3-5-4b", "q4-k-m"), FitKind::CpuOnly);
		// 711 GB of weights do not fit in 512 GB of RAM.
		let f = fit(&hw, recipe("kimi-k3", "ud-iq2-xxs"));
		assert_eq!(f.kind, FitKind::DoesNotFit);
		assert!(f.explanation.contains("RAM"), "{}", f.explanation);
		// vLLM has no CPU mode.
		let f = fit(&hw, recipe("qwen3-1-7b", "bf16"));
		assert_eq!(f.kind, FitKind::DoesNotFit);
		assert!(f.explanation.contains("GPU"), "{}", f.explanation);
	}

	#[test]
	fn a_tiny_amd_igpu_is_not_a_usable_gpu() {
		let mut hw = cpu_512gb();
		hw.gpus.push(Gpu {
			vendor: "amd".to_owned(),
			name: "Radeon Graphics".to_owned(),
			vram_mb: 512,
			vram_used_mb: None,
		});
		assert_eq!(kind(&hw, "qwen3-5-4b", "q4-k-m"), FitKind::CpuOnly);
	}

	#[test]
	fn a_discrete_amd_card_uses_vulkan() {
		let mut hw = chikos_box();
		hw.gpus = vec![Gpu {
			vendor: "amd".to_owned(),
			name: "Radeon RX 7900".to_owned(),
			vram_mb: 24_000,
			vram_used_mb: Some(1_000),
		}];
		let f = fit(&hw, recipe("qwen3-5-4b", "q4-k-m"));
		assert_eq!(
			(f.kind, f.engine),
			(FitKind::GpuFull, Some(EngineKind::LlamaVulkan))
		);
		// vLLM here is NVIDIA-only.
		assert_eq!(kind(&hw, "qwen3-1-7b", "bf16"), FitKind::DoesNotFit);
	}

	#[test]
	fn used_vram_is_not_available() {
		let mut hw = chikos_box();
		hw.gpus[0].vram_used_mb = Some(11_000);
		// 2.7 GB of weights no longer fit in the 1.2 GB left.
		assert_eq!(kind(&hw, "qwen3-5-4b", "q4-k-m"), FitKind::GpuPlusRam);
	}

	#[test]
	fn a_missing_cdi_spec_is_mentioned() {
		let mut hw = chikos_box();
		hw.cdi = proto::Cdi::default();
		let f = fit(&hw, recipe("qwen3-5-4b", "q4-k-m"));
		assert_eq!(f.kind, FitKind::GpuFull);
		assert!(f.explanation.contains("nvidia-container-toolkit"));
	}

	#[test]
	fn the_disk_check_uses_the_smallest_disk_and_includes_headroom() {
		let mut hw = cpu_512gb();
		let r = recipe("qwen3-5-397b", "q4-k-m"); // 244.1 GB, 268.5 GB with headroom
		hw.disks[0].free_mb = 250_000; // 262 GB: holds the files, not the headroom
		assert_eq!(fit(&hw, r).kind, FitKind::DoesNotFit);
		hw.disks[0].free_mb = 400_000;
		hw.disks.push(proto::Disk {
			path: "/data".to_owned(),
			free_mb: 200_000,
			total_mb: 500_000,
		});
		assert_eq!(fit(&hw, r).kind, FitKind::DoesNotFit);
		hw.disks[1].free_mb = 400_000;
		assert_eq!(fit(&hw, r).kind, FitKind::CpuOnly);
	}

	#[test]
	fn no_disk_figures_skips_the_disk_check() {
		let mut hw = chikos_box();
		hw.disks.clear();
		assert_eq!(kind(&hw, "qwen3-5-4b", "q4-k-m"), FitKind::GpuFull);
	}

	#[test]
	fn suggestions_are_ranked_best_first() {
		let hw = chikos_box();
		let ranked = suggest(&hw, model("qwen3-5-4b").unwrap());
		let kinds: Vec<_> = ranked.iter().map(|(_, f)| f.kind).collect();
		assert!(kinds.is_sorted(), "{kinds:?}");
		// The bigger quant leads among equals, the unfit recipe is last.
		assert_eq!(ranked[0].0.id, "q8-0");
		assert_eq!(ranked.last().unwrap().0.id, "bf16");
		assert_eq!(ranked.len(), 3);
	}

	#[test]
	fn verified_recipes_lead_their_verdict() {
		// Spike models have a single recipe; check the comparator directly.
		let hw = chikos_box();
		let mut m = model("qwen3-5-4b").unwrap().clone();
		m.recipes.retain(|r| r.engine == EngineKind::LlamaCuda);
		m.recipes[0].verified = true;
		assert_eq!(suggest(&hw, &m)[0].0.id, m.recipes[0].id);
	}

	#[test]
	fn every_recipe_gets_a_verdict_on_every_fixture() {
		for hw in [chikos_box(), gpu_24gb(), cpu_512gb()] {
			for m in crate::catalog::models() {
				let ranked = suggest(&hw, m);
				assert_eq!(ranked.len(), m.recipes.len());
				for (_, f) in ranked {
					assert!(!f.explanation.is_empty());
					assert_eq!(f.kind == FitKind::DoesNotFit, f.engine.is_none());
				}
			}
		}
	}
}
