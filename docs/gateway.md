# LLM gateway

Agents (applications) call one URL with their own key. Which model answers is a
link, not a deploy: relinking changes the backend without touching the
application's image or env.

```
application container ──► http://llm.podkit.internal:8088 ──► model-<slug>:<port>
        (podkit network)        gateway sidecar                  engine container
```

## Decision: a sidecar container, not a listener in the agent process

The card offered two ways.

**(a) The agent listens on the podkit network's gateway IP; containers use
`host.containers.internal`.** The container → agent half works. The agent →
engine half does not: with rootless Podman the host has no route to container
addresses. Measured on this workstation (Podman 6.1.4, engine container at
`10.89.0.13` on `podkit`): a request from the host process timed out
(`URLError: timed out`). That is why `POST /v1/probes` (AI 04) runs its request
from a throwaway container. A proxy in the agent process would be on the wrong
side of that wall, and the only way round it is to publish every engine's port
on the host, which AI 04 deliberately does not do (engines are never reachable
from outside their network, and have no auth of their own).

**(b) A sidecar container running the same binary in `--gateway` mode, aliased
`llm.podkit.internal`.** It is on the `podkit` network, so it reaches engines by
their `model-<slug>` aliases, and applications reach it by its alias. Nothing is
published on the host. This is what is built.

## How it fits together

- **Binary.** `podkit-agent --gateway [--gateway-dir /gw] [--listen 0.0.0.0:8088]`.
  The agent bind-mounts its own executable read-only into a
  `docker.io/library/python:3.13-slim` container (the image the model downloader and
  the probes already use, so it is on any server that has models; the production agent
  is a static musl binary and needs nothing from the image). A dev build linked
  against the Nix store gets `/nix/store` mounted read-only too.
- **Lifecycle.** The sidecar is `podkit-gateway`, registered with the supervisor
  (restarted when it dies). The agent creates it when it first gets a routing table
  with keys, and replaces it when its own binary changed (label
  `podkit.gateway.exe` = path and mtime), checked at agent start and on every table push.
- **Agent ↔ sidecar** share `<data-dir>/gateway` (mounted at `/gw`): the agent writes
  `config.json` atomically and the sidecar reloads it within a second; the sidecar
  writes its usage counters to `usage/<ns>.json` every 5 s (and on SIGTERM), and the
  agent reads and deletes those files when the server asks. No port is opened between them.
- **Server → agent** (new routes):
  - `PUT /v1/gateway/config`: `{keys: [{key_hash, models: [{model_id, name, host, port, stream_usage}]}], other_models: [name]}`.
    The whole table of one server, idempotent.
  - `POST /v1/gateway/usage/drain`: `[{key_hash, model_id, hour, requests, input_tokens, output_tokens}]`, and forgets them.
- **When the server pushes.** After every link, unlink or default change (right away,
  best effort), and on each status-sync sweep (every 30 s) for servers that have keys.
  That sweep also drains the usage into `model_usage`. So a lost push, a restarted
  agent or a replaced sidecar heals within 30 s. A crash of the sidecar loses at most
  5 s of counters; a crash between drain and database write loses that batch.

## What the gateway does

| Route | |
|---|---|
| `POST /v1/chat/completions`, `/v1/completions`, `/v1/embeddings` | forwarded to the model named by the body's `model` |
| `POST /v1/messages` (Anthropic) | forwarded the same way, no translation |
| `GET /v1/models` | answered locally: the models this key may use |

- **Key.** `Authorization: Bearer <key>` or `x-api-key: <key>`; both are stripped before
  forwarding (as is `accept-encoding`, so usage can be read from plain text). The gateway
  only knows SHA-256 hashes. Missing or unknown key: **401**.
- **Routing.** By the `model` field, among the models the key may use. A model that exists
  on the server but is not the key's: **403**. An unknown name: **404**, listing the
  names the key may use. No `model`: 400. Engine unreachable: 502.
  Errors have the body `{"type":"error","error":{"type","message"}}`, which both SDK families read.
- **Streaming.** The upstream body is passed through chunk by chunk, no buffering; the
  response head is sent as soon as the engine answers.
