# Podkit

[![CI](https://github.com/chikof/podkit/actions/workflows/ci.yml/badge.svg)](https://github.com/chikof/podkit/actions/workflows/ci.yml)
[![License: AGPL v3](https://img.shields.io/badge/License-AGPL_v3-blue.svg)](LICENSE)

Self-hosted PaaS on [Podman](https://podman.io) instead of Docker.

## Status

The core deploy loop is built and live-verified end to end: connect a repo,
deploy a Dockerfile app, get a real public URL (`sslip.io` by default, or
your own domain via Traefik + Let's Encrypt), zero-downtime redeploys with
rollback, env vars, and resource limits. Each server runs `podkit-agent`,
the only process that talks to Podman (it also restarts crashed containers,
since Podman itself is daemonless). The control plane reaches the agent over
a unix socket: an SSH tunnel with a pinned host key for remote servers,
bootstrapped automatically via `POST /teams/{t}/servers`. A durable job
queue drives deploys with live build logs over SSE. Data services include
named volumes, one-click Postgres/MySQL/MongoDB/Redis, and scheduled S3
backups with restore.

## Running it (dev)

```sh
devenv up           # postgres, etc. check devenv.nix
cargo run -p agent -- --data-dir /tmp/pk/agent
LOCAL_AGENT_SOCKET=/tmp/pk/agent/agent.sock cargo run -p server
```

Run the last two commands in separate terminals. Requires `git`, `ssh`, a
reachable Podman socket (`systemctl --user enable --now podman.socket` for
rootless), and (for applications using the `nixpacks` build strategy) the
`nixpacks` CLI on `PATH` for the agent, not the server (in `devenv.nix`'s
`packages` for local dev). Dockerfile-strategy apps don't need it. See
`.env.example` for required environment variables.

The server also serves the web UI at <http://localhost:8080/>. It is plain
HTML rendered by the server, made interactive with
[Datastar](https://data-star.dev), so there is no separate front-end build.
The session cookie is `Secure`, so sign in over HTTPS or on `localhost`.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). Security issues: see
[SECURITY.md](SECURITY.md). Please don't file those as public issues.

## License

[AGPL-3.0-or-later](LICENSE).
