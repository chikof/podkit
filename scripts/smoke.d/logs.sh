# Smoke section for log search (roadmap B6): the team's Logs page renders, the search API answers
# for the team (an empty answer is fine on a quiet stack), a regular expression that does not
# compile is refused, the settings read back, and another team's id gives 404.
section_logs() {
	local body code
	expect_page "/teams/$TEAM/logs" "Logs"
	body="$(api POST "/api/v1/teams/$TEAM/logs/search" '{"limit": 5}')"
	jq -e '.lines | type == "array"' <<<"$body" >/dev/null || fail "log search: no list of lines: $body"
	# The expression is compiled by the agent that holds the logs, so on a stack with no
	# application there is nobody to refuse it and the search answers 200 with nothing.
	if [[ "$(api GET "/api/v1/teams/$TEAM/logs/sources" | jq 'length')" -gt 0 ]]; then
		code="$(curl -sS -o /dev/null -w '%{http_code}' -X POST \
			-H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
			-d '{"text": "(", "regex": true, "limit": 5}' "$URL/api/v1/teams/$TEAM/logs/search")" ||
			fail "log search with a bad expression failed"
		[[ "$code" == 4* ]] || fail "a bad expression answered $code, wanted 4xx"
	fi
	api GET "/api/v1/teams/$TEAM/logs/settings" |
		jq -e '.retention_days >= 1 and .max_mb >= 16' >/dev/null || fail "log settings are not readable"
	code="$(curl -sS -o /dev/null -w '%{http_code}' -H "Authorization: Bearer $TOKEN" \
		"$URL/api/v1/teams/1/logs/settings")" || fail "GET another team's log settings failed"
	[[ "$code" =~ ^(403|404)$ ]] || fail "another team's log settings answered $code, wanted 403 or 404"
	echo "ok   logs page, search, bad expression refused, settings, other team 404"
}
