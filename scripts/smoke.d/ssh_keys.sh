# Smoke sections for the SSH key vault; sourced by scripts/ui-smoke.sh (see its header).
# Generates a key through the same signals the Keys page posts, checks the public half is shown
# and no page or API response carries a private key, then deletes it.
section_ssh_keys() {
	local suffix="$RANDOM$RANDOM" name id base
	name="smoke-key-$suffix"
	base="/teams/$TEAM/keys"

	expect_page "$base" "Keys"
	expect_absent "BEGIN OPENSSH" "PRIVATE KEY"

	ui_post "$base/generate" "$(jq -n --arg n "$name" '{gen_name: $n, gen_algorithm: "ed25519"}')"
	expect_body "ssh-ed25519 " "POST $base/generate"
	expect_absent "BEGIN OPENSSH" "PRIVATE KEY"
	id="$(api GET "/api/v1/teams/$TEAM/ssh-keys" | jq -er --arg n "$name" '.[] | select(.name == $n) | .id')" ||
		fail "generated key $name is not in the API list"
	echo "ok   key $id generated"

	expect_page "$base" "$name"
	expect_absent "BEGIN OPENSSH" "PRIVATE KEY"
	api GET "/api/v1/teams/$TEAM/ssh-keys/$id" | grep -q "PRIVATE KEY" &&
		fail "GET ssh-keys/$id: a private key marker is in the response"
	echo "ok   no private key in the API response"

	ui_post "$base/$id/delete" '{}'
	api GET "/api/v1/teams/$TEAM/ssh-keys" | jq -e --arg n "$name" 'any(.[]; .name == $n) | not' >/dev/null ||
		fail "key $name was not deleted"
	echo "ok   key $id deleted"
}
