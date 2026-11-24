# Command line

`podkit` is a small binary (the `cli` crate) that works over the [JSON API](api.md) with an API token. It can log in, list applications, deploy and follow the build, roll back, print logs and sync a `.env` file. It also serves [MCP over stdio](mcp.md) (`podkit mcp`) and runs the [importers](importers.md) (`podkit import`).

```sh
cargo install --path cli          # or: cargo build -p cli --release, then target/release/podkit
```

## Log in

Make an API token in **Settings → API tokens** (or [with the API](api.md#api-tokens)) with the scopes the commands need:

| Command | Scope |
|---|---|
| `whoami`, `apps`, `deployments`, `logs`, `env pull` | `read` |
| `deploy`, `rollback` | `read`, `deploy` |
| `env push` | `read`, `write` (and `delete` for `--prune`) |

```sh
printf %s "$TOKEN" | podkit login --url https://podkit.example.com
podkit whoami
```

`login` checks the token (`GET /api/v1/me`) and saves the address, the token and a default team in `~/.config/podkit/config.json` (`$XDG_CONFIG_HOME/podkit`, `%APPDATA%\podkit` on Windows, or `$PODKIT_CONFIG`). The file is written with mode 0600 in a 0700 directory, and a file others can read is reported. Without `--token` the token is read from stdin (typed at a prompt without echo, or piped), so it stays out of your shell history. `--team` and `--project` save defaults. `podkit logout` deletes the file; the token keeps working until you revoke it.

The environment wins over the file, which suits CI:

| Variable | Meaning |
|---|---|
| `PODKIT_URL` | podkit's address (default `http://127.0.0.1:8080`) |
| `PODKIT_TOKEN` | the API token, `pk_…` |
| `PODKIT_TEAM`, `PODKIT_PROJECT` | default team and project, slug or id |
| `PODKIT_CONFIG` | the config file |

`--team` and `--project` on a command win over both.

## Picking an application

`<app>` is an application's slug or id. The team is `--team`, `PODKIT_TEAM`, the saved one, or your only team. Without a project the application is looked for in every project you can see and must be in exactly one; otherwise the error lists the projects and asks for `--project`.

## Commands

```sh
podkit apps [--project chiko-web] [--json]        # applications of one project, or of all
podkit deployments web [--limit 20] [--json]      # newest first, with the health gate's verdict
podkit deploy web --wait                          # deploy and follow the build log
podkit deploy web --wait --settle 5m              # ... then watch for an automatic rollback
podkit rollback web [--to 1234] [--wait]          # put the previous good image back
podkit logs web -f [-n 500]                       # the running container's output
podkit env pull web [.env.web] [--force]
podkit env push web .env.web [--yes] [--prune] [--dry-run]
```

### deploy --wait

`deploy` queues a deploy of the application's configured branch or image (`POST …/deployments`); the API has no per-deploy ref or commit, so change the branch in the application's settings to deploy another one. With `--wait` it follows the build log over SSE (`GET …/deployments/{id}/build-logs`), then asks for the deployment's status:

- `running` (and the health gate, if any, says `healthy`): exit 0.
- `failed`, `cancelled`, `crashed`, or the gate kept the old container (`unhealthy`): exit 3, with the error and the gate's reason.
- still going after `--timeout` (default 30m): exit 1. The deploy itself goes on.

The health gate can also roll a release back minutes after it went live. `--settle 5m` keeps watching that long after `running` and exits 3 if the gate rolls it back. With `--json` the build output goes to stderr and the final deployment to stdout.

### rollback

`POST …/deployments/{id}/rollback` redeploys an earlier image without a rebuild. Without `--to` it picks what the health gate would: the newest deployment before the running one that is `stopped`, kept its image, and was not found `unhealthy` or `rolled_back`.

### env pull and push

Variable values are **write-only** in podkit: no response ever carries one, whatever the token. So `env pull` writes the names with empty values (`KEY=`) under a comment saying so, with mode 0600, and refuses to overwrite a file without `--force` (`-` writes to stdout).

`env push` parses the file the way the server does (comments, `export `, quotes), refuses a file with lines the server would skip, and prints what changes, by name only:

```
web (chiko-web):
  + NEW_FLAG
  ~ DATABASE_URL
    SECRET (empty in the file, kept; --allow-empty clears it)
    OLD_KEY (not in the file, kept)
1 to add, 1 to set (values are write-only, so unchanged ones are set again), 0 to delete
Push these changes to web? [y/N]
```

It then sends the variables in one request to `POST …/env-vars/bulk` (one transaction per 500 variables). A variable the application has and the file leaves empty is skipped unless `--allow-empty`, so a pulled file pushed back blanks nothing. `--prune` deletes the application's variables that are not in the file. `--yes` skips the question; without a terminal it is required. Changes take effect on the next deploy.

## Exit status

| Status | Meaning |
|---|---|
| 0 | done |
| 1 | failed: podkit unreachable or refused (the message names the status and the fix), or `--wait` timed out |
| 2 | usage or configuration mistake: unknown option, no token, unknown team, project or application |
| 3 | the deploy or rollback failed, was cancelled, or the health gate kept or rolled it back |

## GitHub Actions

`.github/actions/deploy` is a composite action that builds the CLI from this repository (or downloads a binary you point it at) and runs `podkit deploy --wait`:

```yaml
- uses: chikof/podkit/.github/actions/deploy@main
  with:
    url: https://podkit.example.com
    token: ${{ secrets.PODKIT_TOKEN }}   # scopes read and deploy, ideally one project
    app: web
    project: chiko-web
    settle: 2m
```

Inputs: `url`, `token`, `app` (required), `team`, `project`, `settle`, `timeout`, and `cli-url`, a prebuilt Linux binary to download instead of building the CLI from the action's own checkout (which needs a Rust toolchain on the runner, as GitHub's Ubuntu images have). The token reaches the CLI through `PODKIT_TOKEN`, never the command line. The step fails when the deploy does; the output `deployment` holds the final deployment as JSON.
