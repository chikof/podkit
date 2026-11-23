# Smoke section for quotas and usage; sourced by scripts/ui-smoke.sh (see its header).
# Reads usage and limits through the API and the page, exports the CSV and checks that a team
# that does not exist is a 404. It changes no limit.
section_quotas() {
	local kinds got
	kinds="$(api GET "/api/v1/teams/$TEAM/usage" | jq -er '.quotas | length')"
	[[ "$kinds" -ge 11 ]] || fail "usage lists $kinds quotas, wanted every kind"
	api GET "/api/v1/teams/$TEAM/usage/history" | jq -e . >/dev/null || fail "usage history is not JSON"
	expect_page "/teams/$TEAM/settings/usage" "Usage &amp; limits"
	expect_page "/teams/$TEAM/settings/usage" "/export?grain=hour"
	got="$(curl -sS -o "$BODY" -w '%{http_code}' -H "Authorization: Bearer $TOKEN" "$URL/api/v1/teams/0/usage")"
	[[ "$got" == 404 ]] || fail "usage of a team that does not exist answered $got"
	got="$(curl -sS -o "$BODY" -w '%{http_code}' -b "$JAR" "$URL/teams/$TEAM/settings/usage/export")"
	[[ "$got" == 200 ]] || fail "usage export answered $got"
	echo "ok   usage, limits and CSV export"
}
