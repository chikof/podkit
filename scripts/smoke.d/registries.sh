# Smoke sections for saved registries; sourced by scripts/ui-smoke.sh (see its header).
# The login test needs a registry on the network, so this only checks pages and the API.
section_registries() {
	local pass="smoke-registry-secret" id
	expect_page "/teams/$TEAM/registries" "Registries"
	expect_page "/teams/$TEAM/registries" "Add a registry"
	id="$(api POST "/api/v1/teams/$TEAM/registries" \
		"$(jq -n --arg p "$pass" '{name: "Smoke registry", url: "registry.smoke.invalid:5000", username: "robot", password: $p}')" |
		jq -er .id)"
	expect_page "/teams/$TEAM/registries" "registry.smoke.invalid:5000"
	expect_absent "$pass"
	api GET "/api/v1/teams/$TEAM/registries" | jq -e 'map(has("password")) | any | not' >/dev/null ||
		fail "the registries API returned a password field"
	api DELETE "/api/v1/teams/$TEAM/registries/$id" >/dev/null
	echo "ok   registry $id saved, listed without its password, deleted"
}
