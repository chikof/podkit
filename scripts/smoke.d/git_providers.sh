# Smoke sections for git providers; sourced by scripts/ui-smoke.sh (see its header).
# Connects a Gitea (a tiny stand-in served by python3 on a free loopback port, so no real git host
# is needed), checks the page and the repository picker, tests the connection, checks the token is
# never in a page or an API response, then deletes the provider.
section_git_providers() {
	local suffix="$RANDOM$RANDOM" name id base port stub_pid token="smoke-token-$RANDOM"
	name="smoke-git-$suffix"
	base="/teams/$TEAM/git-providers"
	port="$(python3 -c 'import socket; s = socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1])')"

	python3 - "$port" "$token" <<'PY' &
import json, sys
from http.server import BaseHTTPRequestHandler, HTTPServer
port, token = int(sys.argv[1]), sys.argv[2]
class H(BaseHTTPRequestHandler):
    def log_message(self, *a): pass
    def do_GET(self):
        if self.headers.get("authorization") != "token " + token:
            self.send_response(401); self.end_headers(); return
        if self.path.startswith("/api/v1/user"):
            body = {"login": "smoke"}
        elif self.path.startswith("/api/v1/repos/search"):
            body = {"ok": True, "data": [{"full_name": "smoke/app", "default_branch": "main", "private": True}]}
        elif self.path.endswith("/branches?limit=50&page=1"):
            body = [{"name": "main"}]
        else:
            body = {"full_name": "smoke/app", "default_branch": "main"}
        data = json.dumps(body).encode()
        self.send_response(200); self.send_header("content-type", "application/json"); self.end_headers(); self.wfile.write(data)
HTTPServer(("127.0.0.1", port), H).serve_forever()
PY
	stub_pid=$!
	# shellcheck disable=SC2064
	trap "kill $stub_pid 2>/dev/null || true" RETURN
	for _ in $(seq 1 50); do
		curl -sS -o /dev/null "http://127.0.0.1:$port/api/v1/user" 2>/dev/null && break
		sleep 0.1
	done

	expect_page "$base" "Git providers"
	ui_post "$base/gitea" "$(jq -n --arg n "$name" --arg u "http://127.0.0.1:$port" --arg t "$token" '{ge_name: $n, ge_url: $u, ge_token: $t}')"
	grep -q '_error' "$BODY" && grep -qv '"_error":""' "$BODY" && fail "POST $base/gitea: $(cat "$BODY")"
	id="$(api GET "/api/v1/teams/$TEAM/git-providers" | jq -er --arg n "$name" '.[] | select(.name == $n) | .id')" ||
		fail "connected provider $name is not in the API list"
	echo "ok   provider $id connected"

	expect_page "$base" "$name"
	expect_absent "$token"
	api GET "/api/v1/teams/$TEAM/git-providers/$id" | grep -q "$token" && fail "GET git-providers/$id: the token is in the response"
	echo "ok   no token in the API response"

	expect_page "$base/$id/repos?q=smoke" "smoke/app"
	expect_page "$base/$id/branches?repo=smoke/app" "branch-list"
	ui_post "$base/$id/test" '{}'
	grep -q "Connected as smoke" "$BODY" || fail "POST $base/$id/test: $(cat "$BODY")"
	echo "ok   connection tested"

	ui_post "$base/$id/delete" '{}'
	api GET "/api/v1/teams/$TEAM/git-providers" | jq -e --arg n "$name" 'any(.[]; .name == $n) | not' >/dev/null ||
		fail "provider $name was not deleted"
	echo "ok   provider $id deleted"
}
