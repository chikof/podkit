# Contributing to podkit

## Setup

```sh
devenv up            # postgres + other services, see devenv.nix
cargo run -p server
```

Requires `git`, `ssh`, a reachable Podman socket
(`systemctl --user enable --now podman.socket` for rootless), and (for
`nixpacks`-strategy builds) the `nixpacks` CLI on `PATH`. See
`.env.example` for required environment variables.

For the dashboard:

```sh
cd dashboard
bun install
bun run dev
```

## Before opening a PR

```sh
cargo fmt --all
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
```

```sh
cd dashboard
bun run lint
bun run check
```

Pre-commit hooks (rustfmt, nixfmt) run automatically via `devenv`'s
`git-hooks.nix`, so no separate setup is needed inside the devenv shell.

## Database changes

Migrations live in `crates/database/migrations`. Every `NNNN_name.up.sql`
needs a matching `.down.sql`. Add/update the `sqlx` query cache after
schema changes:

```sh
cargo sqlx prepare --workspace
```

CI runs with `SQLX_OFFLINE=true` against the committed `.sqlx/` cache, so
commit it along with your migration.

## Commit / PR style

- Keep commits focused; conventional-commit-style subjects
  (`feat:`, `fix:`, `chore:`, ...) are preferred but not enforced.
- Fill out the PR template checklist.
- Link the issue you're closing, if any.

## Reporting bugs / requesting features

Use the issue templates. For security issues, see
[SECURITY.md](SECURITY.md) instead of opening a public issue.
