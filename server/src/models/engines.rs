//! The inference engines a model can run on, as data, and the argument
//! builder that turns one of them plus a model's settings into a container
//! command line.
//!
//! The facts come from the engine spike (`docs/spikes/engines.md`): image,
//! port, how "loaded" is detected, and how each engine takes its model path,
//! context size and GPU layers. Nothing here touches Podman or the database,
//! so every template is covered by plain snapshot tests below.

use proto::GpuVendor;
use serde::{Deserialize, Serialize};

use crate::model_files::hf::Selection;

/// Where the model files volume is mounted, read-only, in every engine.
pub const MODELS_MOUNT: &str = "/models";
/// Largest number of extra arguments a model may carry.
const MAX_EXTRA_ARGS: usize = 32;
const MAX_EXTRA_ARG_LEN: usize = 512;

/// A weights format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Format {
	/// One `.gguf` file.
	Gguf,
	/// A Hugging Face directory of `.safetensors` files and their config.
	Safetensors,
	/// A prepared colibri model directory (`*.colibri`).
	Colibri,
}

impl Format {
	#[must_use]
	pub fn as_str(self) -> &'static str {
		match self {
			Self::Gguf => "gguf",
			Self::Safetensors => "safetensors",
			Self::Colibri => "colibri",
		}
	}
}

/// The engine of a model, in the same `snake_case` spelling in the API and the
/// `engine` column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, sqlx::Type, Serialize, Deserialize)]
#[sqlx(type_name = "varchar", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum EngineKind {
	LlamaCpu,
	LlamaCuda,
	LlamaVulkan,
	Ollama,
	Vllm,
	Colibri,
	/// Any image that serves an OpenAI-compatible API under `/v1`.
	Custom,
}

/// Whether an engine runs on a GPU.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GpuNeed {
	/// CPU only; asking for a GPU is refused.
	Never,
	/// Runs either way; the model's `gpu` flag decides.
	Optional,
	/// Does not start without one.
	Required,
}

/// How "the model is loaded and serving" is detected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadyCheck {
	/// `GET path` answers 200.
	Get(&'static str),
	/// Ollama's `GET /` answers before any model is loaded, so the model is
	/// asked to load: `POST /api/generate` with no prompt returns 200 once it is.
	OllamaLoaded,
}

/// One engine template.
#[derive(Debug)]
pub struct EngineSpec {
	pub kind: EngineKind,
	pub label: &'static str,
	/// Pinned image. Empty for [`EngineKind::Custom`], where the caller names it.
	image: &'static str,
	/// Image used instead of `image` when the model runs on a GPU.
	gpu_image: Option<&'static str>,
	/// Whether the agent pulls the image. Images built on the server are not pulled.
	pub pull: bool,
	pub formats: &'static [Format],
	pub gpu: GpuNeed,
	/// The GPU vendor the image is built for; `None` takes whatever the server has.
	pub gpu_vendor: Option<GpuVendor>,
	/// Port the engine listens on inside the container.
	pub port: u16,
	pub ready: ReadyCheck,
	/// Flag (or, for Ollama, environment variable) that sets the context size; empty when
	/// the engine has none and the model gets it as `PODKIT_CONTEXT_SIZE`.
	pub context_flag: &'static str,
	/// Flag that sets how many layers go to the GPU; `None` when the engine decides itself.
	pub gpu_layers_flag: Option<&'static str>,
	/// Seconds between SIGTERM and SIGKILL.
	pub stop_grace_s: u32,
	/// Seconds the engine needs to load a tiny model.
	pub ready_base_s: u64,
	/// Further seconds to load each GB of weights.
	pub ready_s_per_gb: u64,
	/// Whether `extra_args` can reach this engine at all.
	accepts_extra_args: bool,
	/// Flags podkit sets itself; `extra_args` may not repeat them.
	managed_flags: &'static [&'static str],
	/// `/dev/shm` size the engine needs.
	pub shm_bytes: Option<i64>,
	/// Directory the engine keeps its own state in; a volume per model is mounted there.
	pub state_dir: Option<&'static str>,
}

/// Pinned by digest: the llama.cpp project publishes rolling tags for its
/// server images, and the digest is what the spike measured on an RTX 5070.
const LLAMA_CPU: &str = "ghcr.io/ggml-org/llama.cpp:server@sha256:5b43086e3ba7ea5794d72904654f6cd41224a9b1c6ac0398f4940ee380d30f23";
const LLAMA_CUDA: &str = "ghcr.io/ggml-org/llama.cpp:server-cuda@sha256:ae96e52f8b2106ea82bad786ee2d08bb9b0a8c93403df9c27ae1bf065a118089";
const LLAMA_VULKAN: &str = "ghcr.io/ggml-org/llama.cpp:server-vulkan@sha256:5c73025f2072135ad37e306db9a57c685d8a349cfa5cb82b5c0f385b97567c94";

