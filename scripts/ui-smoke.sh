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

SECTIONS=(shell applications)

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

# --- sections -----------------------------------------------------------

section_shell() {
	# `/` redirects a signed-in user to their first team; follow it.
	FOLLOW=-L expect_page / "New project"
	expect_page "/teams/$TEAM/projects" "New project"
	expect_page "/teams/$TEAM/projects" "Backup targets"
	expect_page "/teams/0/projects" "Not found" 404
}

# Creates an application on the local server through the same signals the
# form posts, then checks the pages never show a secret, and deletes it.
section_applications() {
	local suffix="$RANDOM$RANDOM" project server app base secret
	local slug="smoke-app-$suffix" key='-----BEGIN OPENSSH PRIVATE KEY-----smoke-key-material'
	project="$(api POST "/api/v1/teams/$TEAM/projects" \
		"$(jq -n --arg s "smoke-$suffix" '{name: "Smoke apps", slug: $s}')" | jq -er .id)"
	server="$(api GET "/api/v1/teams/$TEAM/servers" |
		jq -er '[.[] | select(.is_local and .status == "active")][0].id')" ||
		fail "no active local server in team $TEAM"
	base="/teams/$TEAM/projects/$project"

	expect_page "$base/apps/new" "New application"
	local form
	form="$(jq -n --arg slug "$slug" --arg server "$server" --arg key "$key" '{
		name: "Smoke app", slug: $slug, server_id: $server,
		repo_url: "git@example.com:smoke/app.git", git_ref: "main", deploy_key: $key,
		git_token: "", build_strategy: "dockerfile", dockerfile_path: "Dockerfile",
		container_port: "3000", memory_limit_mb: "", cpu_limit: ""}')"
	ui_post "$base/apps" "$form"
	grep -qF 'Application created' "$BODY" || fail "create: no confirmation: $(head -c 300 "$BODY")"
	grep -qF "/webhooks/deploy/" "$BODY" || fail "create: no webhook URL"
	secret="$(grep -o '[0-9a-f]\{48\}' "$BODY" | head -1)"
	[[ -n "$secret" ]] || fail "create: no webhook secret in the response"
	echo "ok   POST $base/apps (created, secret shown once)"

	app="$(api GET "/api/v1/teams/$TEAM/projects/$project/applications" | jq -er --arg s "$slug" '.[] | select(.slug == $s) | .id')"
	expect_page "$base" "Smoke app"
	expect_absent "$secret" BEGIN "smoke-key-material"
	expect_page "$base/apps/$app" "Smoke app"
	expect_page "$base/apps/$app" "$slug"
	expect_absent "$secret" BEGIN "smoke-key-material" "Webhook secret"
	expect_page "$base/apps/$app/settings" "Save changes"
	expect_absent "$secret" BEGIN "smoke-key-material"
	expect_page "$base/apps/$app/settings" "A deploy key is stored."

	# Blank credentials keep the stored key; a bad URL is an inline message.
	ui_post "$base/apps/$app/settings" "$(jq '.name = "Smoke app 2" | .deploy_key = ""' <<<"$form")"
	grep -qF 'Settings saved' "$BODY" || fail "save: no toast: $(head -c 300 "$BODY")"
	expect_page "$base/apps/$app/settings" "A deploy key is stored."
	ui_post "$base/apps/$app/settings" "$(jq '.repo_url = "nope"' <<<"$form")"
	grep -qF 'invalid repo_url' "$BODY" || fail "save: bad URL not reported inline"
	echo "ok   settings save keeps credentials, validation is inline"

	ui_post "$base/apps/$app/delete" '{"confirm": "wrong"}'
	grep -qF "doesn't match" "$BODY" || fail "delete: wrong slug accepted"
	ui_post "$base/apps/$app/delete" "$(jq -n --arg s "$slug" '{confirm: $s}')"
	grep -qF "window.location.assign" "$BODY" || fail "delete: no redirect"
	expect_page "$base/apps/$app" "Not found" 404
	api DELETE "/api/v1/teams/$TEAM/projects/$project" >/dev/null 2>&1 || true
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
