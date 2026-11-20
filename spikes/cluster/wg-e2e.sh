#!/usr/bin/env bash
# Throwaway: two "servers" (netns) joined by a veth underlay, WireGuard between them,
# all inside one unprivileged user namespace (no root, no host changes).
set -eu
export PATH=$PATH:/run/current-system/sw/bin
WG=$(nix shell nixpkgs#wireguard-tools --command bash -c 'command -v wg')
export WG; export PATH=$(readlink -f /run/current-system/sw/bin):$PATH
exec unshare -Urnm bash -eu -c '
mount -t tmpfs tmpfs /run; mkdir -p /run/netns; ip link set lo up
ip netns add a; ip netns add b
ip link add va type veth peer name vb
ip link set va netns a; ip link set vb netns b
ip -n a addr add 192.0.2.1/24 dev va; ip -n a link set va up; ip -n a link set lo up
ip -n b addr add 192.0.2.2/24 dev vb; ip -n b link set vb up; ip -n b link set lo up
ka=$($WG genkey); kb=$($WG genkey)
pa=$(echo $ka | $WG pubkey); pb=$(echo $kb | $WG pubkey)
for s in a b; do ip -n $s link add wg0 type wireguard; done
echo $ka > /tmp/ka.$$; echo $kb > /tmp/kb.$$
ip netns exec a $WG set wg0 private-key /tmp/ka.$$ listen-port 51820 peer $pb allowed-ips 10.99.0.2/32 endpoint 192.0.2.2:51820
ip netns exec b $WG set wg0 private-key /tmp/kb.$$ listen-port 51820 peer $pa allowed-ips 10.99.0.1/32 endpoint 192.0.2.1:51820
ip -n a addr add 10.99.0.1/24 dev wg0; ip -n b addr add 10.99.0.2/24 dev wg0
ip -n a link set wg0 up; ip -n b link set wg0 up
rm -f /tmp/ka.$$ /tmp/kb.$$
echo "--- ping over wg (a -> b)"; ip netns exec a ping -c 3 -W 2 10.99.0.2 | tail -3
echo "--- handshake"; ip netns exec a $WG show wg0 | grep -E "latest handshake|transfer"
echo "--- tcp throughput over wg (python)"
ip netns exec b python3 -c "
import socket,time
s=socket.socket();s.setsockopt(1,2,1);s.bind((\"10.99.0.2\",9000));s.listen(1)
c,_=s.accept();n=0
while True:
    d=c.recv(1<<20)
    if not d:break
    n+=len(d)
print(\"recv MB\",n>>20)" &
sleep 0.5
ip netns exec a python3 -c "
import socket,time
s=socket.create_connection((\"10.99.0.2\",9000));buf=b\"x\"*(1<<20);t=time.time()
for _ in range(1000):s.sendall(buf)
s.close();dt=time.time()-t;print(\"1000 MiB in %.2fs = %.0f MiB/s\"%(dt,1000/dt))"
wait
'
