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
# the URL that failed and exits non-zero. Each UI card adds its pages as a
# `section_<name>` function and lists the name in SECTIONS.

set -euo pipefail

SECTIONS=(shell)

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

# --- sections -----------------------------------------------------------

section_shell() {
	# `/` redirects a signed-in user to their first team; follow it.
	FOLLOW=-L expect_page / "New project"
	expect_page "/teams/$TEAM/projects" "New project"
	expect_page "/teams/$TEAM/projects" "Backup targets"
	expect_page "/teams/0/projects" "Not found" 404
}

# --- run ----------------------------------------------------------------

if [[ $# -eq 0 ]]; then
	set -- "${SECTIONS[@]}"
fi
for section in "$@"; do
	declare -F "section_$section" >/dev/null || fail "unknown section: $section (have: ${SECTIONS[*]})"
	echo "== $section"
	"section_$section"
done
echo "all ok"
