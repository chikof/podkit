# Spike: inference engines as containers

Card AI S2. Each engine was run as a rootless Podman container (6.1.4) on the
workstation (Ryzen 7 9850X3D, RTX 5070 12 GB, driver 595.112.07 / CUDA 13.2),
one at a time, with the card's caps: `--memory 12g`, context 8192 (vLLM 4096),
vLLM `--gpu-memory-utilization 0.75`. GPU access is the CDI device
`nvidia.com/gpu=all` from the AI S1 spike (`docs/spikes/gpu-cdi.md`).

Raw numbers: [`engines.json`](engines.json) (one object per variant, read by AI 04
and AI 06) and `raw/` (the script's output and stderr per run).
Reproduce a single run with `scripts/spike-engine.sh <engine> <model>`, all of
them with `scripts/spike-engines-all.sh` (run it in the background, ~15 min).

## Weights used

| Engines | HF repo | File | Bytes |
|---|---|---|---:|
| llama.cpp (all), Ollama (import) | `Qwen/Qwen3-4B-GGUF` | `Qwen3-4B-Q4_K_M.gguf` | 2 497 280 256 |
| Ollama (own pull) | Ollama registry `qwen3:4b` | pulled by the engine (Q4_K_M) | about 2.5 GB |
| vLLM | `Qwen/Qwen3-1.7B` | `model-00001-of-00002.safetensors` + `model-00002-of-00002.safetensors` (+ config/tokenizer files) | 4 063 515 592 |
| colibri | `allenai/OLMoE-1B-7B-0125-Instruct-GGUF` | `OLMoE-1B-7B-0125-Instruct-Q4_K_S.gguf` | 3 963 688 960 |

Sizes were read from the HF tree API before downloading. Qwen3-1.7B is 4.06 GB, so
it is over 4 GB but inside the card's 9 GB safetensors allowance for vLLM. (Qwen3-4B in bf16 is
8 GB, too tight for 0.75 × 12 GB.) Weights live in `~/Projects/.podkit-spikes-cache/models`,
and `~/.cache/podkit-spikes` is a symlink to it. **`~/.cache` is tmpfs on this NixOS box**, so the
symlink is gone after a reboot; recreate it with
`ln -sfn ~/Projects/.podkit-spikes-cache ~/.cache/podkit-spikes`.

## Comparison

All runs: Qwen3 (a thinking model, so the 256 generated tokens are mostly
`reasoning_content`), 8 CPU threads for the CPU engines, `temperature 0`.

| variant | image (size) | model feed | cold start¹ | to first answer² | decode tok/s | prompt tok/s | peak RSS / cgroup MB | VRAM MB | SIGTERM → exit | `/v1/messages` |
|---|---|---|---:|---:|---:|---:|---:|---:|---:|---|
| `colibri-cpu` | `localhost/podkit-colibri:cpu` (126 MB) | gguf-import | 0.825 s | 8.024 s | 57.7 | 261.7 | 4387 / 5246 | 0 | 0.965 s (code 0) | yes, + streaming |
| `colibri-cuda` | `localhost/podkit-colibri:cuda` (3528 MB) | gguf-import | 3.259 s | 3.882 s | 461.47 | 13200.3 | 1027 / 4396 | 4903 | 0.339 s (code 0) | yes, + streaming |
| `llama-cpu` | `ggml-org/llama.cpp:server` (837 MB) | mount | 1.136 s | 13.048 s | 21.84 | 287.7 | 5693 / 3405 | 0 | 0.322 s (code 0) | yes, + streaming |
| `llama-cuda` | `ggml-org/llama.cpp:server-cuda` (4399 MB) | mount | 1.569 s | 3.122 s | 170.56 | 7856 | 2780 / 1476 | 3818 | 0.596 s (code 0) | yes, + streaming |
| `llama-vulkan` | `ggml-org/llama.cpp:server-vulkan` (882 MB) | mount | 1.666 s | 25.891 s | 10.9 | 80.1 | 520 / 703 | 46 | 0.327 s (code 0) | yes, + streaming |
| `ollama-cpu` | `ollama/ollama:latest` (5103 MB) | gguf-import | 4.925 s | 20.427 s | 17.85 | 176 | 4332 / 5999 | 0 | 0.328 s (code 0) | yes, + streaming |
| `ollama-cuda` | `ollama/ollama:latest` (5103 MB) | gguf-import | 23.955 s | 42.311 s | 158.18 | 7465 | 1858 / 4067 | 3943 | 0.555 s (code 0) | yes, + streaming |
| `ollama-cuda-pull` | `ollama/ollama:latest` (5103 MB) | pull | 40.523 s | 58.621 s | 171.41 | 7670 | 1859 / 3973 | 3912 | 0.529 s (code 0) | yes, + streaming |
| `vllm` | `vllm/vllm-openai:v0.34.0-x86_64` (25935 MB) | mount | 128.117 s | 129.887 s | 142.43 | 15409.7 | 6116 / 8214 | 8532 | 1.469 s (code 0) | yes, + streaming |

