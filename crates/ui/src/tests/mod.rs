//! What the tests of the pages share: everything `podkit_core::tests` has (a test
//! `AppState`, requests, the SQL a page test may not write itself), with [`app`] and its
//! variants over the whole router, pages included.

mod access_rules;
mod api_tokens;
mod avatars;
mod firewall;
mod foreign_team_404;
mod log_ui;
mod mcp_servers;
mod routes;
mod shared_env;
mod sso;
mod tailscale;
mod throttle;
mod two_factor;
mod ui;
mod volume_backups;

use axum::Router;
use podkit_core::state::Registration;
pub use podkit_core::tests::*;

/// The whole application over a test [`state`]: the API and the web UI.
pub async fn app(db: PgPool) -> Router {
	crate::routes(state(db)).await.unwrap()
}

/// Like [`app`], with another registration policy (tests default to `Open`). On `first-user`
/// the first account needs [`SETUP_TOKEN`], as on a freshly started instance.
pub async fn app_with_registration(db: PgPool, registration: Registration) -> Router {
	crate::routes(state_with_registration(db, registration))
		.await
		.unwrap()
}

/// [`app`] on a policy with the setup token armed as [`SETUP_TOKEN`], whatever the policy.
pub async fn app_with_setup_token(db: PgPool, registration: Registration) -> Router {
	crate::routes(state_with_setup_token(db, registration))
		.await
		.unwrap()
}
