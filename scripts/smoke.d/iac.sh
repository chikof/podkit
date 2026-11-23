# Smoke sections for infrastructure as code; sourced by scripts/ui-smoke.sh (see its header).
# Exports a project, plans the file back (no changes), applies it twice (a no-op), checks
# that prune is off by default, and opens the pages. Needs no Podman: nothing is deployed.
section_iac() {
	local project server dir file out slug
	dir="$(mktemp -d)"
	slug="iac-smoke-$$"
	server="$(api GET "/api/v1/teams/$TEAM/servers" | jq -er '.[0].name')"
	project="$(api POST "/api/v1/teams/$TEAM/projects" \
		"$(jq -n --arg s "$slug" '{name: "IaC smoke", slug: $s}')" | jq -er .id)"
	file="$dir/podkit.toml"
	cat >"$file" <<EOF
version = 1

[projects.$slug]
name = "IaC smoke"

[projects.$slug.environments.production]
name = "production"

[projects.$slug.environments.production.apps.web]
server = "$server"
slug = "$slug-web"
image = "nginx:1"
port = 8080
EOF
	local base="/api/v1/teams/$TEAM/projects/$project"
	local post=(curl -sS -f -X POST -H "Authorization: Bearer $TOKEN" --data-binary)

	out="$("${post[@]}" "@$file" "$URL$base/plan")" || fail "POST $base/plan"
	jq -e '.changes | length > 0' <<<"$out" >/dev/null || fail "the plan of a new file has no changes"
	echo "ok   plan of a new file: $(jq -r '[.changes[] | .action] | join(",")' <<<"$out")"

	"${post[@]}" "@$file" "$URL$base/apply" >/dev/null || fail "POST $base/apply"
	"${post[@]}" "@$file" "$URL$base/apply" >/dev/null || fail "POST $base/apply (again)"
	echo "ok   apply made the app, and applying again is accepted"
	curl -sS -f -H "Authorization: Bearer $TOKEN" "$URL$base/export" >"$dir/a.toml" ||
		fail "GET $base/export"
	grep -q '^version = 1' "$dir/a.toml" || fail "the export has no version"
	"${post[@]}" "@$dir/a.toml" "$URL$base/plan" | jq -e '.changes | length == 0' >/dev/null ||
		fail "the export of a project does not plan to no changes"
	echo "ok   export | plan: .changes | length == 0"

	# Prune is off unless asked: a file without the app leaves it alone.
	printf 'version = 1\n\n[projects.%s]\nname = "IaC smoke"\n\n[projects.%s.environments.production]\nname = "production"\n' \
		"$slug" "$slug" >"$dir/empty.toml"
	"${post[@]}" "@$dir/empty.toml" "$URL$base/apply" >/dev/null || fail "apply without the app"
	curl -sS -f -H "Authorization: Bearer $TOKEN" "$URL$base/export" | grep -q "$slug-web" ||
		fail "an apply without prune deleted the app"
	echo "ok   apply without prune keeps what the file leaves out"

	expect_page "/teams/$TEAM/projects/$project/iac" "Infrastructure as code"
	expect_page "/teams/$TEAM/projects/$project/iac" "Export podkit.toml"
	expect_page "/teams/$TEAM/import" "Import projects"
	expect_page "/teams/$TEAM/projects/$project/iac/export" "version = 1"

	"${post[@]}" "@$dir/empty.toml" "$URL$base/apply?prune=true" >/dev/null || fail "apply with prune"
	if curl -sS -f -H "Authorization: Bearer $TOKEN" "$URL$base/export" | grep -q "$slug-web"; then
		fail "prune=true did not delete the app"
	fi
	echo "ok   apply with prune=true deleted it"

	api DELETE "$base" >/dev/null || true
	rm -rf "$dir"
}
