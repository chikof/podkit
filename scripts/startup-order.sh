#!/usr/bin/env bash
# Regenerates server/startup.order: the functions the server runs from start to idle.
# Run it when `scripts/footprint.sh --check` says the file-backed idle RSS is over budget.
#
#   DATABASE_URL=postgres://… scripts/startup-order.sh
#
# Builds an unstripped release server, runs it under callgrind through
# scripts/footprint.sh (so it needs valgrind and an empty Postgres database), and lists
# what ran. Takes a few minutes.
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"
command -v valgrind >/dev/null || { echo "startup-order: valgrind is not installed" >&2; exit 2; }

target=${CARGO_TARGET_DIR:-$root/target}/startup-order
# The link map gives the demangled name of every symbol; the order file stays empty.
: >"$target.txt"
CARGO_TARGET_DIR=$target CARGO_PROFILE_RELEASE_STRIP=false PODKIT_SYMBOL_ORDER=$target.txt \
	cargo build --release -p server -p agent

tmp=$(mktemp -d "${TMPDIR:-/tmp}/pk-order.XXXXXX")
trap 'rm -rf "$tmp"' EXIT
cp "$target/release/podkit-agent" "$tmp/"
cat >"$tmp/server" <<EOF
#!/bin/sh
exec valgrind -q --tool=callgrind --demangle=no --callgrind-out-file=$tmp/callgrind.out $target/release/server "\$@"
EOF
chmod +x "$tmp/server"
# The server is some thirty times slower under valgrind: the numbers this prints mean nothing.
scripts/footprint.sh --no-load --idle 10 --bin "$tmp" >/dev/null
python3 scripts/startup_order.py profile "$tmp/callgrind.out" "$target/release/server" "$target.txt.map" >server/startup.order
echo "startup-order: $(wc -l <server/startup.order) functions listed in server/startup.order"
