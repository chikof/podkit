#!/usr/bin/env bash
# Resolves every recipe of crates/core/catalog/models.toml against the Hugging Face
# API (file lists and byte sizes only, never the weights) and checks that the
# pinned revision, total_bytes and memory estimates are what HF reports.
#
#   scripts/catalog-verify.sh          check; exit 0 when everything resolves and matches
#   scripts/catalog-verify.sh --fix    pin revisions, fill total_bytes and the estimates,
#                                      and drop recipes (then models) HF cannot resolve
#
# Needs: bash, python3 (3.11+ for tomllib) and network access. HF_TOKEN is used
# when set (gated repos). HF_ENDPOINT overrides https://huggingface.co.
# A network failure aborts with exit 2 and never drops anything: only a repo,
# revision or file that Hugging Face says does not exist counts as unresolved.
# This is the only part of the catalog that touches the network.
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
exec python3 "$here/catalog_verify.py" "$@"
