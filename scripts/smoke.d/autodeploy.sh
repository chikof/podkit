# Smoke section for autodeploy (roadmap B1): a git application's Git tab shows the rules, a save
# stores them and shows them back, a bad timeout is refused, and a recurring freeze window is added.
# Needs no git host: it only exercises the forms.
section_autodeploy() {
	local suffix="$RANDOM$RANDOM" project server app base slug rules freeze
	slug="smoke-ad-$suffix"
	project="$(api POST "/api/v1/teams/$TEAM/projects" \
		"$(jq -n --arg s "smoke-ad-$suffix" '{name: "Smoke autodeploy", slug: $s}')" | jq -er .id)"
	server="$(api GET "/api/v1/teams/$TEAM/servers" |
		jq -er '[.[] | select(.is_local and .status == "active")][0].id')" ||
		fail "no active local server in team $TEAM"
	app="$(api POST "/api/v1/teams/$TEAM/projects/$project/applications" \
		"$(jq -n --arg s "$slug" --argjson server "$server" \
			'{name: "Smoke autodeploy", slug: $s, server_id: $server, repo_url: "https://example.com/smoke.git", git_ref: "main", container_port: 80}')" |
		jq -er .id)"
	base="/teams/$TEAM/projects/$project/apps/$app/git"

	expect_page "$base" "Autodeploy"
	echo "ok   GET $base"

	rules="$(jq -n '{ad_enabled: true, ad_branches: "main\nrelease/*", ad_tags: false,
		ad_paths: "api/**\n!docs/**", ad_wait: true, ad_timeout: 15, ad_anyway: false, ad_status: true}')"
	ui_post "$base" "$rules"
	grep -qF 'Autodeploy saved' "$BODY" || fail "rules save: $(head -c 300 "$BODY")"
	expect_page "$base" "release/*"
	echo "ok   POST $base (rules saved and shown back)"

	ui_post "$base" "$(jq '.ad_timeout = "abc"' <<<"$rules")"
	grep -qF 'Autodeploy saved' "$BODY" && fail "a bad timeout was saved"
	echo "ok   a bad timeout is refused"

	freeze="$(jq -n '{fz_kind: "recurring", fz_sday: 4, fz_stime: "16:00", fz_eday: 0, fz_etime: "08:00",
		fz_offset: 0, fz_from: "", fz_to: "", fz_note: "weekend"}')"
	ui_post "$base/freezes" "$freeze"
	expect_page "$base" "weekend"
	echo "ok   POST $base/freezes (a recurring freeze)"

	api DELETE "/api/v1/teams/$TEAM/projects/$project/applications/$app" >/dev/null
	api DELETE "/api/v1/teams/$TEAM/projects/$project" >/dev/null || true
}
