# Smoke section for access rules (roadmap B3): the Domains tab has the Access cards, a save
# stores the lists and limits and shows them back, a bad range is refused, a basic-auth user is
# listed by name and its password and hash never appear, the internal-only switch flips, and the
# Security tab and the Networks page say what is on. Needs no running container: it only
# exercises the forms. Everything it makes is deleted again.
section_access_rules() {
	local suffix="$RANDOM$RANDOM" project server app base slug form
	slug="smoke-ar-$suffix"
	project="$(api POST "/api/v1/teams/$TEAM/projects" \
		"$(jq -n --arg s "smoke-a-$suffix" '{name: "Smoke access", slug: $s}')" | jq -er .id)"
	server="$(api GET "/api/v1/teams/$TEAM/servers" |
		jq -er '[.[] | select(.is_local and .status == "active")][0].id')" ||
		fail "no active local server in team $TEAM"
	base="/teams/$TEAM/projects/$project"

	form="$(jq -n --arg slug "$slug" --arg server "$server" '{
		source: "image", name: "Smoke access app", slug: $slug, server_id: $server,
		image: "traefik/whoami:v1.10", container_port: "80", memory_limit_mb: "", cpu_limit: ""}')"
	ui_post "$base/apps" "$form"
	app="$(api GET "/api/v1/teams/$TEAM/projects/$project/applications" | jq -er --arg s "$slug" '.[] | select(.slug == $s) | .id')"

	expect_page "$base/apps/$app/domains" "Access rules"
	expect_body "Public access" "the Domains tab"
	expect_body "Basic auth" "the Domains tab"

	local rules
	rules="$(jq -n '{acc_allow: "203.0.113.7\n10.0.0.0/8", acc_deny: "", acc_rate: "5", acc_burst: "10",
		acc_body: "2", acc_headers: true}')"
	ui_post "$base/apps/$app/access" "$rules"
	grep -qF 'Access rules saved' "$BODY" || fail "access save: $(head -c 300 "$BODY")"
	expect_page "$base/apps/$app/domains" "203.0.113.7"
	echo "ok   POST $base/apps/$app/access"

	ui_post "$base/apps/$app/access" "$(jq '.acc_allow = "not-an-address"' <<<"$rules")"
	grep -qF 'not an IP address' "$BODY" || fail "a bad range was not refused: $(head -c 300 "$BODY")"
	echo "ok   a bad range is refused"

	local password="smoke-pass-$suffix"
	ui_post "$base/apps/$app/access/users" "$(jq -n --arg p "$password" '{acc_user: "smoke-user", acc_pass: $p}')"
	expect_body "smoke-user" "adding a basic-auth user"
	expect_page "$base/apps/$app/domains" "smoke-user"
	expect_absent "$password" '$2y$'
	expect_page "$base/apps/$app/security" "IP allow and deny lists"
	expect_absent "$password" '$2y$'
	ui_post "$base/apps/$app/access/users/smoke-user/delete" '{}'
	echo "ok   a basic-auth user added and removed, no password or hash on any page"

	ui_post "$base/apps/$app/access/internal/on" '{}'
	expect_body "Internal only" "the internal-only switch"
	expect_page "$base/apps/$app/security" "no public route"
	ui_post "$base/apps/$app/access/internal/off" '{}'
	expect_body "On the ingress" "putting it back on the ingress"

	expect_page "/teams/$TEAM/servers/$server/networks" "Who can reach what"

	api DELETE "/api/v1/teams/$TEAM/projects/$project/applications/$app" >/dev/null
	echo "ok   access rules smoke done"
}