const LLAMA_FLAGS: &[&str] = &[
	"-m",
	"--model",
	"--host",
	"--port",
	"-c",
	"--ctx-size",
	"-ngl",
	"--n-gpu-layers",
	"--gpu-layers",
	"-a",
	"--alias",
	"-np",
	"--parallel",
];

const GGUF: &[Format] = &[Format::Gguf];
const ALL_FORMATS: &[Format] = &[Format::Gguf, Format::Safetensors, Format::Colibri];

static ENGINES: &[EngineSpec] = &[
	EngineSpec {
		kind: EngineKind::LlamaCpu,
		label: "llama.cpp (CPU)",
		image: LLAMA_CPU,
		gpu_image: None,
		pull: true,
		formats: GGUF,
		gpu: GpuNeed::Never,
		gpu_vendor: None,
		port: 8080,
		ready: ReadyCheck::Get("/health"),
		context_flag: "-c",
		gpu_layers_flag: Some("-ngl"),
		stop_grace_s: 10,
		ready_base_s: 60,
		ready_s_per_gb: 30,
		accepts_extra_args: true,
		managed_flags: LLAMA_FLAGS,
		shm_bytes: None,
		state_dir: None,
	},
	EngineSpec {
		kind: EngineKind::LlamaCuda,
		label: "llama.cpp (CUDA)",
		image: LLAMA_CUDA,
		gpu_image: None,
		pull: true,
		formats: GGUF,
		gpu: GpuNeed::Required,
		gpu_vendor: Some(GpuVendor::Nvidia),
		port: 8080,
		ready: ReadyCheck::Get("/health"),
		context_flag: "-c",
		gpu_layers_flag: Some("-ngl"),
		stop_grace_s: 10,
		ready_base_s: 60,
		ready_s_per_gb: 20,
		accepts_extra_args: true,
		managed_flags: LLAMA_FLAGS,
		shm_bytes: None,
		state_dir: None,
	},
	EngineSpec {
		kind: EngineKind::LlamaVulkan,
		label: "llama.cpp (Vulkan)",
		image: LLAMA_VULKAN,
		gpu_image: None,
		pull: true,
		formats: GGUF,
		gpu: GpuNeed::Required,
		gpu_vendor: Some(GpuVendor::Amd),
		port: 8080,
		ready: ReadyCheck::Get("/health"),
		context_flag: "-c",
		gpu_layers_flag: Some("-ngl"),
		stop_grace_s: 10,
		ready_base_s: 60,
		ready_s_per_gb: 30,
		accepts_extra_args: true,
		managed_flags: LLAMA_FLAGS,
		shm_bytes: None,
		state_dir: None,
	},
	EngineSpec {
		kind: EngineKind::Ollama,
		label: "Ollama",
		// 0.40.16 is the digest `latest` pointed at when the spike ran.
		image: "docker.io/ollama/ollama:0.40.16",
		gpu_image: None,
		pull: true,
		formats: GGUF,
		gpu: GpuNeed::Optional,
		gpu_vendor: None,
		port: 11434,
		ready: ReadyCheck::OllamaLoaded,
		context_flag: "OLLAMA_CONTEXT_LENGTH",
		gpu_layers_flag: None,
		stop_grace_s: 10,
		// Import copies the file, and the first CUDA inference on Blackwell adds 20 s.
		ready_base_s: 120,
		ready_s_per_gb: 30,
		accepts_extra_args: false,
		managed_flags: &[],
		shm_bytes: None,
		state_dir: Some("/root/.ollama"),
	},
	EngineSpec {
		kind: EngineKind::Vllm,
		label: "vLLM",
		image: "docker.io/vllm/vllm-openai:v0.34.0-x86_64",
		gpu_image: None,
		pull: true,
		formats: &[Format::Safetensors],
		gpu: GpuNeed::Required,
		gpu_vendor: Some(GpuVendor::Nvidia),
		port: 8000,
		ready: ReadyCheck::Get("/health"),
		context_flag: "--max-model-len",
		gpu_layers_flag: None,
		stop_grace_s: 10,
		// torch.compile and CUDA graph capture took 2 minutes for a 4 GB model.
		ready_base_s: 300,
		ready_s_per_gb: 30,
		accepts_extra_args: true,
		managed_flags: &[
			"--model",
			"--served-model-name",
			"--max-model-len",
			"--gpu-memory-utilization",
			"--host",
			"--port",
		],
		shm_bytes: Some(2 << 30),
		state_dir: None,
	},
	EngineSpec {
		kind: EngineKind::Colibri,
		label: "colibri",
		// Built on the server from deploy/engines/colibri, see the README there.
		image: "localhost/podkit-colibri:cpu",
		gpu_image: Some("localhost/podkit-colibri:cuda"),
		pull: false,
		formats: &[Format::Gguf, Format::Colibri],
		gpu: GpuNeed::Optional,
		gpu_vendor: Some(GpuVendor::Nvidia),
		port: 8000,
		ready: ReadyCheck::Get("/health"),
		context_flag: "--ctx",
		gpu_layers_flag: None,
		stop_grace_s: 10,
		ready_base_s: 60,
		ready_s_per_gb: 20,
		accepts_extra_args: true,
		managed_flags: &[
			"--model",
			"--host",
			"--port",
			"--ctx",
			"--model-id",
			"--api-key",
		],
		shm_bytes: None,
		state_dir: None,
	},
	EngineSpec {
		kind: EngineKind::Custom,
		label: "custom image",
		image: "",
		gpu_image: None,
		pull: true,
		formats: ALL_FORMATS,
		gpu: GpuNeed::Optional,
		gpu_vendor: None,
		port: 8000,
		ready: ReadyCheck::Get("/v1/models"),
		context_flag: "",
		gpu_layers_flag: None,
		stop_grace_s: 30,
		ready_base_s: 300,
		ready_s_per_gb: 30,
		accepts_extra_args: true,
		managed_flags: &[],
		shm_bytes: None,
		state_dir: None,
	},
];

