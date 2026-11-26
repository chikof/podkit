#!/usr/bin/env bash
# Builds the release binaries with the server's start-up code laid out in one block
# (docs/footprint.md, "Start-up order"). Needs lld, which is what Rust links with on
# x86_64 Linux.
#
#   scripts/build-ordered.sh [cargo build arguments; default: --release -p server -p agent]
#
# Two links: the first writes a map of this build's symbols, from which the ones listed in
# server/startup.order are picked; the second places them first. When the pick has not
# changed since the last run, the second link is a no-op.
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"
(($#)) || set -- --release -p server -p agent
order=${CARGO_TARGET_DIR:-$root/target}/startup.order.txt
mkdir -p "$(dirname "$order")"
[[ -e $order ]] || : >"$order"
export PODKIT_SYMBOL_ORDER=$order

cargo build "$@"
python3 scripts/startup_order.py resolve "$order.map" server/startup.order >"$order.new"
if cmp -s "$order.new" "$order"; then
	rm "$order.new"
else
	mv "$order.new" "$order"
	cargo build "$@"
fi
