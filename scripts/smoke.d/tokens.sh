# Smoke sections for scoped API tokens; sourced by scripts/ui-smoke.sh (see its header).
section_tokens() {
	local created secret id code
	expect_page "/teams/$TEAM/settings/api-tokens" "API tokens"
	expect_page "/teams/$TEAM/settings/api-tokens" "read:sensitive"
	created="$(api POST "/api/v1/teams/$TEAM/api-tokens" \
		'{"name":"smoke","scopes":["read"],"expires_in_days":1}')"
	secret="$(jq -er .secret <<<"$created")"
	id="$(jq -er .id <<<"$created")"
	[[ "$secret" == pk_* ]] || fail "the token does not start with pk_"
	expect_page "/teams/$TEAM/settings/api-tokens" "smoke"
	expect_absent "$secret"
	# It reads...
	curl -sS -f -H "Authorization: Bearer $secret" "$URL/api/v1/teams" | jq -e 'type == "array"' >/dev/null ||
		fail "a read token could not list teams"
	# ...but cannot write, and cannot make more tokens.
	code="$(curl -sS -o /dev/null -w '%{http_code}' -X POST -H "Authorization: Bearer $secret" \
		-H 'Content-Type: application/json' -d '{"name":"x","slug":"x-smoke"}' "$URL/api/v1/teams")"
	[[ "$code" == 403 ]] || fail "a read token creating a team: status $code, wanted 403"
	code="$(curl -sS -o /dev/null -w '%{http_code}' -X POST -H "Authorization: Bearer $secret" \
		-H 'Content-Type: application/json' -d '{"name":"x","scopes":["read"]}' "$URL/api/v1/teams/$TEAM/api-tokens")"
	[[ "$code" == 403 ]] || fail "a read token making a token: status $code, wanted 403"
	# Revoked, it stops at once.
	api DELETE "/api/v1/teams/$TEAM/api-tokens/$id" >/dev/null
	code="$(curl -sS -o /dev/null -w '%{http_code}' -H "Authorization: Bearer $secret" "$URL/api/v1/teams")"
	[[ "$code" == 401 ]] || fail "a revoked token: status $code, wanted 401"
	echo "ok   token $id: read allowed, write and token-making 403, revoked 401"
}
