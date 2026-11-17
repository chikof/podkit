#!/usr/bin/env bash
# spike-engine.sh <engine> <model-path-or-repo>
#
# Runs ONE inference engine as a rootless Podman container with the spike's
# hardware caps, waits until the model is loaded, measures it, prints one JSON
# object on stdout and removes the container. Logs go to stderr.
#
# Engines:
#   llama-cpu llama-cuda llama-cuda13 llama-vulkan   model: a .gguf file
#   ollama-cpu ollama-cuda                           model: a .gguf file (import) or a tag (ollama pull)
#   vllm                                             model: a directory of safetensors (HF layout)
#   colibri-cpu colibri-vulkan colibri-cuda          model: a .gguf file (OLMoE family)
#
# Needs: bash 5, podman, curl, jq, awk. GPU engines also need nvidia-smi on the
# host and the CDI spec (nvidia.com/gpu=all), see docs/spikes/gpu-cdi.md.
#
# Env overrides: IMAGE, HOST_PORT, MEM_LIMIT (12g), CTX (8192, vllm 4096),
# THREADS (8), READY_TIMEOUT (900), KEEP=1 (do not remove the container),
# LLAMA_EXTRA / VLLM_EXTRA (extra launch args).
set -uo pipefail
export LC_ALL=C

if [[ $# -ne 2 ]]; then
	sed -n '2,15p' "$0" >&2
	exit 2
fi
ENGINE=$1
MODEL=$2
HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
REPO=$(cd "$HERE/.." && pwd)

for bin in podman curl jq awk; do
	command -v "$bin" >/dev/null || { echo "missing $bin" >&2; exit 2; }
done

MEM_LIMIT=${MEM_LIMIT:-12g}
CTX=${CTX:-8192}
THREADS=${THREADS:-8}
READY_TIMEOUT=${READY_TIMEOUT:-900}
HOST_PORT=${HOST_PORT:-$((18000 + RANDOM % 1000))}
CT="pk-spike-$ENGINE"
WORK=$(mktemp -d "${TMPDIR:-/tmp}/spike-$ENGINE.XXXXXX")
BASE="http://127.0.0.1:$HOST_PORT"

log() { echo "[spike:$ENGINE] $*" >&2; }
now() { echo "$EPOCHREALTIME"; }
dur() { awk -v a="$1" -v b="$2" 'BEGIN{printf "%.3f", b-a}'; }
RESULT='{}'
setf() { RESULT=$(jq -c --argjson v "$2" --arg k "$1" '. + {($k): $v}' <<<"$RESULT"); }
sets() { RESULT=$(jq -c --arg v "$2" --arg k "$1" '. + {($k): $v}' <<<"$RESULT"); }

SAMPLER_PID=""
cleanup() {
	[[ -n "$SAMPLER_PID" ]] && kill "$SAMPLER_PID" 2>/dev/null
	if [[ -z "${KEEP:-}" ]]; then
		podman rm -fv "$CT" >/dev/null 2>&1
	fi
	rm -rf "$WORK"
}
trap cleanup EXIT
trap 'exit 130' INT TERM

# One engine at a time, by the card's rules.
if [[ -n "$(podman ps -q --filter name='^pk-spike-')" ]]; then
	echo "another pk-spike-* container is running; refusing to overlap" >&2
	exit 3
fi
podman rm -fv "$CT" >/dev/null 2>&1

# ---------------------------------------------------------------- engine table
GPU=none        # none | cdi | dri
PORT_IN=8080
READY_PATH=/health
ENVS=()
ARGS=()
MOUNTS=()
ENTRY=()
SHM=()
FEED=mount      # how the model reaches the engine: mount | gguf-import | pull
FAMILY=${ENGINE%%-*}
AUTH=()

model_dir_file() { MDIR=$(dirname "$(realpath "$MODEL")"); MFILE=$(basename "$MODEL"); }

case "$ENGINE" in
llama-*)
	case "$ENGINE" in
	llama-cpu) IMAGE=${IMAGE:-ghcr.io/ggml-org/llama.cpp:server} ;;
	llama-cuda) IMAGE=${IMAGE:-ghcr.io/ggml-org/llama.cpp:server-cuda}; GPU=cdi ;;
	llama-cuda13) IMAGE=${IMAGE:-ghcr.io/ggml-org/llama.cpp:server-cuda13}; GPU=cdi ;;
	llama-vulkan) IMAGE=${IMAGE:-ghcr.io/ggml-org/llama.cpp:server-vulkan}; GPU=dri ;;
	*) echo "unknown engine $ENGINE" >&2; exit 2 ;;
	esac
	[[ -f "$MODEL" ]] || { echo "model must be a .gguf file" >&2; exit 2; }
	model_dir_file
	MOUNTS=(-v "$MDIR:/models:ro")
	ARGS=(-m "/models/$MFILE" --host 0.0.0.0 --port 8080 -c "$CTX" -np 1 -t "$THREADS" --no-webui)
	if [[ $GPU != none ]]; then ARGS+=(-ngl 99); else ARGS+=(-ngl 0); fi
	# shellcheck disable=SC2206
	ARGS+=(${LLAMA_EXTRA:-})
	PORT_IN=8080
	READY_PATH=/health
	;;
