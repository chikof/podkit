#!/usr/bin/env bash
# Throwaway: can an unprivileged user make a WireGuard interface?
set -u
export PATH=$PATH:/run/current-system/sw/bin
echo "kernel: $(uname -r); wireguard module loaded: $(ls /sys/module | grep -c '^wireguard$')"
echo "modinfo: $(modinfo wireguard 2>&1 | head -2 | tr '\n' ' ')"
echo "--- as plain user (host netns):"
ip link add wgtest0 type wireguard 2>&1 | head -2
echo "--- inside a user+net namespace (what rootless Podman gives a container):"
unshare -Urn bash -c 'ip link add wgtest0 type wireguard 2>&1; ip link show wgtest0 2>&1 | head -2'
echo "--- tun device (userspace WireGuard, e.g. wireguard-go/boringtun):"
ls -l /dev/net/tun 2>&1
unshare -Urn bash -c 'ip tuntap add dev tuntest0 mode tun 2>&1 && echo "tun create in userns: OK" && ip link show tuntest0 | head -1'
