# Smoke sections for private access through Tailscale; sourced by scripts/ui-smoke.sh.
# The settings page, the write-only key and the card on the Domains tab. No sidecar starts:
# that needs a real tailnet.
section_tailscale() {
	local project server app name="smoke-ts-$$" base
	project="$(api POST "/api/v1/teams/$TEAM/projects" "{\"name\":\"Smoke Tailscale\",\"slug\":\"smoke-ts-$$\"}" | jq -er .id)"
	server="$(api GET "/api/v1/teams/$TEAM/servers" | jq -er '.[0].id')"
	app="$(api POST "/api/v1/teams/$TEAM/projects/$project/applications" \
		"{\"name\":\"$name\",\"slug\":\"$name\",\"server_id\":$server,\"image\":\"docker.io/library/busybox\",\"container_port\":80}" | jq -er .id)"
	base="/teams/$TEAM/projects/$project/apps/$app"

	expect_page "/teams/$TEAM/settings/tailscale" "Tailscale"
	expect_page "$base/domains" "Private access (Tailscale)"

	ui_post "/teams/$TEAM/settings/tailscale" '{"ts_key":"tskey-auth-smoke","ts_login_server":"","ts_tags":"tag:podkit"}'
	expect_page "/teams/$TEAM/settings/tailscale" "tag:podkit"
	expect_absent "tskey-auth-smoke"
	[[ "$(api GET "/api/v1/teams/$TEAM/tailscale" | jq -er .auth_key_set)" == "true" ]] || fail "auth key not stored"
	api GET "/api/v1/teams/$TEAM/tailscale" | grep -q "tskey-auth-smoke" && fail "auth key was read back"
	expect_page "$base/domains" "Turn on"
}