ollama-*)
	IMAGE=${IMAGE:-docker.io/ollama/ollama:latest}
	[[ $ENGINE == ollama-cuda ]] && GPU=cdi
	PORT_IN=11434
	READY_PATH=/
	ENVS=(-e OLLAMA_HOST=0.0.0.0:11434 -e OLLAMA_CONTEXT_LENGTH="$CTX" -e OLLAMA_KEEP_ALIVE=-1 -e OLLAMA_NUM_PARALLEL=1)
	ARGS=(serve)
	if [[ -f "$MODEL" ]]; then
		model_dir_file
		MOUNTS=(-v "$MDIR:/models:ro")
		FEED=gguf-import
	else
		FEED=pull
	fi
	;;
vllm)
	IMAGE=${IMAGE:-docker.io/vllm/vllm-openai:v0.34.0-x86_64}
	GPU=cdi
	PORT_IN=8000
	READY_PATH=/health
	CTX=${VLLM_CTX:-4096}
	[[ -d "$MODEL" ]] || { echo "model must be a directory of safetensors" >&2; exit 2; }
	MOUNTS=(-v "$(realpath "$MODEL"):/model:ro")
	SHM=(--shm-size 2g)
	ENVS=(-e HF_HUB_OFFLINE=1)
	ARGS=(--model /model --served-model-name spike --max-model-len "$CTX" --gpu-memory-utilization 0.75
		--host 0.0.0.0 --port 8000)
	# shellcheck disable=SC2206
	ARGS+=(${VLLM_EXTRA:-})
	;;
colibri-*)
	BACKEND=${ENGINE#colibri-}
	IMAGE=${IMAGE:-localhost/podkit-colibri:$BACKEND}
	[[ $BACKEND != cpu ]] && GPU=cdi
	[[ $BACKEND == vulkan ]] && GPU=cdi
	[[ -f "$MODEL" ]] || { echo "model must be a .gguf file" >&2; exit 2; }
	model_dir_file
	MOUNTS=(-v "$MDIR:/models:ro")
	PORT_IN=8000
	READY_PATH=/health
	FEED=gguf-import
	ENVS=(-e COLI_GGUF="/models/$MFILE" -e OMP_NUM_THREADS="$THREADS" -e COLI_API_KEY=spike)
	AUTH=(-H "authorization: Bearer spike")
	[[ $BACKEND == cuda ]] && ENVS+=(-e COLI_CUDA=1)
	[[ $BACKEND == vulkan ]] && ENVS+=(-e COLI_VULKAN=1)
	ARGS=()
	;;
*)
	echo "unknown engine: $ENGINE" >&2
	exit 2
	;;
esac

GPUARGS=()
case $GPU in
cdi) GPUARGS=(--device nvidia.com/gpu=all) ;;
dri) GPUARGS=(--device /dev/dri) ;;
esac

if ! podman image exists "$IMAGE"; then
	log "pulling $IMAGE"
	podman pull -q "$IMAGE" >/dev/null || { jq -n --arg e "$ENGINE" --arg i "$IMAGE" '{engine:$e,image:$i,ready:false,openai_chat:false,error:"image pull failed"}'; exit 1; }
fi
IMG_BYTES=$(podman image inspect -f '{{.Size}}' "$IMAGE")
IMG_DIGEST=$(podman image inspect -f '{{.Digest}}' "$IMAGE")

# --------------------------------------------------------------------- sampler
VRAM_BASE=0
if command -v nvidia-smi >/dev/null; then
	VRAM_BASE=$(nvidia-smi --query-gpu=memory.used --format=csv,noheader,nounits | head -1 | tr -d ' ')
