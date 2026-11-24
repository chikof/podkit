# Cloud extension point

podkit has quotas and usage metering for everyone. **Billing is not in this repository.** A hosted
offering plugs a payment provider in through one trait; the self-hosted build never compiles it.

## What is here

| Piece | Where |
|---|---|
| The contract: `BillingProvider`, `NoopBilling`, the request and answer types | `crates/billing` (`podkit-billing`, no dependency besides `thiserror`) |
| A stand-in for the private crate: `provider()` returns `NoopBilling` | `crates/cloud` (`podkit-cloud`) |
| The server's hooks, built only with the `cloud` Cargo feature | `crates/core/src/billing` |
| Quotas, metering, CSV export | `crates/core/src/quotas` |

`cloud` is **off by default** (`cargo build -p server --features cloud` turns it on). Without it every
hook is an empty function, so a self-hosted server pays nothing.

## The four hooks

| Trait method | When the server calls it | What an error does |
|---|---|---|
| `create_customer(team)` | A team is created through the API; a team made by sign-up gets its customer on its first create or deploy. The returned id is stored with the team. | Logged; the team is still made. |
| `report_usage(report)` | The hourly metering pass wrote a new `usage_records` hour. Each record is reported once, but an hour can be recomputed, so key on `(team, hour_start)`. | Logged; reported again on the next pass. |
| `check_entitlement(request)` | At every create and deploy, **after** podkit's own quotas said yes. `Deny { reason }` stops the action and shows `reason` as a sentence. | Treated as "allowed": a billing outage must not stop deploys. |
| `handle_webhook(request)` | `POST /webhooks/billing` (no session). The provider verifies its own signature from the raw body and headers. May answer `SetQuotas` to change a team's limits (a plan changed). | The request is answered 400; the message stays in the log. |

Every method has a default that does nothing, so a provider overrides what it needs. Limits set by a
webhook are written to the audit log as the system, not as a user.

## Plugging in a private `podkit-cloud`

1. Keep a private crate named `podkit-cloud` with the same API as `crates/cloud`: a single
   `pub fn provider() -> std::sync::Arc<dyn podkit_billing::BillingProvider>`. It depends on
   `podkit-billing` (path or git) and holds all payment code and credentials.
2. In the private build tree, replace the `crates/cloud` directory with that crate (or point the
   `podkit-cloud` path dependency in `crates/core/Cargo.toml` at it, using a `[patch]` section).
3. Build with `--features cloud`. `main` calls `provider()` once at start-up.

The trait is the whole contract: do not reach into server internals from the overlay. Open source
releases build without `cloud` and without that crate's private code.

## Quotas the provider can set

`applications`, `databases`, `models`, `stacks`, `previews`, `cpu_millis`, `memory_mb`,
`disk_mb`, `gpus`, `model_tokens` (per month), `build_minutes` (per month). A `limit` of `None`
removes the limit.

## Meters in a `UsageReport`

`cpu_seconds`, `memory_gb_hours`, `disk_gb_hours`, `gpu_hours`, `egress_bytes`, `model_tokens`,
`build_minutes`; zero meters are left out.