/// Every template, in display order.
#[must_use]
pub fn all() -> &'static [EngineSpec] {
	ENGINES
}

/// The template of `kind`.
///
/// # Panics
/// Never for a valid [`EngineKind`]: a test keeps the table complete.
#[must_use]
pub fn spec(kind: EngineKind) -> &'static EngineSpec {
	ENGINES
		.iter()
		.find(|e| e.kind == kind)
		.expect("every engine has a template")
}

/// What the builder needs to know about one model.
#[derive(Debug, Clone)]
pub struct LaunchInput<'a> {
	/// Path inside the model files volume, empty for the root.
	pub model_path: &'a str,
	/// Format of what `model_path` points at.
	pub format: Format,
	/// The name clients send as `model`.
	pub served_name: &'a str,
	pub context_size: u32,
	pub gpu: bool,
	pub extra_args: &'a [String],
}

/// The part of a container spec an engine decides.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Launch {
	pub entrypoint: Option<Vec<String>>,
	pub command: Vec<String>,
	pub env: Vec<(String, String)>,
}

/// Run by Ollama's image in place of its entrypoint: start the server, import
/// the GGUF under the served name once it answers, and pass SIGTERM on. The
/// two values arrive as `$1` (file) and `$2` (name), never inside the script.
const OLLAMA_SCRIPT: &str = r#"ollama serve & server=$!
trap 'kill -TERM "$server" 2>/dev/null' TERM INT
until ollama list >/dev/null 2>&1; do sleep 1; done
printf 'FROM %s\n' "$1" > /tmp/Modelfile
ollama create "$2" -f /tmp/Modelfile || kill -TERM "$server"
wait "$server"; wait "$server""#;

impl EngineSpec {
	/// The image a model of this engine runs, `custom` being the caller's.
	#[must_use]
	pub fn image(&self, gpu: bool, custom: Option<&str>) -> String {
		if self.kind == EngineKind::Custom {
			return custom.unwrap_or_default().to_owned();
		}
		match (gpu, self.gpu_image) {
			(true, Some(image)) => image.to_owned(),
			_ => self.image.to_owned(),
		}
	}

	/// Whether `extra_args` can reach this engine at all.
	#[must_use]
	pub fn takes_extra_args(&self) -> bool {
		self.accepts_extra_args
	}

	#[must_use]
	pub fn accepts(&self, format: Format) -> bool {
		self.formats.contains(&format)
	}

	/// Formats as a sentence fragment for error messages.
	#[must_use]
	pub fn formats_text(&self) -> String {
		self.formats
			.iter()
			.map(|f| f.as_str())
			.collect::<Vec<_>>()
			.join(" or ")
	}

	/// How long to wait for the model to load: the engine's base plus a share
	/// per GB of weights.
	#[must_use]
	pub fn ready_timeout_s(&self, weights_bytes: u64) -> u64 {
		const GB: u64 = 1_000_000_000;
		let extra = weights_bytes
			.div_ceil(GB)
			.saturating_mul(self.ready_s_per_gb);
		self.ready_base_s.saturating_add(extra)
	}

