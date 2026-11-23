# Smoke sections for the importers (Coolify, Dokploy, CapRover); sourced by scripts/ui-smoke.sh.
# There is no source platform to read in a smoke run, so this checks the doors: the page and
# its nav link open, an unknown platform is a 404, and a source that cannot be reached is a
# clear error that does not echo the credential and makes nothing. The reading and mapping
# are tested against recorded responses (cargo test -- importers).
section_importers() {
	local secret="smoke-secret-$$" out code projects_before projects_after
	projects_before="$(api GET "/api/v1/teams/$TEAM/projects" | jq 'length')"

	expect_page "/teams/$TEAM/settings/import" "Import"
	expect_page "/teams/$TEAM/settings/import" "Read and plan"
	echo "ok   Settings -> Import opens"

	code="$(curl -sS -o /dev/null -w '%{http_code}' -X POST \
		-H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
		-d '{"url":"http://127.0.0.1:9","token":"x"}' \
		"$URL/api/v1/teams/$TEAM/importers/heroku/plan")"
	[[ "$code" == 404 ]] || fail "an unknown platform answered $code, not 404"
	echo "ok   an unknown platform is a 404"

	for source in coolify dokploy caprover; do
		out="$(curl -sS -X POST -H "Authorization: Bearer $TOKEN" \
			-H 'Content-Type: application/json' \
			-d "$(jq -n --arg t "$secret" '{url: "http://127.0.0.1:9", token: $t}')" \
			-w '\n%{http_code}' "$URL/api/v1/teams/$TEAM/importers/$source/plan")"
		code="${out##*$'\n'}"
		[[ "$code" =~ ^(400|422|502|504)$ ]] || fail "$source: an unreachable source answered $code"
		if grep -q "$secret" <<<"$out"; then
			fail "$source: the credential came back in the answer"
		fi
	done
	echo "ok   an unreachable source is an error that does not echo the credential"

	projects_after="$(api GET "/api/v1/teams/$TEAM/projects" | jq 'length')"
	[[ "$projects_before" == "$projects_after" ]] || fail "a failed import made a project"
	echo "ok   a failed import makes nothing"
}
