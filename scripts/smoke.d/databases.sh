# Smoke sections for the database pages. The password never appears in a page and only the
# Reveal action carries it; every tab opens; the schedule preview lists the next three runs and
# says why a cron is wrong; delete asks for the name and keeps the data volume unless asked.
section_databases() {
	local project server db base password name="smoke-$$" env
	project="$(api POST "/api/v1/teams/$TEAM/projects" "{\"name\":\"Smoke DB\",\"slug\":\"smoke-db-$$\"}" | jq -er .id)"
	server="$(api GET "/api/v1/teams/$TEAM/servers" | jq -er '.[0].id')"
	env="$(api GET "/api/v1/teams/$TEAM/projects/$project/environments" | jq -er '.[0].id')"
	base="/teams/$TEAM/projects/$project/databases"

	expect_page "$base/new?environment=$env&engine=mysql" "Create database"
	expect_body 'value="mysql" checked' "GET $base/new (engine preselected)"
	ui_post "$base" "$(jq -n --arg n "$name" --arg s "$server" --arg e "$env" \
		'{name: $n, engine: "postgres", version: "", server_id: $s, public_port: "", memory_limit_mb: "", cpu_limit: "", environment: $e}')"
	expect_body "window.location.assign" "POST $base"
	db="$(api GET "/api/v1/teams/$TEAM/projects/$project/databases" | jq -er --arg n "$name" '.[] | select(.name == $n) | .id')"
	password="$(api GET "/api/v1/teams/$TEAM/projects/$project/databases/$db" | jq -er .connection.password)"

	expect_page "$base/$db" "Network and limits"
	! grep -qF -- "$password" "$BODY" || fail "GET $base/$db: the password is in the page"
	expect_page "$base/$db/connection" "Reveal password and URLs"
	! grep -qF -- "$password" "$BODY" || fail "GET $base/$db/connection: the password is in the page"
	echo "ok   GET $base/$db/connection has no password"
	ui_post "$base/$db/reveal" '{}'
	grep -qF -- "$password" "$BODY" || fail "POST $base/$db/reveal: no password in the reply"
	echo "ok   POST $base/$db/reveal (has the password; not printed)"
	expect_page "$base/$db/connection" "Reveal password and URLs"
	! grep -qF -- "$password" "$BODY" || fail "GET $base/$db/connection after a reveal: the password is in the page"
	expect_page "$base/$db/logs" "Container log"
	expect_page "$base/$db/backups" "Schedule (cron, UTC)"
	expect_page "$base/$db/advanced" "Danger zone"
	expect_page "/teams/$TEAM/projects/$project" "$name"

	ui_post "$base/$db/schedule" '{"backup_schedule":"61 * * * *"}'
	expect_body "Not a valid schedule" "POST $base/$db/schedule (bad cron)"
	ui_post "$base/$db/schedule" '{"backup_schedule":"0 3 * * *"}'
	expect_body "Next runs" "POST $base/$db/schedule"

	ui_post "$base/$db/delete" "$(jq -n '{confirm: "not-the-name", purge: false}')"
	expect_body "The name doesn't match." "POST $base/$db/delete (wrong name)"
	ui_post "$base/$db/delete" "$(jq -n --arg n "$name" '{confirm: $n, purge: false}')"
	expect_body "window.location.assign" "POST $base/$db/delete"
	expect_page "/teams/$TEAM/projects/$project/databases/0" "Not found" 404
}
