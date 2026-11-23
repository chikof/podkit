#!/usr/bin/env bash
# Measures podkit's idle footprint and, with --check, enforces the budgets.
#
#   DATABASE_URL=postgres://… scripts/footprint.sh [--check] [--idle SECS] [--bin DIR] [--out FILE]
#
# Starts the release `server` and `podkit-agent` in a private data dir (no Podman
# needed: the agent only needs its socket path to exist), waits for them to be
# ready, then records for each process:
#   - RSS (VmRSS from /proc/<pid>/status) right after start, and after SECS idle
#   - peak RSS (VmHWM), threads, open socket fds, CPU % over the idle window
#   - disk used by the data dir
# plus the Postgres connections the server holds (when `psql` is available).
#
# --check fails (exit 1) when an idle RSS is over its budget. The budget of each
# process is min(cap, baseline + 20 %), both read from scripts/footprint.budget:
# the cap is the card's promise, the baseline the last measured value, so a
# regression is caught before it reaches the cap.
#
# --idle SECS  how long to leave the stack alone before the second sample (default
#              30 for --check, 600 for a full report: the "10 minutes of no traffic").
# --bin DIR    where the binaries are (default: target/release). Build them with
#              `cargo build --release -p server -p agent`.
# --out FILE   also write the numbers as KEY=VALUE lines.
# Environment: PODKIT_PODMAN_SOCKET (a real Podman socket for the agent), PODKIT_FOOTPRINT_BUDGET (another budget file).
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
bin=${PODKIT_BIN_DIR:-$root/target/release}
check=0
idle=""
out=""
while (($#)); do
	case $1 in
	--check) check=1 ;;
	--idle) idle=$2; shift ;;
	--bin) bin=$2; shift ;;
	--out) out=$2; shift ;;
	-h | --help) sed -n '2,23p' "$0"; exit 0 ;;
	*) echo "footprint: unknown argument $1" >&2; exit 2 ;;
	esac
	shift
done
idle=${idle:-$((check ? 30 : 600))}

: "${DATABASE_URL:?set DATABASE_URL to an empty Postgres database}"
for b in server podkit-agent; do
	[[ -x $bin/$b ]] || { echo "footprint: $bin/$b is missing; run cargo build --release -p server -p agent" >&2; exit 2; }
done

tmp=$(mktemp -d "${TMPDIR:-/tmp}/pk-footprint.XXXXXX")
pids=()
cleanup() {
	for p in "${pids[@]}"; do kill "$p" 2>/dev/null || true; done
	for p in "${pids[@]}"; do wait "$p" 2>/dev/null || true; done
	rm -rf "$tmp"
}
trap cleanup EXIT

# An age identity is bech32("age-secret-key-", 32 random bytes), upper-cased.
age_key=$(python3 - <<'PY'
import os
C = "qpzry9x8gf2tvdw0s3jn54khce6mua7l"
def pm(v):
    g = [0x3b6a57b2, 0x26508e6d, 0x1ea119fa, 0x3d4233dd, 0x2a1462b3]
    c = 1
    for x in v:
        b = c >> 25
        c = (c & 0x1ffffff) << 5 ^ x
        for i in range(5):
            c ^= g[i] if (b >> i) & 1 else 0
    return c
hrp = "age-secret-key-"
data = []
acc = bits = 0
for byte in os.urandom(32):
    acc = acc << 8 | byte
    bits += 8
    while bits >= 5:
        bits -= 5
        data.append(acc >> bits & 31)
if bits:
    data.append(acc << (5 - bits) & 31)
exp = [ord(c) >> 5 for c in hrp] + [0] + [ord(c) & 31 for c in hrp]
mod = pm(exp + data + [0] * 6) ^ 1
data += [(mod >> 5 * (5 - i)) & 31 for i in range(6)]
print((hrp + "1" + "".join(C[d] for d in data)).upper())
PY
)
port=$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1",0)); print(s.getsockname()[1])')

mkdir -p "$tmp/data" "$tmp/agent" "$tmp/config"
chmod 700 "$tmp/agent"
export XDG_CONFIG_HOME=$tmp/config HOME=$tmp
export DATABASE_URL AGE_SECRET_KEY=$age_key HOST=127.0.0.1 PORT=$port
export DATA_DIR=$tmp/data LOCAL_AGENT_SOCKET=$tmp/agent.sock AGENT_BINARIES_DIR=$bin
export RUST_LOG=${RUST_LOG:-warn}

# The agent refuses to start when its Podman socket path does not exist. Without a
# real one (PODKIT_PODMAN_SOCKET, e.g. $XDG_RUNTIME_DIR/podman/podman.sock) a stub
# socket that accepts and hangs up stands in, so CI needs no Podman.
podman_sock=${PODKIT_PODMAN_SOCKET:-$tmp/podman.sock}
if [[ -z ${PODKIT_PODMAN_SOCKET:-} ]]; then
	python3 - "$podman_sock" <<'PY' &
import os, socket, sys
s = socket.socket(socket.AF_UNIX)
s.bind(sys.argv[1])
s.listen(8)
while True:
    c, _ = s.accept()
    c.close()
PY
	pids+=("$!")
	for _ in $(seq 1 50); do [[ -S $podman_sock ]] && break; sleep 0.1; done
