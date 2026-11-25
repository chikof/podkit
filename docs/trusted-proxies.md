# Where a request comes from (`TRUSTED_PROXIES`)

The audit log, the sign-in throttles and the "add my IP" button all use one answer for "who is
calling": the **socket peer's address**. `X-Forwarded-For` and `Forwarded` are believed only when
the peer is in `TRUSTED_PROXIES`, a comma-separated list of addresses or ranges (for example
`127.0.0.1, 172.18.0.0/16`). It is empty by default, so on a homelab where people reach podkit
directly nobody can write a different address into the audit log by sending a header.

Set it when a reverse proxy sits in front of podkit (Traefik, Caddy, nginx): list that proxy's
address as podkit sees it. podkit then reads the forwarded chain from the right and takes the first
address that is not a trusted proxy, so a client cannot prepend a fake one. An invalid entry stops
podkit from starting.
