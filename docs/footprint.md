# Footprint

podkit's pitch is that it is light. This page measures it, compares it with what
Coolify and Dokploy publish, and says what keeps it that way. The numbers are
enforced: `scripts/footprint.sh --check` (and the `footprint` job in
`.github/workflows/ci.yml`) fails when a process goes over a budget.

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
scripts/build-ordered.sh                                       # cargo build --release, linked twice
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

`--check` fails on any of these, per process, read from `scripts/footprint.budget`:

| check | limit | server | agent | what it catches |
|---|---|---|---|---|
| idle RSS | the cap, the promise | 60 MB | 25 MB | the number podkit advertises |
| anonymous idle RSS (`RssAnon`: heap and stacks) | baseline + 20 % | 4.8 MB (measured 3.9 MB) | 1.5 MB (measured 1.1 MB) | a cache, a pool, a background loop |
| RSS after use | the cap | 60 MB (measured 34.9 MB) | 25 MB (measured 9.2 MB) | what a server in use holds |
| anonymous RSS after use | baseline + 20 % | 10.8 MB (measured 8.7 MB) | 1.5 MB (measured 1.1 MB) | a cache that fills and never empties |
| binary size | baseline + 10 % | 31.3 MB (measured 28.3 MB) | 8.5 MB (measured 7.7 MB) | new code and dependencies |
| shared libraries (`readelf -d`) | an allowlist | libc, libm, libgcc | the same | a second crypto or TLS stack |

A baseline is the last measured value, rounded up. If a change legitimately costs memory or
size, raise the baseline in the same commit and say why.

Total idle RSS has only the cap. Most of it is the binary's own code, mapped, and how much of
that is resident depends on where the linker puts each function, not on what the code does:
the crate split below moved it by 6.6 MB with no new code. The size check allows a refactor
of that kind (the split grew the server binary by 8 %); a new native library fails the
allowlist whatever its size.

The server has one more check, on the file-backed part of its idle RSS (baseline + 20 %,
10.8 MB, measured 9.0 MB). It holds what "Start-up order" below won.

### After use

Idle is a server nobody has touched. After the idle sample the script signs up, signs in,
creates a team and a project, opens every page the home page links to (98 pages) and ten API
lists, waits 5 s and samples again. A page that answers 5xx fails the run.

| server | idle | after use | peak |
|---|---|---|---|
| RSS | 23.0 MB | 34.9 MB | 86.5 MB |
| anonymous | 3.8 MB | 8.7 MB | |

The agent does not move (9.2 MB). Where the server's goes, from a `heaptrack` profile of
the same run:

- **Anonymous memory held after use, +4.9 MB.** 2.4 MB is the template registry: all 488
  templates are parsed on the first visit to the gallery and kept. The other 2.5 MB is not
  attributed to an owner.
- **File-backed pages, +7.0 MB.** The code of the handlers that ran, mapped from the binary.
- **Peak, 86.5 MB.** A password hash: Argon2id with 64 MiB of memory
  (`crates/crypto/src/argon2.rs`), 67 MB in the profile, freed when the hash is done. It is
  over the 60 MB cap for as long as a sign-in takes, and up to one hash per CPU core runs at
  a time, so the worst case is 64 MiB times the core count. The peak is reported and not
  gated.

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

## Wave B: where +6 MB went, and what won it back

Wave B (B1-B12) took the server's idle RSS from 12.4 MB to 18.8 MB and the two binaries
from 25.7 MB to 33.6 MB; INT-B raised the baseline so CI passed. This section is the
bisect and the trim (card F7).

### Bisect, merge by merge

Every merge of `roadmap/INT-B` was built (`cargo build --release -p server -p agent`) and
measured with `scripts/footprint.sh --idle 20` on a fresh database, same machine, one run
each. Run-to-run noise is about ±0.25 MB, so only the big step is a finding. B4 and B5 are
measured at the fix commits right after their merges (`471e17f`, `441f523`): the merge
commits themselves do not build (`EventKind::ALL`, `Tab::ALL` counts).

