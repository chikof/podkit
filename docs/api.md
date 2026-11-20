# API

podkit has a JSON API under `/api/v1`. Anything the web UI does, the API does.

## Authenticate

Two kinds of bearer token are accepted:

- a **session token** from `POST /api/v1/auth/login` (email and password). It has every right you have.
- an **API token** (`pk_…`), made in **Settings → API tokens** or with the API. It acts as you, narrowed to the scopes, projects and expiry you gave it. Use these for scripts and CI.

```sh
export PODKIT=http://127.0.0.1:8080
export PK_TOKEN=pk_1a2b3c4d_…            # shown once when made
curl -s -H "Authorization: Bearer $PK_TOKEN" $PODKIT/api/v1/teams | jq .
```

## API tokens

| Scope | Allows |
|---|---|
| `read` | `GET`. Secrets (such as a database's connection string with its password) are redacted. |
| `read:sensitive` | `read`, and secrets are included. |
| `write` | create and update |
| `deploy` | deploy, restart, stop |
| `delete` | delete |
| `root` | everything, and the only scope that can manage tokens |

A token is the **intersection** of three things: the scopes it carries, your own permissions in the team (a token never does more than you can), and its project limit. A token works only in the team it was made in; other teams answer `404`. A project-limited token gets `404` for other projects.

Failures: `401` for a missing, unknown, revoked or expired token; `403` when a scope is missing; `404` outside its team or projects. A token never works for the web UI.

The secret is `pk_<prefix>_<secret>`. It is shown once, at creation; only a SHA-256 of it is stored, so a lost token cannot be recovered. Revoke it and make another. Revoking takes effect immediately. `last_used_at` is updated at most once a minute.

```sh
# Make a token (needs a session token or a root token).
curl -s -X POST $PODKIT/api/v1/teams/$TEAM/api-tokens \
  -H "Authorization: Bearer $SESSION" -H 'Content-Type: application/json' \
  -d '{"name":"ci","scopes":["read","deploy"],"project_ids":[3],"expires_in_days":90}'
# → {"id": 7, "prefix": "1a2b3c4d", "secret": "pk_1a2b3c4d_…", …}

curl -s -H "Authorization: Bearer $SESSION" $PODKIT/api/v1/teams/$TEAM/api-tokens   # yours
curl -s -X DELETE -H "Authorization: Bearer $SESSION" $PODKIT/api/v1/teams/$TEAM/api-tokens/7
```

`project_ids` empty or omitted means every project of the team. `expires_in_days` omitted means no expiry.

## Registries

A saved registry is a login for a private container registry. Image applications, stacks and templates pick one with `registry_id` instead of carrying credentials. The password is write-only: no response ever contains it.

```sh
curl -s -X POST $PODKIT/api/v1/teams/$TEAM/registries \
  -H "Authorization: Bearer $PK_TOKEN" -H 'Content-Type: application/json' \
  -d '{"name":"GitHub","url":"ghcr.io","username":"robot","password":"ghp_…"}'

curl -s -H "Authorization: Bearer $PK_TOKEN" $PODKIT/api/v1/teams/$TEAM/registries
curl -s -X POST -H "Authorization: Bearer $PK_TOKEN" $PODKIT/api/v1/teams/$TEAM/registries/5/test   # log in from the server
curl -s -X PATCH -H "Authorization: Bearer $PK_TOKEN" -H 'Content-Type: application/json' \
  -d '{"name":"GitHub","username":"robot","password":"new-token"}' $PODKIT/api/v1/teams/$TEAM/registries/5   # omit password to keep it
curl -s -X DELETE -H "Authorization: Bearer $PK_TOKEN" $PODKIT/api/v1/teams/$TEAM/registries/5     # refused while in use
```

`POST …/registries/test` tries a login without saving it. A registry can only be used for images of its own host.
