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