| merge into INT-B | commit | server idle RSS | delta | server | agent | agent idle RSS |
|---|---|---|---|---|---|---|
| DR-A merged into INT-B (before wave B) | `38ec818` | 14.4 MB |  | 22.6 MB | 6.6 MB | 8.1 MB |
| B1 autodeploy | `1de8960` | 14.6 MB | +0.20 MB | 22.8 MB | 6.6 MB | 8.3 MB |
| B2 previews | `4e6b860` | 14.8 MB | +0.21 MB | 23.1 MB | 6.6 MB | 8.4 MB |
| B3 access rules | `bb5ddec` | 14.6 MB | -0.15 MB | 23.2 MB | 6.6 MB | 8.2 MB |
| B4 server firewall | `471e17f` | 14.8 MB | +0.15 MB | 23.4 MB | 6.6 MB | 8.4 MB |
| B5 terminal | `441f523` | 14.7 MB | -0.11 MB | 24.0 MB | 6.7 MB | 8.0 MB |
| B6 logs | `9c64aff` | 14.9 MB | +0.24 MB | 24.2 MB | 7.8 MB | 8.9 MB |
| **B7 SSO + 2FA + passkeys** | `dc8c6f9` | **17.9 MB** | **+2.98 MB** | 25.8 MB | 7.8 MB | 8.9 MB |
| B8 builds | `2e1c355` | 18.2 MB | +0.27 MB | 26.0 MB | 7.9 MB | 9.1 MB |
| B9 replicas | `36cf6f4` | 18.1 MB | -0.09 MB | 26.1 MB | 7.9 MB | 9.0 MB |
| B10 tailscale | `c18e24e` | 18.1 MB | +0.05 MB | 26.2 MB | 7.9 MB | 9.0 MB |
| B11 importers | `8c4e8ca` | 18.2 MB | +0.12 MB | 26.4 MB | 7.9 MB | 9.0 MB |
| B12 quotas | `7ab2d18` | 18.2 MB | -0.03 MB | 26.5 MB | 7.9 MB | 9.1 MB |

What the table says:

- **B7 is 3 MB of the 6.** The other twelve merges add ~1 MB together, each inside the
  noise. B1/B11 (HTTP clients, parsers), B6 and B12 are not the culprits.
- **B6 is the agent's step** (+1.1 MB binary, +0.8 MB RSS): the log collector scans
  Podman every 10 s and keeps the store.
- Threads (5 server / 3 agent) and sockets (9 / 4) never changed: nothing new runs at idle,
  the growth is code pages and a few heap allocations.
