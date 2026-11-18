# Smoke sections for the shell area; sourced by scripts/ui-smoke.sh (see its header).
# The frame around every team page: sidebar, header, command palette, settings.
section_shell() {
	# `/` redirects a signed-in user to their first team; follow it.
	FOLLOW=-L expect_page / "New project"
	expect_page "/teams/$TEAM/projects" "New project"
	expect_page "/teams/$TEAM/projects" "S3 Destinations"
	expect_page "/teams/$TEAM/projects" 'class="ui-sidebar'
	expect_page "/teams/$TEAM/projects" 'id="palette"'
	expect_page "/teams/$TEAM/projects" 'aria-label="Breadcrumb"'
	expect_page "/teams/0/projects" "Not found" 404

	# The palette's endpoint answers with a patch of its listbox, for this team only.
	expect_page "/teams/$TEAM/search?q=pro" 'id="palette-list"'
	expect_page "/teams/$TEAM/search?q=" "Create project"
	expect_page "/teams/0/search?q=pro" "Not found" 404

	# A project's header has a switcher for the project and its environment, and the
	# palette finds the project by name.
	local suffix="$RANDOM$RANDOM" project name
	name="Shell smoke $suffix"
	project="$(api POST "/api/v1/teams/$TEAM/projects" \
		"$(jq -n --arg n "$name" --arg s "smoke-shell-$suffix" '{name: $n, slug: $s}')" | jq -er .id)"
	expect_page "/teams/$TEAM/projects/$project" 'id="crumb-project" popover'
	expect_page "/teams/$TEAM/projects/$project" 'id="crumb-environment" popover'
	expect_page "/teams/$TEAM/search?q=Shell%20smoke%20$suffix" "$name"
	api DELETE "/api/v1/teams/$TEAM/projects/$project" >/dev/null 2>&1 || true

	# Settings: every tab renders, and the rename form takes the name it already has.
	expect_page "/teams/$TEAM/settings" "Team name"
	expect_page "/teams/$TEAM/settings/registration" "Sign-ups on this instance"
	expect_page "/teams/$TEAM/settings/appearance" "Theme"
	expect_page "/teams/$TEAM/members" "Settings sections"
	expect_page "/teams/$TEAM/models/settings" "Settings sections"
	expect_page "/teams/0/settings" "Not found" 404
	local current
	current="$(api GET "/api/v1/teams" | jq -er --argjson t "$TEAM" '.[] | select(.id == $t) | .name')"
	ui_post "/teams/$TEAM/settings" "$(jq -n --arg n "$current" '{name: $n}')"
	expect_body "window.location.assign" "POST /teams/$TEAM/settings"
}