- **Usage.** Non-streaming: the JSON `usage`. Streaming: the SSE events (OpenAI's final chunk;
  Anthropic's `message_start` and `message_delta`), keeping the largest figure per field.
  Prompt tokens are `prompt_tokens`, or `input_tokens + cache_read_input_tokens`
  (docs/spikes/engines.md). For `/v1/chat/completions` and `/v1/completions` streams the
  gateway adds `stream_options.include_usage: true` when the client did not set it, but
  only for engines that take it (every built-in engine, per `docs/spikes/engines.json`;
  not `custom`). A request is counted even when no usage came back (zero tokens); 401, 403
  and 404 are not counted.
  Counters are per key, model and hour; `model_usage` adds them up
  (`key_hash, model_id, hour, application_id, team_id, requests, input_tokens, output_tokens`;
  no foreign keys, so billing history outlives links and models).

## Links and env injection

`application_models (application_id, model_id, is_default)`; one default per application.
The first link is the default; so is a link made with `"default": true`.

| | |
|---|---|
| `GET  /api/v1/teams/{t}/projects/{p}/applications/{a}/models` | list links |
| `POST …/models` `{model_id, default?}` | link (201) |
| `PUT  …/models/{model_id}/default` | change the default |
| `DELETE …/models/{model_id}` | unlink (204) |

Rules: `update application` and `read model`; the model must be in the same team (another team's
id is a 404) and on the application's own server (400 otherwise: the gateway of a
server reaches only that server's engines); two linked models of one application may not share a
`served_name` (409); a model with links cannot be removed (409).

One key per application (`gateway_keys`), issued with its first link and deleted with its
last. It is stored encrypted (`state.secrets`) so each deploy can hand it over, and is never
returned by the API. Deploys of a linked application get

```
OPENAI_BASE_URL=http://llm.podkit.internal:8088/v1
OPENAI_API_KEY=pk-…
ANTHROPIC_BASE_URL=http://llm.podkit.internal:8088
ANTHROPIC_API_KEY=pk-…            (the same key)
PODKIT_MODEL=<served_name of the default link>
```

These win over a user env var of the same name. Link changes apply to the **next
deploy**: a running container keeps its env, as with env vars. After unlinking the last
model and linking again, the application has a new key and must be redeployed.

## Live proof (workstation, rootless Podman 6.1.4, real server + agent + llama.cpp CPU)

Server and agent built from this branch, Postgres from `pk-pg`; a model (OLMoE-1B-7B Q4_K_S,
served as `olmoe-live`) deployed through the models API; an application linked to it and
deployed from a local image through the rollback route (its container only has to carry the env).

```
$ podman inspect <app container> …     # OPENAI_BASE_URL=http://llm.podkit.internal:8088/v1, ANTHROPIC_BASE_URL=…:8088,
                                       # PODKIT_MODEL=olmoe-live, OPENAI_API_KEY == ANTHROPIC_API_KEY
$ python3 -c 'urlopen("http://10.89.0.13:8080/health")'   # from the host
host cannot reach 10.89.0.13 -> URLError timed out         # why (a) is out
$ podman run --network podkit curlimages/curl …            # from a container on the podkit network
wrong key -> 401;  unknown model -> 404 "available models: …";
POST $OPENAI_BASE_URL/chat/completions … | jq -e '.choices[0].message.content | length > 0'   -> true
$ psql -tAc 'select count(*) from model_usage'             -> 2   (3 requests, 27/24 tokens for the first model
                                                                   — non-streaming, streaming and Anthropic — and 1, 14/8 for the second)
```

Qwen3-4B (the first model tried) answers `max_tokens: 8` with an empty `content` and the text in
`reasoning_content`, because it thinks first; the `jq` check needs a non-thinking model.

## Tests

- `devenv shell -- cargo test -p agent -- gateway`: stub engine in axum for both APIs; 401/403/404;
  streaming first byte arrives before the slow engine finishes; usage from streaming and non-streaming
  OpenAI and Anthropic responses; `stream_options` only added when allowed; credentials stripped.
- `devenv shell -- cargo test -p server --all-features -- links`: link rules, the table pushed to a
  fake agent, deploy env with the five variables and without them after unlinking, usage added into `model_usage`.
