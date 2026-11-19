# Smoke sections for the application pages that the older projects.sh sections don't cover.
# Creates an image application through the same signals the form posts, checks General shows
# the image and no page shows the registry password or the webhook secret, rotates the secret,
# then deletes the application.
section_apps_image() {
	local suffix="$RANDOM$RANDOM" project server app base slug form secret new_secret
	local pass="REGPASS-$(cat /proc/sys/kernel/random/uuid)"
	slug="smoke-img-$suffix"
	project="$(api POST "/api/v1/teams/$TEAM/projects" \
		"$(jq -n --arg s "smoke-i-$suffix" '{name: "Smoke image", slug: $s}')" | jq -er .id)"
	server="$(api GET "/api/v1/teams/$TEAM/servers" |
		jq -er '[.[] | select(.is_local and .status == "active")][0].id')" ||
		fail "no active local server in team $TEAM"
	base="/teams/$TEAM/projects/$project"

	expect_page "$base/apps/new?source=image" "Docker image"
	form="$(jq -n --arg slug "$slug" --arg server "$server" --arg pass "$pass" '{
		source: "image", name: "Smoke image app", slug: $slug, server_id: $server,
		image: "traefik/whoami:v1.10", registry_username: "me", registry_password: $pass,
		container_port: "80", memory_limit_mb: "", cpu_limit: ""}')"
	ui_post "$base/apps" "$form"
	grep -qF 'Application created' "$BODY" || fail "create: $(head -c 300 "$BODY")"
	secret="$(grep -o '[0-9a-f]\{48\}' "$BODY" | head -1)"
	[[ -n "$secret" ]] || fail "create: no webhook secret in the response"
	app="$(api GET "/api/v1/teams/$TEAM/projects/$project/applications" | jq -er --arg s "$slug" '.[] | select(.slug == $s) | .id')"
	echo "ok   POST $base/apps (image application $app)"

	expect_page "$base/apps/$app" "traefik/whoami:v1.10"
	expect_absent "$pass" "$secret"
	expect_page "$base/apps/$app/settings" "Registry credentials are stored."
	for tab in env domains deployments logs volumes models webhooks advanced; do
		expect_page "$base/apps/$app/$tab" "Smoke image app"
		expect_absent "$pass" "$secret"
	done

	ui_post "$base/apps/$app/webhooks/rotate" '{}'
	new_secret="$(grep -o '[0-9a-f]\{48\}' "$BODY" | head -1)"
	[[ -n "$new_secret" && "$new_secret" != "$secret" ]] || fail "rotate: no new secret"
	expect_page "$base/apps/$app/webhooks" "Rotate secret"
	expect_absent "$new_secret" "$secret"
	echo "ok   webhook secret rotated and shown once"

	ui_post "$base/apps/$app/delete" "$(jq -n --arg s "$slug" '{confirm: $s}')"
	grep -qF "window.location.assign" "$BODY" || fail "delete: no redirect"
	api DELETE "/api/v1/teams/$TEAM/projects/$project" >/dev/null 2>&1 || true
}

# The application and stack pages as one run: scripts/ui-smoke.sh apps.
section_apps() {
	section_apps_image
	section_app_config
	section_stacks
}
