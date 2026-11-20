# Smoke sections for scheduled tasks; sourced by scripts/ui-smoke.sh (see its header).
# An exec task on an application through the API, its pages and the Cron tab, the preview
# of the next runs in a time zone, and a refused schedule. Nothing runs: the schedule is a
# date far away, so no container is started.
section_cron() {
	local project server app env task name="smoke-cron-$$" base
	project="$(api POST "/api/v1/teams/$TEAM/projects" "{\"name\":\"Smoke Cron\",\"slug\":\"smoke-cron-$$\"}" | jq -er .id)"
	server="$(api GET "/api/v1/teams/$TEAM/servers" | jq -er '.[0].id')"
	app="$(api POST "/api/v1/teams/$TEAM/projects/$project/applications" \
		"{\"name\":\"$name\",\"slug\":\"$name\",\"server_id\":$server,\"image\":\"docker.io/library/busybox\",\"container_port\":80}" | jq -er .id)"
	env="$(api GET "/api/v1/teams/$TEAM/projects/$project/applications" | jq -er --argjson a "$app" '.[] | select(.id == $a) | .environment_id')"
	base="/teams/$TEAM/projects/$project"

	expect_page "$base/cron" "No scheduled tasks yet."
	expect_page "$base/apps/$app/cron" "No scheduled commands."
	expect_page "$base/cron/new?environment=$env&kind=exec&application=$app" "New scheduled command"

	ui_post "$base/cron/preview" '{"schedule":"0 3 * * *","timezone":"Europe/Berlin"}'
	expect_body "Next runs, in Europe/Berlin" "POST $base/cron/preview"
	ui_post "$base/cron/preview" '{"schedule":"not cron","timezone":"UTC"}'
	expect_body "Not a valid schedule" "POST $base/cron/preview (bad expression)"

	ui_post "$base/cron" "$(jq -n --arg n "$name" --arg e "$env" --arg a "$app" \
		'{environment: $e, kind: "exec", application_id: $a, server_id: "", name: $n, image: "", command: "true", schedule: "0 0 29 2 *", timezone: "UTC", timeout_s: "60", concurrency: "skip", max_retries: "0", retry_backoff_s: "60", enabled: true}')"
	expect_body "/cron/" "POST $base/cron"
	task="$(api GET "/api/v1/teams/$TEAM/projects/$project/cron" | jq -er --arg n "$name" '.[] | select(.name == $n) | .id')"
	expect_page "$base/cron/$task" "No runs yet."
	expect_page "$base/apps/$app/cron" "$name"
	expect_page "$base/cron" "$name"

	ui_post "$base/cron/$task/pause" '{}'
	expect_page "$base/cron" "Paused"
	ui_post "$base/cron/$task/delete" '{}'
	expect_body "window.location.assign" "POST $base/cron/$task/delete"
}
