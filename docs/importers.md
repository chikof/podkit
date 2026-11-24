# Importers: Coolify, Dokploy, CapRover

Moving over is: read the source, get a `podkit.toml` plus a report of what did not map, look at the
plan, apply. The importers only read the source (every request is a `GET`; CapRover's login is a
`POST` that changes nothing).

## Use

- **UI:** Settings → Import. Pick the platform, enter its address and credential, *Read and plan*,
  read the report and the plan, *Apply*. The credential is used for that one read and is never stored.
  The secrets the source held wait, encrypted, in the preview for 15 minutes.
- **CLI:**

  ```sh
  export PODKIT_URL=http://127.0.0.1:8080 PODKIT_TOKEN=pk_… PODKIT_TEAM=<team id>
  export IMPORT_TOKEN=<the source's token>     # kept out of argv
  podkit import coolify --url https://coolify.example.com            # plan only
  podkit import coolify --url https://coolify.example.com --apply    # make it
  ```

  `--server NAME` picks the podkit server (default: the team's only server), `--deploy` deploys what
  was made, `--json` prints the raw answer. Without `--apply` nothing is made.
- **API:** `POST /api/v1/teams/{team}/importers/{coolify|dokploy|caprover}/plan` and `…/apply`
  (see `crates/core/src/importers/routes.rs`).

## What is read

| Source | Credential | API (cited in the code) |
|---|---|---|
| Coolify v4 | API token (`read:sensitive` to read variable values) | <https://github.com/coollabsio/coolify/blob/v4.x/openapi.json> |
| Dokploy | API key, sent as `x-api-key` | <https://docs.dokploy.com/docs/api>, <https://github.com/Dokploy/dokploy/blob/canary/openapi.json> |
| CapRover | the dashboard password (a session token is made and dropped) | <https://github.com/caprover/caprover> |

Projects, environments, applications (git and image), variables, domains, databases, compose
services, volumes and scheduled tasks map to the objects of `podkit.toml`. Anything podkit has no
field for is listed in the report as `not supported`, `skipped` or `check`; the report is never empty
by accident, so read it before applying.

## Secrets

Variables, registry passwords and git credentials are `${secret:NAME}` placeholders in the generated
file, never values. An apply puts the values into podkit's encrypted secret store.

## Database data

Only the database itself is made (with generated credentials). **The data is not copied.** The
report says so for every database. To move it:

1. Dump on the source: `pg_dump -Fc -h <source host> -U <user> <db> > db.dump` (or `mysqldump`,
   `mongodump`, `redis-cli --rdb`).
2. Restore into the podkit database with the tool's own restore, through the database's connection
   details on its page in podkit.

A one-click dump-file restore is not built; it needs a way to feed an arbitrary file to a database
container, which podkit's backup restore (podkit-made backups only) does not offer.

## Tests

`cargo test --workspace --all-features -- importers` runs the mapping against recorded fixtures in
`crates/core/tests/fixtures/importers/`. They are hand-recorded to the shapes in each source's OpenAPI
schema and controllers, with invented names and secrets; no real instance was available. Live
verification against a real instance is a manual step.
