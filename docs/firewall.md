# Server firewall

An opt-in nftables firewall for one server, managed from **Servers → (server) → Firewall**.
It is off until you install the helper and apply rules. Nothing else in podkit needs root.

```
podkit ──ssh──► agent (your user) ──unix socket──► podkit-firewall (root) ──nft──► table inet podkit
```

`podkit-firewall` is a separate small binary (`crates/firewall`). It runs as root under systemd,
listens on `/run/podkit-firewall/firewall.sock` and gives that socket to the agent's user only
(mode 0600; every connection is also checked with `SO_PEERCRED`, so another uid is refused even
if the file mode were wrong). It owns exactly one table, `inet podkit`: every script it runs
starts with `add table inet podkit` / `delete table inet podkit` and never names another table or
flushes the ruleset. Docker, Podman/netavark, `fail2ban` or your own tables are left alone.

## What the rules are

| Chain | Rule |
|---|---|
| input (policy drop) | established and related, loopback, traffic from `podman*` interfaces, ICMP and ICMPv6 |
| input | SSH port (22, or the server's configured port), from anywhere or from the ranges you set |
| input | TCP 80 and 443 |
| input | your explicit allows: protocol, port and optionally a source range |
| forward (policy accept) | established and related, loopback, traffic from `podman*` interfaces |
| forward | DNAT'd traffic (a published port) to 80 or 443, or to a port you allowed, from the range you gave |
| forward | every other DNAT'd packet is dropped |

Container-to-host traffic (aardvark-dns on the bridge gateway, `host.containers.internal`) arrives on the `podman*` bridges through `input`, so those interfaces are accepted there; this exposes nothing external. It is covered by a render test; it is not verified against a real netavark bridge on a host.

### The SSH rule cannot lock podkit out

* The ranges you set for SSH are *added to* the sources of every SSH session that is open when
  you apply (read from `/proc/net/tcp*`). podkit's own connection is one of those sessions, so a
  typo in the ranges cannot cut it off.
* A new ruleset is applied with a **confirm window of 60 s**, like `iptables-apply`. podkit
  reaches the server again over a **new** SSH connection; only if that works does it confirm. If it
  does not, podkit asks the helper to revert at once, and the helper reverts by itself after 60 s
  anyway (also if podkit crashed). The team gets a "Firewall change reverted" notification.
* The helper saves the *confirmed* ruleset to `/var/lib/podkit-firewall/active.json` and puts it
  back when it starts, so a reboot never brings up an unconfirmed change.
* The preview shows the exact diff against what is in force before anything is applied.

## Published ports

A container's published port (`-p 5432:5432`) is DNAT'd by netavark in `prerouting`, so the
packet goes through the **forward** hook, never **input**. A firewall that only filters `input`
therefore does not cover published database ports. This one filters both:

* the `forward` chain accepts a DNAT'd packet only if the port the *outside* used
  (`ct original proto-dst`) is 80/443 or one of your allows (and the source matches the allow's
  range), then drops every other DNAT'd packet;
* the Postgres you published on `25432` is unreachable from outside until you add
  `tcp 25432` (ideally from your office range) as an allow. Container-to-container traffic and
  containers reaching the outside (`iifname "podman*"`) are not affected.

What is verified: rendering is snapshot-tested, and the ignored container test loads the real
script with a real `nft` in a `NET_ADMIN` container. What is **not** verified by the automated
tests: a real netavark published port end to end. Do the VM steps in the card hand-off
before relying on this for a production database.

## Installing the helper

* **A remote server added to podkit:** on the Firewall tab press *Install the helper*. The
  bootstrap job uploads `podkit-firewall` to `/usr/local/lib/podkit/` (owned by root, so the
  agent's user cannot swap a binary that root runs), writes
  `/etc/systemd/system/podkit-firewall.service` and starts it, using `sudo -n` over the server's
  SSH login (or directly if that login is root). The login therefore needs **passwordless sudo**;
  if it fails, the bootstrap log on the Overview page says why and the agent keeps running. The
  helper needs `nft` on the server. The agent binary directory of the podkit server must hold
  `podkit-firewall-<arch>` (the container image ships it).
* **The podkit host (the local server):** podkit never installs root software on its own host.
  Install it yourself:

  ```sh
  sudo install -m 0755 -o root -g root target/release/podkit-firewall /usr/local/lib/podkit/podkit-firewall
  ```

  and a unit like the one the bootstrap writes (replace `1000` with the uid of the user the podkit
  agent runs as, and the `nft` path with `command -v nft`):

  ```ini
  [Unit]
  Description=podkit firewall helper (owns the nftables table inet podkit)
  After=network-pre.target

  [Service]
  ExecStart=/usr/local/lib/podkit/podkit-firewall --uid 1000 --nft /usr/sbin/nft
  Restart=always
  RestartSec=2
  RuntimeDirectory=podkit-firewall
  RuntimeDirectoryMode=0755
  StateDirectory=podkit-firewall
  NoNewPrivileges=yes
  ProtectHome=yes
  ProtectSystem=strict
  PrivateTmp=yes

  [Install]
  WantedBy=multi-user.target
  ```

### NixOS

The helper is not installed by podkit on NixOS (`/etc/systemd/system` is managed by the
configuration). Add it to the configuration instead, for example (this is a snippet for you to
adapt; podkit does not change your configuration):

```nix
{ pkgs, ... }:
let
  # Build crates/firewall from the podkit checkout (cargo build --release -p podkit-firewall),
  # or package it with rustPlatform.buildRustPackage.
  podkit-firewall = /path/to/podkit/target/release/podkit-firewall;
in
{
  # The helper owns inet podkit only; make sure the NixOS firewall is not what you want to use
  # for the same ports, or both apply.
  environment.systemPackages = [ pkgs.nftables ];

  systemd.services.podkit-firewall = {
    description = "podkit firewall helper (owns the nftables table inet podkit)";
    wantedBy = [ "multi-user.target" ];
    after = [ "network-pre.target" ];
    serviceConfig = {
      # users.users.<agent user>.uid
      ExecStart = "${podkit-firewall} --uid ${toString config.users.users.chiko.uid} --nft ${pkgs.nftables}/bin/nft";
      Restart = "always";
      RestartSec = 2;
      RuntimeDirectory = "podkit-firewall";
      RuntimeDirectoryMode = "0755";
      StateDirectory = "podkit-firewall";
      NoNewPrivileges = true;
      ProtectHome = true;
      ProtectSystem = "strict";
      PrivateTmp = true;
    };
  };
}
```

Add `config` to the module arguments and set the real user name. With `networking.nftables.enable`
and the NixOS firewall on, remember that NixOS' own `nixos-fw` table is separate from `inet podkit`:
a packet must pass both, so a port the NixOS firewall closes stays closed whatever podkit allows.

## Checking by hand

```sh
sudo nft list table inet podkit          # what is in force
sudo podkit-firewall ctl /run/podkit-firewall/firewall.sock '{"op":"status"}'   # as root
sudo journalctl -u podkit-firewall
```

`sudo nft delete table inet podkit` removes the firewall immediately; the helper would put the
confirmed rules back on its next restart, so also press *Turn off* (or `systemctl stop
podkit-firewall`).

## Protocol

One JSON request line in, one JSON answer line out, per connection (`crates/firewall/src/protocol.rs`):
`status`, `preview`, `apply` (with `confirm_secs`), `confirm`, `rollback`, `disable`. The agent
carries the line to the socket without reading it (`POST /v1/firewall`); the helper validates
every request itself and trusts nobody, the agent included beyond the uid check.