fi
sampler() {
	while :; do
		rss=$(podman top "$CT" rss 2>/dev/null | awk 'NR>1{s+=$1} END{print s+0}')
		vr=0 vp=0
		if command -v nvidia-smi >/dev/null; then
			vr=$(nvidia-smi --query-gpu=memory.used --format=csv,noheader,nounits | head -1 | tr -d ' ')
			hp=$(podman top "$CT" hpid 2>/dev/null | awk 'NR>1{print $1}' | paste -sd' ')
			if [[ -n "$hp" ]]; then
				vp=$(nvidia-smi --query-compute-apps=pid,used_memory --format=csv,noheader,nounits 2>/dev/null |
					awk -F', *' -v pids="$hp" 'BEGIN{n=split(pids,a," ");for(i=1;i<=n;i++)m[a[i]]=1} ($1 in m){s+=$2} END{print s+0}')
			fi
		fi
		echo "$rss $vr $vp" >>"$WORK/samples"
		sleep 0.5
	done
}

# ----------------------------------------------------------------------- start
log "image $IMAGE ($((IMG_BYTES / 1000000)) MB), feed=$FEED, port $HOST_PORT"
T_RUN=$(now)
podman run -d --name "$CT" --memory "$MEM_LIMIT" --memory-swap "$MEM_LIMIT" \
	-p "127.0.0.1:$HOST_PORT:$PORT_IN" "${GPUARGS[@]}" "${SHM[@]}" "${MOUNTS[@]}" "${ENVS[@]}" \
	"${ENTRY[@]}" "$IMAGE" "${ARGS[@]}" >"$WORK/cid" 2>"$WORK/run.err" || {
	jq -n --arg e "$ENGINE" --arg i "$IMAGE" --arg m "$(cat "$WORK/run.err")" '{engine:$e,image:$i,ready:false,openai_chat:false,error:$m}'
	exit 1
}
sampler &
SAMPLER_PID=$!

ready_fail() {
	local why=$1
	podman logs --tail 40 "$CT" >"$WORK/tail" 2>&1
	jq -n --arg e "$ENGINE" --arg i "$IMAGE" --arg w "$why" --rawfile l "$WORK/tail" \
		'{engine:$e,image:$i,ready:false,openai_chat:false,error:$w,log_tail:$l}'
	exit 1
}

wait_http() { # path timeout -> sets WAIT_T (seconds from T_RUN)
	local path=$1 limit=$2 code
	while :; do
		code=$(curl -s "${AUTH[@]}" -o /dev/null -w "%{http_code}" --max-time 3 "$BASE$path" 2>/dev/null)
		[[ $code == 200 ]] && { WAIT_T=$(dur "$T_RUN" "$(now)"); return 0; }
		[[ $(podman inspect -f '{{.State.Running}}' "$CT" 2>/dev/null) == true ]] || return 2
		(($(awk -v a="$T_RUN" -v b="$(now)" -v l="$limit" 'BEGIN{print (b-a>l)}'))) && return 1
		sleep 0.5
	done
}

wait_http "$READY_PATH" "$READY_TIMEOUT" || ready_fail "not ready on $READY_PATH (exited or timed out)"
SERVER_UP_S=$WAIT_T
MODEL_NAME=""

if [[ $FAMILY == ollama ]]; then
	# server is up; the model still has to be imported/pulled and loaded
	MODEL_NAME=spike
	T1=$(now)
	if [[ $FEED == gguf-import ]]; then
		podman exec "$CT" sh -c "printf 'FROM /models/%s\n' '$MFILE' >/tmp/Modelfile && ollama create spike -f /tmp/Modelfile" >"$WORK/create.log" 2>&1 ||
			ready_fail "ollama create failed: $(tail -3 "$WORK/create.log")"
	else
		MODEL_NAME=$MODEL
		podman exec "$CT" ollama pull "$MODEL" >"$WORK/create.log" 2>&1 || ready_fail "ollama pull failed"
	fi
	T2=$(now)
	setf ollama_import_s "$(dur "$T1" "$T2")"
	curl -s --max-time "$READY_TIMEOUT" "$BASE/api/generate" -d "{\"model\":\"$MODEL_NAME\",\"keep_alive\":-1}" >"$WORK/load.json" || ready_fail "ollama load failed"
	T3=$(now)
	setf ollama_load_s "$(dur "$T2" "$T3")"
	setf ollama_ps "$(curl -s "$BASE/api/ps" | jq -c '[.models[]?|{name,size,size_vram,context_length}]')"
	LOADED_S=$(dur "$T_RUN" "$T3")
