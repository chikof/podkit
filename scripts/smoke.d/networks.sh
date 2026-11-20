# Smoke sections for the networks area; sourced by scripts/ui-smoke.sh (see its header).
section_networks() {
	local local_id
	local_id="$(api GET "/api/v1/teams/$TEAM/servers" | jq -er '[.[] | select(.is_local)][0].id')" ||
		fail "no local server in team $TEAM"
	expect_page "/teams/$TEAM/servers/$local_id/networks" "Networks"
	expect_body "Used by" "networks table"
	expect_body "new-network" "new network dialog"
	expect_page "/teams/$TEAM/servers/0/networks" "Not found" 404
	# The API lists the same networks.
	api GET "/api/v1/teams/$TEAM/servers/$local_id/networks" | jq -e 'type == "array"' >/dev/null ||
		fail "GET /servers/$local_id/networks is not a list"
	echo "ok   networks pages and API"
}
