# Smoke sections for the project canvas; sourced by scripts/ui-smoke.sh (see its header).
section_canvas() {
	local suffix="$RANDOM$RANDOM" project server base
	server="$(api GET "/api/v1/teams/$TEAM/servers" | jq -er '[.[] | select(.is_local)][0].id')" ||
		fail "no local server in team $TEAM"
	project="$(api POST "/api/v1/teams/$TEAM/projects" \
		"$(jq -n --arg n "canvas-$suffix" '{name: $n, slug: $n}')" | jq -er .id)"
	base="/teams/$TEAM/projects/$project/canvas"
	# Empty project: the empty state, no canvas script.
	expect_page "$base" "Nothing to connect yet"
	api POST "/api/v1/teams/$TEAM/projects/$project/databases" \
		"$(jq -n --argjson s "$server" '{name: "db", engine: "postgres", server_id: $s}')" >/dev/null
	api POST "/api/v1/teams/$TEAM/projects/$project/applications" \
		"$(jq -n --argjson s "$server" '{name: "web", slug: "web", server_id: $s, repo_url: "https://github.com/acme/x", git_ref: "main", container_port: 3000}')" >/dev/null
	expect_page "$base" "Project map"
	expect_body "id=\"cv-world\"" "the canvas world"
	expect_body "/static/canvas.js" "the canvas script"
	# A project that does not exist is a 404.
	expect_page "/teams/$TEAM/projects/0/canvas" "Not found" 404
	echo "ok   canvas page"
}
