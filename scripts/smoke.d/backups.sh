# Smoke sections for the backups area; sourced by scripts/ui-smoke.sh (see its header).
# Backup targets through the form (the secret key never comes back in a page),
# then a database's Backups tab. Taking and restoring a real backup needs S3;
# see the card's MinIO proof.
section_backups() {
	local project server db base name="smoke-bk-$$" secret="smoke-secret-$$" target
	local targets="/teams/$TEAM/backup-targets"

	ui_post "$targets" "$(jq -n --arg n "$name" --arg s "$secret" \
		'{name: $n, endpoint: "http://localhost:9000", region: "us-east-1", bucket: "podkit", prefix: "", path_style: true, access_key_id: "minio", secret_access_key: $s}')"
	expect_body "window.location.assign" "POST $targets"
	expect_page "$targets" "$name"
	! grep -qF -- "$secret" "$BODY" || fail "GET $targets: the secret key is in the page"
	echo "ok   GET $targets has no secret key"
	target="$(api GET "/api/v1/teams/$TEAM/backup-targets" | jq -er --arg n "$name" '.[] | select(.name == $n) | .id')"

	project="$(api POST "/api/v1/teams/$TEAM/projects" "{\"name\":\"Smoke BK\",\"slug\":\"smoke-bk-$$\"}" | jq -er .id)"
	server="$(api GET "/api/v1/teams/$TEAM/servers" | jq -er '.[0].id')"
	db="$(api POST "/api/v1/teams/$TEAM/projects/$project/databases" \
		"{\"name\":\"$name\",\"engine\":\"postgres\",\"server_id\":$server}" | jq -er .id)"
	base="/teams/$TEAM/projects/$project/databases/$db"
	expect_page "$base/backups" "No backups yet."
	expect_page "$base/settings" "Add a backup target"
	ui_post "$base/backups" '{}'
	expect_body "configure a backup target first" "POST $base/backups (no target yet)"
	ui_post "$base/backups/1/restore" '{"confirm":"wrong"}'
	expect_body "The name doesn't match." "POST $base/backups/1/restore (no typed name)"

	ui_post "$base/delete" "$(jq -n --arg n "$name" '{confirm: $n, purge: false}')"
	ui_post "$targets/$target/delete" '{}'
	expect_body "window.location.assign" "POST $targets/$target/delete"
}
