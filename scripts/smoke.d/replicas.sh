# Smoke section for replicas (roadmap B9): the General tab has the Replicas section, a save
# stores the settings and shows them back, a count over ten is refused whole, and the count
# goes back to one. Needs no running container: it only exercises the form.
section_replicas() {
	local suffix="$RANDOM$RANDOM" project server app base slug form save
	slug="smoke-rp-$suffix"
	project="$(api POST "/api/v1/teams/$TEAM/projects" \
		"$(jq -n --arg s "smoke-r-$suffix" '{name: "Smoke replicas", slug: $s}')" | jq -er .id)"
	server="$(api GET "/api/v1/teams/$TEAM/servers" |
		jq -er '[.[] | select(.is_local and .status == "active")][0].id')" ||
		fail "no active local server in team $TEAM"
	base="/teams/$TEAM/projects/$project"

	form="$(jq -n --arg slug "$slug" --arg server "$server" '{
		source: "image", name: "Smoke replicas app", slug: $slug, server_id: $server,
		image: "traefik/whoami:v1.10", container_port: "80", memory_limit_mb: "", cpu_limit: ""}')"
	ui_post "$base/apps" "$form"
	app="$(api GET "/api/v1/teams/$TEAM/projects/$project/applications" | jq -er --arg s "$slug" '.[] | select(.slug == $s) | .id')"

	expect_page "$base/apps/$app/settings" "Replicas"
	save="$(jq -n --arg slug "$slug" '{
		name: "Smoke replicas app", slug: $slug, repo_url: "", git_ref: "main", image: "traefik/whoami:v1.10",
		container_port: "80", memory_limit_mb: "", cpu_limit: "",
		rp_count: "3", rp_sticky: true, rp_surge: "1", rp_unavail: "0", rp_shared: false}')"
	ui_post "$base/apps/$app/settings" "$save"
	grep -qF 'Settings saved' "$BODY" || fail "replicas save: $(head -c 300 "$BODY")"
	expect_page "$base/apps/$app/settings" "&quot;rp_count&quot;:&quot;3&quot;"
	echo "ok   POST $base/apps/$app/settings (3 replicas)"

	ui_post "$base/apps/$app/settings" "$(jq '.rp_count = "11"' <<<"$save")"
	if grep -qF 'Settings saved' "$BODY"; then fail "11 replicas were accepted"; fi
	echo "ok   a count over ten is refused"

	ui_post "$base/apps/$app/settings" "$(jq '.rp_count = "1"' <<<"$save")"
	grep -qF 'Settings saved' "$BODY" || fail "replicas back to one: $(head -c 300 "$BODY")"
	echo "ok   back to one replica"

	api DELETE "/api/v1/teams/$TEAM/projects/$project/applications/$app" >/dev/null
}
