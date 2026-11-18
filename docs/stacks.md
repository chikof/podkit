# Compose stacks

A stack is a docker-compose file that podkit runs as one unit. The file is
stored as written (`stacks.compose_yaml`) and **translated at deploy time** into
one `DeploySpec` per service, so what runs is always what the file and the
stack variables say now. The design and the supported subset are in
`docs/spikes/compose.md`.

## API

`/api/v1/teams/{t}/projects/{p}/stacks`

| Method and path | What |
| --- | --- |
| `POST /` | create from `{name, compose_yaml, server_id, env?}` |
| `GET /` | list |
| `GET /{id}` | stack with services, variable names (never values), warnings, missing variables |
| `PATCH /{id}` | `name`, `compose_yaml`, `env` (set), `unset_env` |
| `POST /{id}/deploy` | queue a deploy, 202 |
| `POST /{id}/stop` | stop the services, dependents first |
| `DELETE /{id}?purge_volumes=true` | remove containers and the network; volumes only with `purge_volumes` |
| `GET /{id}/services/{name}/logs` | SSE logs of one service |
| `POST /{id}/services/{name}/restart` | recreate one service |
| `PUT` / `DELETE /{id}/services/{name}/domain` | route a hostname to a service (`{hostname, port?}`) |

An unsupported compose file is **422** with `issues: [{service, path, key, message}]`,
every problem at once.

## Rules

- One private network per stack, `podkit-stack-<id>`; services resolve each
  other by compose name. A service with a domain also joins the shared `podkit`
  network under a name unique to the stack so Traefik can reach it.
- Named volumes are `podkit-stack-<id>-<volume>`.
- `depends_on` orders starts; `service_healthy` waits on the container's health
  check (a dependency without one fails the deploy instead of waiting).
- A redeploy replaces only services whose translated spec changed.
- The agent refuses to join or remove any network not named `podkit-stack-*`
  and refuses `ALL`, `SYS_ADMIN`, `SYS_MODULE`, `SYS_PTRACE`, `SYS_RAWIO`.
- Variables are encrypted at rest and write-only over the API.

## Tests

- `cargo test -p server --all-features -- stacks::` (the corpus subset is in `server/src/stacks/fixtures/dokploy`)
- `cargo test -p agent -- stack`
- All 433 fully supported Dokploy templates:
  `PODKIT_COMPOSE_CORPUS=<checkout of Dokploy/templates> cargo test -p server --all-features -- --ignored every_fully_supported`
- Live (real podman, ports 8088/8443): `cargo test -p server --all-features -- --ignored stack_live`
