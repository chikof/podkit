# Templates

One-click services. podkit ships none of its own: they come from the
[podkit-templates](https://github.com/podoes/podkit-templates) repository, whose
[website](https://podoes.github.io/podkit-templates/) is the catalog.

## The catalog

The gallery is empty until the administrator of the instance (the first account) presses
**Download the catalog**. podkit then downloads `index.json` and one archive of every
template, checks the archive against the SHA-256 in the index, and unpacks it under
`<data dir>/catalog`. From then on the gallery needs no network. **Refresh the catalog**
does the same again; when it fails, the catalog already there stays.

`POST /api/v1/templates/refresh` is the same action for a script. It answers
`{commit, templates, skipped}`, where `skipped` lists templates this podkit could not load
and why.

`TEMPLATE_CATALOG_URL` names another place to fetch from (default
`https://podoes.github.io/podkit-templates`). The request goes through the outbound policy,
so a private address needs `OUTBOUND_PRIVATE`. The hash in the index protects against a
broken download and nothing more: the index and the archive come from the same place, so use
an `https` address you trust.

## A template

A template is a folder in the catalog:

```
template.toml        id, name, description, category, tags, version, logo, links,
                     variables, [config] env / domains / mounts, requires, reveal, gateway
docker-compose.yml   the subset of docs/stacks.md
logo.svg | logo.png  SVG, or PNG of at most 64 KiB
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

## From the command line

```sh
podkit templates                 # the catalog
podkit templates password        # the templates that match
podkit templates deploy gotify --project shop --set main_domain=push.example.com
```

`deploy` prints the address and the generated credentials, which are shown that once.
`podkit templates --help` lists the options.

## Importing a template file

A template need not come from the catalog: hand in its `template.toml` and
`docker-compose.yml`.

```sh
podkit templates import ./my-template --project shop
```

The command prints what the files would run and the whole compose file, and asks. The
API is `POST /api/v1/teams/{t}/projects/{p}/templates/import/preview` with
`{template, compose}` (deploys nothing, answers the detail, the compose file and
`official: false`) and `POST …/templates/import` with the same two and the fields of a
deploy.

An import is not the catalog's, whatever id it gives itself:

- It is always a stack, and the stack's `source` is `import:<id>@<hash of the two files>`
  where a catalog template has `template:<id>@<version>`. Its page says "Unofficial".
- It may not set `gateway` or `[mcp]`, and cannot be linked to a model.
- It is never told about a newer version.

## When the catalog's template changes

The catalog gives each template a revision that changes only with what gets deployed: the
compose file, the variables and the config. A new description or logo does not change it.

podkit notes the template and the revision an application or a stack was deployed from
(`template_deploys`, migration 0085). When a refresh brings a different revision, the
page of that application or stack says the catalog has a newer version and links to it.
Nothing is changed for you: the deployed copy is yours. An imported template file is never
told anything.

## Checking a catalog

`server check-templates <dir>` loads every template under `<dir>/templates` the way a
refresh does, prints the ones that do not load with the reason, and exits 1 when there are
any. Run it on a checkout of podkit-templates before a change there is published.

A few templates stay in `crates/core/templates` for podkit's own tests. They are not in the
binary.

## Logos

`logos::url(name)` is `/static/logos/<name>.<hash>.<ext>`, served with
`Cache-Control: public, max-age=31536000, immutable` (a wrong hash is a 404) and
a CSP that stops an SVG from running anything. `logos::for_image("postgres:18")`
guesses a registry logo from an image reference.

## Tests

- `cargo test -p podkit-core --all-features -- templates::`
- Live (podman, ports 18090/18091, under 1 GB of images):
  `cargo test -p podkit-core --all-features -- --ignored template_live`
  (gotify as an application, it-tools as a one-service stack, umami with postgres)
