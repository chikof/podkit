# Smoke sections for image scanning; sourced by scripts/ui-smoke.sh (see its header).
# The Security overview, a resource's Security tab (a database, so nothing is built or
# pulled), the nav entry, the server's scan settings and a refused ignore. A scan itself
# needs Syft and Grype images and the network: `cargo test -p agent -- --ignored scan_live`.
section_scanning() {
	local suffix="$RANDOM$RANDOM" project server db base before
	project="$(api POST "/api/v1/teams/$TEAM/projects" \
		"$(jq -n --arg s "smoke-sc-$suffix" '{name: "Smoke scan", slug: $s}')" | jq -er .id)"
	server="$(api GET "/api/v1/teams/$TEAM/servers" |
		jq -er '[.[] | select(.is_local and .status == "active")][0].id')" ||
		fail "no active local server in team $TEAM"
	base="/teams/$TEAM/projects/$project"

	# The overview and the sidebar entry.
	expect_page "/teams/$TEAM/security" "Security"
	expect_body "href=\"/teams/$TEAM/security\"" "sidebar link to the overview"
	expect_page "/teams/0/security" "Not found" 404

	# A database nobody has scanned: its tab says so, and nothing is blocked by that.
	ui_post "$base/databases" "$(jq -n --arg s "$server" --arg n "smoke-scan-$suffix" \
		'{name: $n, engine: "postgres", version: "", server_id: $s, public_port: "", memory_limit_mb: "", cpu_limit: ""}')"
	db="$(api GET "/api/v1/teams/$TEAM/projects/$project/databases" | jq -er --arg n "smoke-scan-$suffix" '.[] | select(.name == $n) | .id')"
	expect_page "$base/databases/$db/security" "Security"
	expect_body "Database sections" "database tab row"
	expect_page "/teams/$TEAM/security" "smoke-scan-$suffix"
	expect_page "$base/databases/0/security" "Not found" 404

	# The server's settings, and the limits being checked.
	expect_page "/teams/$TEAM/servers/$server" "Image scanning"
	expect_body "scan_cpus" "scan settings on the server page"
	ui_post "/teams/$TEAM/security/servers/$server" '{"scan_enabled": true, "scan_cpus": 64, "scan_memory": 1024}'
	expect_body "_scan_error" "an out-of-range CPU cap is refused"

	# An ignore needs a reason, and a refusal leaves nothing behind.
	before="$(api GET "/api/v1/teams/$TEAM/security/ignores" | jq -er 'length')"
	ui_post "/teams/$TEAM/security/ignores" \
		'{"ig_vuln": "CVE-2000-0001", "ig_pkg": "", "ig_scope": "all", "ig_reason": " ", "ig_days": "7"}'
	expect_body "_ig_error" "an ignore without a reason is refused"
	[[ "$(api GET "/api/v1/teams/$TEAM/security/ignores" | jq -er 'length')" == "$before" ]] ||
		fail "a refused ignore was stored"

	# A scan that does not exist is another team's as far as anyone can tell.
	expect_page "/teams/$TEAM/security/scans/0/sbom" "Not found" 404

	api DELETE "/api/v1/teams/$TEAM/projects/$project" >/dev/null 2>&1 || true
}