¹ Time from `podman run` until the readiness endpoint answered 200 (Ollama: server up + `ollama create`/`pull` + explicit
load). Weights were already in the OS page cache, so this is **not** a cold-disk number.
² Cold start plus the first 256-token chat request. This is what a user waits for after "start":
Ollama's first CUDA inference costs about 17 to 20 s more, vLLM includes CUDA-graph capture.
Decode tok/s = (completion_tokens − 1) / (time of last token chunk − time of first), streamed, 256 tokens.
Prompt tok/s = prompt_tokens / time to first token for a ~1.7k token prompt with a random nonce (no prompt cache), 1 token out.
Peak RSS is the sum of process RSS sampled every 0.5 s (mmapped weight pages count towards it, so CPU llama.cpp shows
the whole model); cgroup is `memory.peak` of the container (it includes page cache, so it moves between runs).
VRAM is the rise of `nvidia-smi` memory.used over the baseline (the desktop itself holds about 1.4 GB). colibri-cpu/-cuda
run a different (and 3× larger, MoE) model than the rest, so don't compare their tok/s with the Qwen3 rows.

`llama-vulkan` runs on the AMD iGPU (Mesa RADV, `--device /dev/dri`), not on the 5070; see the gotchas.

## What each engine needs

| | image / tag that worked | port | readiness ("model loaded") | launch |
|---|---|---|---|---|
| llama.cpp | `ghcr.io/ggml-org/llama.cpp:server` (CPU), `:server-cuda` (works on the 5070 / sm_120, no need for `:server-cuda13`), `:server-vulkan`. All multi-arch. Entrypoint is `llama-server`. | 8080 | `GET /health` is 503 while loading, 200 when loaded | `-m /models/x.gguf --host 0.0.0.0 --port 8080 -c 8192 -np 1 -t 8 -ngl 99 --no-webui` with the GGUF directory mounted `:ro` at `/models`. `-ngl 0` on CPU. |
| Ollama | `docker.io/ollama/ollama:latest` (5.1 GB, one image for CPU and CUDA; no GPU device given means CPU) | 11434 | `GET /` says "Ollama is running" as soon as the server is up, **before** any model is loaded. "Loaded" = `POST /api/generate {"model":..,"keep_alive":-1}` returns, and `/api/ps` lists it. | args `serve`; env `OLLAMA_HOST=0.0.0.0:11434 OLLAMA_CONTEXT_LENGTH=8192 OLLAMA_KEEP_ALIVE=-1 OLLAMA_NUM_PARALLEL=1`. Model feed: either `ollama create spike -f Modelfile` with `FROM /models/x.gguf` (GGUF import, copies the file into the container's `/root/.ollama` volume, 3 to 14 s) or `ollama pull qwen3:4b` (own pull). |
| vLLM | `docker.io/vllm/vllm-openai:v0.34.0-x86_64` works on the 5070 (25.9 GB on disk, about 10 GB to download). Entrypoint is `vllm serve`. | 8000 | `GET /health` is 200 only once the model is loaded and the server is up (about 2 min) | `--model /model --served-model-name spike --max-model-len 4096 --gpu-memory-utilization 0.75 --host 0.0.0.0 --port 8000`, model directory mounted `:ro` at `/model`, `--shm-size 2g`, `HF_HUB_OFFLINE=1` |
| colibri | none official; built by `deploy/engines/colibri/Containerfile` from commit `0a5504a1dc64f86fcd0fe7db298b0448c730e7e6` (`coli` banner v2.1.0, repo tag `v2.1.0` exists), `localhost/podkit-colibri:cpu` (126 MB, builds in ~1 min) and `:cuda` (3.5 GB, ~5 min, `CUDA_ARCH=portable` covers sm_120) | 8000 | `GET /health` 200 (the gateway starts after the engine is up); `/v1/models` also answers | entrypoint imports the GGUF in link mode (`coli import`, nothing is copied) then `coli serve --host 0.0.0.0`; env `COLI_GGUF=/models/x.gguf`, `COLI_API_KEY=...` (required to bind 0.0.0.0), `COLI_CUDA=1` for the CUDA build |

## APIs

All nine variants pass `/v1/models`, `/v1/chat/completions`, SSE streaming and
`stream_options.include_usage`. **All nine also serve the Anthropic
`/v1/messages` API, non-streaming and streaming** (sent with `x-api-key` and
`anthropic-version: 2023-06-01`).

| | OpenAI non-stream usage | OpenAI stream usage | Anthropic usage |
|---|---|---|---|
| llama.cpp | `.usage{prompt_tokens,completion_tokens,total_tokens,prompt_tokens_details.cached_tokens}` plus a llama-specific `.timings` block (prompt_per_second, predicted_per_second) | the final chunk, `choices: []`, when `include_usage` is set; also a `timings` object on chunks | `message_start.message.usage` (`input_tokens` counts only uncached tokens, the rest in `cache_read_input_tokens`; `output_tokens:0`) and `message_delta.usage.output_tokens` |
| Ollama | same `.usage` shape, `cached_tokens` filled | final chunk, `choices: []` | `message_start` has `input_tokens` (without cache) and `output_tokens:0`; `message_delta` has `input_tokens`, `cache_read_input_tokens`, `output_tokens` (so the numbers on the two events differ) |
| vLLM | `.usage{prompt_tokens,completion_tokens,total_tokens,completion_tokens_details:null}` | final chunk, `choices: []` | `message_start` has `input_tokens`, `message_delta` has `input_tokens` and `output_tokens` (full counts on both) |
| colibri | `.usage{prompt_tokens,completion_tokens,total_tokens}` | final chunk, `choices: []` | `message_start` has `input_tokens: 0` (**wrong**) and `message_delta` only `output_tokens`; the real `input_tokens` comes only on the non-streaming response |

Gateway consequence for AI 05: take token counts from the OpenAI-style final
chunk and from `message_delta` for output; for Anthropic streaming input tokens,
colibri gives none, Ollama and llama.cpp split them with `cache_read_input_tokens`,
so a metering layer should sum `input_tokens + cache_read_input_tokens`.

## SIGTERM

Every variant exits with code 0 within 0.3 to 1.5 s of `podman kill -s TERM` (llama.cpp logs
"cleaning up before exit", vLLM takes the longest at about 1.5 s). Nothing needed `SIGKILL`.
A 10 s grace period is enough for the whole set; 30 s is generous. (Two early runs showed 20 to 46 s,
which ran while image pulls were hitting the disk (likely contention); they did not repeat in four later runs.)
colibri works because the entrypoint `exec`s `coli serve`; without the `exec` PID 1 would be `sh`.

## Gotchas

- **Ollama first inference on Blackwell is slow.** `ollama_load_s` is about 17 s and the first chat
  adds about 17 to 20 s more (probably kernel JIT for sm_120, not confirmed; the cache would live in the container, so it repeats
  after a recreate). Readiness should be "`/api/generate` with an empty prompt returns", not `GET /`,
  and a podkit start timeout for Ollama+CUDA should be 90 s or more. A persistent volume for the CUDA
  compute cache (`/root/.nv`) may remove the repeat; not tested.
- **Ollama GGUF import copies the file** into its own store (another 2.5 GB in the container volume,
  `podman rm -v` removes it). The model name is whatever you pass to `create`; `/v1/models` lists it.
  Ollama also needs an extra step beyond "start container": `exec ollama create`, or an `ollama pull`
  with network access. podkit has to drive that through the Ollama HTTP API (`POST /api/create`
  and `/api/pull`), not through `exec`.
- **llama.cpp serves whatever name you ask for** (`model` is ignored; `/v1/models` returns the file path as the id).
  Pass `-np 1` explicitly: with several parallel slots `-c` is divided between them (not measured here; the runs used `-np 1`).
- **Qwen3 thinking output** lands in `message.reasoning_content` (llama.cpp, Ollama) or `reasoning`; `content`
  is `""` until thinking ends. A client that reads only `content` sees nothing for 256 tokens.
- **vLLM:** 25.9 GB image; cold start is about 2 min (torch.compile + CUDA graphs); VRAM takes the full
  0.75 × 12 GB (8.5 GB measured, mostly KV cache) even for a 1.7B model, so it will not share the
  card with another engine. The 5070 (Blackwell) works with `v0.34.0-x86_64`; no other tag was tried.
  `/v1/models` lists the `--served-model-name`. Mount the model dir and set `HF_HUB_OFFLINE=1`, or it
  tries to reach the Hub.
- **colibri:**
  - The repo's own `docker/Dockerfile.slim` sets `COLI_DOCKER_GLM_ONLY=1`, so it only runs GLM; `docker/Dockerfile`
    builds from `git clone` of `main` at image-build time. The Containerfile here pins the commit, builds only
    the `olmoe` engine and copies the launcher files.
  - **It refuses to bind 0.0.0.0 without `COLI_API_KEY`** (or `COLI_ALLOW_INSECURE_BIND=1`). Auth is then a Bearer
    key on `/v1/*` (the Anthropic test sent both `Authorization: Bearer` and `x-api-key`, so which one colibri requires on `/v1/messages` was not isolated). `/health` is public.
  - **The GPU is off unless asked**: the CUDA build with no `COLI_CUDA=1` ran at CPU speed (63 tok/s) and
    used 111 MB of VRAM. With `COLI_CUDA=1`: 461 to 495 tok/s, 4.9 GB VRAM. Vulkan needs `COLI_VULKAN=1`.
  - Cold start looks instant (about 0.6 to 0.8 s) because the GGUF is read in place; the first request
    pays for paging it in and took 3.5 to 7 s extra on CPU. OLMoE has no tool calling in colibri (HTTP 400 on tool declarations).
  - The Vulkan variant was not built: the NVIDIA Vulkan ICD is not in the image or in the CDI mounts, so it would
    run on the iGPU like llama-vulkan.
- **Vulkan images do not see the 5070.** With `--device nvidia.com/gpu=all` only, `llama-server --list-devices`
  prints "(none)"; the CDI spec does not mount an NVIDIA Vulkan ICD and the image only ships Mesa ICDs.
  With `--device /dev/dri` it finds the iGPU (`RADV RAPHAEL_MENDOCINO`, 16 GB shared) and runs at 10.9 tok/s, slower than the CPU.
  On this machine use CUDA for NVIDIA; Vulkan is for AMD/Intel hosts.
- **llama.cpp memory:** with CUDA the weights are copied into VRAM, so host RSS is much
  lower than CPU (1.2 to 2.8 GB vs 5.7 GB); cgroup peak mostly shows page cache.
- Cold-start numbers were taken with the model file in the page cache. A true cold read of 2.5 to 4 GB from NVMe
  adds a few seconds; dropping the cache needs root, so it was not measured.
- Container `--memory 12g` was never hit: peak cgroup was 10.5 GB for vLLM at most (that includes page cache).

## Suggested engine templates for AI 04

| engine | image | formats | GPU | port | ready | model arg | stop grace |
|---|---|---|---|---|---|---|---|
| llama.cpp CPU | `ghcr.io/ggml-org/llama.cpp:server` | gguf | no | 8080 | `GET /health` | `-m <path>` | 10 s |
| llama.cpp CUDA | `ghcr.io/ggml-org/llama.cpp:server-cuda` | gguf | yes (CDI) | 8080 | `GET /health` | `-m <path> -ngl 99` | 10 s |
| llama.cpp Vulkan | `ghcr.io/ggml-org/llama.cpp:server-vulkan` | gguf | AMD/Intel `/dev/dri`, not NVIDIA | 8080 | `GET /health` | `-m <path> -ngl 99` | 10 s |
| Ollama | `docker.io/ollama/ollama` (pin a tag) | gguf (import) or own pull | optional (CDI) | 11434 | `/api/generate` empty prompt + `/api/ps` | create/pull via HTTP API | 10 s |
| vLLM | `docker.io/vllm/vllm-openai:v0.34.0-x86_64` | safetensors | yes (CDI) | 8000 | `GET /health` | `--model <dir> --served-model-name <n>` | 10 s |
| colibri | build `deploy/engines/colibri/Containerfile` | gguf (import) | optional (`COLI_CUDA=1`) | 8000 | `GET /health` | `COLI_GGUF=<path>` env | 10 s |

Ollama's `latest` tag was used for the measurements; pin an explicit one for the template (the tags page showed `0.40.17-rc1` as newest).

## Not done

- No true cold-disk start, no long-context or concurrent-request runs, one model per engine.
- Vulkan on colibri was not built; llama-vulkan was measured on the iGPU only.
- `:server-cuda13` was not needed and was not pulled.
