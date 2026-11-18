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

## Follow-ups

- Environment-level shared environment variables.
- Cloning an environment.
- UI (F2, F3).
