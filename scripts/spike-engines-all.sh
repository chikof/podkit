#!/usr/bin/env bash
# Re-measures every engine variant with the spike's test models and rebuilds
# docs/spikes/engines.json from the raw per-variant output (docs/spikes/raw/).
# Runs ONE container at a time, sequentially. ~15 minutes with warm images, so
# run it in the background. `--aggregate` only rebuilds engines.json.
set -uo pipefail
cd "$(dirname "$0")/.."
M=${MODELS:-$HOME/.cache/podkit-spikes/models}
RAW=docs/spikes/raw
mkdir -p "$RAW"
run() { # engine model [variant]
	local tag=${3:-$1}
	echo "== $tag" >&2
	scripts/spike-engine.sh "$1" "$2" >"$RAW/$tag.json" 2>"$RAW/$tag.err" || true
}
if [[ ${1:-} != --aggregate ]]; then
	run llama-cpu "$M/Qwen3-4B-Q4_K_M.gguf"
	run llama-cuda "$M/Qwen3-4B-Q4_K_M.gguf"
	run llama-vulkan "$M/Qwen3-4B-Q4_K_M.gguf"
	run ollama-cpu "$M/Qwen3-4B-Q4_K_M.gguf"
	run ollama-cuda "$M/Qwen3-4B-Q4_K_M.gguf"
	run ollama-cuda qwen3:4b ollama-cuda-pull
	run vllm "$M/Qwen3-1.7B"
	run colibri-cpu "$M/OLMoE-1B-7B-0125-Instruct-Q4_K_S.gguf"
	run colibri-cuda "$M/OLMoE-1B-7B-0125-Instruct-Q4_K_S.gguf"
fi

WEIGHTS='{
 "qwen3-4b-gguf": {"hf_repo":"Qwen/Qwen3-4B-GGUF","file":"Qwen3-4B-Q4_K_M.gguf","bytes":2497280256},
 "qwen3-1.7b-safetensors": {"hf_repo":"Qwen/Qwen3-1.7B","files":["model-00001-of-00002.safetensors","model-00002-of-00002.safetensors"],"bytes":4063515592},
 "olmoe-gguf": {"hf_repo":"allenai/OLMoE-1B-7B-0125-Instruct-GGUF","file":"OLMoE-1B-7B-0125-Instruct-Q4_K_S.gguf","bytes":3963688960},
 "ollama-qwen3-4b": {"ollama_tag":"qwen3:4b","note":"pulled from the Ollama registry by the engine itself; about 2.5 GB, Q4_K_M"}
}'
for f in "$RAW"/*.json; do
	v=$(basename "$f" .json)
	case $v in
	llama-* | ollama-cpu | ollama-cuda) w=qwen3-4b-gguf ;;
	ollama-cuda-pull) w=ollama-qwen3-4b ;;
	vllm) w=qwen3-1.7b-safetensors ;;
	colibri-*) w=olmoe-gguf ;;
	esac
	jq -c --arg v "$v" --arg w "$w" --argjson W "$WEIGHTS" '. + {variant:$v, weights:$W[$w]}' "$f"
done | jq -s '.' >docs/spikes/engines.json
jq -r '.[]|[.variant,.openai_chat,.decode_tok_s,.prompt_tok_s]|@tsv' docs/spikes/engines.json
