# Environment cloning: the dialog on the environment page, the preview it patches in, a clone
# (names, slugs and default domains get the new slug; the source stays as it was) and the clone
# page with its report. Nothing is deployed and no data is copied, so no container runs. Run it
# as: scripts/ui-smoke.sh clone_env.
section_clone_env() {
	local suffix="$RANDOM$RANDOM" project server app base slug env clone
	slug="smoke-cl-$suffix"
	server="$(api GET "/api/v1/teams/$TEAM/servers" | jq -er '[.[] | select(.is_local)][0].id')" ||
		fail "no local server in team $TEAM"
	project="$(api POST "/api/v1/teams/$TEAM/projects" \
		"$(jq -n --arg s "smoke-cl-$suffix" '{name: "Smoke clone", slug: $s}')" | jq -er .id)"
	base="/teams/$TEAM/projects/$project"

	ui_post "$base/apps" "$(jq -n --arg slug "$slug" --arg server "$server" '{
		source: "image", name: "Smoke clone app", slug: $slug, server_id: $server,
		image: "traefik/whoami:v1.10", registry_username: "", registry_password: "",
		container_port: "80", memory_limit_mb: "", cpu_limit: "", environment: ""}')"
	app="$(api GET "/api/v1/teams/$TEAM/projects/$project/applications" | jq -er --arg s "$slug" '.[] | select(.slug == $s) | .id')"
	env="$(api GET "/api/v1/teams/$TEAM/projects/$project/applications" | jq -er --arg s "$slug" '.[] | select(.slug == $s) | .environment_id')"

	expect_page "$base" "Clone environment"
	expect_body "Leave secret values blank" "the dialog's secrets option"
	echo "ok   GET $base (clone dialog)"

	ui_post "$base/environments/clone/preview" "$(jq -n --arg e "$env" \
		'{envId: $e, cloneName: "Staging", cloneSlug: "staging", cloneBlank: false, cloneData: false, cloneTarget: "", cloneDeploy: false}')"
	grep -qF "$slug-staging" "$BODY" || fail "preview: $(head -c 300 "$BODY")"
	echo "ok   POST $base/environments/clone/preview (names the clone will make)"

	ui_post "$base/environments/clone" "$(jq -n --arg e "$env" \
		'{envId: $e, cloneName: "Staging", cloneSlug: "staging", cloneBlank: true, cloneData: false, cloneTarget: "", cloneDeploy: false}')"
	grep -qF '/environments/clones/' "$BODY" || fail "clone: $(head -c 300 "$BODY")"
	clone="$(grep -o '/environments/clones/[0-9]*' "$BODY" | head -1 | grep -o '[0-9]*$')"
	[[ -n "$clone" ]] || fail "clone: no clone id in the response"
	echo "ok   POST $base/environments/clone (clone $clone)"

	expect_page "$base/environments/clones/$clone" "Smoke clone"
	expect_page "$base/staging" "$slug-staging"
	api GET "/api/v1/teams/$TEAM/projects/$project/environments/clones/$clone" |
		jq -e '.name == "Staging" and .options.secrets == "blank" and .status != "failed"' >/dev/null ||
		fail "clone $clone: wrong record"
	# The same slug again is refused and makes nothing.
	ui_post "$base/environments/clone" "$(jq -n --arg e "$env" \
		'{envId: $e, cloneName: "Staging", cloneSlug: "staging", cloneBlank: false, cloneData: false, cloneTarget: "", cloneDeploy: false}')"
	grep -qF 'already exists' "$BODY" || fail "a second clone with the slug was not refused"
	echo "ok   clone page, clone environment listed, a taken slug refused"

	api DELETE "/api/v1/teams/$TEAM/projects/$project" >/dev/null 2>&1 || true
}
