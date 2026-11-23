# Smoke section for uptime; sourced by scripts/ui-smoke.sh (see its header).
# Makes a TCP check against podkit's own listener (no outside request), publishes the status page
# and reads it back without a session: the page lists the check by its display name and shows
# no address. Everything it makes is deleted again.
section_uptime() {
	local slug id host port
	host="${URL#*://}"
	port="${host##*:}"
	host="${host%%:*}"
	[[ "$port" =~ ^[0-9]+$ ]] || port=80
	slug="$(api GET "/api/v1/teams/$TEAM/uptime/status-page" | jq -er .slug)"
	expect_page "/teams/$TEAM/uptime" "Uptime"
	expect_page "/teams/$TEAM/uptime" "Status page"
	id="$(api POST "/api/v1/teams/$TEAM/uptime/checks" \
		"$(jq -n --arg h "$host" --argjson p "$port" \
			'{name: "Smoke check", kind: "tcp", host: $h, port: $p, interval_secs: 30,
			  status_visible: true, status_name: "Smoke service"}')" | jq -er .id)"
	expect_page "/teams/$TEAM/uptime/$id" "Smoke check"
	api PUT "/api/v1/teams/$TEAM/uptime/status-page" '{"enabled": true, "title": "Smoke status"}' >/dev/null
	local got
	got="$(curl -sS -o "$BODY" -w '%{http_code}' "$URL/status/$slug")" || fail "GET /status/$slug failed"
	[[ "$got" == "200" ]] || fail "GET /status/$slug: status $got"
	grep -qF 'data-check' "$BODY" || fail "the status page lists no check"
	grep -qF 'Smoke service' "$BODY" || fail "the status page misses the display name"
	expect_absent "Smoke check" "$host:$port" "<script"
	api PUT "/api/v1/teams/$TEAM/uptime/status-page" '{"enabled": false}' >/dev/null
	got="$(curl -sS -o /dev/null -w '%{http_code}' "$URL/status/$slug")"
	[[ "$got" == "404" ]] || fail "a switched-off status page answered $got"
	api DELETE "/api/v1/teams/$TEAM/uptime/checks/$id" >/dev/null
	echo "ok   uptime check $id made, listed on the public status page without its address, deleted"
}
