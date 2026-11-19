# Smoke section for the P1 polish: the sidebar cookie, charts without a native tooltip, the
# scripts loaded only where used, and the save bar being clean on load.
section_polish() {
	local server page
	expect_page "/teams/$TEAM/projects" "podkit"
	expect_absent "class=\"ui-shell ui-shell-collapsed\"" "/static/charts.js"

	# The sidebar state is in a cookie and rendered on the server: no flash on load.
	curl -sS -o "$BODY" -b "$JAR" -b "sidebar=collapsed" "$URL/teams/$TEAM/projects" ||
		fail "GET projects with the sidebar cookie"
	expect_body "class=\"ui-shell ui-shell-collapsed\"" "sidebar cookie renders the icon rail"

	# A chart carries its readings in data-points, draws no <title> tooltip, and loads charts.js.
	server="$(api GET "/api/v1/teams/$TEAM/servers" | jq -er '[.[] | select(.is_local)][0].id')" ||
		fail "no local server in team $TEAM"
	expect_page "/teams/$TEAM/servers/$server/monitoring" "Metrics"
	if grep -qF 'data-points=' "$BODY"; then
		expect_body "/static/charts.js" "a page with a chart loads charts.js"
		! grep -qE '<title[ >]' <(sed -e 's/<head>.*<\/head>//' "$BODY") ||
			fail "a chart draws a native <title> tooltip"
		echo "ok   charts have data-points and no <title>"
	else
		echo "skip charts: the local server has no samples yet"
	fi

	# The save bar is in the markup of a settings form but not dirty on load.
	page="$(curl -sS -b "$JAR" "$URL/teams/$TEAM/settings")"
	grep -qF 'ui-save-bar' <<<"$page" || fail "team settings: no save bar"
	! grep -qE '[ "]data-dirty([ =>])' <<<"$page" || fail "team settings: the save bar is dirty on load"
	grep -qF '/static/guard.js' <<<"$page" || fail "team settings: guard.js is not loaded"
	echo "ok   team settings: save bar present, not dirty, guard.js loaded"

	local asset
	for asset in /static/page.js /static/charts.js /static/guard.js; do
		curl -sS -f -o /dev/null -b "$JAR" "$URL$asset" || fail "GET $asset"
		echo "ok   GET $asset"
	done
}
