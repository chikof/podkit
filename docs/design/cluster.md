# Cluster design (spike M1)

Status: **proposal, for chiko to decide the v1 scope.** Nothing here is built. The
throwaway experiments are in `spikes/cluster/` (not workspace members) and their raw
output sits next to the scripts.

Question: how does podkit run as a multi-server cluster with placement and failover,
without Swarm or Kubernetes, inside the existing architecture: one control plane
(server + Postgres), one rootless `podkit-agent` per server over SSH tunnels, one
Traefik per server, per-environment networks (N1, branch `infra/N1-networks`).

## Summary

### Recommendation in one page

- **Don't build an orchestrator. Build a scheduler plus a reconciler in the control
  plane.** The agents stay dumb executors of `DeploySpec`; the control plane already
  owns desired state in Postgres and already sweeps every server every 30 s
  (`servers/sync.rs`). A cluster is: *which server* (placement), *how traffic finds it*
  (routing), *what happens when the server dies* (failover).
- **Placement:** an environment gets a *server pool*. A resource has a placement
  `pinned` (today's behaviour, and the default for everything that exists) or
  `spread` / `binpack`, plus hard requirements (GPU). A pure function
  `decide(candidates, requirements) -> server` does it, fed by the hardware inventory
  and by *reserved* resources (sum of limits), with live metrics as a guard only.
- **Traffic (the crux):** every server keeps its own Traefik. A Traefik can also route
  to **containers on other servers** through Traefik's *file provider*, reaching them at
  the other server's **private address** and a published port bound to that address. No
  overlay network needed. One "edge" server per domain keeps DNS and ACME simple.
- **Cross-server networking v1: none.** An environment network stays per server (it
  already is in N1: `UNIQUE (environment_id, server_id)`). Resources that talk over the
  environment network are co-located; a cross-server link goes over private-address
  ports. **v2:** WireGuard, which I verified works rootless in a user namespace but not
  at host level without root.
- **Image distribution v1:** the target server pulls (image sources, already works) or
  builds (git sources, already works). Add an agent `export | import` relay through the
  control plane to *pre-seed standbys*, because failover can't copy an image from a dead
  server. **v2:** a podkit-run `registry:2`.
- **Failover v1:** stateless, volume-less applications that opted in, single instance.
  Detection = existing sweep plus a grace window. Split-brain is avoided by an agent-side
  **lease with self-fencing** that is shorter than the controller's wait. Databases and
  anything with a volume never fail over automatically: recovery is a human-confirmed
  restore from the existing S3 backups onto another server ("move resource").
- **Replicas (N copies of one app across servers)** is the biggest item and is v1b, not v1a.

### Smallest useful slice

v1a = server health states + pool & placement + remote routing + move-resource for
stateless apps + automatic failover of stateless apps. That alone gives "add a second
server, say `spread`, and a dead server's web apps come back elsewhere". Replicas, the
image relay, database moves and remote links are v1b.

## Where podkit is today

Facts that shape every decision below (all from the code at `d8d5002`):

| Fact | Where | Consequence |
|---|---|---|
| `applications.server_id`, `databases.server_id`, `models.server_id`, `stacks.server_id` are `NOT NULL ... ON DELETE RESTRICT`: one server per resource, chosen by the user | migrations 0013, 0028, 0031, 0036 | Placement = choosing that column automatically. A moved resource is an update of one column plus a redeploy. |
| `UNIQUE (server_id, slug)` for applications; the public hostname is `<slug>.<dashed-ip>.sslip.io` (`applications::public_hostname`) | `applications/mod.rs` | An app's default hostname **encodes its server**. It changes when the app moves. Only a custom domain survives a move (user DNS). |
| `Source::Git` is cloned and built **on the target agent**; `Source::Image` is pulled there with optional `RegistryAuth` | `proto` `Source`, `agent/deploy.rs` | Images already get to any server without new machinery, by rebuilding or pulling. Nothing ships an image *between* servers. |
| `DeploySpec` already carries `memory_bytes`, `cpu_cores`, `gpus`, `PublishedPort { host_ip }`, `Strategy` | `proto` | The scheduler has requests to work with and the agent can already bind a published port to one address. |
| Hardware inventory (`servers.hardware` JSONB, refreshed by the sweep), `server_metrics` (CPU, memory, disk, GPU util, VRAM), `resource_metrics` | migrations 0029, 0038 | Capacity and usage inputs exist. |
| Status sync every 30 s: `health()` fails once -> `mark_unreachable`; success -> `mark_active`; `mark_crashed` for containers that vanished | `servers/sync.rs` | There is a heartbeat but no *grace*, no *down* state, nobody reacts to unreachable. |
| Networks `(server_id, environment_id)` unique; aliases are Podman DNS names, so they only resolve on one server | N1 `0040_networks.up.sql` | An environment can already span servers as separate islands. |
| Backups: `Dump`/`Upload`/`Restore` agent routes, S3 targets, queued as jobs | `agent/routes.rs`, `backups/` | Moving a database is a backup plus a restore with the pieces that exist. |
| Volumes: the agent can only `DELETE /v1/volumes/{name}`; no export/import | `agent/routes.rs` | Moving an app's volume needs one new capability. |
| Agent reached through an SSH tunnel per server; `ssh_user` may be `root` | `agents/tunnel.rs`, `servers/bootstrap.rs` | Agents never talk to each other. Server-to-server data goes through the control plane or a private network. |
| The earlier cloud notes the card mentions: `docs/` has none (`grep -ri cloud docs/*.md` is empty) | | The cloud section is written from the existing tenancy model, not from prior notes. |

## Options and trade-offs

### 1. Placement

**Today:** the user picks a server per resource.

**Proposed model**

- `environment_servers (environment_id, server_id)`: the pool. A pool of size 1 is
  today's behaviour. An environment with no rows uses its resource's current server.
- Per resource: `placement` in `{pinned, spread, binpack}` (default `pinned`; every
  existing row is backfilled `pinned` to its current server, so the migration changes
  nothing) and `requires_gpu` (count, optional min VRAM). `server_id` stays: it is
  where the resource *is*; `placement` is how podkit may choose it next time. Keeping
  the column means no query in the codebase changes in the first card.
- **GPU is a filter, not a policy.** "GPU-required" removes servers without enough
  free GPUs, then `spread`/`binpack` picks among the rest. This composes better than a
  fourth policy and matches `DeploySpec.gpus`.

**The scheduler** is one pure function, unit-testable without Postgres or Podman:

```
decide(candidates: &[Candidate], req: &Requirements, policy) -> Result<ServerId, NoFit>
Candidate    { server, state, capacity (cores, mem, gpus, vram), reserved (sum of limits),
               live (mem_used %, cpu %), labels, resources_of_this_app }
Requirements { mem, cpu, gpus, vram, affinity (must share server with X), anti-affinity }
```

1. Drop candidates that are not `healthy`, lack the GPU, or lack `capacity - reserved >= req`.
2. Drop candidates whose live memory is above 90 % (guard only: live numbers are noisy
   and make placement non-deterministic if used as the main input).
3. `spread`: lowest reserved/capacity ratio, ties by fewest instances of this app, then id.
   `binpack`: highest ratio that still fits, then id. `pinned`: the one server, or an error.
4. A resource with no limits counts as a default request (proposed 256 MiB, 0.25 core),
   stated in the UI, because otherwise nothing ever "fits".

Reserved-not-live is deliberate: placement must give the same answer twice. Live metrics
(B6) are the input for the *guard* and for a later "rebalance suggestions" view.

Replicas for stateless apps: `applications.replicas INT DEFAULT 1`. `deployments` gains
`server_id` and `replica_index`; each replica is a deployment row on its own server,
anti-affined by default. This touches deploy, status sync (`a.server_id` joins), domains
and metrics, so it is its own card (v1b).

**Rejected:** a dynamic scheduler that continuously rebalances. It moves running things
for no operator-visible reason and needs data movement to be cheap, which it isn't here.

### 2. Image distribution

Measured on this workstation (Podman 6.1.4, overlay on btrfs, a private store as the
"second server", warm page cache, **no network in the path**).
`spikes/cluster/image-transfer.sh`, output in `image-transfer.out`:

| Image | Size | `save` | `load` | zstd -3 archive | save+zstd+load pipeline | gzip -1 archive | gzip pipeline |
|---|---|---|---|---|---|---|---|
| alpine:3.24 | 8 MB | 0.08 s | 0.11 s | 3 MB | 0.19 s | 3 MB | 0.24 s |
| typical app (python:slim + 40 MB deps + 1 MB code) | 157 MB | 0.89 s | 0.63 s | 76 MB | 0.92 s | 86 MB | 2.22 s |
| python:3.13-slim | 116 MB | 0.31 s | 0.51 s | 35 MB | 0.76 s | 45 MB | 1.55 s |
| postgres:18 | 459 MB | 1.76 s | 1.32 s | 143 MB | 2.08 s | 174 MB | 5.13 s |

(The app image's 40 MB of random bytes doesn't compress, so its ratio is pessimistic.)

Wire time is then size / bandwidth: postgres:18 zstd (143 MB) is about **1.3 s on 1 Gbit,
13 s on 100 Mbit**, plus the 2 s of CPU. Uncompressed it would be 4 s / 42 s. Ship zstd.

Registry comparison, a loopback `registry:2`, same app image, then a new image that
differs in one 1 MiB layer:

| | cold | after a 1 MiB change |
|---|---|---|
| `podman push` to registry | 0.38 s | **0.10 s** (one layer) |
| `pull` into an empty store / one that has v1 | 0.84 s | **0.08 s** |
| `save \| load` | 0.63 s + 0.89 s | 0.46 s, **all 157 MB on the wire again** |

`save | load` has no layer dedup. A registry is the right answer when images are big and
deploys are frequent; it costs a long-running service, storage, GC, TLS/auth and a
network every server can reach.

| Option | Pros | Cons |
|---|---|---|
| **A. Each target pulls or builds** (today) | Zero new parts. Works for any `Source`. | N builds for N targets; a git build can differ if the branch moves, so **pin the commit sha** per deployment. Failover of a git app means a rebuild, minutes, during an outage. |
| **B. Agent `export`/`import` relayed by the control plane** (`save \| zstd` -> tunnel -> control plane -> tunnel -> `zstd -d \| load`) | No registry, no agent-to-agent trust, works over the SSH tunnels that exist. Cost is predictable (table above). Lets us **pre-seed standbys** at deploy time. | Whole image every time; control plane carries the bytes (stream, never buffer). Poor for multi-GB images deployed often. |
| **C. podkit-run `registry:2`** | Layer dedup, standard `Source::Image` + `RegistryAuth` on the pull side. | Needs a reachable address and TLS (or an insecure-registry setting per agent), storage, GC; one more thing that must be up for deploys. |
| D. Agent-to-agent `podman image scp` | Direct. | Needs SSH keys between servers. Rejected: it breaks "the control plane holds the only keys". |

**Recommendation.** v1a: **A** (nothing to build; add the sha pin). v1b: **B**, mainly to
pre-seed standby servers so failover starts a container instead of rebuilding.
v2: **C** when a team has more than ~3 servers or images over ~1 GB. This is sequenced by
need: nothing in v1a requires an image to move between servers.

### 3. Traffic

An app's default hostname encodes its server's IP, so hostnames don't survive a move
and **replicas can't share the sslip name**. Anything multi-server or failing over needs a
custom domain. The options:

| Option | How | Pros | Cons |
|---|---|---|---|
| **A. DNS only**: each server's Traefik serves the app, several A records | User adds one A record per replica server | No new parts. | DNS doesn't remove a dead server, TTLs delay moves, and **ACME HTTP-01 breaks**: Let's Encrypt validates by connecting to the name, which may land on a server that didn't request the cert. (Several validation perspectives make this worse.) Needs DNS-01 credentials. |
| **B. Edge Traefik routes to backends on other servers** over a private address (file provider) | One edge server per domain gets the DNS record and the cert. Its Traefik has a dynamic config listing every healthy backend `http://<private-ip>:<port>`. | Backend servers can come and go without DNS. Works on a LAN, in a VPC, over any VPN the user already has. HTTP-01 works (one edge). Traefik's own backend health checks drop a dead server within seconds, before the control plane notices. Same mechanism for a moved app. | Backend ports must be reachable on the private network (published on the private IP only, firewalled). The edge is still a single point; edge failure = DNS change (as today). Needs a `private_address` per server. |
| **C. Front Traefik over WireGuard** | B with a WireGuard mesh as the private network. | Encrypted, works across the internet. | Host-level WireGuard needs root (below). Podkit would own network config on servers it otherwise touches only as an unprivileged user. |
| **D. User's external load balancer** | Cloud LB / Cloudflare / HAProxy in front of the servers' Traefiks. | Real HA of the edge; TLS can end there. | Not available on a homelab; podkit can't automate it per provider in v1. |

**Recommendation: B for v1, D documented as the way to make the edge itself HA, C as v2.**

- Homelab: all servers on one LAN, give each a `private_address` (its LAN IP). No cloud LB
  needed; edge failure is one DNS record (the user's router/DDNS), documented.
- chiko's cloud: private addresses are the VPC addresses; the provider's LB or floating IP
  sits in front of the edge servers (D). Several edges for one domain need DNS-01
  (v2, below), so v1 is one edge per domain.
- Mechanism: new agent route `PUT /v1/ingress/remote-routes` carrying the whole table for
  that server's Traefik, idempotent, same pattern as `PUT /v1/gateway/config` (pushed on
  change and re-pushed by the 30 s sweep, so a lost push heals). Traefik gets one more
  provider, `--providers.file.directory=/dyn --providers.file.watch=true`; that changes the
  Traefik config hash, so it is replaced once on first use. Backends on the *same* server
  keep using Docker labels. Backend published ports bind to the private address only
  (`PublishedPort.host_ip` already exists).

Security note for B: the backend port carries plain HTTP between servers. On a LAN or VPC
that is what people already do; for anything else the transport needs v2 (WireGuard) or
Traefik-to-backend TLS.

### 4. Cross-server networking

**Two facts first.** N1 makes the environment network per `(environment, server)`; the
aliases are Podman DNS, so they resolve only on the server. Rootless Podman can't route to
container addresses from the host (measured in `docs/gateway.md`), so there is no "just add a
route" either.

**WireGuard rootless feasibility, measured** (`spikes/cluster/wg-feasibility.sh`,
`wg-e2e.sh`, plus `wg-container.out`; output in `*.out`; kernel 6.18.63, wireguard-tools 1.0.20260223):

| Check | Result |
|---|---|
| `ip link add wg0 type wireguard` as a plain user, in the host network namespace | **Operation not permitted** |
| The same inside a user+net namespace (`unshare -Urn`) | **Works.** The kernel module autoloads. |
| Inside a rootless Podman container with `--cap-add NET_ADMIN` | **Works** (`wg0` is created). |
| Two peers in separate namespaces on a veth underlay: handshake, ping, bulk TCP | Handshake in 2 s, 0.16 ms mean RTT, **1000 MiB in 0.70 s (~1.4 GiB/s)**. This is CPU-bound loopback, not a NIC figure. |
| `/dev/net/tun` in a user namespace (for userspace WireGuard such as wireguard-go) | Works on the host (`ip tuntap add`); not re-tested with a real wireguard-go, which isn't installed here |

So WireGuard **works without root only inside a namespace the user owns**. A tunnel
interface in a container's namespace helps traffic *from that container*, but it takes the
container off its Podman bridge, losing the alias DNS that environment networks provide.
To make `10.99.x.x` reachable from **every** container and from Traefik, the interface has to
be in the host namespace, which needs one-time root (`wg-quick`, a systemd unit). Bootstrap
today deliberately needs no root (`loginctl enable-linger` is the only system-level call).

| Option | Verdict |
|---|---|
| **1. One environment, one server** (as v1 rule), cross-server via private-address ports | **v1.** Nothing to build for the network; the scheduler gets an affinity rule. |
| 2. WireGuard mesh in the host namespace, routed Podman networks (a /24 per server inside the mesh, `ip route` between) | **v2.** Needs the one-time root step per server and key distribution by the control plane (the existing `crypto` crate + K1 vault style). Environment networks become real across servers, but aliases still don't resolve across servers without a DNS layer (CoreDNS or `aardvark-dns` forwarding) which is its own work. |
| 3. Userspace overlay in a sidecar (VXLAN or WireGuard in a container) | Rejected: moves containers off the bridge, loses aliases, fragile on rootless. |

**v1 rule.** Resources linked by environment-network aliases (`links`, stacks, a database
and its app) are placed together: the scheduler treats a link as affinity. If the user
insists on a cross-server link, the link injects the other server's `private_address:port`
(the database already has a `public_port`; v1b binds it to the private address) instead of
an alias. **v2** replaces that with the WireGuard mesh.

### 5. Failover

**Scope of automatic failover in v1:** an application that (a) opted in (`failover =
auto`), (b) has no volumes, (c) is stateless by its owner's declaration, (d) is in a pool of
at least 2 healthy servers. Everything else (databases, stacks with volumes, models with
state, anything pinned) stays down and **tells the user**, with a one-click "move" that goes
through restore (section 6). Never silent data movement.

**Detection.** The sweep already calls `health()` every 30 s per server. Change it to a
state machine and record the evidence:

```
healthy --(fail)--> suspect --(no success for GRACE)--> down --(success)--> recovering --> healthy
```

- `servers.last_seen_at`, `servers.state_since`; a `server_events` row per transition (audit
  and the UI's history).
- `GRACE` = 3 minutes by default, per-server 1 to 30 min. One missed 30 s sweep must not
  evict anything. Agent restarts, tunnel blips and deploys are all shorter.
- A **guard against the control plane's own network failing**: if more than half of the
  *pool* (or all of it) goes suspect in the same window, assume the problem is here, do
  nothing, and raise one alert. A pool of 2 where one server is down is exactly the
  ambiguous case; with 2 servers the rule is "only fail over if the *other* one is
  healthy", which it must be anyway to receive the work.

**Rescheduling.** A reconciler loop (same sweep) finds `failover=auto` resources whose
server is `down`, runs `decide(...)` on the rest of the pool, updates `server_id`, queues a
normal `Deploy` job with the pinned sha, and the remote-routes table (section 3) is
re-pushed. The audit log records "rescheduled from A to B because A was down for N min".

**Split-brain.** If the control plane can't reach A but A is alive (partition, tunnel
down), rescheduling gives *two* running copies. For an HTTP app behind an edge that
removes dead backends this is harmless; for a queue consumer or a cron-ish worker it
isn't. So:

1. **Lease with self-fencing.** Each successful sweep sends `PUT /v1/lease {until: now+T}`
   (agent clock, so no clock-sync dependency: the agent stores `received_at + T`). The
   supervisor (already present in the agent) stops every `failover=auto` container when
   its lease expires, `T` = 2 min. Databases and volumes are *not* fenced: killing a
   database because the control plane has a bad day is worse than the disease.
2. The controller reschedules only after `GRACE > T + margin` (3 min vs 2 min), so the old
   copy has stopped itself before the new one starts, whenever A's agent is alive.
   A's *host* being frozen (not the agent) is covered by "dead is dead".
3. **Reconcile on return.** When A comes back, the sweep lists its containers
   (`podkit.service` labels exist) and removes any whose resource now lives elsewhere. A
   never starts something the database says is on B.
4. A resource has one writer of its `server_id`: the reconciler, under a Postgres advisory
   lock per resource, so two concurrent sweeps can't place it twice.

Cost of this design: a control-plane outage longer than `T` stops *auto-failover apps*
everywhere (they fence themselves). That is a real trade-off: it buys single-writer
semantics at the price of availability during control-plane outages. It is a per-resource
setting precisely so the default (`failover = off`) never does it.

**Stateful.** v1: none automatic. Recovery is "restore the last backup on another server",
section 6. Later: Postgres streaming replicas as a database *template* (primary + standby on
two servers, promotion by the user), which is the database's own replication and needs the
private network of section 4.

**The control plane is the one thing this does not make highly available.** Postgres and
the server process are a single point; the cluster keeps running without them (containers
and Traefik don't need the control plane) but nothing is rescheduled and the lease fences
auto-failover apps after 2 min. HA for the control plane is out of scope.

### 6. Data and the "move resource" flow

Volumes are server-local. There is no shared storage and v1 doesn't add any. Moving goes
through the same primitives as backup/restore:

| Resource | Flow | Downtime | What's new |
|---|---|---|---|
| **Stateless app** | pick destination -> deploy there -> push routes -> health OK -> remove source | none (rolling across servers) | the deploy + route switch; relay if git build |
| **Database** | confirm by typing its name -> final backup to the S3 target (the existing job) -> provision an empty one on the destination -> restore (existing) -> flip `server_id`, re-push routes -> source stopped, **kept for 7 days**, then offered for removal | the dump and restore time | orchestration as one job; "restore into a different server" (today a restore targets the database where it runs) |
| **App with volumes** | stop -> `volume export` (tar a named volume through a helper container) streamed zstd to the destination's `volume import`, through the control plane; or to S3 and back | the copy time | **two agent routes**, `GET /v1/volumes/{name}/export` and `POST /v1/volumes/{name}/import` |
| **Model** | do not copy. Re-download on the destination from the `model_files` source (it records where the files came from) and start there | the download | none beyond the move |

Every move is a job with steps in the existing job log. Source data is never deleted by the
move itself. If a step fails, the resource stays where it was. I did not measure dump or
restore time; it depends on the database and is the number the user will care about, so the
card asks for it to be shown (the backup job already records its size and duration).

### 7. The cloud angle

There are no earlier cloud notes in `docs/` (checked), so this maps the design onto what the
code does today: teams are the tenant, and cross-team access gives 404.

- **Servers belong to a team** (`servers.team_id`). Shared GPU nodes belong to a *platform*
  team, so tenants need a way to *use* a server they don't own: a `server_grants (server_id,
  team_id, quota)` table, and the scheduler's candidate list becomes "the team's own servers
  plus granted ones". That one table is the cloud's main schema delta; it also fits the
  pool design (a pool is a list of candidates).
- **Isolation.** Every rootless container on a shared agent runs under one Unix user, so
  isolation is the kernel's container isolation, enough for the user's own teams, **not
  enough for hostile tenants**. For those: one agent + one Unix user per tenant per host (rootless
  gives separate user namespaces and stores for free, and the architecture already is "one
  agent per rootless user"), or a microVM runtime (Kata, gVisor) as a later option. Env
  networks are already per tenant.
- **GPU sharing.** CDI hands a container a whole device (`nvidia.com/gpu=N`, per
  `docs/spikes/gpu-cdi.md`). The scheduler can do whole-GPU allocation exactly. Fractional
  sharing (MIG, time slicing) is hardware-dependent; until then VRAM is *bookkeeping*
  (sum of requests vs `vram_total` from the inventory), not enforced. State that in the UI.
- **Traffic.** The provider's LB or a floating IP in front of edge servers (option D);
  wildcard domains per tenant need DNS-01, so the DNS-01 work in v2 pays twice.
- **Quotas and billing** hang off the same reservation numbers the scheduler uses
  (`reserved` per team), which is another reason to base placement on reservations rather
  than live metrics.

## v1 work items

Sizes: S under a day, M one to three days, L about a week. IDs are proposals; migration
numbers must be reserved by chiko (the table in the ground rules is full through `0047`).
Order is dependency order. Each card keeps the usual rules: tests next to the code, live
tests `#[ignore]`d, `smoke.d` section, UI in the Quiet system.

### v1a (smallest useful slice)

**CL1: Server health states and grace (S).** Depends on none.
`servers.last_seen_at`, `state_since`, state `healthy | suspect | down | recovering`;
`server_events` table; per-server `failover_grace_s` (default 180). The sweep drives the
state machine; mass-failure guard (more than half of a pool suspect at once = alert, no
down). `servers.private_address` (nullable, validated IP) editable in the server settings.
*Accept:* with a fake clock, one failed sweep gives `suspect`, no success for grace gives
`down`, one success gives `recovering` then `healthy`; the guard test with 3 of 4 suspect
yields no `down`; the servers page shows state and since; transitions are audited; no
change in behaviour for single-server installs.

**CL2: Server pool and the scheduler (M).** Depends on CL1.
`environment_servers`; `placement` and `requires_gpu` on application/database/model/stack
(backfilled `pinned`); `placement::decide` as a pure module with the candidate/requirements
types; "Auto" option in the create forms (pool of the environment); a read-only "why this
server" explanation on the resource page.
*Accept:* unit and property tests: never overcommits reserved memory, same inputs same
output, GPU filter, `spread` vs `binpack` on a fixed fixture, `NoFit` errors name the
constraint; a resource with `pinned` is unchanged by the migration (a snapshot test over
the old rows); cross-team servers are never candidates (404 semantics kept).

**CL3: Remote routing through private addresses (M).** Depends on CL1.
Agent: Traefik file provider (+ one-time replace), `PUT /v1/ingress/remote-routes`
idempotent; published ports for remote-served apps bind to `private_address`. Control
plane: a routes table per edge server computed from placements, pushed on change and by the
sweep. Traefik health checks on backends.
*Accept:* an `#[ignore]` live test with two agents (two private Podman stores, loopback
addresses `127.0.0.1` and `127.0.0.2`): an app on server B is served through server A's
Traefik by host header; stopping B's container makes A answer 503 within the health-check
interval; a re-push after restarting A's agent restores the route; an app on B is not
reachable on B's *public* address through the published port.

**CL4: Move a stateless app (S).** Depends on CL2, CL3.
Job `ApplicationMove`: deploy on the destination, push routes, wait healthy, retire the
source. UI: "Move to server" with the pool's servers and the hostname consequence (sslip
name changes; custom domains keep working only if the edge is unchanged).
*Accept:* move A to B with no failed request in a 1 req/s probe during the move (rolling);
a failing health check on B leaves the app running on A and the job failed with the
reason; forged POST by another team's member is refused; the audit log records it.

