# Smoke sections for the server firewall; sourced by scripts/ui-smoke.sh (see its header).
# The helper is root-only, so this checks the page and the refusal paths, never an apply.
section_firewall() {
	local local_id
	local_id="$(api GET "/api/v1/teams/$TEAM/servers" | jq -er '[.[] | select(.is_local)][0].id')" ||
		fail "no local server in team $TEAM"
	expect_page "/teams/$TEAM/servers/$local_id/firewall" "Firewall"
	expect_body "Always open" "the always-open list"
	expect_body "Explicit allows" "the allows section"
	expect_page "/teams/$TEAM/servers/0/firewall" "Not found" 404
	echo "ok   firewall page"
}
