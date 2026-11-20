# Smoke section for the richer charts: an application's Monitoring tab shows the event counters
# and the CPU and memory charts, its extra-series endpoint answers JSON with the three lists,
# and the Infrastructure overview loads with the "Busiest resources" section either present or
# absent (it only appears once something has a figure). Creates and deletes a project.
section_charts() {
	local suffix="$RANDOM$RANDOM" project server app base
	project="$(api POST "/api/v1/teams/$TEAM/projects" \
		"$(jq -n --arg s "smoke-c-$suffix" '{name: "Smoke charts", slug: $s}')" | jq -er .id)"
	server="$(api GET "/api/v1/teams/$TEAM/servers" |
		jq -er '[.[] | select(.is_local and .status == "active")][0].id')" ||
		fail "no active local server in team $TEAM"
	app="$(api POST "/api/v1/teams/$TEAM/projects/$project/applications" \
		"$(jq -n --arg s "charts-$suffix" --argjson server "$server" \
			'{name: "charts", slug: $s, server_id: $server, repo_url: "https://github.com/traefik/whoami", git_ref: "master", container_port: 80}')" |
		jq -er .id)"
	base="/teams/$TEAM/projects/$project/apps/$app"

	expect_page "$base/monitoring" "Crashes"
	expect_page "$base/monitoring" "Out-of-memory kills"
	api GET "/api/v1/teams/$TEAM/projects/$project/applications/$app/metrics/extra?range=1h" |
		jq -e '(.points | type == "array") and (.events | type == "array") and (.deploys | type == "array")' >/dev/null ||
		fail "extra metrics are not the three lists"
	expect_page "/teams/$TEAM/infrastructure" "Infrastructure"

	api DELETE "/api/v1/teams/$TEAM/projects/$project/applications/$app" >/dev/null || true
	api DELETE "/api/v1/teams/$TEAM/projects/$project" >/dev/null || true
}
