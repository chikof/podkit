# Podkit

[![CI](https://github.com/chikof/podkit/actions/workflows/ci.yml/badge.svg)](https://github.com/chikof/podkit/actions/workflows/ci.yml)
[![License: AGPL v3](https://img.shields.io/badge/License-AGPL_v3-blue.svg)](LICENSE)

Self-hosted PaaS on [Podman](https://podman.io) instead of Docker.

## Status

The core deploy loop is built and live-verified end to end: connect a repo,
deploy a Dockerfile app, get a real public URL (`sslip.io` by default, or
your own domain via Traefik + Let's Encrypt), zero-downtime redeploys with
rollback, env vars, resource limits, and a self-healing restart monitor
(Podman itself is daemonless and doesn't restart crashed containers).
Remote hosts are reached over SSH-tunneled `podman.sock`, so the same code
path runs a server on the same box as podkit or across the network.

## Running it (dev)

```sh
devenv up           # postgres, etc. check devenv.nix
cargo run -p server
```

Requires `git`, `ssh`, a reachable Podman socket
(`systemctl --user enable --now podman.socket` for rootless), and (for
applications using the `nixpacks` build strategy) the `nixpacks` CLI on
`PATH` (in `devenv.nix`'s `packages` for local dev). Dockerfile-strategy
apps don't need it. See `.env.example` for required environment variables.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). Security issues: see
[SECURITY.md](SECURITY.md). Please don't file those as public issues.

## License

[AGPL-3.0-or-later](LICENSE).
