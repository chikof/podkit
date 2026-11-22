# Smoke section for builds; sourced by scripts/ui-smoke.sh (see its header).
# Reads Settings -> Builds as a page. It saves nothing and starts no build, so there is
# nothing to clean up.
section_builds() {
	expect_page "/teams/$TEAM/settings/builds" "Builds"
	expect_page "/teams/$TEAM/settings/builds" "Build servers"
	echo "ok   Settings -> Builds renders"
}
