# Smoke sections for the avatars area; sourced by scripts/ui-smoke.sh (see its header).
# A 1x1 PNG, so the section needs no image tools.
AVATAR_PNG='iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg=='

section_avatars() {
	local png svg dir got ctype
	dir="$(mktemp -d)"
	png="$dir/a.png"
	svg="$dir/a.svg"
	printf '%s' "$AVATAR_PNG" | base64 -d >"$png"
	printf '<svg xmlns="http://www.w3.org/2000/svg"/>' >"$svg"

	expect_page "/teams/$TEAM/settings" "Team picture"
	expect_page "/teams/$TEAM/account" "Account"

	# A picture is stored as a 256 px WebP and served with its type.
	got="$(curl -sS -o /dev/null -w '%{http_code}' -X PUT -H "Authorization: Bearer $TOKEN" \
		-F "file=@$png;type=image/png" "$URL/api/v1/me/avatar")"
	[[ "$got" == 200 ]] || fail "PUT /api/v1/me/avatar: status $got"
	local uid
	uid="$(curl -sS -b "$JAR" "$URL/teams/$TEAM/account" | grep -o '/avatars/user/[0-9]*' | head -1 | cut -d/ -f4)"
	[[ -n "$uid" ]] || fail "the account page does not point at the new picture"
	ctype="$(curl -sS -o /dev/null -w '%{content_type}' -b "$JAR" "$URL/avatars/user/$uid?v=x")"
	[[ "$ctype" == image/webp ]] || fail "GET /avatars/user/<id>: content type $ctype, wanted image/webp"
	echo "ok   GET /avatars/user/<id> ($ctype)"

	# An SVG is refused and a nonexistent team's picture is a 404.
	got="$(curl -sS -o /dev/null -w '%{http_code}' -X PUT -H "Authorization: Bearer $TOKEN" \
		-F "file=@$svg;type=image/svg+xml" "$URL/api/v1/me/avatar")"
	[[ "$got" == 400 ]] || fail "PUT svg avatar: status $got, wanted 400"
	got="$(curl -sS -o /dev/null -w '%{http_code}' -b "$JAR" "$URL/avatars/team/0")"
	[[ "$got" == 404 ]] || fail "GET /avatars/team/0: status $got, wanted 404"
	echo "ok   svg refused (400), unknown team picture (404)"

	# Back to the monogram.
	got="$(curl -sS -o /dev/null -w '%{http_code}' -X DELETE -H "Authorization: Bearer $TOKEN" \
		"$URL/api/v1/me/avatar")"
	[[ "$got" == 204 ]] || fail "DELETE /api/v1/me/avatar: status $got"
	rm -rf "$dir"
}
