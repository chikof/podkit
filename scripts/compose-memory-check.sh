#!/usr/bin/env bash
# Sends compose files that once made the server hold gigabytes and prints, for each, the HTTP status
# and how far anonymous memory rose while it was read. Needs the dev Postgres, curl and jq.
#
#   scripts/compose-memory-check.sh <server binary> <label>
set -u
BIN=$1; LABEL=$2; DB=pk_memchk; D=$(mktemp -d /tmp/pk-memchk.XXXX); PORT=18953
dropdb --if-exists $DB 2>/dev/null; createdb -O podkit $DB
env DATABASE_URL=postgres://podkit:podkit@localhost:5432/$DB DATA_DIR=$D PORT=$PORT HOST=127.0.0.1 SETUP_TOKEN=0123456789abcdef0123456789abcdef RUST_LOG=error LOCAL_AGENT_SOCKET=$D/agent.sock "$BIN" >$D/log 2>&1 &
SP=$!
for i in $(seq 80); do curl -s -o /dev/null http://127.0.0.1:$PORT/health && break; sleep 0.5; done
U=http://127.0.0.1:$PORT/api/v1; J='Content-Type: application/json'
curl -s -o /dev/null -X POST $U/auth/register -H "$J" -d '{"name":"A","email":"a@example.com","password":"correct horse battery staple","setup_token":"0123456789abcdef0123456789abcdef"}'
T=$(curl -s -X POST $U/auth/login -H "$J" -d '{"email":"a@example.com","password":"correct horse battery staple"}' | jq -r .token); A="Authorization: Bearer $T"
TEAM=$(curl -s -X POST $U/teams -H "$A" -H "$J" -d '{"name":"D","slug":"d","logo":""}' | jq -r .id)
P=$(curl -s -X POST $U/teams/$TEAM/projects -H "$A" -H "$J" -d '{"name":"S","slug":"s"}' | jq -r .id)
SRV=$(curl -s -H "$A" $U/teams/$TEAM/servers | jq -r '.[0].id')
python3 - "$D" "$SRV" <<'PY'
import json,sys
d,srv=sys.argv[1],int(sys.argv[2]); big="y"*65536
svc="services:\n  x:\n    image: nginx\n"
def stack(name,compose,env): json.dump({"name":name,"compose_yaml":compose,"server_id":srv,"env":env},open(f"{d}/{name}.json","w"))
stack("listed-25000", svc+"    environment: ["+"BIG,"*25000+"BIG]\n", {"BIG":big})
stack("configs-8000", "configs:\n"+"".join(f"  c{n}: {{environment: BIG}}\n" for n in range(8000))+svc, {"BIG":big})
stack("anchor-64", 'x-a: &a "'+"q"*250000+'"\nx-b: &b [*a, *a, *a, *a]\nservices:\n'+"".join(f"  s{n}:\n    image: nginx\n    command: *b\n" for n in range(64)), {})
t='id = "x"\nname = "X"\ndescription = "d"\ncategory = "other"\nlogo = "logo.svg"\n[[config.mounts]]\nfilePath = "big"\ncontent = "'+"z"*900000+'"\n'
json.dump({"template":t,"compose":"secrets:\n"+"".join(f"  c{n}: {{file: ./big}}\n" for n in range(2000))+svc},open(f"{d}/preview-file-2000.json","w"))
PY
sleep 4
anon(){ awk '/RssAnon/{print int($2/1024)}' /proc/$SP/status 2>/dev/null; }
echo "== $LABEL"
for f in listed-25000 configs-8000 anchor-64 preview-file-2000; do
	case $f in preview*) url=$U/teams/$TEAM/projects/$P/templates/import/preview;; *) url=$U/teams/$TEAM/projects/$P/stacks;; esac
	before=$(anon); : > $D/peak
	( while kill -0 $SP 2>/dev/null; do anon >> $D/peak; sleep 0.05; done ) & W=$!
	code=$(curl -s -m 90 -o $D/out -w '%{http_code} %{time_total}s' -X POST "$url" -H "$A" -H "$J" --data-binary @$D/$f.json)
	sleep 0.3; kill $W 2>/dev/null; wait $W 2>/dev/null
	peak=$(sort -n $D/peak | tail -1)
	printf '%-18s %6s KB body  %-14s anon %4s MB -> peak %5s MB, after %5s MB   %s\n' $f $(( $(stat -c %s $D/$f.json) / 1024 )) "$code" "$before" "$peak" "$(anon)" "$(head -c 90 $D/out | tr '\n' ' ')"
done
kill $SP 2>/dev/null; sleep 1; rm -rf $D; dropdb $DB
