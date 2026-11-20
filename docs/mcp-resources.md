# MCP servers as resources

An MCP server is an application that speaks the [Model Context Protocol](https://modelcontextprotocol.io).
podkit deploys it like any other application, keeps it off the internet, and lets agent
applications reach it through the AI gateway with a key of their own.

## Make one

- **From a template.** Search the templates for `mcp`: Fetch, Filesystem, Memory and GitHub are
  there. The template deploys an application and registers it as an MCP server.
- **From an image or a git repository.** Project → *New MCP server*, or
  `POST /api/v1/teams/{team}/projects/{project}/mcp-servers`.

Pick a transport:

| Transport | The image… | podkit… |
|---|---|---|
| `http` | serves streamable HTTP on `port` at `path` (default `/mcp`) | nothing; it just runs |
| `stdio` | speaks MCP on standard input and output | starts it behind `podkit-agent --mcp-bridge`, which serves streamable HTTP on `port` |

The bridge is the agent binary, mounted read-only into the container; no third-party bridge
image. It serves one child process to every client (request ids are rewritten and restored),
answers `POST <path>` with `application/json`, and answers `GET`/`DELETE` with 405 (no
server-initiated stream, no sessions). `GET /healthz` is `ok`. Without a `command`, the image's
own entrypoint and command are used.

An MCP server gets the network alias `mcp-<slug>` on its environment network and no public
route.

## Link it to an application

Application page → *MCP* tab → *Link an MCP server* (or `POST …/applications/{app}/mcp`).
Both must run on the same server: the gateway reaches only the environment networks it is
attached to. The link has its own key, stored hashed for the gateway and encrypted for the next
deploy. The application gets, on its next deploy:

- `PODKIT_MCP_SERVERS`: a JSON list of `{name, url, key}`
- `MCP_<NAME>_URL` and `MCP_<NAME>_KEY` per link (`<NAME>` is the slug upper-cased, `-` → `_`)

The URL is `http://llm.podkit.internal:8088/mcp/<slug>`. The gateway:

- answers **401** to an unknown key and **403** to a key that belongs to another link;
- strips the client's credentials, forwards every other header (`Mcp-Session-Id`,
  `Last-Event-ID`, …), and streams JSON and SSE answers back;
- counts every forwarded request per key and hour. The server page shows 24 h and 7 d.

A variable the user set under the same name wins over the derived one. Unlinking takes effect
on the next deploy, but the key stops working at once.

## The server page

Project → the MCP server. It shows the connection, a **List tools** button (a live `tools/list`
through the gateway, run from a short-lived container), the linked applications, usage and the
log. Tool names and descriptions come from the server and are only ever escaped text.

An MCP server cannot be removed while applications are linked to it.

## Tests

```
devenv shell -- cargo test --workspace --all-features -- mcp_resource
devenv shell -- cargo test -p server --all-features -- --ignored mcp_resource_live
```

The ignored live test needs Podman and the network; see `server/src/mcp_servers/tests.rs`.