	/// The request that tells whether the model is loaded: `(method, path, body)`.
	#[must_use]
	pub fn ready_request(&self, served_name: &str) -> (&'static str, &'static str, Option<String>) {
		match self.ready {
			ReadyCheck::Get(path) => ("GET", path, None),
			ReadyCheck::OllamaLoaded => (
				"POST",
				"/api/generate",
				Some(serde_json::json!({ "model": served_name, "keep_alive": -1 }).to_string()),
			),
		}
	}

	/// Checks `args` against what the engine allows.
	///
	/// # Errors
	/// A message naming the offending argument.
	pub fn validate_extra_args(&self, args: &[String]) -> Result<(), String> {
		if args.is_empty() {
			return Ok(());
		}
		if !self.accepts_extra_args {
			return Err(format!("{} takes no extra arguments", self.label));
		}
		if args.len() > MAX_EXTRA_ARGS {
			return Err(format!("at most {MAX_EXTRA_ARGS} extra arguments"));
		}
		for arg in args {
			if arg.is_empty() || arg.len() > MAX_EXTRA_ARG_LEN || arg.chars().any(char::is_control)
			{
				return Err(format!(
					"extra argument {arg:?} must be 1 to {MAX_EXTRA_ARG_LEN} characters without control characters"
				));
			}
			let flag = arg.split_once('=').map_or(arg.as_str(), |(flag, _)| flag);
			if self.managed_flags.contains(&flag) {
				return Err(format!(
					"extra argument {flag} is set by podkit for {}; change the model's settings instead",
					self.label
				));
			}
		}
		Ok(())
	}

	/// The entrypoint, command and environment that start the engine. Pure:
	/// the same input always gives the same output, which the snapshot tests
	/// rely on. Extra arguments go last, so for a flag podkit does not manage
	/// the engine sees the user's value after the default.
	#[must_use]
	pub fn launch(&self, input: &LaunchInput<'_>) -> Launch {
		let path = model_file_path(input.model_path);
		let ctx = input.context_size.to_string();
		let port = self.port.to_string();
		let mut launch = Launch::default();
		match self.kind {
			EngineKind::LlamaCpu | EngineKind::LlamaCuda | EngineKind::LlamaVulkan => {
				let layers = if input.gpu { "99" } else { "0" };
				launch.command = args(&[
					"-m",
					&path,
					"--host",
					"0.0.0.0",
					"--port",
					&port,
					"-c",
					&ctx,
					"-np",
					"1",
					"-ngl",
					layers,
					"--alias",
					input.served_name,
					"--no-webui",
				]);
			}
			EngineKind::Vllm => {
				launch.command = args(&[
					"--model",
					&path,
					"--served-model-name",
					input.served_name,
					"--max-model-len",
					&ctx,
					"--gpu-memory-utilization",
					"0.75",
					"--host",
					"0.0.0.0",
					"--port",
					&port,
				]);
				// The weights are mounted; without this vLLM tries to reach the Hub.
				launch.env = vec![("HF_HUB_OFFLINE".to_owned(), "1".to_owned())];
			}
			EngineKind::Ollama => {
				launch.entrypoint = Some(args(&["sh", "-c", OLLAMA_SCRIPT, "sh"]));
				launch.command = args(&[&path, input.served_name]);
				launch.env = vec![
					("OLLAMA_HOST".to_owned(), format!("0.0.0.0:{port}")),
					("OLLAMA_CONTEXT_LENGTH".to_owned(), ctx),
					("OLLAMA_KEEP_ALIVE".to_owned(), "-1".to_owned()),
					("OLLAMA_NUM_PARALLEL".to_owned(), "1".to_owned()),
				];
			}
			EngineKind::Colibri => {
				launch.command = args(&["--ctx", &ctx, "--model-id", input.served_name]);
				let source = if input.format == Format::Colibri {
					"COLI_MODEL_DIR"
				} else {
					"COLI_GGUF"
				};
				launch.env = vec![
					(source.to_owned(), path),
					("COLI_PORT".to_owned(), port),
					// It refuses to bind 0.0.0.0 without a key; the network is internal.
					("COLI_ALLOW_INSECURE_BIND".to_owned(), "1".to_owned()),
				];
				if input.gpu {
					launch.env.push(("COLI_CUDA".to_owned(), "1".to_owned()));
				}
			}
			EngineKind::Custom => {
				launch.env = vec![
					("PODKIT_MODEL_PATH".to_owned(), path),
					(
						"PODKIT_SERVED_NAME".to_owned(),
						input.served_name.to_owned(),
					),
					("PODKIT_CONTEXT_SIZE".to_owned(), ctx),
				];
			}
		}
		launch.command.extend(input.extra_args.iter().cloned());
		launch
	}
}

