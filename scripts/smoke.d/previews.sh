# Smoke section for previews (roadmap B2): the Git tab of a git application has a Previews card,
# turning previews on is refused while the application is not cloned through a provider, a bad
# limit is refused, a variable can be marked "available to previews" and is shown as such, and
# a bad mark is refused. Needs no git host: it only exercises the forms.
section_previews() {
	local suffix="$RANDOM$RANDOM" project server app base slug form var env
	slug="smoke-pv-$suffix"
	project="$(api POST "/api/v1/teams/$TEAM/projects" \
		"$(jq -n --arg s "smoke-pv-$suffix" '{name: "Smoke previews", slug: $s}')" | jq -er .id)"
	server="$(api GET "/api/v1/teams/$TEAM/servers" |
		jq -er '[.[] | select(.is_local and .status == "active")][0].id')" ||
		fail "no active local server in team $TEAM"
	app="$(api POST "/api/v1/teams/$TEAM/projects/$project/applications" \
		"$(jq -n --arg s "$slug" --argjson server "$server" \
			'{name: "Smoke previews", slug: $s, server_id: $server, repo_url: "https://example.com/smoke.git", git_ref: "main", container_port: 80}')" |
		jq -er .id)"
	base="/teams/$TEAM/projects/$project/apps/$app/git"
	env="/teams/$TEAM/projects/$project/apps/$app/env"

	expect_page "$base" "Previews"
	echo "ok   GET $base (the Previews card)"

	form="$(jq -n '{ad_enabled: false, pv_enabled: true, pv_command: "npm run migrate", pv_max: 3, pv_ttl: 7,
		pv_mem: "", pv_cpu: "", pv_fork: false}')"
	ui_post "$base" "$form"
	grep -qF 'Saved' "$BODY" && fail "previews were turned on without a git provider"
	echo "ok   previews need a git provider"

	ui_post "$base" "$(jq '.pv_enabled = false | .pv_max = 99' <<<"$form")"
	grep -qF 'Saved' "$BODY" && fail "a limit of 99 open previews was saved"
	echo "ok   a bad limit is refused"

	ui_post "$base" "$(jq '.pv_enabled = false' <<<"$form")"
	grep -qF 'Saved' "$BODY" || fail "settings save: $(head -c 300 "$BODY")"
	echo "ok   POST $base (settings saved)"

	api POST "/api/v1/teams/$TEAM/projects/$project/applications/$app/env" \
		'{"key":"APP_MODE","value":"preview"}' >/dev/null
	var="$(api GET "/api/v1/teams/$TEAM/projects/$project/applications/$app/env" | jq -er '.[0].id')"
	expect_page "$env" "Allow in previews"
	ui_post "$env/$var/preview/on" '{}'
	expect_page "$env" "In previews"
	echo "ok   a variable is marked available to previews"
	ui_post "$env/$var/preview/sideways" '{}'
	grep -qF 'In previews' "$BODY" && fail "a bad mark was accepted"
	echo "ok   a bad mark is refused"

	api DELETE "/api/v1/teams/$TEAM/projects/$project/applications/$app" >/dev/null
	api DELETE "/api/v1/teams/$TEAM/projects/$project" >/dev/null || true
}
