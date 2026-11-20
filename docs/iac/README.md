# Infrastructure as code

A project, or a team's whole setup, as a file called `podkit.toml`. Export it, keep it in git,
edit it, then **plan** it against a project to see what would change, and **apply** it. It works
like `terraform plan` and `terraform apply`: nothing changes until you apply, and applying the
same file twice does nothing the second time.

- [Quick start](#quick-start)
- [The file](#the-file)
- [Secrets](#secrets)
- [What is named, not described](#what-is-named-not-described)
- [Plan](#plan)
- [Apply](#apply)
- [Importing into a team](#importing-into-a-team)
- [The API](#the-api)
- [In the web UI](#in-the-web-ui)
- [What it does not cover](#what-it-does-not-cover)
- [Examples](#examples)

The schema is [`podkit.schema.json`](podkit.schema.json) (JSON Schema, draft 2020-12). Point an editor
at it for completion and checking.

## Quick start

```sh
export PODKIT=http://127.0.0.1:8080
export PK_TOKEN=pk_…                       # Settings → API tokens; `read` is enough to export and plan
P=$PODKIT/api/v1/teams/$TEAM/projects/$PROJECT

curl -s -H "Authorization: Bearer $PK_TOKEN" $P/export > podkit.toml
$EDITOR podkit.toml
curl -s -X POST -H "Authorization: Bearer $PK_TOKEN" --data-binary @podkit.toml $P/plan | jq .
curl -s -X POST -H "Authorization: Bearer $PK_TOKEN" --data-binary @podkit.toml $P/apply | jq .
```

A file that was just exported plans nothing:

```sh
curl -s -H "Authorization: Bearer $PK_TOKEN" $P/export > a.toml &&
  curl -s -X POST -H "Authorization: Bearer $PK_TOKEN" --data-binary @a.toml $P/plan |
  jq -e '.changes | length == 0'
```

## The file

`version = 1` is the first line. Everything below it is keyed by **name**, never by id, so the
same file means the same thing in any team.

```
projects.<slug>                                      name
  environments.<slug>                                name
    apps.<name>        a git repository that is built, or an image that is pulled
    databases.<name>   postgres, mysql, mongodb or redis
    stacks.<name>      a compose file, inline
    models.<name>      an inference engine serving downloaded model files
    cron.<name>        a command on a cron schedule, in an app or in a one-shot container
  networks[]           user networks that have a member in this project
```

An export is deterministic: tables are sorted by key, lists are sorted, and the same project
always produces the same bytes, so `git diff` shows only what changed.

What an application, database, stack, model and network can say is in the
[schema](podkit.schema.json). The things worth knowing:

- **Applications** have exactly one of `repo` (built from git: `ref`, `build`, `dockerfile`) and
  `image` (pulled). `port` is required. `domains` (custom hostnames) and `volumes` (mount paths) are
  lists; `env` is a table. `models` links the application to models of the same project, by name.
- **Databases** are made with a generated password, which is not in the file. Leave `version` out for
  the engine's default.
- **Stacks** hold their compose file inline. A stack made from a template says so in `source`; its
  variables are in `env`. `domains` routes a hostname to one of its services.
- **Models** name the download they serve by repository and revision (`repo`, `revision`) and the
  server it is on. The download must already exist: podkit does not fetch weights for you.
- **Networks** are the user networks (not the automatic one every environment gets). A network lists
  the resources of this project on it, with their aliases.
- **Scheduled tasks** (`cron`) are `kind = "exec"` (a command inside the running container of the
  application named in `app`) or `kind = "container"` (a one-shot container from `image`, or from the
  application's image, started on its `server` or the application's). `schedule` is five cron
  fields read in `timezone` (UTC by default). `timeout`, `concurrency` (`skip`, `queue`, `allow`),
  `retries`, `retry_delay` and `enabled` are left out of an export when they are the defaults. What a
  task runs in (`kind` and `app`) cannot change; rename the task to make a new one. Runs and their logs
  are history, not configuration, and are not in the file.

## Secrets

**A secret is never in a file podkit writes.** Environment variable values, stack variables, a
stack's template files, a git token and a registry login are each a placeholder:

```toml fragment
[projects.shop.environments.production.apps.web.env]
DATABASE_URL = "${secret:production/web/DATABASE_URL}"
```

The placeholder is `${secret:NAME}`. An exported name is `<environment>/<resource>/<KEY>` for a
variable and `<environment>/<resource>#<field>` for the rest. You can use any name you like in a
file you write by hand, and a value without a placeholder is used as it is (handy for a value that is
not secret, but then it is in git).

Values come from a separate **secrets file**, a flat TOML table that you do not commit:

```toml secrets
"production/web/DATABASE_URL" = "postgres://app:s3cret@db/app"
"production/web/SESSION_KEY" = "…"
```

- To the API, send JSON instead of the bare file:
  `{ "file": "<the podkit.toml>", "secrets": { "NAME": "value" }, "prune": false, "deploy": false }`.
- In the web UI, upload the secrets file next to the file, or paste it.

What a placeholder does when no value is given:

| The object | Without a value |
|---|---|
| exists, and the variable is set | the stored value stays; the plan shows no change |
| is new, or the variable is new | the plan has an **issue** naming the secret, and apply refuses |

A secret never comes out: not in an export, not in a plan (a changed secret shows as `(secret)`), not in an
apply report. podkit compares secrets by a SHA-256 in memory, to tell a changed one from an unchanged one.

## What is named, not described

Servers, SSH keys and registries are not part of a file: they are secrets-bearing, shared and made
once. A file names them, and they must exist in the team where it is applied.

| Field | Names |
|---|---|
| `server` | a server of the team, by its name |
| `deploy_key` | an SSH key of the team (Keys in the sidebar) |
| `registry` | a saved registry ([Registries](../api.md#registries)) |
| `repo` and `revision` of a model | a model download on that server |

A name that does not exist is a **plan issue** that says which one, before anything changes:

```json
{ "path": "production/web.deploy_key", "message": "no SSH key named \"ci\" in this team" }
```

## Plan

`POST …/plan` compares the file with the project as it is and changes nothing.

```json
{
  "project": "shop",
  "prune": false,
  "changes": [
    {
      "action": "update",
      "kind": "app",
      "environment": "production",
      "name": "web",
      "fields": [{ "field": "port", "from": 8080, "to": 9090 }],
      "will_redeploy": true
    }
  ],
  "extra": [{ "kind": "app", "environment": "production", "name": "cache", "fields": [] }],
  "unchanged": [{ "kind": "database", "environment": "production", "name": "main" }],
  "issues": [],
  "summary": { "create": 0, "update": 1, "delete": 0, "unchanged": 1 }
}
```

- **`changes`** is what an apply would do now, in the order it would do it: networks, databases,
  models, stacks, applications; then deletes in the reverse order. Each is `create`, `update` or
  `delete`, with the fields that differ. A collection entry is one field: `env.KEY`,
  `domain.host.example.com`, `volume./data`, `link.<model>`, `member.<env>/<kind>/<name>`.
- **`will_redeploy`** says that applying replaces something that runs. A database and a model are
  started by the apply itself; an application and a stack take the change on their next deploy, and
  the apply deploys them when asked to (`deploy=true`).
- **`extra`** is what exists and the file leaves out. Without `prune` it stays; with it, it is in
  `changes` as a delete.
- **`issues`** stop an apply: a name that does not exist, a secret with no value, a field that cannot
  be changed in place (a database's engine, a model, a server).
- **`unchanged`** is what already matches.

## Apply

`POST …/apply` runs the same plan, through the same services the web pages and the rest of the API
use, so every permission and every rule of those applies.

- **Permissions are checked first.** Before the first change runs, the permission every change in
  the plan needs is checked. A caller who may not do all of it changes nothing, and gets a `403`.
- **Dependency order, stop at the first error.** Networks, databases, models, stacks, applications,
  then links, domains and volumes. The first change that fails stops the run; the answer says what
  was done (`report.applied`), what failed (`report.failed`) and what did not run (`report.not_run`).
  It is `409` then.
- **Idempotent.** Apply, then apply again: the second plan is empty.
- **Deletes only with `prune=true`.** Without it, nothing is ever deleted, not a resource and not a
  variable, domain, volume, link or network member. Pruning an application removes its containers
  and its volumes (as deleting it in the UI does); a database's data volume and a stack's volumes
  are kept. A token needs the `delete` scope to prune and the `deploy` scope to deploy, besides
  `write`.
- **A project keeps at least one environment**, and an environment goes after what is in it.

An apply with issues answers `422` with the plan and changes nothing.

## Importing into a team

The team-level import makes **new projects** from a file with one or several of them. It is the
way to bring a project into an empty team, or to copy one.

```sh
curl -s -X POST -H "Authorization: Bearer $PK_TOKEN" --data-binary @podkit.toml \
  $PODKIT/api/v1/teams/$TEAM/import/plan | jq .projects
```

A project whose slug already exists is an issue: open that project and apply the file there. A
new project starts with its `production` environment; the import renames or removes it to match
the file.

A hostname (`domains`) and an application's `slug` on a server are unique across the whole
install. Copying a project within one install means changing them.

## The API

| | |
|---|---|
| `GET  /api/v1/teams/{t}/projects/{p}/export` | the project as `podkit.toml` (`application/toml`) |
| `POST /api/v1/teams/{t}/projects/{p}/plan` | the plan of a file |
| `POST /api/v1/teams/{t}/projects/{p}/apply` | apply a file |
| `POST /api/v1/teams/{t}/import/plan` | the plans of a file that makes new projects |
| `POST /api/v1/teams/{t}/import/apply` | make them |

The body is the file (`--data-binary @podkit.toml`) or JSON with secrets (see above). `?prune=true` and
`?deploy=true` can go on the query. Another team's project is `404`.

Token scopes: export and plan need `read`; apply needs `write`, plus `delete` when it prunes and
`deploy` when it deploys. A token limited to some projects cannot import into the team.

## In the web UI

A project's **Infrastructure as code** page (`/teams/{t}/projects/{p}/iac`):

1. **Export** downloads `podkit.toml`.
2. Choose a file (and a secrets file, if it has placeholders to fill) and **Plan**. The plan is shown
   as a diff: green for what is created, amber for what changes, red for what is deleted, with the
   fields. Issues are listed first.
3. **Apply** asks for a confirmation and runs it. **Prune** and **Deploy** are options you tick.

The projects page has **Import**, which makes new projects from a file the same way.

## What it does not cover

Kept out of the first version, on purpose:

- **Database backups** (targets, schedules) and **backup targets**: set them in the UI or API.
- **Git providers and webhooks.** An application's webhook secret is generated and not in the file.
- **A template as a template.** A stack made from one is exported as the stack it is (its compose
  file and variables). Its mounted files are secrets in the file and cannot be changed from it.
- **Users, roles, members, API tokens, SSH keys, registries and servers**: these are the things a
  file names. Manage them in their own pages.
- **The automatic network of an environment**, which every environment gets.
- **Database credentials**, which podkit generates.
- A `podkit export | plan | apply` command line. The API above is what it would call.

## Examples

A small application:

```toml
version = 1

[projects.blog]
name = "Blog"

[projects.blog.environments.production]
name = "production"

[projects.blog.environments.production.apps.site]
server = "local"
slug = "blog"
repo = "https://github.com/example/blog.git"
ref = "main"
port = 3000
domains = ["blog.example.com"]

[projects.blog.environments.production.apps.site.env]
SECRET_KEY_BASE = "${secret:production/site/SECRET_KEY_BASE}"
```

An image from a private registry, with a volume and a database next to it, in two environments:

```toml
version = 1

[projects.shop]
name = "Shop"

[projects.shop.environments.production]
name = "production"

[projects.shop.environments.production.apps.api]
server = "local"
slug = "shop-api"
image = "ghcr.io/example/api:1.4"
registry = "GitHub"
port = 8080
memory_mb = 512
volumes = ["/var/lib/api"]

[projects.shop.environments.production.apps.api.env]
LOG_LEVEL = "info"
DATABASE_URL = "${secret:production/api/DATABASE_URL}"

[projects.shop.environments.production.databases.main]
server = "local"
engine = "postgres"
version = "17"
memory_mb = 1024

[projects.shop.environments.staging]
name = "Staging"

[projects.shop.environments.staging.databases.main]
server = "local"
engine = "postgres"
```

A compose stack and a user network that two resources share:

```toml
version = 1

[projects.platform]
name = "Platform"

[projects.platform.environments.production]
name = "production"

[projects.platform.environments.production.stacks.queue]
server = "local"
compose = """
services:
  broker:
    image: rabbitmq:3
    environment:
      RABBITMQ_DEFAULT_PASS: ${BROKER_PASSWORD}
"""

[projects.platform.environments.production.stacks.queue.env]
BROKER_PASSWORD = "${secret:production/queue/BROKER_PASSWORD}"

[projects.platform.environments.production.apps.worker]
server = "local"
slug = "worker"
image = "ghcr.io/example/worker:2"
port = 9000

[[projects.platform.networks]]
name = "backend"
server = "local"
internal = true

[[projects.platform.networks.members]]
environment = "production"
kind = "stack"
name = "queue"

[[projects.platform.networks.members]]
environment = "production"
kind = "application"
name = "worker"
aliases = ["jobs"]
```

An application linked to a model that is already downloaded:

```toml
version = 1

[projects.assistant]
name = "Assistant"

[projects.assistant.environments.production]
name = "production"

[projects.assistant.environments.production.models.tiny]
server = "local"
repo = "acme/open"
revision = "main"
engine = "llama_cpu"
served_name = "tiny"
context_size = 4096

[projects.assistant.environments.production.apps.chat]
server = "local"
slug = "chat"
image = "ghcr.io/example/chat:1"
port = 3000

[[projects.assistant.environments.production.apps.chat.models]]
model = "tiny"
default = true
```

Scheduled tasks: the Laravel scheduler inside an application, and a nightly cleanup in a one-shot
container:

```toml
version = 1

[projects.shop]
name = "Shop"

[projects.shop.environments.production]
name = "production"

[projects.shop.environments.production.apps.web]
server = "local"
slug = "web"
image = "ghcr.io/example/shop:1"
port = 8080

[projects.shop.environments.production.cron.scheduler]
kind = "exec"
app = "web"
command = "php artisan schedule:run"
schedule = "* * * * *"

[projects.shop.environments.production.cron.cleanup]
kind = "container"
server = "local"
image = "docker.io/library/busybox"
command = "find /tmp -mtime +7 -delete"
schedule = "0 3 * * *"
timezone = "Europe/Berlin"
concurrency = "skip"
retries = 2
```

A file for several projects, for the team import:

```toml
version = 1

[projects.one]
name = "One"

[projects.one.environments.production]
name = "production"

[projects.two]
name = "Two"

[projects.two.environments.production]
name = "production"
```