fn args(parts: &[&str]) -> Vec<String> {
	parts.iter().map(|p| (*p).to_owned()).collect()
}

/// Absolute path of a file or directory of the model files volume.
#[must_use]
pub fn model_file_path(model_path: &str) -> String {
	let rel = model_path.trim_matches('/');
	if rel.is_empty() {
		MODELS_MOUNT.to_owned()
	} else {
		format!("{MODELS_MOUNT}/{rel}")
	}
}

/// Checks a path inside the volume: relative, no `..`, no characters that mean
/// something to a shell or a flag parser.
///
/// # Errors
/// A message for a path that could point outside the volume or look like a flag.
pub fn validate_model_path(path: &str) -> Result<(), String> {
	if path.len() > 500 {
		return Err("model_path is at most 500 characters".to_owned());
	}
	if path.starts_with('-') || path.starts_with('/') && path.trim_matches('/').is_empty() {
		return Err(format!("invalid model_path {path:?}"));
	}
	let bad = path.split('/').any(|part| part == "..")
		|| path
			.chars()
			.any(|c| c.is_control() || c.is_whitespace() || matches!(c, '\\' | ':' | '"' | '\''));
	if bad {
		return Err(format!("invalid model_path {path:?}"));
	}
	Ok(())
}

/// The format a path names by its extension: `.gguf` or `.colibri`. A
/// directory of safetensors has no telltale name, so it is `None`.
#[must_use]
pub fn format_of_path(path: &str) -> Option<Format> {
	let extension = std::path::Path::new(path.trim_matches('/'))
		.extension()?
		.to_str()?;
	if extension.eq_ignore_ascii_case("gguf") {
		Some(Format::Gguf)
	} else if extension.eq_ignore_ascii_case("colibri") {
		Some(Format::Colibri)
	} else {
		None
	}
}

/// What a model serves: the path inside the volume and its format.
///
/// `requested` is the caller's `model_path`. When it is absent the path comes
/// from the selection that was downloaded (a single named `.gguf`, or the
/// volume root for safetensors). The engine's accepted formats are checked as
/// soon as the files' format is known, so GGUF on vLLM is refused with that
/// said, not with a question about paths.
///
/// # Errors
/// A message saying what is wrong with the files or the path.
pub fn resolve_model(
	engine: &EngineSpec,
	selection: &Selection,
	requested: Option<&str>,
) -> Result<(String, Format), String> {
	let requested = requested
		.map(|p| p.trim_matches('/'))
		.filter(|p| !p.is_empty());
	if let Some(path) = requested {
		validate_model_path(path)?;
	}
	let by_path = requested.and_then(format_of_path);
	let names = selection.patterns();
	let has = |ext: &str| names.iter().any(|n| n.ends_with(ext));
	let by_files = match (has(".gguf"), has(".safetensors")) {
		(true, false) => Some(Format::Gguf),
		(false, true) => Some(Format::Safetensors),
		_ => None,
	};
	if let Some(format) = by_path.or(by_files)
		&& !engine.accepts(format)
	{
		return Err(format!(
			"{} serves {}, but these files are {}",
			engine.label,
			engine.formats_text(),
			format.as_str()
		));
	}

	match (requested, by_path, by_files) {
		(Some(path), Some(format), _) => Ok((path.to_owned(), format)),
		(Some(path), None, Some(Format::Gguf)) => Err(format!(
			"model_path {path:?} is not a .gguf file, but these files are GGUF"
		)),
		(Some(path), None, _) => Ok((path.to_owned(), Format::Safetensors)),
		(None, _, Some(Format::Gguf)) => sole_gguf(selection)
			.map(|path| (path, Format::Gguf))
			.ok_or_else(|| {
				"these files hold GGUF weights, so model_path must name the .gguf file to serve \
				(its path inside the download, like Qwen3-4B-Q4_K_M.gguf)"
					.to_owned()
			}),
		(None, _, Some(format)) => Ok((String::new(), format)),
		(None, _, None) if names.is_empty() => Err(
			"the download covers a whole repo, so model_path must say what to serve: a .gguf file, \
			a .colibri directory, or a directory of safetensors"
				.to_owned(),
		),
		(None, _, None) if has(".gguf") => Err(
			"these files hold both GGUF and safetensors weights; set model_path to the one to serve"
				.to_owned(),
		),
		(None, _, None) => Ok((String::new(), Format::Safetensors)),
	}
}

