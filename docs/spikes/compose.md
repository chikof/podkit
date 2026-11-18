# Spike: docker-compose subset and Dokploy template coverage

Card B3. Question: if podkit translates compose stacks into its own `DeploySpec`s
(agent supervises restarts, Traefik routes, named volumes) instead of handing them
to `docker compose` or `podman-compose`, which subset of the compose spec do real
templates need, and how much of the ecosystem does that cover?

**Answer: about 81 % of Dokploy's templates (511 of 629) and 87 % of Coolify's
(335 of 386) translate with the subset below.** Two thirds of Dokploy's need no
caveat at all. The refusals are concentrated in four causes (init containers,
`docker.sock`, host binds, `build`) and all four are product decisions, not
parser limits.

This is **static analysis only**. Nothing from the corpus was deployed. "Supported"
means "every key and every template.toml reference is inside the subset",
not "the app runs". Only the two runtime assumptions the design rests on were
tested live (health status and network aliasing, see below).

Files:

| Path | What |
|---|---|
| `spikes/compose/` | throwaway analyser crate (own `[workspace]`, not a workspace member) |
| `docs/spikes/compose-coverage.json` | one entry per Dokploy template (629) plus `summary` |
| `docs/spikes/compose-coverage-coolify.json` | same for Coolify's compose templates (386), comparison only |
| `spikes/compose/health.mjs`, `net.mjs` | live checks against a rootless Podman socket |
| `spikes/compose/bomb.yml` | alias-bomb input for the parser comparison |

Reproduce (corpora are not committed; clone them into any scratch dir):

```sh
git clone --depth 1 https://github.com/Dokploy/templates dokploy     # tested at 6313a2e (2026-11-16), MIT
git clone --depth 1 https://github.com/coollabsio/coolify coolify    # tested at 171eae0 (2026-11-18), Apache-2.0
cargo run --manifest-path spikes/compose/Cargo.toml -- dokploy --json | jq '.summary'
cargo run --manifest-path spikes/compose/Cargo.toml -- coolify --json | jq '.summary'   # coolify-only numbers
cargo run --manifest-path spikes/compose/Cargo.toml -- dokploy coolify --compare-parsers # parser table below
jq -e '.summary.total > 100 and .summary.supported > 0' docs/spikes/compose-coverage.json
```

The Dokploy `--json` summary was checked to be byte-identical to the committed file's
`.summary`. Output is deterministic (no timestamps, sorted keys).

## Coverage

Tiers, per template (a template is the worst tier of any of its services):

- **full**: every key is in the supported set and maps without loss.
- **after dropping**: supported, but one or more optional keys are ignored or
  approximated (listed under "drop reasons"). Behaviour may differ slightly.
- **refused**: at least one refusal reason. Refusal reasons are not mutually
  exclusive, so the per-reason counts add up to more than the refused total.

| | Dokploy | Coolify (compose/) |
|---|---:|---:|
| templates | **629** | 386 |
| fully supported | **433** (68.8 %) | 300 (77.7 %) |
| supported after dropping optional keys | **78** (12.4 %) | 35 (9.1 %) |
| **supported (sum)** | **511 (81.2 %)** | 335 (86.8 %) |
| refused | **118** (18.8 %) | 51 (13.2 %) |

Coolify's numbers are less comparable than they look: its templates lean on
`SERVICE_FQDN_*`/`SERVICE_PASSWORD_*` magic variables, which the analyser treats as
resolved (they would need their own generator mapping), and 18 of its templates carry
the Coolify-only `exclude_from_hc` key, which is counted as a drop.

### Top refusal reasons, Dokploy

"Only reason" is the number of templates for which this is the single refusal, i.e.
what is gained by lifting it.

| Reason | Templates | Only reason | Notes |
|---|---:|---:|---|
| `depends_on_completed` | 41 | **30** | `condition: service_completed_successfully`: run-to-completion init/migration container |
| `docker_sock` | 32 | 21 | mounts `docker.sock`/`podman.sock` (monitoring and management tools). Refused on purpose |
| `host_bind` | 22 | 10 | absolute host path (`/`, `/tmp`, `/opt/...`, `/proc`) |
| `build` | 16 | 11 | `build:` instead of `image:` |
| `dangerous_cap` | 7 | 2 | `cap_add` of SYS_ADMIN, ALL, SYS_MODULE, SYS_PTRACE, SYS_RAWIO |
| `devices` | 7 | 2 | `/dev/...` passthrough |
| `env_file` | 6 | 3 | `env_file` that is neither `.env` nor a template file mount |
| `volume_driver_opts` | 5 | 0 | named volume with `driver_opts` (bind/nfs; all four ERPNext variants and one more) |
| `network_mode` | 4 | 2 | `host`, `service:x` |
| `port_range` | 4 | 4 | `5060-5063:5060-5063` |
| `privileged` | 3 | 0 | |
| `bind_file_unmapped` | 2 | 1 | single-file bind with no matching `config.mounts` entry |
| `pid` / `replicas>1` / `runtime` | 2 / 2 / 1 | 0 / 1 / 1 | |

