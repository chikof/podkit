# Builds: layer cache and build server

Applications built from a repository (Dockerfile or Nixpacks) are built by the agent with
Podman. This page says what is kept between builds, where a build can run, and what it may use.
Everything is optional: with nothing configured an application builds on the server it runs on,
reuses layers, has no caps, and the cache is kept under 10 GB.

## Layer cache

Podman keeps the layers of every build as untagged intermediate images. A later build with the
same steps and the same files reuses them, so only the steps after the first change run again.
That is the cache; podkit adds no cache service.

- **Per application: on or off.** App → Build → "Reuse layers from earlier builds". Off asks Podman for a clean build
  (`nocache`) every time.
- **Nixpacks** produces a Dockerfile (`.nixpacks/Dockerfile`) and builds it the same way, so it
  is cached by the same layers.
- **Package-manager caches.** Podman builds honour `RUN --mount=type=cache,target=...` in a
  Dockerfile (for example `/var/cache/apk`, `/root/.cache/pip`, `/root/.npm`). What a package
  manager downloaded stays on the server between builds, even when the step itself has to run
  again. The live test builds a Dockerfile that uses one.
- **Pruning, by size, per server.** Settings → Builds shows what the cache takes on each server
  and lets you set its size (default 10 GB; 0 keeps nothing beyond the images in use). After
  every build the agent prunes down to that size. "Prune to the size" and "Clear the cache" do it now.
  - What counts as cache: layers that no tagged image and no container uses. The layers of a
    running app's image are never touched.
  - What goes first: the oldest. Layers are removed newest-first within the plan, so a child goes
    before its parent.

## Build server

By default an application builds where it runs. A team can name a **build server** (Settings →
Builds), and one application can name its own (App → Build), so a build does not compete with
what is serving.

- Choice order: the application's server, else the team's, else the server it runs on.
- The build runs on the build server's agent (`POST /v1/build`), clones the repository there,
  and writes into the same deployment log as a local build, after a line "building on <server>".
- **Delivery.** M1 (`docs/design/cluster.md`, "Image distribution") recommends agent-to-agent
  save/load for v1 over a registry: a registry is a service that has to be running, and
  moving an image is rare enough that sending changed layers only does not pay yet. So the
  control plane relays `podman save` from the build server into `podman load` on the target as
  one stream (never held in memory) and checks that the image on the target has the content id
  of the one sent. A mismatch fails the deploy. The deploy then runs the image without building.
- The build server must be active (its agent running) and belong to the team; a server of
  another team is "not found".

## Limits

- **CPU and memory caps** (Settings → Builds, team-wide) go to Podman's build as `cpuperiod` /
  `cpuquota` and `memory` / `memswap` (no swap on top). Memory above 2 GiB is sent as 2 GiB
  because of the client's 32-bit field. Both are empty by default.
- **Builds at once** per server (default 2, 1 to 16). The rest wait in the order they came, and
  the deployment log says "waiting for a build place on <server>". The count is in the control
  plane's memory; after a restart it starts from zero (the interrupted deploys are failed or
  requeued as before).

## Where it is

| Where | What |
|---|---|
| Settings → Builds | team build server, CPU and memory caps; per server: builds at once, cache size, prune, clear |
| App → Build | strategy, Dockerfile path, reuse layers, build server (git apps only) |
| Migration `0064` | `build_settings` (team), `server_build_settings`, `app_build_settings` |
| Agent | `POST /v1/build`, `GET /v1/build-cache`, `POST /v1/build-cache/prune`, `GET /v1/images/export`, `POST /v1/images/import` |

Every change is in the audit log (`builds.settings`, `builds.server`, `builds.prune`,
`app.build`).

## Measured times

Live test `agent/tests/build_cache.rs`, run on the development machine with rootless Podman
against the real socket (`cargo test -p agent --test build_cache -- --ignored --nocapture`). The
sample Dockerfile is `FROM alpine:3.21` with two steps of `sleep 4` standing in for an install
and a compile (one of them with a cache mount). The base image was already pulled.

| Build | Time |
|---|---|
| Cold (cache off) | 8.62 s |
| Same Dockerfile, cache on (layers reused) | 0.06 s |
| Cache off again | 8.63 s |

These are the best case: nothing changed between the builds. A real application that changes
its source but not its dependencies reuses the layers up to the first changed `COPY`, so the
gain is the time of the steps before it. The numbers are one run, not an average. Not measured:
a Nixpacks app, a build on a second server (the delivery steps are tested against stub agents;
the relay itself was not timed on a real network), and the cost of the prune after the build.

## Not done

- No registry: every deploy to another server sends the whole image, not the changed layers.
- Concurrency is per control plane process; two control planes would not see each other's builds.
- Caps are team-wide, not per application.