/// The only file of the selection when it names exactly one `.gguf` and no
/// glob, so `model_path` can be left out.
#[must_use]
pub fn sole_gguf(selection: &Selection) -> Option<String> {
	let mut ggufs = selection
		.patterns()
		.into_iter()
		.filter(|p| format_of_path(p) == Some(Format::Gguf) && !p.contains(['*', '?']));
	let first = ggufs.next()?;
	ggufs.next().is_none().then_some(first)
}

#[cfg(test)]
mod tests {
	use super::*;

	fn input<'a>(path: &'a str, format: Format, gpu: bool, extra: &'a [String]) -> LaunchInput<'a> {
		LaunchInput {
			model_path: path,
			format,
			served_name: "qwen",
			context_size: 8192,
			gpu,
			extra_args: extra,
		}
	}

	fn line(launch: &Launch) -> String {
		launch.command.join(" ")
	}

	#[test]
	fn the_flag_data_matches_what_the_builder_emits() {
		for e in all() {
			let l = e.launch(&input("q.gguf", Format::Gguf, true, &[]));
			if !e.context_flag.is_empty() {
				let in_args = l
					.command
					.windows(2)
					.any(|w| w[0] == e.context_flag && w[1] == "8192");
				let in_env = l
					.env
					.iter()
					.any(|(k, v)| k == e.context_flag && v == "8192");
				assert!(
					in_args || in_env,
					"{} does not pass the context size",
					e.label
				);
			}
			if let Some(flag) = e.gpu_layers_flag {
				assert!(
					l.command.windows(2).any(|w| w[0] == flag && w[1] == "99"),
					"{}",
					e.label
				);
			}
		}
	}

	#[test]
	fn every_engine_has_exactly_one_template() {
		for kind in [
			EngineKind::LlamaCpu,
			EngineKind::LlamaCuda,
			EngineKind::LlamaVulkan,
			EngineKind::Ollama,
			EngineKind::Vllm,
			EngineKind::Colibri,
			EngineKind::Custom,
		] {
			assert_eq!(
				all().iter().filter(|e| e.kind == kind).count(),
				1,
				"{kind:?}"
			);
			assert_eq!(spec(kind).kind, kind);
		}
		assert_eq!(all().len(), 7);
	}

	#[test]
	fn pinned_images_carry_a_tag_or_digest() {
		for e in all().iter().filter(|e| e.kind != EngineKind::Custom) {
			for image in std::iter::once(e.image).chain(e.gpu_image) {
				assert!(
					image.contains('@')
						|| image.rsplit_once(':').is_some_and(|(_, t)| t != "latest"),
					"{image} is not pinned"
				);
			}
		}
	}

	#[test]
	fn llama_cpu_snapshot() {
		let l = spec(EngineKind::LlamaCpu).launch(&input("q.gguf", Format::Gguf, false, &[]));
		assert_eq!(
			line(&l),
			"-m /models/q.gguf --host 0.0.0.0 --port 8080 -c 8192 -np 1 -ngl 0 --alias qwen --no-webui"
		);
		assert_eq!(l.entrypoint, None);
		assert!(l.env.is_empty());
	}

	#[test]
	fn llama_cuda_and_vulkan_offload_every_layer() {
		for kind in [EngineKind::LlamaCuda, EngineKind::LlamaVulkan] {
			let l = spec(kind).launch(&input("sub/q.gguf", Format::Gguf, true, &[]));
			assert_eq!(
				line(&l),
				"-m /models/sub/q.gguf --host 0.0.0.0 --port 8080 -c 8192 -np 1 -ngl 99 --alias qwen --no-webui"
			);
		}
	}

	#[test]
	fn llama_extra_args_come_last() {
		let extra = vec!["--threads".to_owned(), "4".to_owned()];
		let l = spec(EngineKind::LlamaCpu).launch(&input("q.gguf", Format::Gguf, false, &extra));
		assert!(line(&l).ends_with("--no-webui --threads 4"), "{}", line(&l));
	}

	#[test]
	fn vllm_snapshot() {
		let l = spec(EngineKind::Vllm).launch(&input("", Format::Safetensors, true, &[]));
		assert_eq!(
			line(&l),
			"--model /models --served-model-name qwen --max-model-len 8192 \
			--gpu-memory-utilization 0.75 --host 0.0.0.0 --port 8000"
		);
		assert_eq!(l.env, [("HF_HUB_OFFLINE".to_owned(), "1".to_owned())]);
		assert_eq!(spec(EngineKind::Vllm).shm_bytes, Some(2 << 30));
	}

