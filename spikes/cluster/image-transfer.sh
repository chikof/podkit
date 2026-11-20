#!/usr/bin/env bash
# Throwaway: time `podman save | podman load` into a private store (a stand-in for a
# second server), with and without compression, and compare to a registry:2 push/pull
# including the "small change on a big base" case. Everything lives in $W and is removed.
set -eu
export PATH=$PATH:/run/current-system/sw/bin
W=/tmp/pk-m1
rm -rf "$W" 2>/dev/null || podman unshare rm -rf "$W"
mkdir -p "$W/ctx"
PB="podman --root $W/store --runroot $W/run --storage-driver overlay"
t() { local s=$(date +%s%N); "$@" >/dev/null 2>&1; local e=$(date +%s%N); python3 -c "print('%.2f' % (($e-$s)/1e9))"; }
fresh() { $PB system reset -f >/dev/null 2>&1 || true; }

# A "typical app" image: python:slim + 1 MiB layer of app code + 40 MiB deps layer
printf 'FROM docker.io/library/python:3.13-slim\nRUN head -c 41943040 /dev/urandom > /deps.bin\nCOPY app.bin /app.bin\n' > "$W/ctx/Containerfile"
head -c 1048576 /dev/urandom > "$W/ctx/app.bin"
podman build -q -t localhost/pkm1-app:v1 "$W/ctx" >/dev/null
head -c 1048576 /dev/urandom > "$W/ctx/app.bin"
podman build -q -t localhost/pkm1-app:v2 "$W/ctx" >/dev/null

echo "image,size_MB,save_s,archive_MB,load_s,gzip_archive_MB,gzip_total_s,zstd_archive_MB,zstd_total_s"
for img in docker.io/library/alpine:3.24 localhost/pkm1-app:v1 docker.io/library/python:3.13-slim docker.io/library/postgres:18 ; do
	size=$(podman image inspect "$img" --format '{{.Size}}')
	fresh
	s=$(t podman save -o "$W/a.tar" "$img"); am=$(( $(stat -c %s "$W/a.tar") / 1048576 ))
	l=$(t $PB load -i "$W/a.tar")
	fresh
	gt=$(t bash -c "podman save $img | gzip -1 | gunzip | $PB load")
	gm=$(podman save "$img" | gzip -1 | wc -c); gm=$((gm/1048576))
	fresh
	zt=$(t bash -c "podman save $img | zstd -3 -T0 | zstd -d | $PB load")
	zm=$(podman save "$img" | zstd -3 -T0 | wc -c); zm=$((zm/1048576))
	echo "$img,$((size/1048576)),$s,$am,$l,$gm,$gt,$zm,$zt"
	rm -f "$W/a.tar"
done

echo "--- registry:2 (loopback), push v1 then v2 (only the 1 MiB layer differs)"
fresh
podman rm -f pkm1-reg >/dev/null 2>&1 || true
podman run -d --rm --name pkm1-reg -p 127.0.0.1:55001:5000 docker.io/library/registry:2 >/dev/null
sleep 2
p1=$(t podman push --tls-verify=false localhost/pkm1-app:v1 127.0.0.1:55001/app:v1)
p2=$(t podman push --tls-verify=false localhost/pkm1-app:v2 127.0.0.1:55001/app:v2)
echo "push v1 (cold): ${p1}s   push v2 (delta only): ${p2}s"
q1=$(t $PB pull --tls-verify=false 127.0.0.1:55001/app:v1)
q2=$(t $PB pull --tls-verify=false 127.0.0.1:55001/app:v2)
echo "pull v1 into empty store: ${q1}s   pull v2 (layers present): ${q2}s"
echo "--- save|load of v2 onto a store that already has v1 (no layer dedup on the wire)"
s2=$(t bash -c "podman save localhost/pkm1-app:v2 | $PB load"); echo "save|load v2 over v1: ${s2}s, wire bytes = whole image ($(( $(podman save localhost/pkm1-app:v2 | wc -c) / 1048576 )) MB)"
echo "--- cleanup"
podman rm -f pkm1-reg >/dev/null 2>&1 || true
podman rmi -f localhost/pkm1-app:v1 localhost/pkm1-app:v2 >/dev/null 2>&1 || true
$PB system reset -f >/dev/null 2>&1 || true
podman unshare rm -rf "$W"
echo done
