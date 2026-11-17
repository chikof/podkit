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
Templates use Tailwind utility classes in `class="..."`; `server/build.rs`
runs [encre-css](https://crates.io/crates/encre-css) at compile time to emit
only the CSS those classes need. Shared buttons, inputs and cards live in
`server/src/ui/components.rs`. Their colors (`bg-primary`,
`text-muted-foreground`, ...) are the `--name` variables in
`server/src/ui/theme.css`, which has light and dark values and follows the
system setting; adding a variable there makes it usable as a color.

Write each class out in full inside a string literal under `server/src/ui`.
The build fails when a class in a `class="..."` literal or in `components.rs`
has no CSS. It never sees maud's `.class` shorthand or class names built at
runtime with `format!`; those get no CSS and nothing reports it. It also can't
tell when a class generates the wrong CSS: encre-css 0.21.1's `divide-y` puts
its border on the wrong side, so use row borders instead.

Sign-in works over plain HTTP, for example in a homelab, but the password and
session then cross the network unencrypted. Behind an HTTPS reverse proxy
that sets `X-Forwarded-Proto: https`, the session cookie is also marked
`Secure`.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). Security issues: see
[SECURITY.md](SECURITY.md). Please don't file those as public issues.

## License

[AGPL-3.0-or-later](LICENSE).
