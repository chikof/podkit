# Smoke sections for the MCP server area; sourced by scripts/ui-smoke.sh (see its header).
section_mcp() {
	local project
	project="$(api GET "/api/v1/teams/$TEAM/projects" | jq -r '.[0].id // empty')"
	if [[ -z "$project" ]]; then
		project="$(api POST "/api/v1/teams/$TEAM/projects" '{"name":"Smoke","slug":"smoke"}' | jq -er .id)"
	fi
	expect_page "/teams/$TEAM/projects/$project/mcp/new" "New MCP server"
	expect_page "/teams/$TEAM/projects/$project/mcp/0" "Not found" 404
	api GET "/api/v1/teams/$TEAM/projects/$project/mcp-servers" | jq -e 'type == "array"' >/dev/null ||
		fail "the MCP server list is not an array"
	echo "ok   MCP servers listed"
}
