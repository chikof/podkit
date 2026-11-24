# Environments

Project → Environments → Resources. Every application, database and model
belongs to exactly one environment of its project (`production`, `staging`, …).

## Model

- `environments (id, project_id, name, slug, position, created_at)`;
  `UNIQUE (project_id, slug)` and `UNIQUE (id, project_id)`.
- `applications`, `databases` and `models` carry `environment_id NOT NULL`
  with a composite foreign key `(environment_id, project_id) →
  environments (id, project_id)`: a resource can't point at another project's
  environment, even with raw SQL.
- Migration `0034` backfills one `production` environment per project (its id
  is the project's id) and moves every existing resource into it.
- A trigger on `projects` creates `production` in the same statement that
  inserts a project, so no code path can make a project without one. A trigger
  on the three resource tables fills a missing `environment_id` with the
  project's first environment, so older raw inserts keep working.
- The default environment is the one with the lowest `position`. Reordering
  moves the default.

## API

`/api/v1/teams/{t}/projects/{p}/environments`

| Call | Permission | Notes |
|---|---|---|
| `GET /` | read project | display order |
| `POST /` `{name, slug}` | create project | appended last |
| `GET /{id}` | read project | |
| `PATCH /{id}` `{name, slug?}` | update project | slug kept when omitted |
| `PUT /order` `{ids}` | update project | every environment once; first = default |
| `DELETE /{id}` | delete project | 409 while it holds resources (names them); 409 for the last one |

Resource create endpoints (applications, databases, models) accept an optional
`environment_id` in the body; the list endpoints accept `?environment_id=`.
Both default to the project's first environment. Responses include
`environment_id`. An environment of another project or team is a 404. Item
routes (`.../applications/{id}`, …) are unchanged.

A project-wide view (every environment) is `projects::service::get`; the
list endpoints show one environment at a time.

## Slugs

Application slugs stay unique per server (`UNIQUE (server_id, slug)`), because
the public hostname derives from `(slug, server)`. This migration doesn't change
that: outside the default environment the UI suggests `<slug>-<env>`
(`api-staging`) so the same app can run in two environments on one server.

## Promote

Promoting takes the image an application runs in one environment (staging) and deploys it to
the matching application of another environment of the same project (production), without
a build. It is the rollback path pointed at another application: the new deployment of the
target carries the source deployment's `image_tag` and `commit_sha`, so the Deploy job runs
that image as it is (`deployments::promote`).

- **Which image.** The source's newest `running` deployment that has an image. Nothing
  running is a refusal. The API and the dialog may name the deployment they showed; if the
  source has moved on since, the promotion is refused (`409`) instead of shipping something
  nobody looked at.
- **Which application.** By slug, in the target environment, best first: the same slug
  (`api` and `api` on two servers); the slug without the source environment's suffix
  (`api-staging` in staging is `api` in production); the base with the target's suffix (`api`
  in production is `api-staging` in staging, the name a clone or the create form gives it);
  the whole slug with the target's suffix (`api-staging-qa`). The first candidate that one
  application has wins. None is a refusal that lists the slugs it looked for; two (same slug
  on two servers) is a refusal too. Create the application there, or clone the environment.
- **What the target keeps.** Everything but the image: its own variables and the shared
  ones of its environment, domains, volumes, limits, replicas, access rules and health gate.
  The health gate and the zero-downtime swap work exactly as for any deploy of the target.
- **Another server.** When the target runs on another server than the source, the Deploy
  job first moves the image there through the control plane (`podman save` on the source's
  agent streamed into `podman load` on the target's, checked by content id): the relay a
  remote build uses (`builds::delivery`, see `docs/design/cluster.md`). No registry is
  involved. A source server that is down, or an agent that cannot export or import images,
  fails the deployment with that reason in its log.
- **Where it came from.** `deployments.promoted_from` points at the source deployment
  (`NULL` once it is deleted) and `promoted_from_environment` keeps its environment's name
  (migration `0077`). The deployments list shows "Promoted from staging" with a link to the
  source deployment, and the audit log has `app.promote` on the target.
- **Permission.** What deploying the target needs: `create deployment` (and `read
  deployment` to look at the source). A token needs the `deploy` scope.

In the UI, the application's menu has **Promote** while it runs: a dialog shows the
deployment, commit and image, and for each other environment the application it would go to
(or why there is none), what it replaces and whether the image is copied between servers.
API: `GET`/`POST …/applications/{id}/promote`, see [api.md](api.md#promote-between-environments).

## Follow-ups

- Environment-level shared environment variables.
- Cloning an environment.
- UI (F2, F3).
