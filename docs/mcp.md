# MCP server

podkit is a [Model Context Protocol](https://modelcontextprotocol.io/specification/2025-06-18) server, so an AI agent (Claude, Hermes, any MCP client) can look after your servers with a scoped API token. It adds no new process: the server answers at `/mcp`, and `podkit mcp` is a small relay for clients that only speak stdio.

## What an agent can do

An agent can do exactly what its token can: the token's scopes, its team and its project limit, intersected with your own permissions. Every tool call goes through podkit's JSON API under that token, so it is checked and audit-logged like any API call. The audit log shows these calls with the actor **MCP** (and names the token).

| Tool | Scope | What it does |
|---|---|---|
| `list_projects` | `read` | Projects the token can see |
| `list_resources` | `read` | A project's applications, databases, models and stacks |
| `get_resource` | `read` | One resource; an application includes its latest deployments and their errors |
| `get_logs` | `read` | The last lines of a resource's logs: at most 500 lines, 64 KB |
| `get_metrics` | `read` | CPU, memory and network over `1h`, `24h` or `7d` (newest 60 points) |
| `get_scan` | `read` | Security scan results and badge |
| `list_templates` | `read` | One-click templates |
| `deploy` | `deploy` | Deploy an application or stack |
| `restart`, `stop` | `deploy` | Restart or stop a resource |
| `set_env_var` | `write` | Set an application variable. **Write-only**: stored encrypted, never shown again |
| `create_from_template` | `deploy` | Deploy a template into a project |
| `run_cron_task` | `deploy` | Run a scheduled task now (it is queued, and the task's own concurrency setting still applies). Audited as `cron.run` by **MCP** |
| `delete_resource` | `delete` | Delete a resource. Needs `confirm`: the resource's exact name |

A refused call comes back as a tool error naming the missing scope (`403`), or `404` for another team's or another project's resources. Secrets are never returned, whatever the token: database passwords, deploy keys, tokens and `user:password@` in URLs are masked (`•••`), even for a `read:sensitive` or `root` token.

The project tree is also offered as MCP **resources** (`podkit://projects`, `podkit://projects/{id}/resources`, `podkit://projects/{id}/{kind}/{id}`), and there is one **prompt**, `diagnose_failing_deploy`, which walks the agent through the deployment error, the logs and the scan, and tells it to change nothing until you agree.

Running a task executes its command, so the API asks the `deploy` scope for it (`POST …/cron/{task}/run`), over MCP and over plain HTTP alike.

## Make a token

Make a token for the agent, with only the scopes it needs, ideally limited to one project and with an expiry (**Settings → API tokens**, or [the API](api.md#api-tokens)). A read-only agent needs just `read`. Keep `delete` for an agent you watch.

```sh
curl -s -X POST $PODKIT/api/v1/teams/$TEAM/api-tokens \
  -H "Authorization: Bearer $SESSION" -H 'Content-Type: application/json' \
  -d '{"name":"claude","scopes":["read","deploy"],"project_ids":[3],"expires_in_days":30}'
```

## Connect a client

### Streamable HTTP (nothing to install)

The endpoint is `POST {podkit}/mcp` with `Authorization: Bearer pk_…`. It answers with plain JSON and keeps no session. A browser page cannot reach it: a request with a foreign `Origin` is refused. Put podkit behind HTTPS when the agent is not on the same machine.

**Claude Code**

```sh
claude mcp add --transport http podkit https://podkit.example.com/mcp \
  --header "Authorization: Bearer pk_1a2b3c4d_…"
```

`--header` (`-H`) and `--env` (`-e`) take several values, so give them after the name, and keep the `--` before a stdio command. The default scope is this project only; add `--scope user` to have it everywhere. Check it with `claude mcp list`.

**Hermes** (`~/.hermes/config.yaml`; with profiles, the active profile's `config.yaml`)

Keep the token out of the config file: put `PODKIT_TOKEN=pk_…` in `~/.hermes/.env` and refer to it. (Hermes refuses to connect a remote server whose `url` or `headers` name a variable that is not set, rather than send the placeholder.)

```yaml
mcp_servers:
  podkit:
    url: https://podkit.example.com/mcp
    headers:
      Authorization: "Bearer ${PODKIT_TOKEN}"
    trust: untrusted   # optional: ask before any tool that changes something
```

Start a new session or run `/reload-mcp`. The tools show up as `mcp_podkit_list_projects`, `mcp_podkit_deploy` and so on. The token's scopes still decide what works; `trust: untrusted` adds a question to you before every tool that is not read-only.

### stdio (`podkit mcp`)

`podkit mcp` reads JSON-RPC lines on stdin, relays them to `PODKIT_URL/mcp` with `PODKIT_TOKEN`, and prints the replies. Build it with `cargo build --release -p cli` (the binary is `podkit`).

| Variable | Meaning |
|---|---|
| `PODKIT_URL` | podkit's address, default `http://127.0.0.1:8080` |
| `PODKIT_TOKEN` | the API token (`pk_…`), required |

**Claude Desktop** (`claude_desktop_config.json`)

```json
{
  "mcpServers": {
    "podkit": {
      "command": "/usr/local/bin/podkit",
      "args": ["mcp"],
      "env": {
        "PODKIT_URL": "https://podkit.example.com",
        "PODKIT_TOKEN": "pk_1a2b3c4d_…"
      }
    }
  }
}
```

**Claude Code**

```sh
claude mcp add podkit --env PODKIT_URL=https://podkit.example.com \
  --env PODKIT_TOKEN=pk_1a2b3c4d_… -- /usr/local/bin/podkit mcp
```

**Hermes** (`~/.hermes/config.yaml`, with `PODKIT_TOKEN=pk_…` in `~/.hermes/.env`)

```yaml
mcp_servers:
  podkit:
    command: /usr/local/bin/podkit
    args: [mcp]
    env:
      PODKIT_URL: https://podkit.example.com
      PODKIT_TOKEN: "${PODKIT_TOKEN}"
```

The token sits in the client's config file, so give that file the same care as a password, and use a token you can revoke.

## Try it by hand

```sh
curl -s $PODKIT/mcp -H "Authorization: Bearer $PK_TOKEN" -H 'Content-Type: application/json' \
  -d '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"list_projects","arguments":{}}}'
```

Status codes: `401` for a missing, unknown, revoked or expired token (a login session does not work here), `403` for a foreign `Origin`, `405` for `GET` and `DELETE` (there is no event stream and no session).

## Tests

```sh
cargo test --workspace --all-features -- mcp
# Drives the real `podkit mcp` binary against a running server:
PODKIT_LIVE_URL=http://127.0.0.1:8080 PODKIT_LIVE_TOKEN=pk_… cargo test -p cli -- --ignored mcp_live
```
