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
}
