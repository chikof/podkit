# Smoke section for the projects grid, the environment view and its actions; sourced by
# scripts/ui-smoke.sh (see its header). Creates a project, works its environments through
# the same signals the dialogs post, and deletes it.
section_projects() {
	local suffix="$RANDOM$RANDOM" project server slug name base list app
	slug="smoke-grid-$suffix"
	name="Smoke grid $suffix"
	list="/teams/$TEAM/projects"

	expect_page "$list" "New project"
	expect_page "$list" 'id="project-create"'
	expect_page "/teams/0/projects" "Not found" 404

	# Create: a bad slug is said inline, a good one redirects to the new project.
	ui_post "$list" "$(jq -n --arg n "$name" '{name: $n, slug: "Not A Slug"}')"
	expect_body "Slug must be" "POST $list (bad slug)"
	ui_post "$list" "$(jq -n --arg n "$name" --arg s "$slug" '{name: $n, slug: $s}')"
	expect_body "window.location.assign" "POST $list"
	project="$(api GET "/api/v1/teams/$TEAM/projects" | jq -er --arg s "$slug" '.[] | select(.slug == $s) | .id')"
	base="$list/$project"

	# The grid has a card that leads to the default environment.
	expect_page "$list" "$name"
	expect_page "$list" "href=\"$base/production\""
	expect_page "$list" "Nothing deployed yet"

	# The environment view, by default and by slug; the picker is on it.
	expect_page "$base" "Nothing in production yet"
	expect_page "$base/production" "Create resource"
	expect_page "$base/production" "href=\"$base/production/create\""
	expect_page "$base/production/create" "Step 1 of 2"
	expect_page "$base/production/create" "$base/apps/new"
	expect_page "$base/production/create" "$base/databases/new"
	expect_page "$base/nope/create" "Not found" 404
	expect_page "$base/nope" "Not found" 404
	expect_page "$base?environment=0" "Not found" 404

	# Environments: create, a reserved or taken slug is refused, rename keeps the slug.
	ui_post "$base/environments" '{"newName": "Apps", "newSlug": "apps"}'
	expect_body "is reserved" "POST $base/environments (reserved slug)"
	ui_post "$base/environments" '{"newName": "Again", "newSlug": "production"}'
	expect_body "already exists" "POST $base/environments (taken slug)"
	ui_post "$base/environments" '{"newName": "Staging", "newSlug": "staging"}'
	expect_body "$base/staging" "POST $base/environments"
	expect_page "$base/staging" "Nothing in Staging yet"
	expect_page "$base/production" "href=\"$base/staging\""
	local staging
	staging="$(api GET "/api/v1/teams/$TEAM/projects/$project/environments" | jq -er '.[] | select(.slug == "staging") | .id')"
	local production
	production="$(api GET "/api/v1/teams/$TEAM/projects/$project/environments" | jq -er '.[] | select(.slug == "production") | .id')"
	ui_post "$base/environments/rename" "$(jq -n --arg i "$staging" '{envId: $i, envName: "Pre-prod"}')"
	expect_body "$base/staging" "POST $base/environments/rename"
	expect_page "$base/staging" "Pre-prod"

	# A resource in production makes deleting production refuse, naming it, inline.
	server="$(api GET "/api/v1/teams/$TEAM/servers" |
		jq -er '[.[] | select(.is_local and .status == "active")][0].id')" ||
		fail "no active local server in team $TEAM"
	ui_post "$base/apps" "$(jq -n --arg slug "$slug-app" --arg server "$server" '{
		name: "Grid app", slug: $slug, server_id: $server,
		repo_url: "https://example.com/smoke.git", git_ref: "main", deploy_key: "",
		git_token: "", build_strategy: "dockerfile", dockerfile_path: "Dockerfile",
		container_port: "3000", memory_limit_mb: "", cpu_limit: ""}')"
	app="$(api GET "/api/v1/teams/$TEAM/projects/$project/applications" | jq -er '.[0].id')"
	expect_page "$base/production" "Grid app"
	expect_page "$base/production" "$base/apps/$app"
	expect_page "$base/staging" "Nothing in Pre-prod yet"

	# The filter fragment answers a patch of the grid, narrowed by the signals.
	local q
	q="$(jq -rn --arg s '{"q":"grid","type":"application"}' '$s|@uri')"
	expect_page "$base/production/resources?datastar=$q" 'id="resource-grid"'
	expect_page "$base/production?type=database" "Nothing matches"

	ui_post "$base/environments/delete" "$(jq -n --arg i "$production" '{envId: $i}')"
	expect_body "still has resources" "POST $base/environments/delete (not empty)"
	expect_body "Grid app" "the refusal names the resource"
	ui_post "$base/environments/delete" "$(jq -n --arg i "$staging" '{envId: $i}')"
	expect_body "window.location.assign" "POST $base/environments/delete (empty)"
	ui_post "$base/environments/delete" "$(jq -n --arg i "$production" '{envId: $i}')"
	expect_body "at least one environment" "POST $base/environments/delete (last)"

	# Rename and delete a project from the card's dialogs.
	ui_post "$list/rename" "$(jq -n --arg i "$project" '{target: $i, name: "Renamed grid"}')"
	expect_body "window.location.assign" "POST $list/rename"
	expect_page "$list" "Renamed grid"
	ui_post "$list/delete" "$(jq -n --arg i "$project" '{target: $i, confirm: "wrong"}')"
	expect_body "doesn't match" "POST $list/delete (wrong slug)"
	ui_post "$list/delete" "$(jq -n --arg i "$project" --arg s "$slug" '{target: $i, confirm: $s}')"
	expect_body "window.location.assign" "POST $list/delete"
	expect_page "$base" "Not found" 404
}

# The Quiet shapes of the grid and the environment: tiles, rows, the filter fragment's
# change marker, and the picker's radio group.
section_projects_grid() {
	local suffix="$RANDOM$RANDOM" project slug name base list q
	slug="smoke-tiles-$suffix"
	name="Smoke tiles $suffix"
	list="/teams/$TEAM/projects"
	project="$(api POST "/api/v1/teams/$TEAM/projects" "$(jq -n --arg n "$name" --arg s "$slug" '{name: $n, slug: $s}')" | jq -er .id)"
	base="$list/$project"

	expect_page "$list" 'id="project-grid"'
	expect_page "$list" 'class="ui-card ui-link-card ui-proj"'
	expect_page "$list" "Never deployed"
	expect_page "$list" "Team summary"

	expect_page "$base/production" "0 resources in production"
	expect_page "$base/production/create" 'name="rtype"'
	expect_page "$base/production/create" 'data-panel="git"'
	expect_page "$base/production/create" 'data-panel="model"'

	# The fragment answers with the grid, given what the page last showed.
	q="$(jq -rn '{q: "", type: "all", seen: "application:1=building"} | tojson | @uri')"
	expect_page "$base/production/resources?datastar=$q" 'id="resource-grid"'

	api DELETE "/api/v1/teams/$TEAM/projects/$project" >/dev/null 2>&1 || true
}
