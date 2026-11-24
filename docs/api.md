# API

podkit has a JSON API under `/api/v1`. Anything the web UI does, the API does.

## Authenticate

Two kinds of bearer token are accepted:

- a **session token** from `POST /api/v1/auth/login` (email and password). It has every right you have. When the account has two-step verification on, add `"code"` to the body: a 6-digit app code or a recovery code. Without it (or with a wrong one) the answer is 401. Passkeys sign in through the web page only.
- an **API token** (`pk_…`), made in **Settings → API tokens** or with the API. It acts as you, narrowed to the scopes, projects and expiry you gave it. Use these for scripts and CI.

Wrong guesses are throttled, per account and per client address. After 5 wrong passwords, codes or passkey answers on one account within 15 minutes, sign-in waits 1 s, then 2 s, 4 s … up to 15 minutes, and a right password is refused while it waits: the answer is `429` with `Retry-After` and `Too many attempts, try again in N minutes.` An unknown email is counted and answers the same, so nothing says which accounts exist. An address that sends 30 wrong guesses (over any accounts) or 30 wrong API tokens waits the same way. A full sign-in resets the account. A lockout is written to the audit log (`session.locked`), raises a `sign_in_locked` notification for the account's teams, and a member with `update membership` clears it with `POST /api/v1/teams/{team}/members/{user_id}/unlock` or the Unlock button on the Members page.

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

## Notifications

Channels say where a message goes, rules say which events go to which channels, metric alerts raise events of their own. Settings → Notifications in the web UI manages the same things. Webhook URLs, bot tokens, signing secrets and the SMTP password are write-only: no response ever contains them (a channel lists only its host as `hint`).

```sh
N=$PODKIT/api/v1/teams/$TEAM/notifications
curl -s -H "Authorization: Bearer ***" $N/events                       # every event kind a rule can list
curl -s -X POST -H "Authorization: Bearer ***" -H 'Content-Type: application/json' $N/channels \
  -d '{"name":"Chat","kind":"discord","url":"https://discord.com/api/webhooks/…"}'
#   kinds: discord, slack (url) · telegram (token, chat_id) · ntfy (server, topic, token?) ·
#          email (to) · webhook (url, signing_secret)
curl -s -X POST -H "Authorization: Bearer ***" $N/channels/5/test      # send a test message now
curl -s -X PATCH -H "Authorization: Bearer ***" -H 'Content-Type: application/json' \
  -d '{"enabled":false}' $N/channels/5                                  # blank secret fields keep the stored ones
curl -s -X PUT -H "Authorization: Bearer ***" -H 'Content-Type: application/json' $N/smtp \
  -d '{"host":"smtp.example.com","port":587,"security":"starttls","username":"u","password":"…","from_addr":"podkit <podkit@example.com>"}'
curl -s -X POST -H "Authorization: Bearer ***" -H 'Content-Type: application/json' $N/rules \
  -d '{"name":"Failures","kinds":["deploy_failed","backup_failed"],"channel_ids":[5],"min_severity":"warning","project_id":null}'
curl -s -X POST -H "Authorization: Bearer ***" -H 'Content-Type: application/json' $N/alerts \
  -d '{"name":"Disk","scope":"server","server_id":3,"metric":"disk","threshold":90,"duration_minutes":10}'
curl -s -X PUT -H "Authorization: Bearer ***" -H 'Content-Type: application/json' $N/settings \
  -d '{"dedup_minutes":10,"quiet_enabled":true,"quiet_start_min":1320,"quiet_end_min":420,"utc_offset_min":0}'
curl -s -H "Authorization: Bearer ***" $N/deliveries                    # recent sends and the dead letters
curl -s -X POST -H "Authorization: Bearer ***" $N/deliveries/9/retry    # put a dead letter back in the queue
curl -s -H "Authorization: Bearer ***" $N/feed                          # the bell: history and unread count
curl -s -X POST -H "Authorization: Bearer ***" $N/feed/read-all
```

A failed send is retried after 30 s, 1, 2, 4 and 8 minutes, then waits as a dead letter. Each channel gets at most 20 messages a minute. The same event inside `dedup_minutes` is sent once, and followed by one "happened N times" message. During quiet hours only critical events are sent at once; the rest leave as one digest when they end. An up/down pair (server unreachable/recovered) that changes state four times in half an hour is reported once as "Flapping" and muted until it settles.

A generic webhook receives a JSON body (`event`, `severity`, `title`, `body`, `link`, `count`, `at`, `team_id`, `project_id`, `environment_id`) with `X-Podkit-Event` and `X-Podkit-Signature: sha256=<hex>`, the HMAC-SHA256 of the raw body keyed with the channel's signing secret. Links in messages need `PUBLIC_URL`; without it they are left out.

Metric alerts: `scope` is `server` (metric `cpu`, `mem`, `disk` or `gpu`, a percent) or `resource` (`resource_kind` application, database, model or stack, with `cpu` as percent of the host or `mem_mb`). An alert fires once the newest sample has been over the threshold for the whole duration, and sends a recovery when it is back under. They are judged after every metrics sample.
