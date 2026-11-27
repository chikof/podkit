#!/usr/bin/env bash
# Measures podkit's idle footprint and, with --check, enforces the budgets.
#
#   DATABASE_URL=postgres://… scripts/footprint.sh [--check] [--idle SECS] [--bin DIR] [--out FILE]
#
# Starts the release `server` and `podkit-agent` in a private data dir (no Podman
# needed: the agent only needs its socket path to exist), waits for them to be
# ready, then records for each process:
#   - RSS (VmRSS from /proc/<pid>/status) right after start, and after SECS idle
#   - the anonymous part of the idle RSS (RssAnon: heap and stacks)
#   - peak RSS (VmHWM), threads, open socket fds, CPU % over the idle window
#   - disk used by the data dir, and the size of each binary
# plus the Postgres connections the server holds (when `psql` is available).
# Then it signs up, signs in and opens every page and API list the home page links to,
# and records RSS and anonymous RSS again: what the stack holds after use.
#
# --check fails (exit 1) on any of these, read from scripts/footprint.budget:
#   - RSS over the cap (the card's promise), idle or after use
#   - peak RSS over the cap plus the one transient the budget names (a password hash)
#   - anonymous RSS over its baseline + 20 %, idle or after use: memory the process holds
#   - binary size over its baseline + 10 %: code and dependencies
#   - a shared library that is not on the allowlist: a second crypto or TLS stack
# Total RSS has no baseline: most of it is the binary's own code, mapped, and how much of
# that is resident moves with the layout of the binary, not with what the code does.
#
# --idle SECS  how long to leave the stack alone before the second sample (default
#              30 for --check, 600 for a full report: the "10 minutes of no traffic").
# --bin DIR    where the binaries are (default: target/release). Build them with
#              `scripts/build-ordered.sh`.
# --no-load    stop at the idle sample (scripts/startup-order.sh profiles that part).
# --catalog DIR  a built template catalog (the `dist/` of podkit-templates) for the server to
#              download during the load pass, so "after use" is a server that holds one.
#              It is served from this machine: the numbers then do not depend on the
#              network or on what the published catalog has grown to.
# --out FILE   also write the numbers as KEY=VALUE lines.
# Environment: PODKIT_PODMAN_SOCKET (a real Podman socket for the agent), PODKIT_FOOTPRINT_BUDGET (another budget file).
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
bin=${PODKIT_BIN_DIR:-$root/target/release}
check=0
load=1
catalog=""
idle=""
out=""
while (($#)); do
	case $1 in
	--check) check=1 ;;
	--no-load) load=0 ;;
	--idle) idle=$2; shift ;;
	--bin) bin=$2; shift ;;
	--catalog) catalog=$2; shift ;;
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
if [[ -n $catalog ]]; then
	[[ -f $catalog/index.json ]] || { echo "footprint: $catalog has no index.json" >&2; exit 2; }
	# podkit never fetches from a loopback address, so the catalog is served on this machine's
	# own address, which the server is told it may reach.
	address=$(python3 -c 'import socket; s=socket.socket(socket.AF_INET, socket.SOCK_DGRAM); s.connect(("192.0.2.1", 9)); print(s.getsockname()[0])')
	catalog_port=$(python3 -c 'import socket,sys; s=socket.socket(); s.bind((sys.argv[1],0)); print(s.getsockname()[1])' "$address")
	python3 -m http.server "$catalog_port" --bind "$address" --directory "$catalog" >"$tmp/catalog.log" 2>&1 &
	pids+=($!)
	export TEMPLATE_CATALOG_URL=http://$address:$catalog_port OUTBOUND_PRIVATE=allow
fi
SETUP_TOKEN=$(python3 -c 'import secrets; print(secrets.token_hex(16))')
export SETUP_TOKEN

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

declare -A after_start idle_rss idle_anon bin_kb peak thr fds cpu
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
	idle_anon[$n]=$(status "$p" RssAnon)
	peak[$n]=$(status "$p" VmHWM)
	thr[$n]=$(status "$p" Threads)
	fds[$n]=$(socks "$p")