- The `38ec818` row is already **14.4 MB**, not the 12.4 MB of this page's first table:
  ~2 MB came in with wave A and the design waves before any B branch. That was not bisected
  here (out of this card's scope); it is the remainder that follows.

Why B7: `webauthn-rs` 0.5 pulls OpenSSL. `ldd` on the B7 server shows `libssl.so.3` and
`libcrypto.so.3` loaded at start, and `/proc/<pid>/smaps` shows 1.75 MB + 0.3 MB resident
in those two libraries (B6 has neither), plus ~1.2 MB more of the server's own text for
webauthn-rs-core (196 KB of code), OpenSSL bindings and X.509 parsing. podkit already had
rustls for everything else, so this was a second TLS and crypto stack.

### Trims, each with before and after

Server idle RSS after 20 s, from the same script; three runs where a number has a range.
Every row is on top of the one before.

| step | server idle RSS | server binary | what |
|---|---|---|---|
| wave B as merged (`75e19ea`, DR-B) | 18.3 MB (18764 KB) | 26.5 MB | |
| 1. `webauthn-rs` 0.5.4 to **0.6.2** (RustCrypto, no OpenSSL) | 16.0-16.1 MB | 26.4 MB | `libssl`/`libcrypto` no longer linked; `ldd` shows only libc, libm, libgcc |
| 2. glibc malloc: 2 arenas, 64 KiB trim threshold | 15.4-15.5 MB | 26.4 MB | `server/src/malloc.rs` |
| 3. HTTP client built on first use | 15.0-15.2 MB | 26.4 MB | `AppState::http` is a lazy `Http` |
| **after all three** (three runs) | **15.0-15.5 MB** (15380 / 15444 / 15844 KB) | 26.4 MB | |

1. **One crypto stack.** `webauthn-rs` 0.6.2 is built on RustCrypto (`crypto-glue`), so
   `cargo tree -i openssl-sys` is now empty. It needed `jsonwebtoken` 10 instead of 12 (12
   wants `rsa` 0.10, which `crypto-glue` cannot share), which also puts `jsonwebtoken` on the
   `rsa` 0.9 already in the tree. Stored passkeys keep working: the JSON of a passkey saved by
   0.5.4 loads and re-serialises identically in 0.6 (test
   `two_factor_passkey_saved_by_webauthn_rs_0_5_loads_unchanged`, fixture generated with
   0.5.4). In-flight sign-in or registration challenges (minutes old) do not survive the
   upgrade of a running server. Test code: the soft authenticator is now a backend itself
   (`SoftPasskey`), `EncodingKey::from_secret` no longer returns a `Result`.
   Duplicates left in `cargo tree -d -e normal`: `rsa` 0.9 / 0.10-rc, `p256` 0.13 / 0.14,
   `sha2` and `base64` in two or three versions; there is one `reqwest`, one `hyper`, one
   `rustls` (ring), one `tokio`. `ring` is the only TLS provider (no aws-lc).
2. **Allocator.** glibc gives each thread its own arena and keeps 128 KiB of freed heap.
   `malloc::tune()` sets `M_ARENA_MAX=2` and `M_TRIM_THRESHOLD=64 KiB` before the runtime
   starts (skipped when `GLIBC_TUNABLES` is set, and on non-glibc targets). With
   `GLIBC_TUNABLES` alone: arena 1 = -0.34 MB, + trim = -0.8 MB, arena 2 + trim = -0.6 MB.
   It did nothing for the agent (3 threads), so only the server has it.
3. **Lazy HTTP client.** `reqwest::Client::new()` loads the platform root certificates:
   ~1 MB in a standalone probe, 0.2-0.3 MB in the server (part of that is shared with the
   Postgres TLS setup). A server with no outbound traffic does not pay it; the first webhook,
   registry, importer or catalog call does.

Looked at and left alone:

- **Statics:** the catalog, template registry, logos and asset hashes are already
  `OnceLock`/`LazyLock`, nothing builds at start. The regex crate is only in the agent.
- **Threads and pools:** four tokio workers at most and five Postgres connections with a
  30 s idle timeout were already in place (A1); `TOKIO_WORKER_THREADS=2` saves ~0.3 MB
  (within noise) at the price of throughput, so it stays at `min(cores, 4)`.
- **Feature-gating** webauthn or SSO: they are product features, not optional builds, and
  once OpenSSL was gone the remaining crates are small. `cargo bloat --release --crates`:
  `.text` is 12.8 MB, of which `server` 3.8 MB, `axum` 2.7 MB (one monomorphised handler
  per route), `std` 1.2 MB, then nothing above 310 KB (`serde_core`, `rustls`,
  `serde_saphyr`, `sqlx_postgres`, `datastar`, `image`, `webauthn_rs_core` 196 KB).
- **Non-PIE:** relocations of the position-independent binary are 0.68 MB of dirty pages;
  a static-relocation build would save them, but it gives up ASLR on a network service.
- What is left: of 15.2 MB, ~6.9 MB is the server's own text, 2.7 MB read-only data, 1.8 MB
  heap, 1.8 MB libc, 0.7 MB relocations, 0.7 MB libm; the file-backed part is shared, clean
  and reclaimable, and it grows with every route (the 2 MB between 12.4 MB and 14.4 MB).

### Result

| | before F7 | after F7 | old budget |
|---|---|---|---|
| server idle RSS | 18.3-18.8 MB | **15.0-15.5 MB** | 15.6 MB |
| agent idle RSS | 9.1-9.3 MB | 9.1-9.4 MB (unchanged) | 9.6 MB |
| `server` binary | 26.5 MB | 26.4 MB | |

`server_baseline_kb` is back to 13568: its limit is 15.9 MB, 3 % over the worst of three
runs, so a return to 18.8 MB fails. The agent baseline stays 8192 (limit 9.6 MB, it passes
at 9.1-9.4 MB); the B6 log collector is the cost of that margin being thin.

## The crate split: +6.6 MB of mapped code

Splitting `server` into `podkit-core`, `podkit-ui` and a thin binary (`c0bf36d`) moved code and
changed none of it, and the server's idle RSS went from 16.0 MB to 22.6 MB. Same machine, same
script (`--idle 20`, fresh database), Rust 1.100.0, one release build per commit:

| commit | server idle RSS | server binary |
|---|---|---|
| `f035d7e` (before the split) | 16.0 MB | 25.7 MB |
| `c0bf36d` (the split) | 22.6 MB | 27.7 MB |
| `7ab0ac8` | 22.9 MB (23.0 MB on CI) | 28.3 MB |

What it is not: `cargo tree -p server -e normal,features` is the same on both sides apart from
the two new crates. It is the binary's own code, mapped: 6.9 MB of text resident before,
13.5 MB after, which is the whole difference. Read from `/proc/<pid>/pagemap` against
the symbols of an unstripped build:

| | text | resident at idle |
|---|---|---|
| `server` crate, before | 6.9 MB | 2.2 MB (32 %) |
| `podkit_core`, after | 4.8 MB | 3.8 MB (80 %) |
| `podkit_ui`, after | 3.4 MB | 3.4 MB (100 %) |

Why: every route has a few tiny functions of axum glue, and start-up runs them once per route
when it builds the route table. `MakeErasedHandler::clone_box` is one of them, 27 bytes each.
Before the split all 711 sat in one 64 KiB stretch of the binary. After it the 714 are spread
over 134 such stretches across 10.9 MB, each next to its handler. The kernel maps 64 KiB
around every first touch of a page (fault-around), so one 27-byte function brings in its
handler's neighbours: `perf record -e page-faults` shows 105 faults in `clone_box` alone at
start-up, none before. The pages are clean and file-backed, so the kernel can drop them under
pressure; they still count as RSS.

Tried and not kept:

- **Building the API route table twice instead of cloning it for the MCP endpoint**
  (`routes_with`): 23.3 MB, so the clone is not the only start-up pass that touches them.
- **`lto = "thin"`**: the server idles at 19.5 MB, but its binary grows from 28.3 MB to
  32.8 MB and the agent goes from 9.0 MB to 9.9 MB idle, over its own budget.

The pages are the binary's own code, clean and reclaimable, and the split is worth them.
The next section gets them back another way.

## Start-up order: 22.7 MB to 12.9 MB idle

From start to idle the server runs 1.0 MB of its 15.5 MB of code: 3,800 functions, spread
over the whole binary, 219 stretches of 64 KiB (`callgrind`). The kernel maps 64 KiB around
each first touch, so 13.5 MB of code ends up resident. The crate split made this worse and
did not cause it.

`scripts/build-ordered.sh` links the release binaries with those functions first, in one
block (lld's `--symbol-ordering-file`). Same machine, same script, two runs each:

| server | `cargo build --release` | `scripts/build-ordered.sh` |
|---|---|---|
| idle RSS | 22.7 MB | **12.6-13.0 MB** |
| RSS after use | 35.1 MB | 33.4-33.9 MB |
| anonymous idle RSS | 3.9 MB | 3.9 MB |
| binary | 28.3 MB | 28.3 MB |

How it is put together:

- `server/startup.order` lists the functions, one hash per line. The hash is of the
  demangled name, so a dependency bump or a new compiler does not change it; a mangled name
  has a per-crate hash in it and does. axum's per-route glue is also listed without its
  type arguments, so a new route's glue is ordered without touching the list.
- The build links twice (`server/build.rs` passes the flags when the script asks). The
  first link writes lld's map, from which `scripts/startup_order.py` picks this build's
  symbols for the listed hashes; the second places them first. It adds about 70 s to a
  release build. A plain `cargo build` is not affected.
- `scripts/startup-order.sh` makes the list again: an unstripped build, run under
  `callgrind` through `scripts/footprint.sh --no-load`. About 4 minutes; it needs valgrind
  and an empty database.

The list goes stale when the start-up path changes: each start-up function that is not on
it can keep 64 KiB resident. The file-backed check in "Budgets" fails when that adds up to
2 MB; run `scripts/startup-order.sh` and commit the result.

Not done: the container image. The `Dockerfile` still runs `cargo build`, and its musl
target has not been tried with these linker flags.

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