**CL5: Automatic failover of stateless apps (M).** Depends on CL1, CL2, CL3, CL4.
`applications.failover` (`off` default | `auto`), refused when the app has a volume;
agent `PUT /v1/lease` and supervisor fencing of `auto` containers; reconciler with advisory
locks; orphan removal when a server returns; alert event when a non-eligible resource's
server goes `down`.
*Accept:* `#[ignore]` live test: block the tunnel to A -> A's `auto` container stops itself
within the lease, B starts the app after grace, the edge routes to B; unblock -> A removes
nothing wrongly and its stale copy is gone; a database on A is untouched throughout; the
mass-failure guard test (all servers suspect) reschedules nothing; two concurrent
sweeps place the app once (lock test).

### v1b (once v1a is used)

**CL6: Image relay and standby pre-seeding (M).** Depends on CL5.
Agent `GET /v1/images/{ref}/export` (zstd stream) and `POST /v1/images/import`; control plane
relays without buffering; digest checked; deploy tags images by commit sha; after a
successful deploy, optionally seed `N` standbys; failover prefers a seeded server and
falls back to a rebuild.
*Accept:* a 460 MB image (postgres:18) moves in under 10 s on loopback and the digests
match; a cancelled transfer leaves nothing in the target's store; failover onto a seeded
server skips the build (assert no build phase in the deploy events).

