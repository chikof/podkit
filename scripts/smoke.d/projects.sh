# Smoke sections for the projects area; sourced by scripts/ui-smoke.sh (see its header).
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
	grep -qF "That repository URL isn't valid" "$BODY" || fail "save: bad URL not reported inline"
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