else
	LOADED_S=$SERVER_UP_S
fi
setf cold_start_s "$LOADED_S"
setf server_up_s "$SERVER_UP_S"
log "ready in ${LOADED_S}s"

curl -s "${AUTH[@]}" --max-time 10 "$BASE/v1/models" >"$WORK/models.json"
MCODE=$(curl -s "${AUTH[@]}" -o /dev/null -w "%{http_code}" --max-time 10 "$BASE/v1/models")
OPENAI_MODELS=false
if [[ $MCODE == 200 ]] && jq -e '.data|length>0' "$WORK/models.json" >/dev/null 2>&1; then OPENAI_MODELS=true; fi
[[ -z $MODEL_NAME ]] && MODEL_NAME=$(jq -r '.data[0].id // "default"' "$WORK/models.json")
setf openai_models "$OPENAI_MODELS"
sets model_id "$MODEL_NAME"
log "model id: $MODEL_NAME"

# ------------------------------------------------------------- chat (non-stream)
STORY='Write a long, detailed story about a fox who learns to read. Do not stop early.'
chat_body() { # prompt max_tokens stream
	jq -nc --arg m "$MODEL_NAME" --arg p "$1" --argjson n "$2" --argjson s "$3" \
		'{model:$m,messages:[{role:"user",content:$p}],max_tokens:$n,temperature:0,stream:$s} +
		 (if $s then {stream_options:{include_usage:true}} else {} end)'
}
T0=$(now)
CODE=$(curl -s "${AUTH[@]}" -o "$WORK/chat.json" -w '%{http_code}' --max-time 300 "$BASE/v1/chat/completions" \
	-H 'content-type: application/json' -d "$(chat_body "$STORY" 256 false)")
setf first_chat_wall_s "$(dur "$T0" "$(now)")"
setf cold_to_first_answer_s "$(awk -v a="$LOADED_S" -v b="$(dur "$T0" "$(now)")" 'BEGIN{printf "%.3f", a+b}')"
OPENAI_CHAT=false
if [[ $CODE == 200 ]] && jq -e '((.choices[0].message|((.content // "")+(.reasoning_content // "")+(.reasoning // "")))|length)>0' "$WORK/chat.json" >/dev/null 2>&1; then
	OPENAI_CHAT=true
fi
setf openai_chat "$OPENAI_CHAT"
setf chat_http_code "$CODE"
setf usage_nonstream "$(jq -c '.usage // null' "$WORK/chat.json" 2>/dev/null || echo null)"
setf usage_nonstream_has_fields "$(jq -c '(.usage|type=="object") and (.usage.prompt_tokens!=null) and (.usage.completion_tokens!=null)' "$WORK/chat.json" 2>/dev/null || echo false)"
# engine-native timing blocks, kept as extra evidence
setf native_timings "$(jq -c '.timings // null' "$WORK/chat.json" 2>/dev/null || echo null)"
if [[ $OPENAI_CHAT != true ]]; then log "chat failed: $(head -c 300 "$WORK/chat.json")"; fi

