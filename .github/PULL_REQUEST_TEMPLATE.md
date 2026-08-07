## What & why

<!-- What changes, and why. Link the issue if any (Closes #123). -->

## Checklist

- [ ] `cargo fmt --all` / `cargo clippy --workspace --all-targets --all-features`
- [ ] `cargo test --workspace` passes
- [ ] `bun run lint` / `bun run check` (if `dashboard/` touched)
- [ ] Migrations added under `crates/database/migrations` with matching `.down.sql` (if schema touched)

## Testing

<!-- How you verified this. Commands run, manual steps, screenshots for dashboard changes. -->
