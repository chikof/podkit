# Smoke sections for the shell area; sourced by scripts/ui-smoke.sh (see its header).
section_shell() {
	# `/` redirects a signed-in user to their first team; follow it.
	FOLLOW=-L expect_page / "New project"
	expect_page "/teams/$TEAM/projects" "New project"
	expect_page "/teams/$TEAM/projects" "Backup targets"
	expect_page "/teams/0/projects" "Not found" 404
}
