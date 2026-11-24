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

`GET /api/v1/me` says who the caller is: `id`, `name`, `email`, and for an API token `token` (`id`, `team_id`, `scopes`, `project_ids`; `null` for a session). It never carries the secret, and needs only `read`, also for a project-limited token. The [command line](cli.md)'s `podkit whoami` shows it.

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

A team you are not a member of is `404` for everyone, signed in with a token or a session, on every `/api/v1/teams/{team_id}/…` route and whatever the method or body: the answer never says whether the team exists. A member who lacks a permission still gets `403`.

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

## Promote between environments

Deploys the image an application runs now to the matching application of another environment of the same project, without building it again. How the match is found and what the target keeps is in [environments.md](environments.md#promote).

```sh
A=$PODKIT/api/v1/teams/$TEAM/projects/$PROJECT/applications/$APP   # the source, e.g. api-staging
curl -s -H "Authorization: Bearer $PK_TOKEN" $A/promote | jq .
# → {"application_id": …, "environment": {"id": …, "name": "Staging", "slug": "staging"},
#    "source": {"deployment_id": 9203, "commit_sha": "3e7b119…", "image": "podkit-app-12:3e7b119"},
#    "destinations": [{"environment": {…production…}, "application": {"id": 14, "slug": "api", "server_name": "node-01",
#      "running_deployment_id": 9150, "running_image": "…"}, "refusal": null, "moves_image": false, "already_running": false}]}
curl -s -X POST -H "Authorization: Bearer $PK_TOKEN" -H 'Content-Type: application/json' \
  -d '{"environment_id": 42, "deployment_id": 9203}' $A/promote
# → 202 {"id": …, "application_id": 14, "status": "queued", "image_tag": "podkit-app-12:3e7b119",
#        "promoted_from": 9203, "promoted_from_environment": "Staging", …}
```

`GET` needs `read application` and `read deployment`; `POST` needs `create deployment` (what deploying the target needs) and, for a token, the `deploy` scope. `source` is `null` when nothing runs. A destination with no matching application has `application: null` and a `refusal` saying what was looked for. `deployment_id` is optional: when given and the source now runs another deployment, the answer is `409`, so you never ship a release you did not look at. Its own environment, nothing running, or no match is `400` with the reason; an environment of another project is `404`. The new deployment is a normal one of the target: poll it at `…/applications/{target}/deployments/{id}`. Deployment responses carry `promoted_from` and `promoted_from_environment` when they were promoted. The event is `app.promote`, on the target, with the source environment, application, deployment and image.

## Sign up

```sh
curl -s -X POST $PODKIT/api/v1/auth/register -H 'Content-Type: application/json' \
  -d '{"name":"Ada","email":"ada@example.com","password":"correct horse battery","setup_token":"3f9c…"}'
```

`201` on success. While no account exists, `setup_token` is required: the one-time token podkit printed in its log when it started (or `SETUP_TOKEN`). Without it the answer is `403` (`a setup token is required …`), with a wrong one `403` (`wrong setup token …`), and wrong ones are throttled like wrong passwords (`429` with `Retry-After`). Once the first account exists the field is ignored and `REGISTRATION` decides. To sign up with an invite, send `"invite": "<token from the link>"` instead; `email` must be the invited address.

## Invites

A member with `create membership` invites an address with a role; reading the list needs `read membership`, revoking `delete membership`. The role may not grant anything the inviter's own role does not. API tokens need the `root` scope for every change, like members.

```sh
I=$PODKIT/api/v1/teams/$TEAM/invites
curl -s -X POST -H "Authorization: Bearer $SESSION" -H 'Content-Type: application/json' $I \
  -d '{"email":"dee@example.com","role_id":2}'
# → 201 {"id": 9, "email": "dee@example.com", "role_id": 2, "role_name": "Member", "invited_by": "Ada",
#        "created_at": "…", "expires_at": "…", "expired": false,
#        "emailed": true, "link": null, "note": null}
curl -s -H "Authorization: Bearer $SESSION" $I                          # open invites, expired ones too
curl -s -X POST -H "Authorization: Bearer $SESSION" $I/9/resend         # a new link, 7 more days
curl -s -X DELETE -H "Authorization: Bearer $SESSION" $I/9              # revoke: the link stops working
```

The email goes through the team's SMTP relay (`…/notifications/smtp`) with a link built from `PUBLIC_URL`. When it cannot be sent (no relay, no `PUBLIC_URL`, or the relay refused), the invite is still made: `emailed` is `false`, `note` says why and `link` holds the link, this once. Nothing else ever returns it; only its SHA-256 is stored. Errors: `409` when the address is already a member or already has an open invite (resend it), `400` for another team's role or one wider than yours, `429` past 20 invite emails per team per hour (new and resent).

The link opens `/invite/{token}`. A new address signs up there (or with `POST /api/v1/auth/register` and `"invite"`, above), whatever `REGISTRATION` says. An address that has an account signs in and accepts:

```sh
curl -s -X POST -H "Authorization: Bearer $SESSION" -H 'Content-Type: application/json' \
  $PODKIT/api/v1/auth/invites/accept -d '{"token":"<token from the link>"}'
# → {"team_id": 3}
```

Accepting needs a session (not an API token) whose email is the invited one; the link of someone else is `400` and stays usable for them. A link works once and for 7 days; after that, or once revoked, it is `404` (`this invite is not valid any more; ask for a new one`). The audit log records `invite.create`, `invite.resend`, `invite.revoke` and `invite.accept`. Emails are stored in lower case. An invite whose sender has since lost `create membership`, no longer holds everything its role grants, or whose account is gone answers `404` like any dead link, and a role change drops such invites (`member.update` then carries `invites_dropped`). A person signed out who opens a link for an existing account is sent to `/login?next=/invite`: the token waits in an `HttpOnly`, `SameSite=Strict` cookie scoped to `/invite` for an hour, never in the sign-in URL.

The same role rule holds for `POST …/members` and `PATCH …/members/{user_id}`: the role given may not grant anything the caller's own role does not (`400`), and nobody changes the role of a member who can do more than they can (`400`).

## Move an application, drain a server

Moves an application to another server of the team. It keeps its settings, variables, domains, volumes' contents and history; only its server changes. How it goes depends on the application:

| Method | When | What happens | Downtime |
|---|---|---|---|
| `redeploy` | no volumes | The image it runs is copied to the target (the relay a remote build uses) and started there through the normal deploy, with its health check. Once it is up the application points at the target and the old container is removed. If the deploy fails the application stays where it was. | next to none |
| `stop_copy` | named volumes | It is stopped, each volume is archived to the backup destination you pick and restored on the target (into a volume of the same name), and it is started there. The volumes on the old server are kept until you remove them. If a step fails it is put back and started again where it was. | from the stop until it runs on the target |
| `rebuild` | the old server does not answer | Nothing can be copied: the target builds the branch again (git) or pulls the image reference again (registry). Not possible with volumes. | until it runs on the target |

Refused, with the reason: its own server, a target that is not ready or is draining, a target with an application of the same slug, a move or deploy already in flight, a pull request preview, a Tailscale sidecar, model or MCP links (they go through the old server's gateway), user networks of the old server, and volumes without a backup destination (or on a server that does not answer). The plan also says what you have to do afterwards: the generated address changes (it has the server's address in it), custom domains need their DNS pointed at the new server, and resources of the same environment that stay behind can no longer be reached by name. Older images stay on the old server, so roll back by redeploying. A stopped application is moved without being started. Podman's checkpoint and restore (CRIU) is not used: a restored container would keep the old server's routing labels and addresses.

```sh
A=$PODKIT/api/v1/teams/$TEAM/projects/$PROJECT/applications/$APP
curl -s -H "Authorization: Bearer $PK_TOKEN" $A/move | jq .
# → {"application_id": …, "server": {"id": 1, "name": "node-01", "status": "active", "draining": false},
#    "volumes": 0, "backup_targets": [{"id": 5, "name": "B2"}], "active": null,
#    "candidates": [{"server": {"id": 2, "name": "node-02", …}, "movable": true, "method": "redeploy",
#      "method_label": "Redeploy the running image", "downtime": "Next to none: …", "starts": true,
#      "refusals": [], "warnings": ["The generated address changes from …"]}]}
curl -s -X POST -H "Authorization: Bearer $PK_TOKEN" -H 'Content-Type: application/json' \
  -d '{"server_id": 2, "backup_target_id": 5}' $A/move      # backup_target_id only with volumes
# → 202 {"id": …, "status": "queued", "method": "redeploy", "from_server": "node-01", "to_server": "node-02",
#        "step": "waiting for its turn", "deployment_id": null, …}
curl -s -H "Authorization: Bearer $PK_TOKEN" $A/moves      # the last ten, newest first: status, step, error
```

`GET …/move?backup_target_id=5` judges volume moves as if that destination were picked. Reading needs `read application` and `read server`; moving needs `update application` and `read server`, plus `create backup` when volumes are copied, and for a token the `write` scope. A move is `queued`, `running`, then `succeeded`, `failed` or `cancelled`; `step` says what it does now or how it ended, `deployment_id` is the deploy it started on the target. While it runs, deploys, restarts and rollbacks of the application answer `409`. The event is `app.move`.

**Draining** a server moves every application that can move to one server you pick, one at a time (quick ones first, the ones that stop last), and marks the server draining: it is left out of the create forms, creating an application, database, stack or model file on it answers `409`, and builds that would go to it run where the application runs. Databases, stacks, models and model files stay, with the reason in the report; scheduled tasks that run an image of their own are pointed at the target. The mark stays after the drain ends, until you take it off.

```sh
S=$PODKIT/api/v1/teams/$TEAM/servers/$SERVER
curl -s -H "Authorization: Bearer $PK_TOKEN" "$S/drain?target_server_id=2" | jq .
# → {"server": {…, "draining": false}, "draining_since": null, "targets": […],
#    "items": [{"kind": "application", "id": …, "name": "web", "project": "shop", "outcome": "pending",
#               "method": "redeploy", "verdict": {…}},
#              {"kind": "database", …, "outcome": "kept", "detail": "Databases are not moved: …"}],
#    "backup_targets": […], "latest": null}
curl -s -X POST -H "Authorization: Bearer $PK_TOKEN" -H 'Content-Type: application/json' \
  -d '{"target_server_id": 2, "backup_target_id": 5}' $S/drain    # → 202, the drain and its report
curl -s -X POST -H "Authorization: Bearer $PK_TOKEN" $S/undrain   # take the mark off, stop a running drain
```

Each report item has an `outcome`: `pending`, `moving`, `moved`, `failed`, `kept` (stays, `detail` says why) or `skipped` (the drain was stopped first). The drain ends `succeeded` (everything moved), `partial`, `failed` or `cancelled`. Stopping a drain lets the move in flight finish and starts no other. Reading needs `read server`, draining and undraining `update server` (and `create backup` with a destination). The events are `server.drain` and `server.undrain`.

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

## Image auto-update

An application deployed from a registry image (`ghcr.io/me/app:latest`) can be kept up to date with its tag. podkit checks on a schedule which image the tag names, and when it is a new one it redeploys through the normal deploy, so the health gate, the zero-downtime swap and the automatic rollback still apply. It is off until turned on. The web UI has it on the application's Webhooks tab.

```sh
AU=$PODKIT/api/v1/teams/$TEAM/projects/$PROJECT/applications/$APP/auto-update
curl -s -H "Authorization: Bearer $PK_TOKEN" $AU                       # settings and the last check
curl -s -X PUT -H "Authorization: Bearer $PK_TOKEN" -H 'Content-Type: application/json' $AU \
  -d '{"enabled":true,"interval":"daily","window":{"start":"02:00","end":"05:00","utc_offset_minutes":60}}'
curl -s -X POST -H "Authorization: Bearer $PK_TOKEN" $AU/check          # check now
```

- `PUT` replaces the settings: a field left out takes its default. `interval` is `hourly`, `6h` or `daily` (the default). `window` is optional: scheduled checks, and the deploys they start, happen only between `start` and `end` (`HH:MM`, in a fixed offset from UTC; `22:00` to `04:00` wraps over midnight). `null` or leaving it out means any time.
- Reading needs `read application` (token scope `read`), changing the settings `update application` (`write`), and `check` needs `create deployment` (token scope `deploy`), because it deploys what it finds. Check now does not wait for the maintenance window; autodeploy freeze windows hold it like a scheduled check. It answers `400` while auto-update is off.
- The application's own server agent asks the registry (the place its pulls go, with the application's saved registry login) for the tag's digest with one manifest `HEAD`; nothing is pulled. The digest is compared with every digest the running image is known by.
- A deploy started by a scheduled check is in the audit log as `app.deploy` with `"trigger": "auto-update"`; one started by Check now is the user's own, and `app.auto_update.check` is logged with its result. Changing the settings logs `app.auto_update`.
- An auto-update that fails, is cancelled, crashes, never gets healthy or is rolled back puts its digest in `bad_digest`: it is not deployed again until the tag names another image, and the team gets one `deploy_failed` notification. A digest auto-update already deployed is not deployed again either, so a manual rollback holds until the tag moves.
- `last_result` is `up_to_date`, `deployed`, `skipped_bad`, `busy` (a deploy was in progress), `frozen`, `outside_window`, `not_running` (nothing runs, so nothing is updated) or `error` (`last_detail` says why). After `busy`, `frozen` and `outside_window` the next check comes after 15 minutes; after `error` within the hour.
- A container under auto-update carries the label `podkit.autoupdate=registry`. podkit does not set `io.containers.autoupdate`: podman's own `podman auto-update` would pick the container up and fail on it, since it is not a systemd unit, and podkit's check is what redeploys it.
- A tag pinned by digest (`app@sha256:…`) and a git application cannot be auto-updated (`400`).

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

