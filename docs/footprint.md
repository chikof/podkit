# Footprint

podkit's pitch is that it is light. This page measures it, compares it with what
Coolify and Dokploy publish, and says what keeps it that way. The numbers are
enforced: `scripts/footprint.sh --check` (and the `footprint` job in
`.github/workflows/ci.yml`) fails when an idle RSS goes over its budget.

## What was measured

Release build (`cargo build --release`, `opt-level = "z"`, LTO, stripped), Linux
6.18 on a 16-thread workstation, Postgres 18 in a container (`pk-pg`), rootless
Podman. Idle means: started, ready, **no requests and no registered remote server**,
then left alone for 600 s (10 minutes). "After start" is 5 s after readiness.

| Process | After start | After 10 min idle | Peak | Threads | Sockets | CPU (idle) |
|---|---|---|---|---|---|---|
| `server` (control plane, with its UI) | 12.2 MB | **12.4 MB** | 12.4 MB | 5 | 9 | 0.00 % |
| `podkit-agent` (one per server) | 7.6 MB | **7.8 MB** | 7.8 MB | 3 | 4 | 0.01 % |
| gateway sidecar (`podkit-agent --gateway`) | n/a | **7.1 MB** | n/a | 4 | n/a | n/a |
| Traefik v3.7.19 (`podman stats`: 119 MB incl. page cache; RSS 89.7 MB) | n/a | **89.7 MB** | n/a | 15 | 7 fds | 0.12 % |
| Scanner (Syft / Grype) when idle | — | **0 MB**: no container exists | | | | |
| Metrics sampler | — | inside the server: no process, wakes every 30 s | | | | |

Disk: the two binaries are 19.5 MB (server, UI and migrations embedded) and 6.2 MB
(agent) as built here; an empty data directory is a few KB. Postgres is the one
dependency you run yourself: the server holds **5** connections open at idle
(`pg_stat_activity`), where it held 6 before this card.

Notes on the numbers:

- The server and agent rows are from `scripts/footprint.sh --idle 600`; the other
  two were measured by hand (below). Everything is RSS (`VmRSS` in
  `/proc/<pid>/status`), not virtual size. "Sockets" counts socket file
  descriptors, which is the cheap way to see open connections without `ss`.
- The agent was started against a stub Podman socket (the same one CI uses), so
  its supervisor and sweep find no containers. With containers to watch it keeps a
  little state per container; it is not measured here.
- Traefik is **not** started with podkit. The agent starts it when the first
  application with a domain is deployed (`ingress::ensure_traefik`). It was run as
  `traefik:v3.7.19 --providers.docker=false --entrypoints.web.address=:80`, with no
  routes; the real one also mounts the Podman socket and watches it, so expect a
  little more under load.
- The gateway sidecar is the agent binary run in `--gateway` mode, bind-mounted
  into a `python:3.13-slim` container that never runs Python. It was measured as a
  host process (same binary, same code); the container adds nothing to its RSS.
  It exists only while a model link exists (see "Lazy-start" below).

## Method

```
cargo build --release -p server -p agent
DATABASE_URL=postgres://… scripts/footprint.sh --idle 600      # the full table above
DATABASE_URL=postgres://… scripts/footprint.sh --check         # 30 s idle, exit 1 over budget
```

`scripts/footprint.sh` creates a private data dir and config home under `$TMPDIR`,
generates a throwaway age key, starts the agent and the server, waits until both
answer, waits 5 s more for migrations and the first sweeps, samples, sleeps for the
idle window and samples again. CPU % is (utime + stime) over the window. The
Postgres connection count needs `psql` and is `n/a` without it; the figure above was read with
`select count(*) from pg_stat_activity where datname = …`. No Podman is needed:
without `PODKIT_PODMAN_SOCKET` the agent is given a stub socket.

Traefik: `podman run -d --network none docker.io/library/traefik:v3.7.19 …`, left
for 60 s, then `podman stats --no-stream` and `/proc/<pid>/status` of the container.
Gateway: `podkit-agent --gateway --gateway-dir <dir> --listen 127.0.0.1:<port>`,
read after 20 s.

### Budgets

`scripts/footprint.budget` holds, per process, a **cap** (server 60 MB, agent
25 MB: the promise) and a **baseline** (the last measured idle RSS, rounded up:
server 13 MB, agent 8 MB). `--check` fails when the idle RSS is over
`min(cap, baseline + 20 %)`, which today is **15.6 MB** for the server and
**9.6 MB** for the agent. So a 25 % regression fails CI long before it reaches the cap.
If a change legitimately costs memory, raise the baseline in the same commit and say why.

## What changed on this card (before / after)

Same machine, same script, 600 s idle, old and new binaries.

