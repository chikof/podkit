#!/usr/bin/env bash
B=/tmp/pk-t_b6efe1ba
export DATABASE_URL="postgres://postgres:postgres@127.0.0.1:55432/pk_t_b6efe1ba_live"
export AGE_SECRET_KEY="$(cat $B/age.key)"
export PORT=26411 INGRESS_PORT=26412 HTTPS_PORT=26413 DATA_DIR=$B/data/server HOST=127.0.0.1 REGISTRATION=open RUST_LOG=info
setsid nohup ${1:-$B/server-bin} >$B/server.log 2>&1 &
echo $! > $B/server.pid
sleep 6
curl -s -o /dev/null -w 'server %{http_code}\n' http://127.0.0.1:26411/
