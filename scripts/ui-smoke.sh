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

SECTIONS=(shell members servers applications deployments app-config databases backups models)

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

# Deploys through the UI, then walks the deployment pages. Works without an
# agent: the deployment is queued, then fails or stays queued, and the pages
# and refusals are the same either way.
section_deployments() {
	local suffix="$RANDOM$RANDOM" project server app base slug form dep
	slug="smoke-dep-$suffix"
	project="$(api POST "/api/v1/teams/$TEAM/projects" \
		"$(jq -n --arg s "smoke-d-$suffix" '{name: "Smoke deploys", slug: $s}')" | jq -er .id)"
	server="$(api GET "/api/v1/teams/$TEAM/servers" |
		jq -er '[.[] | select(.is_local and .status == "active")][0].id')" ||
		fail "no active local server in team $TEAM"
	base="/teams/$TEAM/projects/$project"
	form="$(jq -n --arg slug "$slug" --arg server "$server" '{
		name: "Smoke deploy", slug: $slug, server_id: $server,
		repo_url: "https://example.invalid/smoke.git", git_ref: "main", deploy_key: "",
		git_token: "", build_strategy: "dockerfile", dockerfile_path: "Dockerfile",
		container_port: "3000", memory_limit_mb: "", cpu_limit: ""}')"
	ui_post "$base/apps" "$form"
	app="$(api GET "/api/v1/teams/$TEAM/projects/$project/applications" | jq -er --arg s "$slug" '.[] | select(.slug == $s) | .id')"
	base="$base/apps/$app"

	expect_page "$base/deployments" "Nothing has been deployed yet."
	expect_page "$base/logs" "isn't running"
	ui_post "$base/stop" '{}'
	grep -qF 'not currently running' "$BODY" || fail "stop: no inline refusal: $(head -c 300 "$BODY")"
	ui_post "$base/restart" '{}'
	grep -qF 'no previous successful build' "$BODY" || fail "restart: no inline refusal: $(head -c 300 "$BODY")"
	echo "ok   stop and restart refusals are inline"

	ui_post "$base/deploy" '{}'
	dep="$(grep -o "/deployments/[0-9]*'" "$BODY" | head -1 | tr -dc '0-9')"
	[[ -n "$dep" ]] || fail "deploy: no redirect to the deployment page: $(head -c 300 "$BODY")"
	echo "ok   POST $base/deploy (deployment $dep)"
	expect_page "$base/deployments/$dep" "Deployment #$dep"
	expect_page "$base/deployments" "Deployments"
	expect_page "$base/deployments" "/deployments/$dep"
	expect_page "$base/deployments/table" "deployments"
	# The build log is a live stream; read it only until it ends or 3 seconds pass.
	curl -sS -N --max-time 3 -o "$BODY" -b "$JAR" "$URL$base/deployments/$dep/log" || true
	echo "ok   GET $base/deployments/$dep/log (stream opened)"

	ui_post "$base/deployments/$dep/cancel" '{}'
	grep -qE 'Cancelling|not in progress' "$BODY" || fail "cancel: unexpected answer: $(head -c 300 "$BODY")"
	ui_post "$base/deployments/$dep/rollback" '{}'
	grep -qE 'never produced an image|live or still in progress' "$BODY" ||
		fail "rollback: not refused: $(head -c 300 "$BODY")"
	expect_page "$base/deployments/0" "Not found" 404
	echo "ok   cancel and rollback answer inline"

	api DELETE "/api/v1/teams/$TEAM/projects/$project/applications/$app" >/dev/null 2>&1 || true
	api DELETE "/api/v1/teams/$TEAM/projects/$project" >/dev/null 2>&1 || true
}

