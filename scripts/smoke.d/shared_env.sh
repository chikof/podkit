# Shared variables: set a secret at team, project and environment scope, check the pages name
# it and never show its value, that an application's Environment tab lists it as inherited,
# then clean up. Run it as: scripts/ui-smoke.sh shared_env.
section_shared_env() {
	local suffix="$RANDOM$RANDOM" project server app base slug env secret
	secret="SHARED-$(cat /proc/sys/kernel/random/uuid)"
	slug="smoke-sv-$suffix"
	server="$(api GET "/api/v1/teams/$TEAM/servers" | jq -er '[.[] | select(.is_local)][0].id')" ||
		fail "no local server in team $TEAM"
	project="$(api POST "/api/v1/teams/$TEAM/projects" \
		"$(jq -n --arg s "smoke-sv-$suffix" '{name: "Smoke shared", slug: $s}')" | jq -er .id)"
	base="/teams/$TEAM/projects/$project"

	ui_post "$base/apps" "$(jq -n --arg slug "$slug" --arg server "$server" '{
		source: "image", name: "Smoke shared app", slug: $slug, server_id: $server,
		image: "traefik/whoami:v1.10", registry_username: "", registry_password: "",
		container_port: "80", memory_limit_mb: "", cpu_limit: "", environment: ""}')"
	app="$(api GET "/api/v1/teams/$TEAM/projects/$project/applications" | jq -er --arg s "$slug" '.[] | select(.slug == $s) | .id')"
	env="$(api GET "/api/v1/teams/$TEAM/projects/$project/applications" | jq -er --arg s "$slug" '.[] | select(.slug == $s) | .environment_id')"

	expect_page "/teams/$TEAM/settings/shared-variables" "Shared variables"
	expect_page "$base/shared-variables" "Shared variables"
	expect_page "$base/environments/$env/shared-variables" "Shared variables"
	for url in "/teams/$TEAM/settings/shared-variables" "$base/shared-variables" "$base/environments/$env/shared-variables"; do
		ui_post "$url" "$(jq -n --arg v "$secret" '{sv_key: "SMOKE_SHARED", sv_value: $v, sv_secret: true}')"
		grep -qF 'SMOKE_SHARED' "$BODY" || fail "set at $url: $(head -c 300 "$BODY")"
		expect_absent "$secret"
		expect_page "$url" "SMOKE_SHARED"
		expect_absent "$secret"
	done
	echo "ok   shared variables set at team, project and environment scope"

	expect_page "$base/apps/$app/env" "Inherited variables"
	expect_body "SMOKE_SHARED" "the inherited name"
	expect_absent "$secret"
	# A reference to a name nothing sets is refused.
	ui_post "$base/apps/$app/env" '{"env_key": "URL", "env_value": "${{shared.NOT_SET_ANYWHERE}}"}'
	grep -qF 'NOT_SET_ANYWHERE' "$BODY" || fail "missing reference was not reported"
	echo "ok   inherited variables listed, missing reference refused"

	api DELETE "/api/v1/teams/$TEAM/projects/$project" >/dev/null 2>&1 || true
}
