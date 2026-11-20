# Smoke sections for notifications; sourced by scripts/ui-smoke.sh (see its header).
# Nothing is sent anywhere: the channel points at an address nobody listens on, and the
# section checks the pages, the write-only secret and the bell.
section_notifications() {
	local secret="smoke-notify-secret-token" id rule
	expect_page "/teams/$TEAM/settings/notifications" "Notifications"
	expect_page "/teams/$TEAM/settings/notifications" "Add a channel"
	expect_page "/teams/$TEAM/notifications" "Notifications"
	id="$(api POST "/api/v1/teams/$TEAM/notifications/channels" \
		"$(jq -n --arg s "$secret" '{name: "Smoke channel", kind: "discord", url: ("http://127.0.0.1:9/api/webhooks/1/" + $s)}')" |
		jq -er .id)"
	expect_page "/teams/$TEAM/settings/notifications" "Smoke channel"
	expect_absent "$secret"
	api GET "/api/v1/teams/$TEAM/notifications/channels" | grep -q "$secret" &&
		fail "the channels API returned a secret"
	rule="$(api POST "/api/v1/teams/$TEAM/notifications/rules" \
		"$(jq -n --argjson c "$id" '{name: "Smoke rule", kinds: ["deploy_failed"], channel_ids: [$c]}')" |
		jq -er .id)"
	api GET "/api/v1/teams/$TEAM/notifications/feed" | jq -e '.items | type == "array"' >/dev/null ||
		fail "the notification feed is not a list"
	api DELETE "/api/v1/teams/$TEAM/notifications/rules/$rule" >/dev/null
	api DELETE "/api/v1/teams/$TEAM/notifications/channels/$id" >/dev/null
	echo "ok   notification channel $id saved without exposing its secret, rule $rule added and deleted"
}