**CL7: Replicas (L).** Depends on CL2, CL3.
`applications.replicas`; `deployments.server_id` and `replica_index`; spread with
anti-affinity; rolling update per replica; the edge's backend list covers all; status
sync and metrics read `deployments.server_id`; default hostname rules (replicated apps
need a custom domain, enforced in the UI).
*Accept:* 3 replicas land on 3 servers under `spread` and on one under `binpack`; killing
one leaves the others serving; a deploy never takes all replicas down at once; `UNIQUE
(server_id, slug)` still holds; existing single-replica apps unchanged.

**CL8: Move a database and an app with volumes (M).** Depends on CL4.
`volume export/import` agent routes; `DatabaseMove` job: backup, provision, restore on
the destination, flip, keep the source stopped for 7 days; confirm-by-name dialog.
*Accept:* a Postgres database with a known table moves with row counts equal on both
sides; a failure at restore leaves the source running; the source can't be removed before
7 days without an explicit confirm; the job log shows the timings per step.

**CL9: Cross-server links (S).** Depends on CL1.
A link to a database on another server injects `private_address:port`; the database's
published port binds to the private address; the scheduler's affinity rule defaults to
co-location and warns when overridden.
*Accept:* an app on A reads from a database on B in the live test; the link is refused with a
clear message when either server has no `private_address`; the port is not reachable on a
public address.