	#[test]
	fn ollama_snapshot() {
		let l = spec(EngineKind::Ollama).launch(&input("q.gguf", Format::Gguf, true, &[]));
		let entry = l.entrypoint.clone().unwrap();
		assert_eq!(&entry[..2], ["sh", "-c"]);
		assert_eq!(entry[3], "sh");
		// The values are arguments, not part of the script.
		assert!(!entry[2].contains("q.gguf") && !entry[2].contains("qwen"));
		assert_eq!(l.command, ["/models/q.gguf", "qwen"]);
		assert_eq!(
			l.env,
			[
				("OLLAMA_HOST".to_owned(), "0.0.0.0:11434".to_owned()),
				("OLLAMA_CONTEXT_LENGTH".to_owned(), "8192".to_owned()),
				("OLLAMA_KEEP_ALIVE".to_owned(), "-1".to_owned()),
				("OLLAMA_NUM_PARALLEL".to_owned(), "1".to_owned()),
			]
		);
		assert_eq!(spec(EngineKind::Ollama).state_dir, Some("/root/.ollama"));
	}

	#[test]
	fn colibri_snapshot() {
		let c = spec(EngineKind::Colibri);
		let cpu = c.launch(&input("olmoe.gguf", Format::Gguf, false, &[]));
		assert_eq!(line(&cpu), "--ctx 8192 --model-id qwen");
		assert_eq!(
			cpu.env,
			[
				("COLI_GGUF".to_owned(), "/models/olmoe.gguf".to_owned()),
				("COLI_PORT".to_owned(), "8000".to_owned()),
				("COLI_ALLOW_INSECURE_BIND".to_owned(), "1".to_owned()),
			]
		);
		let cuda = c.launch(&input("olmoe.colibri", Format::Colibri, true, &[]));
		assert_eq!(
			cuda.env[0],
			(
				"COLI_MODEL_DIR".to_owned(),
				"/models/olmoe.colibri".to_owned()
			)
		);
		assert_eq!(
			cuda.env.last().unwrap(),
			&("COLI_CUDA".to_owned(), "1".to_owned())
		);
		assert_eq!(c.image(false, None), "localhost/podkit-colibri:cpu");
		assert_eq!(c.image(true, None), "localhost/podkit-colibri:cuda");
		assert!(!c.pull);
	}

	#[test]
	fn custom_snapshot() {
		let extra = vec!["serve".to_owned(), "--fast".to_owned()];
		let c = spec(EngineKind::Custom);
		let l = c.launch(&input("m/dir", Format::Safetensors, false, &extra));
		assert_eq!(line(&l), "serve --fast");
		assert_eq!(
			l.env,
			[
				("PODKIT_MODEL_PATH".to_owned(), "/models/m/dir".to_owned()),
				("PODKIT_SERVED_NAME".to_owned(), "qwen".to_owned()),
				("PODKIT_CONTEXT_SIZE".to_owned(), "8192".to_owned()),
			]
		);
		assert_eq!(c.image(false, Some("example.com/x:1")), "example.com/x:1");
	}

	#[test]
	fn ready_requests() {
		assert_eq!(
			spec(EngineKind::LlamaCpu).ready_request("x"),
			("GET", "/health", None)
		);
		assert_eq!(
			spec(EngineKind::Vllm).ready_request("x"),
			("GET", "/health", None)
		);
		assert_eq!(
			spec(EngineKind::Custom).ready_request("x"),
			("GET", "/v1/models", None)
		);
		let (method, path, body) = spec(EngineKind::Ollama).ready_request("qw\"en");
		assert_eq!((method, path), ("POST", "/api/generate"));
		let body: serde_json::Value = serde_json::from_str(&body.unwrap()).unwrap();
		assert_eq!(
			body,
			serde_json::json!({ "model": "qw\"en", "keep_alive": -1 })
		);
	}

	#[test]
	fn ready_timeout_grows_with_the_files() {
		let llama = spec(EngineKind::LlamaCpu);
		assert_eq!(llama.ready_timeout_s(0), 60);
		assert_eq!(llama.ready_timeout_s(2_497_280_256), 60 + 3 * 30);
		assert!(spec(EngineKind::Vllm).ready_timeout_s(4_063_515_592) > 300);
	}

