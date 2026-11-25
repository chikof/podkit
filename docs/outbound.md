# Where podkit may send requests

Several features make podkit send a request to an address somebody typed: a
notification channel (webhook, Discord, Slack, ntfy, Telegram, the SMTP relay),
an import from Coolify, Dokploy or CapRover, an uptime check, a git provider
(GitLab, Gitea, GitHub Enterprise), an identity provider (OIDC discovery, the
token and userinfo endpoints), a backup storage endpoint, and the log export to
Loki or an OTLP collector. All of them go through one policy
(`crates/outbound`):

- Loopback (`127.0.0.0/8`, `::1`), link-local (`169.254.0.0/16`, which holds
  the cloud metadata address, and `fe80::/10`), unspecified, multicast,
  reserved and documentation ranges, and podkit's own listen address are
  **never** reachable. An IPv4 address written as IPv6 (`::ffff:127.0.0.1`) is
  judged as the IPv4 address.
- Private networks (RFC 1918, `fc00::/7`, CGNAT `100.64.0.0/10`, which Tailscale
  uses) are reachable only while `outbound_private` is `allow`.
- A name is resolved once and the connection goes to the address that was
  checked, so a name that answers differently the second time (DNS rebinding)
  gains nothing. Redirects are not followed (an uptime check follows up to
  three and checks each hop), the system proxy is not used, requests time out
  and answers are size-capped.
- What the other side answered is never shown: a failed test says
  `HTTP 403` or `cannot connect`, and a failed import says the same thing
  whatever went wrong, so a typed address cannot be used to map a network.

`outbound_private` defaults to `allow` when `REGISTRATION` is `first-user` or
`closed` (one owner's homelab, where an ntfy or Gotify on the LAN is the normal
case) and to `deny` when it is `open` (a hosted instance, where any member could
aim podkit at the network behind it). The instance administrator can change it
under Settings → Registration, which applies at once to every team; setting
`OUTBOUND_PRIVATE=allow` or `OUTBOUND_PRIVATE=deny` (or `outbound_private` in
`config.toml`) pins it over the page and the default.

A service on the same machine as podkit is not reachable as `localhost`: use the
machine's LAN address (for example `http://192.168.1.10:8081` for an ntfy
container that publishes port 8081), with `outbound_private` on. Only podkit's
own address and port are refused: `http://192.168.1.10:8080` is not allowed
when podkit itself listens on 8080 there.