# Sets an env var, a domain and a volume through the same signals the forms
# post, and checks the secret value is stored but appears in no page.
section_app_config() {
	local suffix="$RANDOM$RANDOM" project server app base cfg
	local secret="S3CR3T-$(cat /proc/sys/kernel/random/uuid)"
	local slug="smoke-cfg-$suffix" host="smoke-$suffix.example.com"
	project="$(api POST "/api/v1/teams/$TEAM/projects" \
		"$(jq -n --arg s "smoke-cfg-$suffix" '{name: "Smoke config", slug: $s}')" | jq -er .id)"
	server="$(api GET "/api/v1/teams/$TEAM/servers" |
		jq -er '[.[] | select(.is_local and .status == "active")][0].id')" ||
		fail "no active local server in team $TEAM"
	base="/teams/$TEAM/projects/$project"
	ui_post "$base/apps" "$(jq -n --arg slug "$slug" --arg server "$server" '{
		name: "Smoke cfg", slug: $slug, server_id: $server,
		repo_url: "https://example.com/smoke.git", git_ref: "main", deploy_key: "",
		git_token: "", build_strategy: "dockerfile", dockerfile_path: "Dockerfile",
		container_port: "3000", memory_limit_mb: "", cpu_limit: ""}')"
	grep -qF 'Application created' "$BODY" || fail "create: $(head -c 300 "$BODY")"
	app="$(api GET "/api/v1/teams/$TEAM/projects/$project/applications" | jq -er --arg s "$slug" '.[] | select(.slug == $s) | .id')"
	cfg="$base/apps/$app"
	local T="$TOKEN"

	# Environment: one variable through the form, then a paste with a bad line.
	expect_page "$cfg/env" "Save variable"
	ui_post "$cfg/env" "$(jq -n --arg v "$secret" '{env_key: "SMOKE", env_value: $v}')"
	grep -qF 'Changes apply on the next deploy' "$BODY" || fail "env: no saved banner"
	if grep -qF "$secret" "$BODY"; then fail "env: the SSE response contains the value"; fi
	curl -s -H "Authorization: Bearer $T" "$URL/api/v1/teams/$TEAM/projects/$project/applications/$app/env-vars" |
		jq -e 'any(.[]; .key=="SMOKE")' >/dev/null || fail "env: SMOKE is not in the JSON API"
	echo "ok   env var set through the UI is in the API"
	ui_post "$cfg/env/bulk" "$(jq -n --arg v "$secret" '{env_bulk: ("# c\nBULK_A=1\nnot a pair " + $v + "\nBULK_B=\"2\"\n")}')"
	grep -qF 'Saved 2 variables.' "$BODY" || fail "bulk: $(head -c 300 "$BODY")"
	grep -qF 'Line 3: expected KEY=VALUE.' "$BODY" || fail "bulk: bad line not reported by number"
	if grep -qF "$secret" "$BODY"; then fail "bulk: the response contains the value"; fi
	expect_page "$cfg/env" "BULK_B"
	expect_absent "$secret"
	echo "ok   bulk paste saved the good lines and named line 3"

	# Domains: add, a duplicate is a conflict, DNS help is shown.
	expect_page "$cfg/domains" "DNS setup"
	ui_post "$cfg/domains" "$(jq -n --arg h "$host" '{domain_hostname: $h}')"
	grep -qF "$host" "$BODY" || fail "domain: not added: $(head -c 300 "$BODY")"
	ui_post "$cfg/domains" "$(jq -n --arg h "$host" '{domain_hostname: $h}')"
	grep -qF 'already routed' "$BODY" || fail "domain: duplicate not reported"
	expect_page "$cfg/domains" "$host"
	echo "ok   domain added, duplicate is a conflict"

	# Volumes: add, a duplicate path is a conflict, delete asks first.
	ui_post "$cfg/volumes" '{"volume_path": "/smoke-data"}'
	grep -qF '/smoke-data' "$BODY" || fail "volume: not added"
	ui_post "$cfg/volumes" '{"volume_path": "/smoke-data"}'
	grep -qF 'already mounted' "$BODY" || fail "volume: duplicate not reported"
	expect_page "$cfg/volumes" "permanently deletes all data"
	expect_absent "$secret"
	echo "ok   volume added, duplicate is a conflict, delete is behind a confirm"

	expect_page "$cfg" "Smoke cfg"
	expect_absent "$secret"
	api DELETE "/api/v1/teams/$TEAM/projects/$project/applications/$app" >/dev/null 2>&1 || true
	api DELETE "/api/v1/teams/$TEAM/projects/$project" >/dev/null 2>&1 || true
}

# expect_body MARKER WHAT: the last response must contain MARKER.
expect_body() {
	grep -qF -- "$1" "$BODY" || fail "$2: missing text: $1"
	echo "ok   $2 (has \"$1\")"
}

# A database made through the form: its page never carries the password, the
# Reveal action does, and delete without the data checkbox keeps the volume.
section_databases() {
	local project server db base password name="smoke-$$"
	project="$(api POST "/api/v1/teams/$TEAM/projects" "{\"name\":\"Smoke DB\",\"slug\":\"smoke-db-$$\"}" | jq -er .id)"
	server="$(api GET "/api/v1/teams/$TEAM/servers" | jq -er '.[0].id')"
	base="/teams/$TEAM/projects/$project/databases"

	expect_page "$base/new" "Create database"
	ui_post "$base" "$(jq -n --arg n "$name" --arg s "$server" \
		'{name: $n, engine: "postgres", version: "", server_id: $s, public_port: "", memory_limit_mb: "", cpu_limit: ""}')"
	expect_body "window.location.assign" "POST $base"
	db="$(api GET "/api/v1/teams/$TEAM/projects/$project/databases" | jq -er --arg n "$name" '.[] | select(.name == $n) | .id')"
	password="$(api GET "/api/v1/teams/$TEAM/projects/$project/databases/$db" | jq -er .connection.password)"

	expect_page "$base/$db" "Reveal password and URLs"
	! grep -qF -- "$password" "$BODY" || fail "GET $base/$db: the password is in the page"
	echo "ok   GET $base/$db has no password"
	ui_post "$base/$db/reveal" '{}'
	grep -qF -- "$password" "$BODY" || fail "POST $base/$db/reveal: no password in the reply"
	echo "ok   POST $base/$db/reveal (has the password; not printed)"
	expect_page "$base/$db/logs" "Container log"
	expect_page "$base/$db/settings" "Backup schedule"
	expect_page "/teams/$TEAM/projects/$project" "$name"

	ui_post "$base/$db/schedule" '{"backup_schedule":"61 * * * *"}'
	expect_body "Not a valid schedule" "POST $base/$db/schedule (bad cron)"
	ui_post "$base/$db/schedule" '{"backup_schedule":"0 3 * * *"}'
	expect_body "Next run" "POST $base/$db/schedule"

	ui_post "$base/$db/delete" "$(jq -n --arg n "$name" '{confirm: $n, purge: false}')"
	expect_body "window.location.assign" "POST $base/$db/delete"
	expect_page "/teams/$TEAM/projects/$project/databases/0" "Not found" 404
}