done
cpu[server]=$(awk -v a="$t0_server" -v b="$(ticks "$server")" -v hz="$hz" -v s="$idle" 'BEGIN { printf "%.2f", (b - a) / hz / s * 100 }')
cpu[agent]=$(awk -v a="$t0_agent" -v b="$(ticks "$agent")" -v hz="$hz" -v s="$idle" 'BEGIN { printf "%.2f", (b - a) / hz / s * 100 }')
disk_kb=$(du -sk "$tmp/data" "$tmp/agent" | awk '{ s += $1 } END { print s }')
bin_kb[server]=$(du -Lk "$bin/server" | awk '{ print $1 }')
bin_kb[agent]=$(du -Lk "$bin/podkit-agent" | awk '{ print $1 }')
bins_kb=$((bin_kb[server] + bin_kb[agent]))
pg_conns=n/a
if command -v psql >/dev/null 2>&1; then
	pg_conns=$(psql "$DATABASE_URL" -Atc "select count(*) from pg_stat_activity where datname = current_database() and pid <> pg_backend_pid()" 2>/dev/null || echo n/a)
fi

if ((load)); then
echo "footprint: load…" >&2
python3 - "http://127.0.0.1:$port" "$SETUP_TOKEN" "$catalog" >&2 <<'PY'
import http.cookiejar, json, re, sys, urllib.error, urllib.request

base, setup, catalog = sys.argv[1], sys.argv[2], sys.argv[3]
opener = urllib.request.build_opener(urllib.request.HTTPCookieProcessor(http.cookiejar.CookieJar()))

def call(path, body=None, headers=(), method=None):
    req = urllib.request.Request(base + path, data=json.dumps(body).encode() if body else None, method=method)
    for k, v in (("Content-Type", "application/json"),) * bool(body) + tuple(headers):
        req.add_header(k, v)
    try:
        with opener.open(req, timeout=30) as r:
            return r.status, r.read()
    except urllib.error.HTTPError as e:
        return e.code, e.read()

who = {"email": "footprint@example.com", "password": "correct horse battery staple"}
status, body = call("/api/v1/auth/register", {"name": "Footprint", "setup_token": setup, **who})
assert status == 201, f"register: {status} {body[:200]}"
status, body = call("/api/v1/auth/login", who)
assert status == 200, f"login: {status} {body[:200]}"
bearer = (("Authorization", "Bearer " + json.loads(body)["token"]),)
# The session cookie comes from the same Datastar POST the login page makes.
status, _ = call("/login", who, (("datastar-request", "true"), ("Origin", base)))
assert status == 200, f"/login: {status}"
status, body = call("/api/v1/teams", {"name": "Footprint", "slug": "footprint", "logo": ""}, bearer)
assert status == 200, f"create team: {status} {body[:200]}"
team = json.loads(body)["id"]
status, body = call(f"/api/v1/teams/{team}/projects", {"name": "Demo", "slug": "demo"}, bearer)
assert status < 300, f"create project: {status} {body[:200]}"

templates = 0
if catalog:
    status, body = call("/api/v1/templates/refresh", headers=bearer, method="POST")
    assert status == 200, f"refresh the catalog: {status} {body[:300]}"
    templates = json.loads(body)["templates"]
    assert templates > 0, "the catalog has no template"

seen, queue, bad = set(), ["/"], []
while queue and len(seen) < 200:
    path = queue.pop(0)
    if path in seen or "logout" in path:
        continue
    seen.add(path)
    status, html = call(path)
    if status >= 500:
        bad.append(f"{status} {path}")
    queue += re.findall(r'href="(/[^"#?]*)"', html.decode(errors="replace"))
lists = ["projects", "servers", "audit", "registries", "ssh-keys", "notifications", "uptime", "api-tokens"]
for path in ["/api/v1/catalog", "/api/v1/templates"] + [f"/api/v1/teams/{team}/{x}" for x in lists]:
    status, _ = call(path, headers=bearer)
    if status >= 500:
        bad.append(f"{status} {path}")
assert not bad, f"server errors: {bad}"
assert len(seen) > 10, f"the crawl found only {sorted(seen)}"
print(f"footprint: {len(seen)} pages and {len(lists) + 2} API lists, {templates} templates in the catalog")
PY
sleep 5
fi
declare -A load_rss load_anon
for n in server agent; do
	p=${!n}
	load_rss[$n]=$(status "$p" VmRSS)
	load_anon[$n]=$(status "$p" RssAnon)
	peak[$n]=$(status "$p" VmHWM)