Of the 118 refused templates, 88 have exactly one refusal reason, 25 have two.

Lifting only `depends_on_completed` (one-shot jobs, see risks) would take
Dokploy from 511 to **541 (86 %)**. Everything else on the list is either a
deliberate safety refusal (`docker_sock`, `privileged`, `devices`, `dangerous_cap`,
`host_bind`, `pid`) or not worth the engineering for under 2 % each.

### Drop reasons (supported, with a caveat), Dokploy

| Reason | Templates | What happens |
|---|---:|---|
| `bind_to_named_volume` | 43 | relative bind (`./data:/data`, no template mount) becomes a named volume. See risk 3 |
| `drop:security_opt` | 18 | `no-new-privileges`, `seccomp=unconfined`, ... ignored |
| `drop:logging` | 16 | podkit has its own log path |
| `drop:pull_policy` | 15 | |
| `localtime_bind` | 11 | `/etc/localtime:ro` ignored; set `TZ` |
| `traefik_labels` | 11 | the template ships its own `traefik.*` labels; routing comes from the template's domains instead |
| `drop:tty`, `drop:stdin_open` | 9, 8 | |
| `drop:deploy.restart_policy`, `drop:links` | 5, 5 | supervisor owns restarts; links are redundant on a shared network |
| `drop:platform`, `drop:stop_signal`, `healthcheck_no_test`, `drop:pids_limit`, `drop:profiles`, `healthy_without_healthcheck` | 4, 3, 3, 2, 2, 2 | |

The full per-template findings (with service and detail) are in the JSON.

## Parser

Evaluated on all 1015 compose files (629 Dokploy + 386 Coolify), same input for each:

| Crate | Version / last release | Licence | Parsed | Merge keys (`<<`, 26 files) | Alias bomb |
|---|---|---|---:|---|---|
| **`serde-saphyr`** | 2.2.0, 2026-11-18, ~15.8 M recent downloads | MIT OR Apache-2.0 | 1013 | **resolved natively, 26/26**, including `!!merge <<` and merges inside anchors | rejected in 0.3 s (node budget) |
| `serde_norway` | 0.9.42, 2024-12-21 | MIT OR Apache-2.0 | 1015 | none without `Value::apply_merge()`; with it 24/26 (not `!!merge`, not a merge inside an anchored mapping) | rejected in 25 ms (repetition limit) |
| `serde_yaml_ng` | 0.10.0, 2024-05-26 | MIT | 1015 | same as `serde_norway` | rejected in 24 ms |
| `docker-compose-types` (typed) | 0.24.0, 2026-06-02, ~280 k | MIT/Apache-2.0 | 901 (**114 fail**) | rejects `<<` and any key it does not model (`platform`, `pids_limit`, Coolify's `exclude_from_hc`) | n/a |
| `serde_yml` | 0.0.13 | MIT OR Apache-2.0 | not run | | its own description: deprecated, unmaintained |

(Alias bomb: 10 nested levels of 9 aliases, run under `ulimit -v 2000000`. All three
reject it without exhausting memory.)

- **Interpolation is not the parser's job** in any of them. `${VAR}`,
  `${VAR:-d}`, `${VAR-d}`, `${VAR:?e}`, `${VAR:+alt}`, `$VAR` and `$$` are handled by
  about 80 lines in `spikes/compose/src/main.rs` (`interpolate`/`expand`), applied to
  every string after parsing. Corpus evidence that it matters: 30 Dokploy templates
  interpolate a compose variable that the template does not define (optional
  settings such as `JVM_MAX_MEM`, required tokens such as `CLOUDFLARE_TUNNEL_TOKEN`,
  and a few template bugs such as `POSTGRES_password` vs `POSTGRES_PASSWORD`).
- **`x-` extensions** are only a problem for the typed crate. With a generic
  `serde_json::Value` they pass through (and `x-` anchors work, since the merge is
  resolved before we see them).
- **The typed crate is the wrong shape**: 114 of 1015 files fail on keys that real
  templates use and that we would drop anyway. We want to read every key and decide.

**Recommendation: `serde-saphyr` into `serde_json::Value`**, then our own typed
extraction (the analyser is the prototype). Reasons: the only actively maintained generic YAML-serde option
option here, native merge keys (22 Dokploy templates use `<<`, and two of them
defeat `apply_merge`), pure Rust, a node budget against hostile templates.
The cost is strictness: it rejects the 2 Coolify files (`gramps-web`, `langfuse`)
that merge a *sequence of strings* with `<<:`, which is not valid YAML merge usage; no
Dokploy template is affected. Fallback if that matters: `serde_norway` plus
`apply_merge()`, accepting that nested merges silently survive as a literal `<<` key
(which the analyser refuses as an unknown key, so it fails loudly).

## Supported key list

Service keys, with the number of Dokploy templates that use each (of 629). "DeploySpec"
says whether `crates/proto` already has the field or B4 must add one.

| Key | Templates | Translation | DeploySpec |
|---|---:|---|---|
| `image` | 621 | `Source::Image` | exists |
| `volumes` (named, anonymous, `tmpfs`, template file mounts) | 585 | `VolumeMount` / file mount | exists; **file mounts new** (53 supported templates), `tmpfs` new |
| `restart` | 565 | supervisor policy (container itself is created with `no`) | exists (supervisor) |
| `environment` (list or map) | 545 | `env` after interpolation | exists |
| `expose` | 410 | container port; routing target | exists (`container_port`) |
| `depends_on` (list/map, `service_started`, `service_healthy`) | 303 | stack-level start order and wait | stack orchestrator, no spec field |
| `healthcheck` | 285 | Podman healthcheck on the container | **new** (217 supported templates) |
| `command`, `entrypoint` (string or list) | 177, 58 | `command`, `entrypoint` (string form split with shlex) | exists |
| `user` | 30 | | **new** (16) |
| `ports` | 26 | routed port becomes routing, others `published_ports` | exists; `/udp` and host IP **new** (8) |
| `stop_grace_period` | 26 | `stop_timeout_s` | exists |
| `deploy.resources.limits`, `mem_limit`, `cpus` | 19 / 8 / 3 | `memory_bytes`, `cpu_cores` | exists |
| `shm_size` | 16 | `shm_bytes` | exists |
| `ulimits` | 16 | | **new** (9) |
| `cap_add` / `cap_drop` | 15 / 12 | refused for SYS_ADMIN, ALL, SYS_MODULE, SYS_PTRACE, SYS_RAWIO | **new** (4 / 4) |
| `hostname`, `container_name` | 13, 1 | extra DNS aliases | exists (`network_aliases`) |
| `labels` | 12 | metadata only; `traefik.*` ignored | **new** (6) |
| `tmpfs` | 12 | | **new** (6) |
| `read_only` | 11 | | **new** (5) |
| `extra_hosts`, `working_dir`, `init`, `dns`, `sysctls`, `group_add` | 9, 9, 7, 4, 2, 1 | | **new** |
| `networks` | (most) | collapsed into the stack network | see network design |
| `configs`, `secrets` (inline `content`, or `file:` that has a template mount) | 3, 1 | file mount | **new** |
| `env_file` | 18 | only `.env`, or a file that is a template mount (parsed into env) | exists |
| `gpus`, `deploy.resources.reservations.devices` | 1 | `GpuRequest` | exists (the one user also hits another refusal) |

Ignored with a warning (drop): `logging`, `platform`, `tty`, `stdin_open`,
`pull_policy`, `stop_signal`, `profiles`, `links`, `security_opt`, `mem_reservation`,
`memswap_limit`, `pids_limit`, `cpu_shares`, `cpuset`, `oom_*`, `domainname`,
`mac_address`, `annotations`, `develop`, `post_start`, `pre_stop`, `dns_search`,
`dns_opt`, `storage_opt`, `exclude_from_hc`, other `deploy.*` (restart_policy,
placement, update_config), top-level `version`/`name`, `x-*`.

Refused: `build`, `privileged`, `network_mode` other than `bridge`, `devices`,
`pid`, `ipc`, `uts`, `userns_mode`, `cgroup_parent`, `runtime`, `extends`,
`volumes_from`, `external_links`, `include`, `scale`/`deploy.replicas` > 1,
`deploy.mode: global`, `env_file` that is not `.env` or a template mount, any
`docker.sock`/`podman.sock` mount, absolute host binds (except `/etc/localtime`),
single-file binds without a template mount, external volumes, volume drivers and
`driver_opts`, port ranges, `depends_on: service_completed_successfully`, `depends_on`
cycles, anything unknown.

Two deviations from the card's proposed list, both from the data:
`env_file: .env` and `env_file` pointing at a template file mount are accepted
(Dokploy writes `config.env` to a `.env`; Airbyte ships its env file as a mount), and
relative binds without a template mount are mapped to a named volume rather than
refused (43 templates), except when the source looks like a file (basename has a
dot), which is refused.

Counting what B4 has to add to `DeploySpec`: 241 of the 511 supported templates need
no new field at all. With only `healthcheck` and file mounts added, 453 do (88.7 % of
supported). The rest of the "new" fields each touch 1 to 16 templates.

## Template format

Keep Dokploy's `template.toml` shape so the corpus imports without rewriting, and keep
`docker-compose.yml` verbatim. Podkit stores the compose text as received and
translates at deploy time.

```toml
[variables]                       # evaluated once, when the stack is created
main_domain = "${domain}"
db_password = "${password:32}"

[config]
env = ["GHOST_HOST=${main_domain}", "DB_PASSWORD=${db_password}"]   # or a [config.env] table

[[config.domains]]                # one per routed service
serviceName = "ghost"
port = 2368
host = "${main_domain}"           # path = "/" optional
[[config.mounts]]                 # file content, mounted read-only
filePath = "./config/app.conf"
content = """..."""
```

Corpus facts (Dokploy, 629 templates): all parse as TOML (0 failures); 606 have
domains, 61 of them more than one; 95 have mounts; `${domain}` is used 680 times,
`${password:N}` 772, `${base64:N}` 144, `${hash:N}` 101, `${email}` 39, `${username}` 38,
`${jwt:N}`/`${jwt_secret}` 9, `${uuid}` 9, `${randomPort}` 3, `${timestamp}` family 3.

Podkit equivalents:

| Dokploy | Podkit |
|---|---|
| `${domain}` | a generated hostname under the instance's base domain, shown and editable on the create form |
| `${password:N}` | N random alphanumeric characters, secret, stored encrypted, write-only afterwards |
| `${base64:N}`, `${hash:N}`, `${jwt:N}` | same secret store. **Semantics of N are not verified** (length of output or of input bytes); check Dokploy's `processTemplate` before B4 and match it, since templates were written against it |
| `${uuid}` | UUIDv4 |
| `${email}`, `${username}` | random values, not secret |
| `${randomPort}` | a free host port, allocated at deploy (3 templates) |
| `${timestamp}` family | not supported in v1 (3 templates); treat as unknown helper, refuse |
| `[variables]` | resolved at stack creation into a per-stack variable table; secrets flagged. Referencing a name that is neither a helper nor a variable is a warning at import (19 templates have such references inside template.toml, e.g. `${POSTGRES_PASSWORD}`, which Dokploy leaves untouched) |
| `config.env` | the stack's interpolation environment for the compose file (and what `.env` carries in Dokploy) |
| `[[config.domains]]` | `Routing` for that service on `port`; only that service joins the ingress network |
| `[[config.mounts]]` | file written under the stack's data dir, bind-mounted read-only wherever the compose file references it (`../files/<path>`) |

Stack record: `template_id`, compose text, resolved variables, mounts, one row per
service, each translated to a `DeploySpec` with `service = "stk-{stack_id}-{svc}"`.
Volume names are `stk-{stack_id}-{volume}`. Deploy order is topological over
`depends_on`; a `service_healthy` edge waits on the container's health status;
`Strategy::Recreate` whenever a service has volumes (as today).

## Network design

One network per stack (`pk-stk-{stack_id}`). Every service of the stack joins it with
its compose service name (and `container_name`/`hostname`/network aliases) as DNS
aliases. Only services that have a domain also join the shared ingress network
(the one Traefik is on), with an alias that is unique per stack, `stk-{stack_id}-{svc}`.
Compose `networks:` sections are collapsed into the stack network; templates that
use more than one network lose that isolation (drop reason `multiple_networks`).

Tested through the Docker-compatible API on a rootless Podman 6.1.4 (aardvark-dns),
with the same calls bollard makes (`NetworkingConfig.EndpointsConfig[net].Aliases`,
`POST /networks/{id}/connect`): two stacks both having `db` and `web`, one routed
service each, and a stand-in for Traefik on the ingress network.

| Lookup | Result |
|---|---|
| `web` in stack 1 resolves `db` | stack 1's db (10.89.2.2) |
| `web` in stack 2 resolves `db` | stack 2's db (10.89.3.2), same name, no collision |
| ingress container resolves `stack1-web`, `stack2-web` | both resolve (10.89.1.2, 10.89.1.3) |
| ingress container resolves `db` or `web` | NXDOMAIN |
| stack 1's db resolves stack 2's names | NXDOMAIN (and it is not on ingress) |

So the design holds: same service names in different stacks do not collide, stack
aliases do not leak onto the ingress network, and unrouted services (databases) are
unreachable from the ingress.

## Health status through the Docker-compatible API

Tested on the same socket (`podman system service`, API 1.44, "Podman Engine 6.1.4"),
`spikes/compose/health.mjs`:

| Case | `GET /containers/{id}/json` |
|---|---|
| no healthcheck | `State.Health` absent |
| `Healthcheck {Test: ["CMD-SHELL","test -f /tmp/ok"], Interval: 2s, Retries: 1}` | `State.Health.Status`: `starting` (0 s) then `unhealthy` (1 s), after creating the file `healthy` (3 s), `FailingStreak: 0` |
| `Healthcheck {Test: ["NONE"]}` | `State.Health` absent |

So `depends_on: service_healthy` can be implemented by polling `State.Health.Status`
until `healthy`. A dependency with no healthcheck never leaves "absent"; the analyser
flags that case (2 templates, `healthy_without_healthcheck`) and the deployer should
fail fast rather than wait for it.

## Open risks

1. **Init containers.** 41 templates (30 only blocked by this) use
   `service_completed_successfully`. The supervisor currently restarts anything that
   exits. Needs a one-shot service kind: run once, no restart, record exit code,
   dependents wait for exit 0. Largest single unlock (+4.8 points).
2. **`docker.sock`.** 32 templates (21 only blocked by this) are monitoring/management
   tools. Refusing is right; mounting the user's Podman socket into a container hands
   out the whole account.
3. **Bind mounts mapped to named volumes** (43 templates) change semantics silently: an
   app that expects its image-baked or repo-shipped files in `./data` finds an empty
   volume (Podman copies image content into a *new named volume* on first use, which
   covers the common case but not host-provided files). The file-versus-directory
   split is a heuristic (basename has a dot). Needs a visible warning on the stack page.
4. **Healthchecks need systemd timers** in rootless Podman. They worked here (user
   systemd), but a host without a user systemd manager gets healthchecks that never
   run, so `service_healthy` waits would time out. Detect and refuse at import.
5. **Rootless limits.** Published host ports below 1024 (5 templates), `user:` and
   volume ownership (30 templates set `user`; named volumes get the container's uid
   mapped through the user namespace), `ulimits`, `sysctls` and capabilities are all
   bounded by the user namespace. Not tested against real images.
6. **Cross-stack reachability on ingress.** Routed services of different stacks share
   the ingress network, so they can reach each other by IP or ingress alias. Databases
   cannot be reached, but a compromised front end can talk to another stack's front
   end. Alternative with full isolation: Traefik joins each routed stack's network
   instead (Traefik is then on N networks, restarts when a stack is added). Not tested.
7. **Fixed published ports.** 20 templates publish host ports (11 supported); two
   stacks, or a stack and an app, can collide. Needs port allocation checks at deploy.
8. **Variable helper semantics** (`base64:N`, `hash:N`, `jwt`) are unverified (see
   template format). Wrong lengths would produce secrets that apps reject.
9. **Interpolation fidelity.** Our `${...}` rules are re-implemented from the compose
   spec. Edge cases not covered: nested defaults with `}`, `${VAR:?}` prompts, `.env`
   files with quoting. 30 Dokploy templates interpolate names nothing defines (see
   Parser); the stack create form must ask for the required ones (`${VAR:?}`) and treat
   the rest as empty, as compose does. Not audited one by one.
10. **Parser strictness and age.** `serde-saphyr` 2.x is the least battle-tested of the
    three and rejects 2 of 1015 files that merge sequences with `<<`. Pin the version.
11. **Classifier heuristics** (file versus directory binds, `stub` placeholders for
    template variables inside ports) and the dangerous-capability list are judgement
    calls. Counts are accurate for these rules, not for ground truth.
12. **Coverage is static.** Image availability, architecture (`platform` is dropped),
    startup races and real resource needs are not part of these numbers. A follow-up
    worth doing in B4: deploy 5 to 10 representative templates end to end (Ghost,
    a Postgres plus app pair, one with a file mount, one with `service_healthy`).
