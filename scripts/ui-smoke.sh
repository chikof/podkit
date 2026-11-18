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

SECTIONS=(shell models)

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


# ui_post PATH JSON: a Datastar action. The SSE answer is left in $BODY.
ui_post() {
	local path="$1" json="$2"
	curl -sS -f -X POST -b "$JAR" -H 'Content-Type: application/json' \
		-H 'datastar-request: true' -H "Origin: $URL" -d "$json" -o "$BODY" "$URL$path" ||
		fail "POST $URL$path"
}

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

section_models() {
	expect_page "/teams/$TEAM/models" "Browse the catalog"
	expect_page "/teams/$TEAM/models/catalog" "S tier"
	expect_page "/teams/$TEAM/models/catalog?size=tiny" "Qwen3 4B"
	expect_page "/teams/$TEAM/models/catalog?tab=advanced" "Search Hugging Face"
	expect_page "/teams/$TEAM/models/catalog/qwen3-4b" "Overrides"
	expect_page "/teams/$TEAM/models/settings" "Hugging Face token"
	expect_page "/teams/$TEAM/models/catalog/no-such-model" "Not found" 404
	expect_page "/teams/0/models" "Not found" 404

	# The create flow downloads and runs a real model, so it is opt-in:
	#   PODKIT_SMOKE_MODEL=qwen3-4b scripts/ui-smoke.sh models
	# It removes the model and its files at the end unless PODKIT_SMOKE_KEEP is set.
	# A model with no recipe of at most 4 GB is refused.
	local model="${PODKIT_SMOKE_MODEL:-}"
	if [[ -z "$model" ]]; then
		echo "skip create flow (PODKIT_SMOKE_MODEL=<catalog id> downloads and runs one, at most 4 GB)"
		return
	fi
	local server project recipe query file path engine model_id page
	server="$(api GET "/api/v1/teams/$TEAM/servers" | jq -er '.[0].id')"
	project="$(api GET "/api/v1/teams/$TEAM/projects" | jq -r '.[0].id // empty')"
	if [[ -z "$project" ]]; then
		project="$(api POST "/api/v1/teams/$TEAM/projects" '{"name":"Smoke","slug":"smoke"}' | jq -er .id)"
	fi
	recipe="$(api GET "/api/v1/catalog/$model" | jq -er \
		'[.recipes[] | select(.total_bytes <= 4000000000)] | sort_by(.total_bytes) | .[0].id')" ||
		fail "$model has no recipe of at most 4 GB"

	ui_post "/teams/$TEAM/models/downloads/recipe" "$(jq -n --arg c "$model" --arg r "$recipe" \
		--arg s "$server" --arg p "$project" \
		'{catalog: $c, recipe: $r, server: $s, project: $p, name: "Smoke model", served: "smoke-model", ctx: "4096"}')"
	query="$(grep -o "/downloads/[0-9]*?[^']*" "$BODY" | head -n 1)" ||
		fail "the download did not start: $(head -c 400 "$BODY")"
	file="${query#/downloads/}"
	file="${file%%\?*}"
	echo "ok   POST download $file"
	wait_for "/teams/$TEAM/models/downloads/$file/progress?${query#*\?}" "Start the model" 1800

	engine="$(grep -o 'engine=[a-z_]*' <<<"$query" | cut -d= -f2)"
	path="$(grep -o 'path=[^&]*' <<<"$query" | cut -d= -f2)"
	ui_post "/teams/$TEAM/models/downloads/$file/start" "$(jq -n --arg p "$project" --arg e "$engine" --arg path "$path" \
		'{project: $p, name: "Smoke model", engine: $e, path: $path, ctx: "4096", served: "smoke-model"}')"
	model_id="$(grep -o "/models/[0-9]*'" "$BODY" | head -n 1 | tr -dc '0-9')"
	[[ -n "$model_id" ]] || fail "the model did not start: $(head -c 400 "$BODY")"
	echo "ok   POST start model $model_id"
	page="/teams/$TEAM/projects/$project/models/$model_id"
	expect_page "$page" "Smoke model"
	wait_for "$page/status?logs=1" "Running" 900

	ui_post "$page/chat" '{"prompt":"Say hello in one short sentence.","chat":[]}'
	grep -q 'pg-ans-0' "$BODY" || fail "chat: no answer element: $(head -c 400 "$BODY")"
	grep -Eq '[0-9]+ tokens' "$BODY" || fail "chat: no token count: $(head -c 400 "$BODY")"
	echo "ok   POST chat ($(grep -Eo '[0-9]+ tokens[^<]*' "$BODY" | tail -n 1))"

	if [[ -n "${PODKIT_SMOKE_KEEP:-}" ]]; then
		echo "kept the model: $URL$page"
		return
	fi
	ui_post "$page/remove" '{}'
	wait_for "/teams/$TEAM/models" "Browse the catalog" 120
	while curl -sS -b "$JAR" "$URL/teams/$TEAM/models" | grep -qF "$model_id"; do sleep 3; done
	ui_post "/teams/$TEAM/models/downloads/$file/delete" '{}'
	echo "ok   removed the model and its files"
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
