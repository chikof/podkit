# Private access with Tailscale

Serve an application, or podkit itself, only on your tailnet. Nothing here adds an
always-on service: a sidecar exists only for an application you turned private access on for.

## An application on the tailnet

1. **Create an auth key.** In the Tailscale admin console go to Settings → Keys → *Generate
   auth key*. A reusable key lets every application you expose join with it. If you use tags,
   tick *Tags* and pick the same ones you set in step 2. (On Headscale:
   `headscale preauthkeys create --user <user> --reusable`.)
2. **Store it.** In podkit open Settings → Tailscale, paste the key, optionally set the tags
   the nodes advertise (`tag:podkit`) and, for Headscale, the login server
   (`https://headscale.example.com`), then save. The key is encrypted and write-only: the page
   only says whether one is stored. Leave the field empty to keep it; use *Remove the key* to drop it.
3. **Turn it on.** Open the application's Domains tab, *Private access (Tailscale)*, choose a node
   name (the application's slug by default) and press *Turn on*.
4. **Open it.** Once the card says *On the tailnet*, open
   `https://<name>.<tailnet>.ts.net` from any device signed in to the same tailnet.
   HTTPS needs *MagicDNS* and *HTTPS Certificates* enabled in the Tailscale DNS settings.

### What runs

A `tailscale/tailscale` container (BSD-3-Clause) joins the application's environment network
and runs `tailscale serve` from a generated config: HTTPS on 443, proxied to the application's
container port. It uses userspace networking, so it needs no extra capabilities. Its identity
lives in the volume `ts-app-<id>-state`.

### Removing it

*Turn off* runs `tailscale logout` in the sidecar, then removes the container and its volume,
so the node disappears from your tailnet. Deleting the application does the same.

### No public route at all

Private access is separate from the public domain. To have no public route, leave the
application without a domain, or mark its network internal; the sidecar still reaches the
control server through its own network.

## podkit itself on the tailnet

Pick one on the machine that runs podkit:

- **Behind `tailscale serve`:** set `HOST=127.0.0.1` in podkit's configuration and run
  `tailscale serve --bg 8080` (use your `PORT`). Only your tailnet reaches the UI, over HTTPS.
- **Bound to the Tailscale interface:** set `HOST` to the machine's Tailscale address
  (`tailscale ip -4`, a `100.x.y.z` one). podkit then listens on that interface only. Start
  podkit after `tailscaled` is up, or the bind fails.

Agents on other servers connect to the server's address, so give them its tailnet name or
address too, and keep a firewall rule that blocks the port on the public interface.

## Headscale

Set the login server in Settings → Tailscale to your Headscale URL (`http://` or `https://`,
no path) and use a pre-auth key from it. The sidecars pass `--login-server`. Headscale has no
`ts.net` names: the node's address is the one MagicDNS gives it, shown on the card once joined.

## Troubleshooting

- *Failed: auth key refused*: the key expired, was single-use, or its tags are not allowed.
  Store a new key and press *Try again*.
- The card stays on *Starting*: the sidecar image is being pulled, or the node cannot reach
  the control server.
