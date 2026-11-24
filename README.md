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
bootstrapped automatically via `POST /api/v1/teams/{t}/servers`. A durable job
queue drives deploys with live build logs over SSE. Data services include
named volumes, one-click Postgres/MySQL/MongoDB/Redis, and scheduled S3
backups with restore. Model weights from Hugging Face download into their own
volumes (`/api/v1/teams/{t}/model-files`): the size is checked against the
server's free disk before anything is queued, gated repos use a write-only
team token, and an interrupted download resumes. A downloaded model runs as a
one-click inference service (`/api/v1/teams/{t}/projects/{p}/models`) on
llama.cpp, Ollama, vLLM, colibri or any image that serves `/v1`; it is
reachable only as `model-<slug>` on the server's internal network and the
create call refuses a format, GPU or memory size the server cannot take.

## How light is podkit?

Measured on a release build after 10 minutes with no traffic (RSS, from
`/proc/<pid>/status`); method, budgets and the comparison with Coolify and
Dokploy are in [docs/footprint.md](docs/footprint.md).

| Part | Idle RSS | Runs when |
|---|---|---|
| `server` (control plane and UI) | 12.4 MB | always (plus your Postgres) |
| `podkit-agent` (per server) | 7.8 MB | always |
| Traefik | 89.7 MB | from the first deploy with a domain |
| LLM gateway sidecar | 7.1 MB | only while a model link exists |
| Vulnerability scanner (Syft, Grype) | 0 MB | only during a scan |

For scale, Coolify's docs ask for 2 CPU cores and 2 GB RAM and Dokploy's for
2 GB RAM and 30 GB disk. Those are host minimums, not process sizes: see the
doc for what that does and does not compare. CI fails when the server goes over
15.6 MB or the agent over 9.6 MB idle (hard caps 60 and 25 MB); run the same
check with `scripts/footprint.sh --check`.

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
only the CSS those classes need. The design system (tokens, components, the
rules for using them) is specified in [`docs/design/README.md`](docs/design/README.md).
Shared components live in `server/src/ui/components/`, one file each; their
colors (`bg-primary`, `text-muted-foreground`, ...) are the `--name` variables
in `server/src/ui/theme.css`, which has light and dark values, follows the
system setting unless the user picked a theme (a cookie the server renders as a
class on `<html>`), and makes a variable added there usable as a color.
`server/src/ui/components.css` holds what utilities can't express (dialogs,
popovers, anchor positioning, view transitions, container queries). Fonts,
icons and the one small `podkit.js` are vendored in `server/static/`, so the UI
loads nothing from another origin. In a debug build (or with
`PODKIT_UI_GALLERY=1`) a signed-in user can open `/_ui` to see every component.

Write each class out in full inside a string literal under `server/src/ui`.
The build fails when a class in a `class` attribute, or in a `&str` constant in
`components/`, is neither a utility nor a `.ui-*` rule in `components.css`.
It never sees maud's `.class` shorthand or class names built at
runtime with `format!`; those get no CSS and nothing reports it. It also can't
tell when a class generates the wrong CSS: encre-css 0.21.1's `divide-y` puts
its border on the wrong side, so use row borders instead, and it has no
`size-*` (write `h-4 w-4`).

### Who can sign up

`REGISTRATION` (or `registration` in `config.toml`) decides who may create an
account, through `/register` and `POST /api/v1/auth/register` alike:

- `first-user` (the default): only while no account exists. On a homelab the
  first person to register is the admin, and from then on sign-up answers
  `403 registration is closed`. To bring in teammates, set `open` while they
  sign up, then switch back and add them from the team's Members page (there
  are no email invites yet).
- `open`: anyone may sign up. Set this for a hosted instance.
- `closed`: nobody may.

An instance reachable from the internet with the default policy lets a
stranger become the admin if they register before you do, so create your
account right after the first start.

Sign-in works over plain HTTP, for example in a homelab, but the password and
session then cross the network unencrypted. Behind an HTTPS reverse proxy
that sets `X-Forwarded-Proto: https`, the session cookie is also marked
`Secure`.

### Where podkit may send requests

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

## Networks

Every environment gets a private Podman network on each server it has resources on
(`podkit-env-<environment id>`), so production and staging cannot reach each
other. Apps, databases and models join their environment's network the first
time they are deployed; Traefik joins only the networks that hold a routed
container, and the LLM gateway joins the networks of the apps that use a model
and of the models. Podman's shared `podkit` network is left to those two system
containers. A compose stack keeps its own network and joins others only when you
attach it.

To connect resources on purpose, open a server's **Networks** tab and create a
network (or import one that already exists on the server). Then open an
application, database, model or stack, go to its **Networks** tab and attach
the network under the names it should answer to there. Attaching is saved at
once and applies on the next deploy. A network with resources on it cannot be
deleted, and `No outbound` creates it without access to the outside.

### Moving a server that already runs resources

A resource moves to its environment's network on its next deploy. To move
everything running without a restart, use **Migrate** on the server's Networks
tab (or `POST /api/v1/teams/{team}/servers/{server}/networks/migrate`). For each
running app, database and model it connects the container to its environment's
network (and any network attached to it) and only then disconnects it from
`podkit`, so its address never disappears. Traefik does not notice a container
changing networks, so the agent starts a throwaway container after every
connect and disconnect to make it re-read the addresses (no restart). Before
that is done, a request can fail for a fraction of a second. In the live test
(`networks_live`), 64 requests sent 20 ms apart to a moved container all got
a 200.

The first run also replaces a Traefik started by an earlier version (its
config hash differs): that one is recreated, so ingress is briefly down while
the new one starts. Stacks are left alone and move on their next deploy.
Running it again is harmless.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). Security issues: see
[SECURITY.md](SECURITY.md). Please don't file those as public issues.

## License

[AGPL-3.0-or-later](LICENSE).
