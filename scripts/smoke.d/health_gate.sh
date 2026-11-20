# Smoke section for the health gate (roadmap A5): the General tab has the Health section, a
# save stores the settings and shows them back, a bad number is refused whole, and turning the
# gate off removes it. Needs no running container: it only exercises the form.
section_health_gate() {
	local suffix="$RANDOM$RANDOM" project server app base slug form
	slug="smoke-hg-$suffix"
	project="$(api POST "/api/v1/teams/$TEAM/projects" \
		"$(jq -n --arg s "smoke-h-$suffix" '{name: "Smoke health", slug: $s}')" | jq -er .id)"
	server="$(api GET "/api/v1/teams/$TEAM/servers" |
		jq -er '[.[] | select(.is_local and .status == "active")][0].id')" ||
		fail "no active local server in team $TEAM"
	base="/teams/$TEAM/projects/$project"

	form="$(jq -n --arg slug "$slug" --arg server "$server" '{
		source: "image", name: "Smoke health app", slug: $slug, server_id: $server,
		image: "traefik/whoami:v1.10", container_port: "80", memory_limit_mb: "", cpu_limit: ""}')"
	ui_post "$base/apps" "$form"
	app="$(api GET "/api/v1/teams/$TEAM/projects/$project/applications" | jq -er --arg s "$slug" '.[] | select(.slug == $s) | .id')"

	expect_page "$base/apps/$app/settings" "Wait for the new container"
	local save
	save="$(jq -n --arg slug "$slug" '{
		name: "Smoke health app", slug: $slug, repo_url: "", git_ref: "main", image: "traefik/whoami:v1.10",
		container_port: "80", memory_limit_mb: "", cpu_limit: "",
		hc_mode: "http", hc_path: "/", hc_status: "200", hc_command: "", hc_interval: "2",
		hc_timeout: "1", hc_retries: "3", hc_start: "5", hc_wait: "30", hc_drain: "2",
		hc_window: "5", hc_after: "3"}')"
	ui_post "$base/apps/$app/settings" "$save"
	grep -qF 'Settings saved' "$BODY" || fail "health save: $(head -c 300 "$BODY")"
	expect_page "$base/apps/$app/settings" "&quot;hc_mode&quot;:&quot;http&quot;"
	echo "ok   POST $base/apps/$app/settings (health gate on)"

	ui_post "$base/apps/$app/settings" "$(jq '.hc_interval = "abc"' <<<"$save")"
	grep -qF 'interval must be a whole number' "$BODY" || fail "bad number not refused: $(head -c 300 "$BODY")"
	echo "ok   a bad number is refused"

	ui_post "$base/apps/$app/settings" "$(jq '.hc_mode = "off"' <<<"$save")"
	grep -qF 'Settings saved' "$BODY" || fail "health off: $(head -c 300 "$BODY")"
	expect_page "$base/apps/$app/settings" "&quot;hc_mode&quot;:&quot;off&quot;"
	echo "ok   health gate off"

	api DELETE "/api/v1/teams/$TEAM/projects/$project/applications/$app" >/dev/null
}
