# Smoke sections for the template gallery and the compose stack pages; sourced by
# scripts/ui-smoke.sh (see its header). Both deploy for real: one small single-service
# template (gotify, an image app) and one 2-service template (umami + postgres), and both are
# removed again at the end.

# Looks up the first server of the team and the production environment of project $1.
smoke_target() {
	SMOKE_SERVER="$(api GET "/api/v1/teams/$TEAM/servers" | jq -er '.[0].id')"
	SMOKE_ENV="$(api GET "/api/v1/teams/$TEAM/projects/$1/environments" | jq -er '.[0].id')"
}

# A project of its own, so a failed run leaves one thing to delete.
smoke_project() {
	local slug="$1"
	ui_post "/teams/$TEAM/projects" "$(jq -n --arg n "$slug" --arg s "$slug" '{name: $n, slug: $s}')"
	expect_body "window.location.assign" "POST /teams/$TEAM/projects" >&2
	api GET "/api/v1/teams/$TEAM/projects" | jq -er --arg s "$slug" '.[] | select(.slug == $s) | .id'
}

section_templates() {
	local gallery="/teams/$TEAM/templates" slug project target
	slug="smoke-tpl-$RANDOM$RANDOM"

	# The gallery: one page of cards, categories, search, a category, a template.
	expect_page "$gallery" "Search templates"
	expect_page "$gallery" "data-grid-nav"
	expect_page "$gallery" "loading=\"lazy\""
	expect_page "$gallery?q=gotify" "Gotify"
	expect_absent "Umami"
	expect_page "$gallery?q=%3Cscript%3E" "No template matches"
	expect_absent "<script>alert"
	expect_page "$gallery?category=database" "aria-current=\"page\""
	expect_page "$gallery/gotify" "Variables"
	expect_page "$gallery/no-such-template" "Not found" 404
	expect_page "/teams/0/templates" "Not found" 404

	# Deploy a single-service template: an application, and its deployment page.
	project="$(smoke_project "$slug")"
	smoke_target "$project"
	ui_post "$gallery/gotify/deploy" "$(jq -n --arg p "$project" --arg e "$SMOKE_ENV" --arg s "$SMOKE_SERVER" \
		'{project: $p, environment: $e, server: $s, name: "", v0: "", v1: ""}')"
	expect_body "/projects/$project/apps/" "POST $gallery/gotify/deploy"
	target="$(grep -oE "assign\('[^']+" "$BODY" | head -1 | sed "s/assign('//")"
	[[ -n "$target" ]] || fail "gotify deploy gave no redirect"
	local app_page="${target%%/deployments/*}"
	wait_for "$app_page" "Running" 900

	# Deploy a two-service template: a stack with two services running.
	ui_post "$gallery/umami/deploy" "$(jq -n --arg p "$project" --arg e "$SMOKE_ENV" --arg s "$SMOKE_SERVER" \
		'{project: $p, environment: $e, server: $s, name: ""}')"
	expect_body "/projects/$project/stacks/" "POST $gallery/umami/deploy"
	target="$(grep -oE "assign\('[^']+" "$BODY" | head -1 | sed "s/assign('//")"
	[[ -n "$target" ]] || fail "umami deploy gave no redirect"
	local stack_page="${target%/deployments}"
	wait_for "$stack_page" "Running" 900

	# Both are cards in the environment.
	expect_page "/teams/$TEAM/projects/$project" "Gotify"
	expect_page "/teams/$TEAM/projects/$project" "data-kind=\"stack\""

	# Remove both, then the project.
	local stack_id="${stack_page##*/}"
	ui_post "$stack_page/remove" "$(jq -n '{confirm: "Umami", purge: true}')"
	api DELETE "/api/v1/teams/$TEAM/projects/$project/applications/${app_page##*/}" >/dev/null 2>&1 || true
	wait_for "/teams/$TEAM/projects/$project" "Nothing in production yet" 300 || true
	: "$stack_id"
}

section_stacks() {
	local slug project base stack compose
	slug="smoke-stk-$RANDOM$RANDOM"
	project="$(smoke_project "$slug")"
	smoke_target "$project"
	base="/teams/$TEAM/projects/$project/stacks"

	expect_page "$base/new" "Check file"
	expect_page "/teams/$TEAM/projects/0/stacks/new" "Not found" 404

	# A file podkit cannot run: every refused key is named, nothing is stored.
	compose=$'services:\n  web:\n    image: nginx\n    build: .\n'
	ui_post "$base/validate" "$(jq -n --arg c "$compose" --arg s "$SMOKE_SERVER" --arg e "$SMOKE_ENV" \
		'{name: "x", server: $s, environment: $e, compose: $c, envText: "", deploy: false}')"
	expect_body "services.web.build" "POST $base/validate (build)"

	# A good one: create, look at each tab, deploy, remove.
	compose=$'services:\n  web:\n    image: traefik/whoami:v1.10\n  other:\n    image: traefik/whoami:v1.10\n'
	ui_post "$base" "$(jq -n --arg c "$compose" --arg s "$SMOKE_SERVER" --arg e "$SMOKE_ENV" \
		'{name: "Smoke", server: $s, environment: $e, compose: $c, envText: "", deploy: true}')"
	expect_body "/deployments" "POST $base"
	stack="$(grep -oE "stacks/[0-9]+" "$BODY" | head -1 | cut -d/ -f2)"
	[[ -n "$stack" ]] || fail "create gave no stack id"
	for tab in "" /compose /env /domains /deployments /logs /advanced; do
		expect_page "$base/$stack$tab" "Smoke"
	done
	wait_for "$base/$stack" "Running" 600
	expect_page "$base/$stack/logs" "stack-log"

	ui_post "$base/$stack/remove" '{"confirm": "Smoke", "purge": true}'
	expect_body "window.location.assign" "POST $base/$stack/remove"
	api DELETE "/api/v1/teams/$TEAM/projects/$project" >/dev/null 2>&1 || true
}
