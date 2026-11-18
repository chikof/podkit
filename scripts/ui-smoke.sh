#!/usr/bin/env bash
# Smoke-tests the podkit web UI of a running server with curl and jq.
#
#   PODKIT_EMAIL=you@example.com PODKIT_PASSWORD=... scripts/ui-smoke.sh [section...]
#
# Environment:
#   PODKIT_URL       base URL, default http://127.0.0.1:8080
#   PODKIT_EMAIL     an existing account
#   PODKIT_PASSWORD  its password
#
# With no section it runs all of them. It stops at the first failure, prints
# the URL that failed and exits non-zero.
#
# Sections live in scripts/smoke.d/<area>.sh, one file per area. Each defines
# `section_<name>` functions (underscores in the function name are dashes in the
# section name: `section_app_config` runs as `app-config`). A card adds its own
# file and edits no other. Every file is sourced, so every function in it can use
# the helpers below ($TEAM, expect_page, ui_post, api, ...). Sections create what
# they need and must not depend on each other.

set -euo pipefail


URL="${PODKIT_URL:-http://127.0.0.1:8080}"
URL="${URL%/}"
: "${PODKIT_EMAIL:?set PODKIT_EMAIL}"
: "${PODKIT_PASSWORD:?set PODKIT_PASSWORD}"

JAR="$(mktemp)"
BODY="$(mktemp)"
trap 'rm -f "$JAR" "$BODY"' EXIT

fail() {
	echo "FAIL: $*" >&2
	exit 1
}

# api METHOD PATH [JSON]: JSON API call with the bearer token, body on stdout.
api() {
	local method="$1" path="$2" data="${3:-}"
	local args=(-sS -f -X "$method" -H "Authorization: Bearer $TOKEN")
	if [[ -n "$data" ]]; then
		args+=(-H 'Content-Type: application/json' -d "$data")
	fi
	curl "${args[@]}" "$URL$path" || fail "API $method $URL$path"
}

# expect_page PATH MARKER [STATUS]: GET PATH with the session cookie, check the
# status (default 200) and that the body contains MARKER.
expect_page() {
	local path="$1" marker="$2" want="${3:-200}" got
	got="$(curl -sS ${FOLLOW:-} -o "$BODY" -w '%{http_code}' -b "$JAR" "$URL$path")" ||
		fail "GET $URL$path: request failed"
	[[ "$got" == "$want" ]] || fail "GET $URL$path: status $got, wanted $want"
	grep -qF -- "$marker" "$BODY" || fail "GET $URL$path: missing text: $marker"
	echo "ok   GET $path ($got, has \"$marker\")"
}

# expect_absent PATTERN...: the page last fetched by expect_page must not
# contain any of the patterns (fixed strings).
expect_absent() {
	local pattern
	for pattern in "$@"; do
		if grep -qF -- "$pattern" "$BODY"; then
			fail "last page contains forbidden text: $pattern"
		fi
	done
	echo "ok   page has none of: $*"
}

# ui_post PATH JSON: a Datastar action, SSE response in $BODY.
ui_post() {
	curl -sS -f -o "$BODY" -b "$JAR" -X POST \
		-H 'Content-Type: application/json' -H 'datastar-request: true' -H "Origin: $URL" \
		-d "$2" "$URL$1" || fail "POST $URL$1"
}

# --- setup --------------------------------------------------------------

login_json="$(jq -n --arg e "$PODKIT_EMAIL" --arg p "$PODKIT_PASSWORD" '{email: $e, password: $p}')"

TOKEN="$(curl -sS -f -X POST -H 'Content-Type: application/json' -d "$login_json" \
	"$URL/api/v1/auth/login" | jq -er .token)" || fail "POST $URL/api/v1/auth/login"

# The cookie jar comes from the same Datastar POST the login page makes.
curl -sS -f -o /dev/null -c "$JAR" -X POST \
	-H 'Content-Type: application/json' -H 'datastar-request: true' -H "Origin: $URL" \
	-d "$login_json" "$URL/login" || fail "POST $URL/login"
grep -q $'\tsession\t' "$JAR" || fail "POST $URL/login: no session cookie (wrong credentials?)"

TEAM="$(api GET /api/v1/teams | jq -r '.[0].id // empty')"
if [[ -z "$TEAM" ]]; then
	TEAM="$(api POST /api/v1/teams '{"name":"Smoke","slug":"smoke"}' | jq -er .id)"
fi

# --- helpers shared by the sections ----------------------------------------

# wait_for PATH MARKER SECONDS: GET PATH every 3 s until the body has MARKER.
wait_for() {
	local path="$1" marker="$2" seconds="$3" waited=0
	until curl -sS -b "$JAR" -o "$BODY" "$URL$path" && grep -qF -- "$marker" "$BODY"; do
		if ((waited >= seconds)); then
			fail "GET $URL$path: no \"$marker\" after ${seconds}s"
		fi
		sleep 3
		waited=$((waited + 3))
	done
	echo "ok   GET $path (has \"$marker\")"
}

# expect_body MARKER WHAT: the last response must contain MARKER.
expect_body() {
	grep -qF -- "$1" "$BODY" || fail "$2: missing text: $1"
	echo "ok   $2 (has \"$1\")"
}

# --- sections -----------------------------------------------------------

for file in "$(dirname "${BASH_SOURCE[0]}")"/smoke.d/*.sh; do
	# shellcheck source=/dev/null
	source "$file"
done
SECTIONS=()
while read -r _ _ fn; do
	[[ "$fn" == section_* ]] && SECTIONS+=("$(tr _ - <<<"${fn#section_}")")
done < <(declare -F)
((${#SECTIONS[@]})) || fail "no sections found in scripts/smoke.d"

# --- run ----------------------------------------------------------------

if [[ $# -eq 0 ]]; then
	set -- "${SECTIONS[@]}"
fi
for section in "$@"; do
	fn="section_${section//-/_}"
	declare -F "$fn" >/dev/null || fail "unknown section: $section (have: ${SECTIONS[*]})"
	echo "== $section"
	"$fn"
done
echo "all ok"