fi
"$bin/podkit-agent" --data-dir "$tmp/agent" --podman-socket "$podman_sock" --socket "$tmp/agent.sock" >"$tmp/agent.log" 2>&1 &
agent=$!
pids+=("$agent")
"$bin/server" >"$tmp/server.log" 2>&1 &
server=$!
pids+=("$server")

for _ in $(seq 1 120); do
	[[ -S $tmp/agent.sock ]] && python3 -c "import socket,sys; socket.create_connection(('127.0.0.1',$port),1)" 2>/dev/null && break
	kill -0 "$server" 2>/dev/null || { cat "$tmp/server.log" >&2; echo "footprint: the server exited" >&2; exit 1; }
	kill -0 "$agent" 2>/dev/null || { cat "$tmp/agent.log" >&2; echo "footprint: the agent exited" >&2; exit 1; }
	sleep 0.5
done
[[ -S $tmp/agent.sock ]] || { echo "footprint: not ready after 60 s" >&2; exit 1; }
sleep 5 # startup work (migrations, first sweeps) settles

status() { awk -v k="$2:" '$1 == k { print $2 }' "/proc/$1/status"; }
ticks() { awk '{ print $14 + $15 }' "/proc/$1/stat"; }
socks() { { ls -l "/proc/$1/fd" 2>/dev/null || true; } | grep -c 'socket:' || true; }

declare -A after_start idle_rss peak thr fds cpu
for n in server agent; do
	p=${!n}
	after_start[$n]=$(status "$p" VmRSS)
done
t0_server=$(ticks "$server")
t0_agent=$(ticks "$agent")
echo "footprint: idle for ${idle}s…" >&2
sleep "$idle"
hz=$(getconf CLK_TCK)
for n in server agent; do
	p=${!n}
	kill -0 "$p" 2>/dev/null || { echo "footprint: $n died while idle" >&2; tail -5 "$tmp/$n.log" >&2; exit 1; }
	idle_rss[$n]=$(status "$p" VmRSS)
	peak[$n]=$(status "$p" VmHWM)
	thr[$n]=$(status "$p" Threads)
	fds[$n]=$(socks "$p")
done
cpu[server]=$(awk -v a="$t0_server" -v b="$(ticks "$server")" -v hz="$hz" -v s="$idle" 'BEGIN { printf "%.2f", (b - a) / hz / s * 100 }')
cpu[agent]=$(awk -v a="$t0_agent" -v b="$(ticks "$agent")" -v hz="$hz" -v s="$idle" 'BEGIN { printf "%.2f", (b - a) / hz / s * 100 }')
disk_kb=$(du -sk "$tmp/data" "$tmp/agent" | awk '{ s += $1 } END { print s }')
bins_kb=$(du -Lsk "$bin/server" "$bin/podkit-agent" | awk '{ s += $1 } END { print s }')
pg_conns=n/a
if command -v psql >/dev/null 2>&1; then
	pg_conns=$(psql "$DATABASE_URL" -Atc "select count(*) from pg_stat_activity where datname = current_database() and pid <> pg_backend_pid()" 2>/dev/null || echo n/a)
fi

mb() { awk -v k="$1" 'BEGIN { printf "%.1f", k / 1024 }'; }
printf '\n%-8s %12s %12s %10s %8s %8s %8s\n' process 'start RSS' "idle RSS" 'peak RSS' threads sockets 'CPU %'
for n in server agent; do
	printf '%-8s %9s MB %9s MB %7s MB %8s %8s %8s\n' "$n" "$(mb "${after_start[$n]}")" "$(mb "${idle_rss[$n]}")" "$(mb "${peak[$n]}")" "${thr[$n]}" "${fds[$n]}" "${cpu[$n]}"
done
printf '\nidle window %ss · data dirs %s KB · binaries %s KB · Postgres connections held by the server: %s\n' "$idle" "$disk_kb" "$bins_kb" "$pg_conns"

if [[ -n $out ]]; then
	{
		for n in server agent; do
			echo "${n}_start_kb=${after_start[$n]}"
			echo "${n}_idle_kb=${idle_rss[$n]}"
			echo "${n}_peak_kb=${peak[$n]}"
			echo "${n}_threads=${thr[$n]}"
			echo "${n}_sockets=${fds[$n]}"
			echo "${n}_cpu_pct=${cpu[$n]}"
		done
		echo "idle_secs=$idle"
		echo "data_kb=$disk_kb"
		echo "binaries_kb=$bins_kb"
		echo "pg_connections=$pg_conns"
	} >"$out"
fi

((check)) || exit 0

# shellcheck source=scripts/footprint.budget
source "${PODKIT_FOOTPRINT_BUDGET:-$root/scripts/footprint.budget}"
fail=0
for n in server agent; do
	cap_var=${n}_cap_kb
	base_var=${n}_baseline_kb
	cap=${!cap_var}
	base=${!base_var}
	limit=$((base * 120 / 100))
	((limit < cap)) || limit=$cap
	if ((idle_rss[$n] > limit)); then
		printf 'FAIL %s: idle RSS %s MB is over the budget of %s MB (cap %s MB, baseline %s MB + 20%%)\n' "$n" "$(mb "${idle_rss[$n]}")" "$(mb "$limit")" "$(mb "$cap")" "$(mb "$base")"
		fail=1
	else
		printf 'ok   %s: idle RSS %s MB <= %s MB\n' "$n" "$(mb "${idle_rss[$n]}")" "$(mb "$limit")"
	fi
done
exit "$fail"