# Backup targets through the form (the secret key never comes back in a page),
# then a database's Backups tab. Taking and restoring a real backup needs S3;
# see the card's MinIO proof.
section_backups() {
	local project server db base name="smoke-bk-$$" secret="smoke-secret-$$" target
	local targets="/teams/$TEAM/backup-targets"

	ui_post "$targets" "$(jq -n --arg n "$name" --arg s "$secret" \
		'{name: $n, endpoint: "http://localhost:9000", region: "us-east-1", bucket: "podkit", prefix: "", path_style: true, access_key_id: "minio", secret_access_key: $s}')"
	expect_body "window.location.assign" "POST $targets"
	expect_page "$targets" "$name"
	! grep -qF -- "$secret" "$BODY" || fail "GET $targets: the secret key is in the page"
	echo "ok   GET $targets has no secret key"
	target="$(api GET "/api/v1/teams/$TEAM/backup-targets" | jq -er --arg n "$name" '.[] | select(.name == $n) | .id')"

	project="$(api POST "/api/v1/teams/$TEAM/projects" "{\"name\":\"Smoke BK\",\"slug\":\"smoke-bk-$$\"}" | jq -er .id)"
	server="$(api GET "/api/v1/teams/$TEAM/servers" | jq -er '.[0].id')"
	db="$(api POST "/api/v1/teams/$TEAM/projects/$project/databases" \
		"{\"name\":\"$name\",\"engine\":\"postgres\",\"server_id\":$server}" | jq -er .id)"
	base="/teams/$TEAM/projects/$project/databases/$db"
	expect_page "$base/backups" "No backups yet."
	expect_page "$base/settings" "Add a backup target"
	ui_post "$base/backups" '{}'
	expect_body "configure a backup target first" "POST $base/backups (no target yet)"
	ui_post "$base/backups/1/restore" '{"confirm":"wrong"}'
	expect_body "The name doesn't match." "POST $base/backups/1/restore (no typed name)"

	ui_post "$base/delete" "$(jq -n --arg n "$name" '{confirm: $n, purge: false}')"
	ui_post "$targets/$target/delete" '{}'
	expect_body "window.location.assign" "POST $targets/$target/delete"
}

section_servers() {
	expect_page "/teams/$TEAM/servers" "Add a server"
	expect_page "/teams/$TEAM/servers" "Local"
	local local_id
	local_id="$(api GET "/api/v1/teams/$TEAM/servers" | jq -er '[.[] | select(.is_local)][0].id')" ||
		fail "no local server in team $TEAM"
	expect_page "/teams/$TEAM/servers/$local_id" "Architecture"
	expect_page "/teams/$TEAM/servers/0" "Not found" 404
	# The key is write-only: no page may carry one.
	grep -qF -- "BEGIN" "$BODY" && fail "GET /teams/$TEAM/servers/0: a private key marker is in the page"
	curl -sS -b "$JAR" "$URL/teams/$TEAM/servers" | grep -qF -- "BEGIN" &&
		fail "GET /teams/$TEAM/servers: a private key marker is in the page"
	echo "ok   no private key in the server pages"
}

section_members() {
	expect_page "/teams/$TEAM/members" "Members"
	expect_page "/teams/$TEAM/members" "Roles"
	# The sign-up page answers 200 while registration is allowed and 403
	# with its own message when it is not; anything else is a bug.
	local got
	got="$(curl -sS -o "$BODY" -w '%{http_code}' "$URL/register")" ||
		fail "GET $URL/register: request failed"
	case "$got" in
	200) grep -qF "Create account" "$BODY" || fail "GET $URL/register: missing text: Create account" ;;
	403) grep -qF "Registration is closed" "$BODY" || fail "GET $URL/register: missing text: Registration is closed" ;;
	*) fail "GET $URL/register: status $got, wanted 200 or 403" ;;
	esac
	echo "ok   GET /register ($got)"
	expect_page "/teams/0/members" "Not found" 404
}

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
