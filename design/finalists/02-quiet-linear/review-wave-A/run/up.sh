#!/usr/bin/env bash
B=/tmp/pk-t_87d44fd2
export DATABASE_URL=postgres://postgres:postgres@127.0.0.1:55432/pk_t_87d44fd2_live
export AGE_SECRET_KEY="$(cat $B/age.key)"
export PORT=24731 INGRESS_PORT=24732 HTTPS_PORT=24733 DATA_DIR=$B/data/server HOST=127.0.0.1 REGISTRATION=open RUST_LOG=info
setsid nohup ${1:-$B/server-bin} >$B/server.log 2>&1 &
echo $! > $B/server.pid
sleep 6
curl -s -o /dev/null -w 'server %{http_code}\n' http://127.0.0.1:24731/