### v2 (not proposed for a card yet)

- Host WireGuard mesh with a one-time root bootstrap step; routed environment networks
  and cross-server aliases (needs a DNS layer).
- DNS-01 for ACME (provider credentials in the K2 secrets style) so several edges can
  serve one domain; then multi-A edges.
- `registry:2` run by podkit (layer dedup) with GC.
- Postgres replica template with user-initiated promotion.
- Floating-IP/DNS provider integrations for edge failover.
- `server_grants` and per-tenant agents for the cloud.

## Risks

| Risk | Impact | Mitigation |
|---|---|---|
| **Split-brain on a partition** | Two copies of a non-idempotent workload | Lease + self-fencing, grace longer than lease, `failover=off` default, databases never auto-moved |
| **Lease fences apps during a control-plane outage** | Auto-failover apps stop when podkit is down longer than 2 min | Explicit, per-resource opt-in, shown in the UI; the trade is availability for single-writer |
| **False positives** (tunnel blip, agent upgrade) | Needless moves | Grace 3 min, `suspect` state, mass-failure guard, reconciler skips servers mid-deploy |
| **Control plane partitioned from the whole pool** | Mass reschedule | The "more than half suspect" guard |
| **Edge is a single point** | Domain down until DNS changes | Documented; provider LB (cloud); multi-edge with DNS-01 in v2 |
| **Backend traffic is plain HTTP on the private network** | Eavesdropping on untrusted networks | Documented requirement; WireGuard in v2 |
| **ACME with several A records** | Certificate issuance flakes | One edge per domain in v1; DNS-01 before multi-edge |
| **sslip hostname moves with the server** | Bookmarks and webhooks break on a move | Custom domain recommended and surfaced; the UI shows the consequence before moving |
| **Rootless prevents host-level overlay** | No WireGuard without root | Measured; v1 doesn't need it; v2 asks for a one-time root step per server through `needs_input`, as other root changes do |
| **Reservation vs reality** (limits unset or wrong) | Overcommit, OOM | Default requests when unset; live memory guard at 90 %; "limits missing" warning in the pool view |
| **Stateful move is slow or fails midway** | Long downtime | Source kept and untouched; confirm; timings shown; not automatic |
| **Control plane is a single point** | No rescheduling during an outage | Out of scope; containers keep running, documented |
| **Scope**: replicas touch deploy, sync, domains and metrics | Regression across a wide surface | Own card (CL7) after the scheduler is proven; `server_id` kept so earlier cards change no query |
| **Measured numbers are local** | Real servers are slower | Table gives size and formulae; CL6 acceptance re-measures on a real pair of servers |

