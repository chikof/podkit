# Smoke sections for the settings area; sourced by scripts/ui-smoke.sh (see its header).
section_members() {
	expect_page "/teams/$TEAM/members" "Members"
	expect_page "/teams/$TEAM/members" "Roles"
	# The sign-up page answers 200 while registration is allowed and 403
	# with its own message when it is not; anything else is a bug.
	local got
	got="$(curl -sS -o "$BODY" -w '%{http_code}' "$URL/register")" ||
		fail "GET $URL/register: request failed"
	case "$got" in
	200) grep -qF "Create account" "$BODY" || fail "GET $URL/register: missing text: Create account" ;;
	403) grep -qF "Registration is closed" "$BODY" || fail "GET $URL/register: missing text: Registration is closed" ;;
	*) fail "GET $URL/register: status $got, wanted 200 or 403" ;;
	esac
	echo "ok   GET /register ($got)"
	expect_page "/teams/0/members" "Not found" 404
}

# The Quiet settings layout, the sign-in page and the error pages.
section_settings() {
	expect_page "/teams/$TEAM/settings" 'class="ui-snav"'
	expect_page "/teams/$TEAM/settings" 'aria-label="Settings sections"'
	expect_page "/teams/$TEAM/members" "Add a member"
	expect_page "/teams/$TEAM/settings/appearance" 'class="ui-theme-opts"'
	expect_page "/teams/$TEAM/projects" 'class="ui-tabbar"'
	# A page that does not exist is a 404 inside the frame, with its own heading.
	expect_page "/teams/$TEAM/projects/0" "This page doesn’t exist" 404
	# Signed out, the sign-in page is the bare Quiet column.
	local got
	got="$(curl -sS -o "$BODY" -w '%{http_code}' "$URL/login")"
	[[ "$got" == 200 ]] || fail "GET $URL/login: status $got"
	grep -qF 'class="ui-auth"' "$BODY" || fail "GET /login: not the Quiet sign-in page"
	echo "ok   GET /login (200, Quiet sign-in)"
	expect_page "/teams/new" "_slugEdited"
}
