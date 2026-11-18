# Smoke sections for the models area; sourced by scripts/ui-smoke.sh (see its header).
section_models() {
	expect_page "/teams/$TEAM/models" "Browse the catalog"
	expect_page "/teams/$TEAM/models/catalog" "S tier"
	expect_page "/teams/$TEAM/models/catalog?size=tiny" "Qwen3 4B"
	expect_page "/teams/$TEAM/models/catalog?tab=advanced" "Search Hugging Face"
	expect_page "/teams/$TEAM/models/catalog/qwen3-4b" "Overrides"
	expect_page "/teams/$TEAM/models/settings" "Hugging Face token"
	expect_page "/teams/$TEAM/models/catalog/no-such-model" "Not found" 404
	expect_page "/teams/0/models" "Not found" 404

	# The create flow downloads and runs a real model, so it is opt-in:
	#   PODKIT_SMOKE_MODEL=qwen3-4b scripts/ui-smoke.sh models
	# It removes the model and its files at the end unless PODKIT_SMOKE_KEEP is set.
	# A model with no recipe of at most 4 GB is refused.
	local model="${PODKIT_SMOKE_MODEL:-}"
	if [[ -z "$model" ]]; then
		echo "skip create flow (PODKIT_SMOKE_MODEL=<catalog id> downloads and runs one, at most 4 GB)"
		return
	fi
	local server project recipe query file path engine model_id page
	server="$(api GET "/api/v1/teams/$TEAM/servers" | jq -er '.[0].id')"
	project="$(api GET "/api/v1/teams/$TEAM/projects" | jq -r '.[0].id // empty')"
	if [[ -z "$project" ]]; then
		project="$(api POST "/api/v1/teams/$TEAM/projects" '{"name":"Smoke","slug":"smoke"}' | jq -er .id)"
	fi
	recipe="$(api GET "/api/v1/catalog/$model" | jq -er \
		'[.recipes[] | select(.total_bytes <= 4000000000)] | sort_by(.total_bytes) | .[0].id')" ||
		fail "$model has no recipe of at most 4 GB"

	ui_post "/teams/$TEAM/models/downloads/recipe" "$(jq -n --arg c "$model" --arg r "$recipe" \
		--arg s "$server" --arg p "$project" \
		'{catalog: $c, recipe: $r, server: $s, project: $p, name: "Smoke model", served: "smoke-model", ctx: "4096"}')"
	query="$(grep -o "/downloads/[0-9]*?[^']*" "$BODY" | head -n 1)" ||
		fail "the download did not start: $(head -c 400 "$BODY")"
	file="${query#/downloads/}"
	file="${file%%\?*}"
	echo "ok   POST download $file"
	wait_for "/teams/$TEAM/models/downloads/$file/progress?${query#*\?}" "Start the model" 1800

	engine="$(grep -o 'engine=[a-z_]*' <<<"$query" | cut -d= -f2)"
	path="$(grep -o 'path=[^&]*' <<<"$query" | cut -d= -f2)"
	ui_post "/teams/$TEAM/models/downloads/$file/start" "$(jq -n --arg p "$project" --arg e "$engine" --arg path "$path" \
		'{project: $p, name: "Smoke model", engine: $e, path: $path, ctx: "4096", served: "smoke-model"}')"
	model_id="$(grep -o "/models/[0-9]*'" "$BODY" | head -n 1 | tr -dc '0-9')"
	[[ -n "$model_id" ]] || fail "the model did not start: $(head -c 400 "$BODY")"
	echo "ok   POST start model $model_id"
	page="/teams/$TEAM/projects/$project/models/$model_id"
	expect_page "$page" "Smoke model"
	wait_for "$page/status?logs=1" "Running" 900

	ui_post "$page/chat" '{"prompt":"Say hello in one short sentence.","chat":[]}'
	grep -q 'pg-ans-0' "$BODY" || fail "chat: no answer element: $(head -c 400 "$BODY")"
	grep -Eq '[0-9]+ tokens' "$BODY" || fail "chat: no token count: $(head -c 400 "$BODY")"
	echo "ok   POST chat ($(grep -Eo '[0-9]+ tokens[^<]*' "$BODY" | tail -n 1))"

	if [[ -n "${PODKIT_SMOKE_KEEP:-}" ]]; then
		echo "kept the model: $URL$page"
		return
	fi
	ui_post "$page/remove" '{}'
	wait_for "/teams/$TEAM/models" "Browse the catalog" 120
	while curl -sS -b "$JAR" "$URL/teams/$TEAM/models" | grep -qF "$model_id"; do sleep 3; done
	ui_post "/teams/$TEAM/models/downloads/$file/delete" '{}'
	echo "ok   removed the model and its files"
}