done

mb() { awk -v k="$1" 'BEGIN { printf "%.1f", k / 1024 }'; }
printf '\n%-8s %12s %12s %12s %12s %12s %10s %8s %8s %8s\n' process 'start RSS' "idle RSS" 'idle anon' 'used RSS' 'used anon' 'peak RSS' threads sockets 'CPU %'
for n in server agent; do
	printf '%-8s %9s MB %9s MB %9s MB %9s MB %9s MB %7s MB %8s %8s %8s\n' "$n" "$(mb "${after_start[$n]}")" "$(mb "${idle_rss[$n]}")" "$(mb "${idle_anon[$n]}")" "$(mb "${load_rss[$n]}")" "$(mb "${load_anon[$n]}")" "$(mb "${peak[$n]}")" "${thr[$n]}" "${fds[$n]}" "${cpu[$n]}"
done
printf '\nidle window %ss · data dirs %s KB · binaries %s KB · Postgres connections held by the server: %s\n' "$idle" "$disk_kb" "$bins_kb" "$pg_conns"

if [[ -n $out ]]; then
	{
		for n in server agent; do
			echo "${n}_start_kb=${after_start[$n]}"
			echo "${n}_idle_kb=${idle_rss[$n]}"
			echo "${n}_idle_anon_kb=${idle_anon[$n]}"
			echo "${n}_used_kb=${load_rss[$n]}"
			echo "${n}_used_anon_kb=${load_anon[$n]}"
			echo "${n}_bin_kb=${bin_kb[$n]}"
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
# gate NAME WHAT VALUE_KB LIMIT_KB
gate() {
	if (($3 > $4)); then
		printf 'FAIL %s: %s %s MB is over the budget of %s MB\n' "$1" "$2" "$(mb "$3")" "$(mb "$4")"
		fail=1
	else
		printf 'ok   %s: %s %s MB <= %s MB\n' "$1" "$2" "$(mb "$3")" "$(mb "$4")"
	fi
}
declare -A file=([server]=server [agent]=podkit-agent)
for n in server agent; do
	cap=${n}_cap_kb anon=${n}_anon_baseline_kb used=${n}_used_anon_baseline_kb size=${n}_bin_baseline_kb
	gate "$n" 'idle RSS' "${idle_rss[$n]}" "${!cap}"
	gate "$n" 'anonymous idle RSS' "${idle_anon[$n]}" $((${!anon} * 120 / 100))
	gate "$n" 'RSS after use' "${load_rss[$n]}" "${!cap}"
	transient=${n}_transient_kb
	gate "$n" 'peak RSS' "${peak[$n]}" $((${!cap} + ${!transient}))
	gate "$n" 'anonymous RSS after use' "${load_anon[$n]}" $((${!used} * 120 / 100))
	gate "$n" 'binary' "${bin_kb[$n]}" $((${!size} * 110 / 100))
	# Only the server has one: its start-up code is laid out by scripts/build-ordered.sh.
	mapped=${n}_idle_file_baseline_kb
	[[ -z ${!mapped:-} ]] || gate "$n" 'file-backed idle RSS' $((idle_rss[$n] - idle_anon[$n])) $((${!mapped} * 120 / 100))
	# The libraries the binary itself asks for, as stems: "libc.so.6" is "libc".
	extra=$(readelf -dW "$bin/${file[$n]}" | sed -n 's/.*Shared library: \[\(.*\)\.so.*\]/\1/p' | while read -r lib; do
		for ok in $allowed_libs; do
			# shellcheck disable=SC2053 # $ok is a glob on purpose
			[[ $lib == $ok ]] && continue 2
		done
		printf '%s ' "$lib"
	done)
	if [[ -n $extra ]]; then
		printf 'FAIL %s: links shared libraries that are not allowed: %s\n' "$n" "$extra"
		fail=1
	else
		printf 'ok   %s: shared libraries\n' "$n"
	fi
done
exit "$fail"
