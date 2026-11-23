# Smoke sections for single sign-on, two-step verification and sessions; sourced by
# scripts/ui-smoke.sh (see its header).
section_auth() {
	local got
	expect_page "/teams/$TEAM/account/security" "Recovery codes"
	expect_page "/teams/$TEAM/account/security" "Sessions"
	expect_page "/teams/$TEAM/account/security" "This device"
	expect_page "/teams/$TEAM/settings/authentication" "Two-step verification"
	# A provider that does not exist cannot start a sign-in.
	got="$(curl -sS -o /dev/null -w '%{http_code}' "$URL/sso/0/start")"
	[[ "$got" == 404 ]] || fail "GET /sso/0/start: status $got, wanted 404"
	# A session token still logs in with email and password alone while no second step is on.
	api GET "/api/v1/teams" | jq -e 'type == "array"' >/dev/null || fail "the API token cannot list teams"
	echo "ok   auth: security page, authentication settings, unknown provider 404"
}
