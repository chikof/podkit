# Smoke sections for the servers area; sourced by scripts/ui-smoke.sh (see its header).
section_servers() {
	expect_page "/teams/$TEAM/servers" "Add a server"
	expect_page "/teams/$TEAM/servers" "Local"
	local local_id
	local_id="$(api GET "/api/v1/teams/$TEAM/servers" | jq -er '[.[] | select(.is_local)][0].id')" ||
		fail "no local server in team $TEAM"
	expect_page "/teams/$TEAM/servers/$local_id" "Architecture"
	expect_page "/teams/$TEAM/servers/0" "Not found" 404
	# The key is write-only: no page may carry one.
	grep -qF -- "BEGIN" "$BODY" && fail "GET /teams/$TEAM/servers/0: a private key marker is in the page"
	curl -sS -b "$JAR" "$URL/teams/$TEAM/servers" | grep -qF -- "BEGIN" &&
		fail "GET /teams/$TEAM/servers: a private key marker is in the page"
	echo "ok   no private key in the server pages"

	# The stepped add dialog is on the list page and the key field starts empty.
	expect_page "/teams/$TEAM/servers" "add-step-2"
	expect_body "ssh_private_key: ''" "GET /teams/$TEAM/servers"

	# Monitoring: the page, all ranges (one unknown, which falls back), the refresh.
	expect_page "/teams/$TEAM/servers/$local_id/monitoring" "Monitoring"
	expect_body 'aria-label="Time range"' "monitoring page"
	expect_body 'document.hidden || @get(' "monitoring page polls only while visible"
	local range
	for range in 1h 24h 7d forever; do
		expect_page "/teams/$TEAM/servers/$local_id/monitoring?range=$range" "Updated"
	done
	curl -sS -f -b "$JAR" -H 'datastar-request: true' -o "$BODY" \
		"$URL/teams/$TEAM/servers/$local_id/monitoring/charts?range=1h" ||
		fail "GET monitoring/charts"
	expect_body 'id="metrics-body"' "monitoring refresh"
	expect_page "/teams/$TEAM/servers/0/monitoring" "Not found" 404
}
