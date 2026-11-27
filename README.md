<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/brand/banner-podkit-dark.svg">
  <img src="docs/brand/banner-podkit-light.svg" alt="podkit. A self hosted PaaS on Podman. Built in Rust for small hosts." width="1280">
</picture>

# podkit

[![CI](https://github.com/chikof/podkit/actions/workflows/ci.yml/badge.svg)](https://github.com/chikof/podkit/actions/workflows/ci.yml)
[![License: AGPL v3](https://img.shields.io/badge/License-AGPL_v3-blue.svg)](LICENSE)

A self hosted PaaS on [Podman](https://podman.io) instead of Docker. Built in
Rust for small hosts.

Connect a repository, deploy it, and get a public URL. podkit is made for a
homelab or a small server: the control plane idles at about 13 MB of memory.

## What it does

- **Apps.** Deploy from a Dockerfile or with nixpacks, with zero-downtime
  redeploys, rollback, env vars, resource limits and live build logs.
- **Domains.** A working URL from the first deploy (`sslip.io`), or your own
  domain through Traefik and Let's Encrypt.
- **Databases.** One-click Postgres, MySQL, MongoDB and Redis, named volumes,
  and scheduled S3 backups with restore.
- **Compose stacks and templates.** Run a docker-compose file as one unit, or
  start from a template.
- **Models.** Download weights from Hugging Face and run them on llama.cpp,
  Ollama, vLLM or any image that serves `/v1`, reachable only on the server's
  internal network.
- **More than one server.** Each server runs `podkit-agent`, the only process
  that talks to Podman. The control plane reaches it over an SSH tunnel with a
  pinned host key.
- **A CLI and an API.** `podkit` drives an instance from a terminal or CI with
  an API token.

The core deploy loop is built and verified end to end. podkit is still young,
so expect rough edges.

## How light is it?

Memory (RSS) of a release build, after 10 minutes with no traffic and again after use
(signing in, downloading the template catalog and opening every page):

| Part | Idle | After use | Runs when |
|---|---|---|---|
| `server` (control plane and UI) | 13.0 MB | 32.9 MB | always (plus your Postgres) |
| `podkit-agent` (per server) | 9.2 MB | 9.4 MB | always |
| Traefik | 89.7 MB | | from the first deploy with a domain |
| LLM gateway sidecar | 7.1 MB | | only while a model link exists |
| Vulnerability scanner (Syft, Grype) | 0 MB | | only during a scan |

The server is built with `scripts/build-ordered.sh`, in the container image too; a plain
`cargo build --release` idles at 22.9 MB. Hashing a password takes 64 MiB more for
about 100 ms, one hash at a time, so a sign-in peaks near 100 MB: give the server's
container 128 MB.

CI fails when the server or the agent grows past its budget. The method, the
budgets and what a comparison with Coolify and Dokploy does and does not show
are in [docs/footprint.md](docs/footprint.md).

## Running it (dev)

```sh
devenv up           # postgres, etc. check devenv.nix
cargo run -p agent -- --data-dir /tmp/pk/agent
LOCAL_AGENT_SOCKET=/tmp/pk/agent/agent.sock cargo run -p server
```

Run the last two commands in separate terminals, then open
<http://localhost:8080/>. The first account needs the one-time setup token
that the server prints to its log.

You need `git`, `ssh` and a reachable Podman socket
(`systemctl --user enable --now podman.socket` for rootless). Apps built with
nixpacks also need the `nixpacks` CLI on the agent's `PATH`. See `.env.example`
for the environment variables.

## Command line

```sh
printf %s "$TOKEN" | podkit login --url https://podkit.example.com
podkit apps
podkit deploy web --wait          # follows the build log; exits 3 if it fails or is rolled back
podkit logs web -f
```

`.github/actions/deploy` wraps `podkit deploy --wait` for GitHub Actions. See
[docs/cli.md](docs/cli.md).

## Documentation

Running an instance:

- [Who can sign up](docs/registration.md): registration modes, the setup token, invites.
- [Where a request comes from](docs/trusted-proxies.md): `TRUSTED_PROXIES` behind a reverse proxy.
- [Where podkit may send requests](docs/outbound.md): the policy for webhooks, imports and other outbound calls.
- [Networks](docs/networks.md): per-environment networks, attaching resources, migrating a server.
- [Server firewall](docs/firewall.md) and [private access with Tailscale](docs/tailscale.md).
- [Footprint](docs/footprint.md).

Deploying things:

- [Environments](docs/environments.md), [builds](docs/builds.md) and [pull request previews](docs/previews.md).
- [Compose stacks](docs/stacks.md) and [templates](docs/templates.md).
- [Model catalog](docs/catalog.md), [models UI](docs/models-ui.md), [LLM gateway](docs/gateway.md) and [MCP servers as resources](docs/mcp-resources.md).
- [Importers](docs/importers.md): Coolify, Dokploy, CapRover.
- [Quadlet view and eject](docs/quadlet.md).

Automating and extending:

- [API](docs/api.md), [command line](docs/cli.md) and [MCP server](docs/mcp.md).
- [Cloud extension point](docs/cloud-extension.md).

Working on podkit:

- [CONTRIBUTING.md](CONTRIBUTING.md).
- [The web UI](docs/ui.md) and its [design system](docs/design/README.md).

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). Security issues: see
[SECURITY.md](SECURITY.md). Please don't file those as public issues.

## License

[AGPL-3.0-or-later](LICENSE).