# ------------------------------------------------------------------- streaming
stream_chat() { # body outfile ; writes "ts<TAB>json" lines, and DONE flag to outfile.done
	local out=$2 line payload
	: >"$out"
	rm -f "$out.done"
	curl -sN "${AUTH[@]}" --max-time 300 "$BASE/v1/chat/completions" -H 'content-type: application/json' -d "$1" 2>/dev/null |
		while IFS= read -r line; do
			line=${line%$'\r'}
			[[ $line == data:* ]] || continue
			payload=${line#data:}
			payload=${payload# }
			if [[ $payload == '[DONE]' ]]; then : >"$out.done"; continue; fi
			printf '%s\t%s\n' "$EPOCHREALTIME" "$payload" >>"$out"
		done
}
STREAM_JQ='
[split("\n")[]|select(length>0)|split("\t")|{t:(.[0]|tonumber), j:(.[1]|fromjson? // null)}|select(.j!=null)] as $c
| def txt: ((.j.choices // [])[0].delta // {}) | ((.content // "") + (.reasoning_content // "") + (.reasoning // ""));
  ($c|map(select(txt|length>0))) as $tok
| ($c|map(select(.j.usage!=null))) as $u
| {chunks:($c|length), token_chunks:($tok|length),
   first_tok:($tok[0].t // null), last_tok:($tok[-1].t // null),
   first_any:($c[0].t // null),
   usage:($u[0].j.usage // null),
   usage_chunk_index:(if ($u|length)>0 then ($c|map(.j.usage!=null)|index(true)) else null end),
   usage_chunk_is_last:(if ($u|length)>0 then ($c[-1].j.usage!=null) else null end),
   usage_chunk_choices_empty:(if ($u|length)>0 then (($u[0].j.choices // [])|length==0) else null end),
   usage_on_every_chunk:(($u|length)==($c|length) and ($c|length)>1),
   finish:(($c|map(((.j.choices // [])[0].finish_reason)|select(.!=null))|.[-1]) // null),
   native_timings:($c|map(.j.timings|select(.!=null))|.[-1] // null)}'

T0=$(now)
stream_chat "$(chat_body "$STORY" 256 true)" "$WORK/s1"
T1=$(now)
SJ=$(jq -Rrs "$STREAM_JQ" "$WORK/s1" 2>/dev/null || echo '{}')
[[ -f "$WORK/s1.done" ]] && DONE=true || DONE=false
setf stream_wall_s "$(dur "$T0" "$T1")"
setf openai_stream "$(jq -c --argjson d "$DONE" '(.token_chunks // 0) >= 2 and $d' <<<"$SJ")"
setf stream_include_usage "$(jq -c '.usage != null' <<<"$SJ")"
setf usage_stream "$(jq -c '.usage' <<<"$SJ")"
setf usage_stream_location "$(jq -c 'if .usage==null then "absent"
	elif .usage_on_every_chunk then "every chunk"
	elif .usage_chunk_choices_empty then "final chunk with choices=[] (OpenAI style)"
	elif .usage_chunk_is_last then "last chunk, together with choices" else "a middle chunk" end' <<<"$SJ")"
setf stream_finish_reason "$(jq -c '.finish' <<<"$SJ")"
# client side decode speed: tokens after the first one / time between first and last token chunk
CTOK=$(jq -r '.usage.completion_tokens // .token_chunks // 0' <<<"$SJ")
setf completion_tokens "$CTOK"
setf decode_tok_s "$(jq -c --argjson n "$CTOK" 'if .first_tok!=null and .last_tok>.first_tok and $n>1 then (($n-1)/(.last_tok-.first_tok)*100|round/100) else null end' <<<"$SJ")"
setf ttft_s "$(jq -c --argjson t0 "$T0" 'if .first_tok!=null then ((.first_tok-$t0)*1000|round/1000) else null end' <<<"$SJ")"
setf stream_native_timings "$(jq -c '.native_timings' <<<"$SJ")"

# prompt processing: ~1.5k token prompt with a nonce so no prompt cache can help, 1 token out
NONCE="session-$RANDOM-$RANDOM"
LONG="$NONCE. $(for i in $(seq 1 90); do printf 'The quick brown fox jumps over the lazy dog near the river bank, number %d. ' "$i"; done) Reply with one word."
T0=$(now)
stream_chat "$(chat_body "$LONG" 1 true)" "$WORK/s2"
PJ=$(jq -Rrs "$STREAM_JQ" "$WORK/s2" 2>/dev/null || echo '{}')
PT=$(jq -r '.usage.prompt_tokens // 0' <<<"$PJ")
setf prompt_tokens "$PT"
setf prompt_tok_s "$(jq -c --argjson t0 "$T0" --argjson n "$PT" '(.first_tok // .first_any) as $f | if $f!=null and $n>0 then ($n/($f-$t0)*10|round/10) else null end' <<<"$PJ")"

# ---------------------------------------------------------------- Anthropic API
AB=$(jq -nc --arg m "$MODEL_NAME" '{model:$m,max_tokens:64,messages:[{role:"user",content:"Say hello in five words."}]}')
ACODE=$(curl -s "${AUTH[@]}" -o "$WORK/anth.json" -w '%{http_code}' --max-time 120 "$BASE/v1/messages" \
	-H 'content-type: application/json' -H 'x-api-key: spike' -H 'anthropic-version: 2023-06-01' -d "$AB")
ANTH=false
if [[ $ACODE == 200 ]] && jq -e '.type=="message" and (.content|length>0)' "$WORK/anth.json" >/dev/null 2>&1; then ANTH=true; fi
setf anthropic_messages "$ANTH"
setf anthropic_http_code "$ACODE"
setf anthropic_usage "$(jq -c '.usage // null' "$WORK/anth.json" 2>/dev/null || echo null)"
if [[ $ANTH != true ]]; then sets anthropic_error "$(head -c 200 "$WORK/anth.json" | tr -d '\n')"; fi
ASB=$(jq -nc --arg m "$MODEL_NAME" '{model:$m,max_tokens:64,stream:true,messages:[{role:"user",content:"Say hello in five words."}]}')
curl -sN "${AUTH[@]}" --max-time 120 "$BASE/v1/messages" -H 'content-type: application/json' -H 'x-api-key: spike' \
	-H 'anthropic-version: 2023-06-01' -d "$ASB" >"$WORK/anth.sse" 2>/dev/null
ASTREAM=false
grep -q '^event: message_stop' "$WORK/anth.sse" && grep -q '^event: content_block_delta' "$WORK/anth.sse" && ASTREAM=true
setf anthropic_stream "$ASTREAM"
setf anthropic_stream_usage "$(grep '^data:' "$WORK/anth.sse" | sed 's/^data: *//' | jq -cs '[ .[] | select(.type=="message_start" or .type=="message_delta") | {event:.type, usage:(.usage // .message.usage)} ]' 2>/dev/null || echo null)"

# ------------------------------------------------------------------ memory/VRAM
sleep 1
kill "$SAMPLER_PID" 2>/dev/null
SAMPLER_PID=""
PEAK_RSS_KB=$(awk 'BEGIN{m=0} $1>m{m=$1} END{print m}' "$WORK/samples")
PEAK_VR=$(awk 'BEGIN{m=0} $2>m{m=$2} END{print m}' "$WORK/samples")
PEAK_VP=$(awk 'BEGIN{m=0} $3>m{m=$3} END{print m}' "$WORK/samples")
setf peak_rss_mb "$((PEAK_RSS_KB / 1024))"
CGPEAK=$(podman exec "$CT" cat /sys/fs/cgroup/memory.peak 2>/dev/null | tr -d '\r\n')
setf peak_cgroup_mb "$([[ $CGPEAK =~ ^[0-9]+$ ]] && echo $((CGPEAK / 1048576)) || echo null)"
setf vram_peak_mb "$([[ $GPU == none ]] && echo 0 || echo "$((PEAK_VR - VRAM_BASE))")"
setf vram_peak_container_mb "$([[ $GPU == none ]] && echo 0 || echo "$PEAK_VP")"
setf vram_baseline_mb "$VRAM_BASE"

# --------------------------------------------------------------------- SIGTERM
podman logs --tail 60 "$CT" >"$WORK/final.log" 2>&1
T0=$(now)
podman kill -s TERM "$CT" >/dev/null 2>&1
EXITED=false
for _ in $(seq 1 120); do
	if [[ $(podman inspect -f '{{.State.Running}}' "$CT" 2>/dev/null) != true ]]; then EXITED=true; break; fi
	sleep 0.25
done
T1=$(now)
if [[ $EXITED == true ]]; then
	setf sigterm_exit_s "$(dur "$T0" "$T1")"
	setf sigterm_exit_code "$(podman inspect -f '{{.State.ExitCode}}' "$CT")"
else
	setf sigterm_exit_s null
	setf sigterm_exit_code null
fi
setf sigterm_stopped "$EXITED"

# ------------------------------------------------------------------------ emit
jq -nc --arg engine "$ENGINE" --arg image "$IMAGE" --arg digest "$IMG_DIGEST" --argjson bytes "$IMG_BYTES" \
	--arg model "$MODEL" --arg feed "$FEED" --argjson port "$PORT_IN" --arg ready "$READY_PATH" \
	--arg gpu "$GPU" --arg mem "$MEM_LIMIT" --argjson ctx "$CTX" \
	--argjson args "$(printf "%s\n" ${ARGS[@]+"${ARGS[@]}"} | jq -R . | jq -cs "map(select(length>0))")" \
	--argjson r "$RESULT" \
	'{engine:$engine,image:$image,image_digest:$digest,image_size_mb:($bytes/1000000|round),
	  model:$model,model_feed:$feed,container_port:$port,launch_args:$args,
	  gpu:$gpu,mem_limit:$mem,context:$ctx,readiness_endpoint:$ready,ready:true} + $r'
