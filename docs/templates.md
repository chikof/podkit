# Templates

One-click services. A template is a folder under `crates/core/templates/<id>/` that
`crates/core/build.rs` embeds in the binary, so nothing is fetched at run time.

```
template.toml        id, name, description, category, tags, version, logo, links,
                     variables, [config] env / domains / mounts, requires, reveal, gateway
docker-compose.yml   the subset of docs/stacks.md
logo.svg | logo.png  SVG, or PNG of at most 64 KiB; or `logo = "@name"` for a file of crates/core/static/logos
```

`template.toml` keeps the shape of Dokploy's blueprints (MIT) so they import
without rewriting. Unknown keys, unknown variable types and unsupported helpers
are refused when the template loads.

## Variables

`[variables]` maps a name to a string with `${...}` expressions, or to a question:

| Expression | Value |
|---|---|
| `${password:N}` | N chars of `a-z0-9` (16) |
| `${base64:N}` | N random bytes, base64 with padding (32) |
| `${hash:N}` | N lowercase hex chars (8) |
| `${jwt}`, `${jwt:N}`, `${jwt:secret:payload}` | HS256 token, N random bytes as hex, or a token signed with a variable |
| `${uuid}`, `${email}`, `${username}` | random |
| `${domain}` | `<service>-<name slug>.<server ip>.sslip.io` of the service the variable routes to |
| `{ type = "string", default, label, description, secret }` | asked of the user |

`${timestamp}` and `${randomPort}` are refused. `${name}` of a variable is that
variable's value; any other `${...}` is left as written. Secrets (everything but
`domain`, `email`, `username`) are stored encrypted and never returned by `GET`.
`reveal = ["admin_password"]` lists generated variables the user needs to log
in with: their values are in the **deploy response, once** (like an application's
webhook secret), and nowhere else.

## Deploying

`POST /api/v1/teams/{t}/projects/{p}/templates/{id}/deploy`
`{server_id, environment_id?, name?, inputs?, model_id?}` answers 202 with
`{kind, id, name, urls, values, credentials, deploy_id}`.

- One service, one domain, an image, environment and named volumes, nothing else
  (`plain_app`): an **image application** (B2). Env vars, volumes and a custom
  domain go through their own slices; `model_id` links a podkit model for templates
  with `gateway = true` (n8n, OpenClaw), so the gateway variables land in the env.
- Anything else, including one service with a health check or a command: a
  **stack** (B4). Its mounted files are stored in `stacks.files` (migration 0037),
  encrypted, and handed to the compose parser at every deploy.
- A template is one domain per service. Routing by path is not supported.

`GET /api/v1/templates?q=&category=` and `GET /api/v1/templates/{id}` list and
describe (variables, services, translation warnings); no generated value.

## Importing Dokploy's blueprints

`scripts/templates-import` fetches Dokploy/templates, homarr-labs/dashboard-icons
and simple-icons at pinned revisions, converts every blueprint, keeps those that
pass the compose parser and translation, vendors the registry logos of
`scripts/logos.txt`, and writes `crates/core/templates/IMPORT_REPORT.md` (counts,
what was skipped and why, each template's upstream path), `NOTICE` and
`crates/core/static/logos/SOURCES.tsv`. Hand-written templates (no `upstream`) are kept
and win over a blueprint of the same id. Images of hand-written templates are
checked with `podman manifest inspect`; `--verify-images=all` checks every image
(Docker Hub rate-limits anonymous reads, so it is not the default).

## Logos

`logos::url(name)` is `/static/logos/<name>.<hash>.<ext>`, served with
`Cache-Control: public, max-age=31536000, immutable` (a wrong hash is a 404) and
a CSP that stops an SVG from running anything. `logos::for_image("postgres:18")`
guesses a registry logo from an image reference.

## Tests

- `cargo test -p podkit-core --all-features -- templates::`
- `grep '^| imported' crates/core/templates/IMPORT_REPORT.md`
- Live (podman, ports 18090/18091, under 1 GB of images):
  `cargo test -p podkit-core --all-features -- --ignored template_live`
  (gotify as an application, it-tools as a one-service stack, umami with postgres)