| Change | Before | After |
|---|---|---|
| Async runtime workers: server `min(cores, 4)`, agent 2 (`TOKIO_WORKER_THREADS` still overrides) | server 17 threads, 12.8 MB; agent 17 threads, 8.0 MB | server 5 threads, 12.4 MB; agent 3 threads, 7.8 MB |
| Gateway sidecar with the same worker cap | 18 threads, 7.2 MB | 4 threads, 7.1 MB |
| Postgres pool: max 10 → 5, idle connections closed after 30 s | 6 backends open at idle | 5 backends |
| Gateway sidecar removed when the last model link goes (it used to stay up, idle, forever) | 7.1 MB per server that ever had a link | 0 MB with no link |
| Broadcast buffer of a running job's live log: 1024 → 256 lines | up to 8 MiB per running job (1024 × 8 KiB lines) | up to 2 MiB |

Honest reading: the RSS of the server and agent barely moved (-0.4 MB and -0.2 MB),
because they were already small; the thread cut matters more (17 → 5 and 17 → 3
threads, so fewer stacks and less scheduler churn on a small box). The pool change
is one backend fewer on the Postgres side: the pool never went idle because the
job dispatcher polls every 5 s, so the 30 s idle timeout rarely gets to close
anything, and the cap is what helps. The log buffer is a worst-case bound, not an idle
number: an idle server has none. The lazy gateway is the real saving, 7 MB and a
container per server.

Already lean before this card, checked and left alone:

- **Scanner containers** exist only for the length of a scan and are force-removed
  afterwards, on success, failure and cancellation (`agent/src/scan.rs`); only the
  Grype database volume stays. Idle cost: 0 MB, but that volume is about 3.5 GB of disk.
- **Traefik** is created on the first routed deploy, not at install.
- **Sampler and loops** (all in the server process): metrics sample every 30 s
  (`metrics_interval_secs`, floor 5 s), server sync every 30 s, backup schedule
  check every 60 s, job dispatcher poll every 5 s, maintenance hourly. Each is one
  cheap query or one request to the agent; with no servers registered they do nothing,
  which is why idle CPU rounds to 0.00 %.
- **Per-job logs** are capped per line (8 KiB) and per job (10 MiB on disk);
  memory holds only the broadcast window above.

### Lazy-start of the gateway sidecar

`docs/gateway.md` already said the sidecar is created when the agent first gets a
routing table with keys. What was missing was the other half: after the last
link was removed the container stayed up. `sidecar::apply` now follows
`lazy_action(has_keys, exists)`:

| Model link (has keys) | Sidecar exists | Action |
|---|---|---|
| yes | any | create or start it, join its networks |
| no | yes | stop supervising it and remove it |
| no | no | nothing: it is never started |

The agent also does not start it at boot unless the stored table has keys. Test:
`cargo test -p agent -- gateway_lazy`.

## podkit versus Coolify and Dokploy

Neither project publishes idle RSS figures for its own containers, and nothing was
installed here, so there is no measured number to put in those cells. What they
do publish is the hardware to provide and the services they run.

| | podkit | Coolify v4 | Dokploy |
|---|---|---|---|
| Published minimum | none beyond the host; budget: server ≤ 60 MB, agent ≤ 25 MB RSS (measured 12.4 / 7.8 MB) | 2 CPU cores, 2 GB RAM, 10 GB disk ([installation docs](https://coolify.io/docs/get-started/installation)) | at least 2 GB RAM and 30 GB disk ([installation docs](https://docs.dokploy.com/docs/core/installation)) |
| Smaller setups | runs next to other workloads on a homelab box | "can run on smaller servers (for example: 1 CPU core, 512 MB RAM, 4 GB disk), but this is not recommended" (same page) | not stated |
| Always-on services of the control plane | `server` (1 process) + Postgres; `podkit-agent` (1 process per server) | `coolify` (PHP app), `postgres`, `redis`, `soketi` realtime, all from one compose file ([docker-compose.prod.yml](https://raw.githubusercontent.com/coollabsio/coolify/v4.x/docker-compose.prod.yml)); plus a proxy it creates for each server | `dokploy` app, `postgres:16`, and `traefik:v3.7.19`, created by the installer as Docker Swarm services ([install.sh](https://dokploy.com/install.sh)) |
| Reverse proxy | Traefik, started on the first routed deploy | proxy container on every managed server | Traefik from install |
| Needs Docker / Swarm | no: rootless Podman, no daemon | Docker | Docker with Swarm |
| Example from Coolify's docs | | "Memory 4 GB, average usage 2.6 GB, 2 vCPU 60–70 %" for a mixed production workload (that includes the apps, not just Coolify) | |

How to read it: the published minimums are about what the *whole host* needs, builds
included, so they are not comparable to a process RSS. The fair comparison is the
count of always-on services: podkit's control plane is one binary plus the Postgres
you probably already run (three to four containers or services for the others),
and the per-server part is one 7.8 MB process rather than a Docker daemon plus a
proxy. A like-for-like RSS table needs both projects installed on a spare machine
and `docker stats` after 10 minutes; that was out of scope and is the obvious
follow-up.

## Keeping it light

- A new always-on service needs a reason in its card; helpers start lazily.
- Before merging a change that adds a background loop, a cache or a pool, run
  `scripts/footprint.sh --check`.
- CI repeats it on every pull request, against a Postgres service and no Podman.
