# Smoke sections for the infrastructure area; sourced by scripts/ui-smoke.sh (see its header).
# The overview, the networks page and the guided setup. The last part dismisses the setup
# checklist and shows it again, so a team that had dismissed it sees it after a run.
section_infrastructure() {
	local base="/teams/$TEAM/infrastructure"
	expect_page "$base" "What runs where"
	expect_body 'id="nav-infrastructure"' "the sidebar's Infrastructure section"
	expect_body "<h1>Infrastructure</h1>" "overview heading"
	expect_page "$base/networks" "Networks"
	expect_page "$base/setup" "Set up your infrastructure"
	local step
	for step in server proxy networks access security project; do
		expect_page "$base/setup?step=$step" 'aria-current="step"'
	done
	expect_page "$base/setup?step=nope" "Not found" 404
	expect_page "/teams/0/infrastructure" "Not found" 404
	expect_page "/teams/0/infrastructure/setup" "Not found" 404

	# Dismissing hides the checklist from the overview; showing it again brings it back.
	ui_post "$base/setup/dismiss" '{}'
	expect_body "window.location.assign" "POST $base/setup/dismiss"
	expect_page "$base" "What runs where"
	expect_absent "Continue setup"
	ui_post "$base/setup/reopen" '{}'
	expect_body "window.location.assign" "POST $base/setup/reopen"
	ui_post "$base/setup/skip/everything" '{}'
	expect_body "_error" "POST skip of an unknown part"
}