	#[test]
	fn extra_args_are_validated() {
		let llama = spec(EngineKind::LlamaCpu);
		let ok = |a: &[&str]| {
			llama.validate_extra_args(&a.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>())
		};
		assert!(ok(&["--threads", "4"]).is_ok());
		assert!(ok(&[]).is_ok());
		assert!(ok(&["--port", "9"]).unwrap_err().contains("--port"));
		assert!(ok(&["-ngl=5"]).unwrap_err().contains("-ngl"));
		assert!(ok(&["a\nb"]).is_err());
		assert!(ok(&[""]).is_err());
		assert!(
			llama
				.validate_extra_args(&vec!["x".to_owned(); 33])
				.is_err()
		);
		let ollama = spec(EngineKind::Ollama).validate_extra_args(&["--x".to_owned()]);
		assert!(ollama.unwrap_err().contains("no extra arguments"));
	}

	#[test]
	fn model_paths_stay_inside_the_volume() {
		for good in ["", "q.gguf", "a/b/q.gguf", "dir"] {
			assert!(validate_model_path(good).is_ok(), "{good}");
		}
		for bad in ["../x", "a/../b", "-m", "a b", "a:b", "a\\b", "q\"", "/"] {
			assert!(validate_model_path(bad).is_err(), "{bad}");
		}
		assert_eq!(model_file_path(""), "/models");
		assert_eq!(model_file_path("/a/b/"), "/models/a/b");
	}

	#[test]
	fn format_comes_from_the_path_or_the_files() {
		let sel = |include: &[&str], files: &[&str]| Selection {
			include: include.iter().map(|s| (*s).to_owned()).collect(),
			files: files.iter().map(|s| (*s).to_owned()).collect(),
		};
		let llama = spec(EngineKind::LlamaCpu);
		let vllm = spec(EngineKind::Vllm);
		let colibri = spec(EngineKind::Colibri);
		let ok = |e: &EngineSpec, s: &Selection, p: Option<&str>| resolve_model(e, s, p);

		// A named file settles it; so does an explicit path.
		assert_eq!(
			ok(llama, &sel(&[], &["q.gguf", "config.json"]), None),
			Ok(("q.gguf".to_owned(), Format::Gguf))
		);
		assert_eq!(
			ok(llama, &sel(&["*.gguf"], &[]), Some("/sub/q.gguf/")),
			Ok(("sub/q.gguf".to_owned(), Format::Gguf))
		);
		assert_eq!(
			ok(colibri, &sel(&[], &[]), Some("m.colibri")),
			Ok(("m.colibri".to_owned(), Format::Colibri))
		);
		assert_eq!(
			ok(vllm, &sel(&["*.safetensors", "*.json"], &[]), None),
			Ok((String::new(), Format::Safetensors))
		);
		assert_eq!(
			ok(vllm, &sel(&[], &[]), Some("dir")),
			Ok(("dir".to_owned(), Format::Safetensors))
		);

		// A glob does not name one file.
		let err = ok(llama, &sel(&["*Q4.gguf"], &[]), None).unwrap_err();
		assert!(err.contains("model_path must name the .gguf file"), "{err}");
		assert!(
			ok(llama, &sel(&[], &[]), None)
				.unwrap_err()
				.contains("whole repo")
		);
		assert!(
			ok(llama, &sel(&["*.gguf", "*.safetensors"], &[]), None)
				.unwrap_err()
				.contains("both")
		);
		assert!(
			ok(llama, &sel(&["*.gguf"], &[]), Some("dir"))
				.unwrap_err()
				.contains("not a .gguf")
		);
		assert!(ok(llama, &sel(&[], &[]), Some("../x")).is_err());

		// The engine's formats are checked first, naming both sides.
		let err = ok(vllm, &sel(&["*.gguf"], &[]), None).unwrap_err();
		assert_eq!(err, "vLLM serves safetensors, but these files are gguf");
		let err = ok(llama, &sel(&["*.safetensors"], &[]), None).unwrap_err();
		assert!(err.contains("llama.cpp (CPU) serves gguf"), "{err}");
		assert!(ok(vllm, &sel(&[], &[]), Some("q.gguf")).is_err());

		assert_eq!(
			sole_gguf(&sel(&[], &["q.gguf", "config.json"])).as_deref(),
			Some("q.gguf")
		);
		assert_eq!(sole_gguf(&sel(&["*Q4.gguf"], &[])), None);
		assert_eq!(sole_gguf(&sel(&[], &["a.gguf", "b.gguf"])), None);
	}

	#[test]
	fn formats_are_declared_per_engine() {
		assert!(spec(EngineKind::Vllm).accepts(Format::Safetensors));
		assert!(!spec(EngineKind::Vllm).accepts(Format::Gguf));
		assert_eq!(spec(EngineKind::Vllm).formats_text(), "safetensors");
		assert!(spec(EngineKind::Colibri).accepts(Format::Colibri));
		assert!(!spec(EngineKind::LlamaCpu).accepts(Format::Safetensors));
	}
}