## Decisions I need from chiko

1. **Scope:** v1a only, or v1a + v1b? My advice is v1a first, then decide with real use.
2. **Private address:** are you happy with the user supplying `private_address` per server
   (LAN or VPC IP) rather than podkit creating a network? This is the choice that avoids root.
3. **Failover default:** `off` with an explicit opt-in per app (my advice), or on for
   volume-less apps?
4. **One edge per domain in v1** (no multi-A with ACME): acceptable?
5. **Cloud:** should `server_grants` be designed now (schema only, in CL2) or left for the
   cloud card?

## What was run

| File | What |
|---|---|
| `spikes/cluster/image-transfer.sh` / `.out` | save/load, zstd/gzip, registry:2 push/pull timings |
| `spikes/cluster/wg-feasibility.sh` / `.out` | WireGuard and TUN as a plain user, host vs user namespace |
| `spikes/cluster/wg-e2e.sh` / `.out` | two namespaces, WireGuard handshake, ping, 1000 MiB TCP |
| `spikes/cluster/wg-container.out` | `ip link add wg0 type wireguard` in a rootless Podman container |

All of it ran in a private Podman store or an unprivileged user namespace and has been
removed (store, registry container, test images, the pulled `registry:2`). Not tested:
wireguard-go, real network links between two hosts, Traefik's file provider against a
remote backend (documented Traefik behaviour, to be proven by CL3's live test), database
dump/restore time.