## Restore drills

A drill proves a database's backups come back. It takes the newest succeeded backup, restores it on the database's own server into a throwaway container of the same engine and version, and counts the tables (collections for MongoDB) of the copy. The copy has no network at all, no published port and no named volume, runs with memory and CPU limits (the database's own, at least 256 MiB and at most 4 GiB and 2 CPUs; 1 GiB and 1 CPU without limits), is named `podkit-drill-<id>` and is removed when the drill ends, whatever happens. The real database, its volume and its networks are never touched.

A drill passes when the copy has tables and at least as many as the database had when the backup was taken (counted read-only before each dump and kept as the backup's `source_tables`; backups made before this, or by an older agent, have none). A database that was empty at backup time may come back empty. A failed drill raises the `restore_drill_failed` notification, which rules can list like any other event. Redis databases have no backups, so they have no drills.

```sh
D=$PODKIT/api/v1/teams/$TEAM/projects/$PROJECT/databases/$DB/drills
curl -s -X POST -H "Authorization: Bearer $PK_TOKEN" $D             # run a drill now → 202 and the queued drill
curl -s -H "Authorization: Bearer $PK_TOKEN" "$D?limit=20"          # history, newest first; ?before=<drill id> pages
curl -s -H "Authorization: Bearer $PK_TOKEN" $D/schedule            # {"schedule": null, "next_run_at": null}
curl -s -X PUT -H "Authorization: Bearer $PK_TOKEN" -H 'Content-Type: application/json' \
  -d '{"schedule":"0 4 * * 0"}' $D/schedule                          # weekly, Sunday 04:00 UTC; null or "" turns it off
```

A drill: `id`, `database_id`, `backup_id` (null once retention removed the backup), `backup_taken_at`, `status` (`queued`, `running`, `passed`, `failed`), `tables_found`, `source_tables`, `error` (why it failed), `triggered_by` (null when scheduled), `created_at`, `started_at`, `finished_at`, `duration_ms`.

Scheduled drills are off by default; the schedule is a five-field cron in UTC like the backup schedule. Reading drills needs `read backup`, starting one `create backup`, and changing the schedule `update database`. Starting a drill answers `400` when there is no succeeded backup yet and `409` while another drill of the same database is queued or running; a scheduled drill that finds no backup is recorded as failed and notified. Starting a drill is audited as `backup.drill`, a schedule change as `database.drill_schedule`.

## Deployments and variables

```sh
APP=$PODKIT/api/v1/teams/$TEAM/projects/$PROJECT/applications/$APP_ID
curl -s -X POST -H "Authorization: Bearer $PK_TOKEN" $APP/deployments                # queue a deploy (scope deploy)
curl -s -H "Authorization: Bearer $PK_TOKEN" $APP/deployments/42                      # poll it
curl -sN -H "Authorization: Bearer $PK_TOKEN" $APP/deployments/42/build-logs          # SSE: event log {seq,ts,stream,line}, then end
curl -s -X POST -H "Authorization: Bearer $PK_TOKEN" $APP/deployments/41/rollback     # redeploy 41's image
curl -s -X POST -H "Authorization: Bearer $PK_TOKEN" -H 'Content-Type: application/json' \
  -d '{"text":"# from .env\nDATABASE_URL=postgres://…\nexport FLAG=\"on\"\n"}' $APP/env-vars/bulk
# → {"saved": 2, "errors": []}
```

A deployment has `status` (`queued`, `building`, `deploying`, `running`, `failed`, `stopped`, `crashed`, `cancelled`) and, when the application has a health gate, `health` (`waiting`, `healthy`, `unhealthy`: the old container kept serving, `rolled_back`: it went live, failed and the previous image was put back) with `health_detail`. Both the list and `GET …/deployments/{id}` carry them.

`POST …/env-vars/bulk` sets every good line of a `.env` text in one transaction, the same rules as the Environment tab's paste box: blank lines and `#` comments are skipped, `export ` is allowed, a value in matching quotes is taken as is, an unquoted one loses a trailing ` # comment`. At most 500 lines. Variables not in the text are left alone. Lines that do not parse are skipped and listed in `errors` by `line` and `message`, never by content. It needs `create env_var` (scope `write`) and is audit-logged as `env_var.bulk_set` with the names only. Values are write-only: no endpoint returns one.

## Quadlet

The Podman Quadlet units that would run a resource under systemd, without podkit; see [quadlet.md](quadlet.md). The view never contains a secret: the environment goes into an env file the unit names, and only its variable names are listed.

```sh
P=$PODKIT/api/v1/teams/$TEAM/projects/$PROJECT
curl -s -H "Authorization: Bearer $PK_TOKEN" $P/applications/12/quadlet | jq .   # also databases/{id}, models/{id}, stacks/{id}
# → {"kind":"application","id":12,"name":"web","server":"node-01","directory":"~/.config/containers/systemd",
#    "units":[{"name":"podkit-app-12.container","path":"~/.config/containers/systemd/podkit-app-12.container","content":"…"}, …],
#    "env_files":[{"name":"podkit-app-12.env","keys":["DATABASE_URL"]}],"mounted_files":[],"services":["podkit-app-12.service"]}

# Eject: every unit of the project (or one environment), one folder per server with a README.
curl -s -X POST -H "Authorization: Bearer $PK_TOKEN" -o units.tar.gz "$P/quadlet/export?environment_id=3"
# With the env files and mounted files, secret values included:
curl -s -X POST -H "Authorization: Bearer $PK_TOKEN" -o units.tar.gz "$P/quadlet/export?secrets=true"
# Every project of the team:
curl -s -X POST -H "Authorization: Bearer $PK_TOKEN" -o units.tar.gz $PODKIT/api/v1/teams/$TEAM/quadlet/export
```

The view needs `read` on the resource and `read env_var`. A resource whose spec cannot be built (a stack whose compose file no longer parses, a missing shared variable) answers `409` with the reason a deploy would fail with. The export needs `read` on projects, applications, databases, models, stacks and env vars; `secrets=true` also needs a session or a token with `read:sensitive` (else `403`, missing scope) and `update` on env vars, databases, models and stacks. Every export is written to the audit log as `quadlet.export`, with whether secrets went out. A resource that cannot be exported is listed in the README under "Not exported" instead of failing the archive.
