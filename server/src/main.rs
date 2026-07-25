use std::sync::Arc;

use database::PgPool;
use database::connection::{get_db_connection, migrate};
use database::hashing::Argon2PasswordHasher;
use database::models::token_revocations::TokenRevocation;
use database::repositories::authorizer::PgAuthorizer;
use database::repositories::project::PgProjectRepository;
use database::repositories::role::PgRoleRepository;
use database::repositories::team::PgTeamRepository;
use database::repositories::team_member::PgTeamMemberRepository;
use database::repositories::user::PgUserRepository;
use podkit_core::domain::project::repository::ProjectRepository;
use podkit_core::domain::role::repository::RoleRepository;
use podkit_core::domain::shared::authorization::Authorizer;
use podkit_core::domain::team::repository::TeamRepository;
use podkit_core::domain::team_member::repository::TeamMemberRepository;
use podkit_core::domain::user::{PasswordHasher, UserRepository};
use tokio::net::TcpListener;
use tracing::{info, warn};

use crate::auth::token::TokenService;
use crate::config::ServerConfig;
use crate::error::AppResult;
use crate::routes::routes;

mod auth;
mod config;
mod error;
mod routes;

#[derive(Clone)]
pub struct AppState {
	pub tokens: Arc<TokenService>,
	pub pool: &'static PgPool,
	pub users: Arc<dyn UserRepository>,
	pub teams: Arc<dyn TeamRepository>,
	pub team_members: Arc<dyn TeamMemberRepository>,
	pub roles: Arc<dyn RoleRepository>,
	pub projects: Arc<dyn ProjectRepository>,
	pub authorizer: Arc<dyn Authorizer>,
	pub password_hasher: Arc<dyn PasswordHasher>,
}

#[tokio::main]
async fn main() -> AppResult<()> {
	#[cfg(debug_assertions)]
	dotenvy::dotenv().ok(); // I wasn’t really sure whether we should use dotenv_override() or not

	better_tracing::fmt().init();

	#[cfg(feature = "config_file")]
	ServerConfig::create_if_missing()?;

	let config = ServerConfig::load()?;
	info!(version = env!("CARGO_PKG_VERSION"), "podkit starting");

	let addr = format!("{}:{}", config.host, config.port);
	let pool = get_db_connection(Some(&config.database_url)).await?;
	let state = AppState {
		tokens: Arc::new(TokenService::new(config.jwt_secret.as_bytes())),
		pool,
		users: Arc::new(PgUserRepository(pool)),
		teams: Arc::new(PgTeamRepository(pool)),
		team_members: Arc::new(PgTeamMemberRepository(pool)),
		roles: Arc::new(PgRoleRepository(pool)),
		projects: Arc::new(PgProjectRepository(pool)),
		authorizer: Arc::new(PgAuthorizer(pool)),
		password_hasher: Arc::new(Argon2PasswordHasher),
	};

	// run db migrations
	migrate().await?;
	clean_expired_tokens(state.pool);

	let routes = routes(state).await?;
	let listener = TcpListener::bind(&addr).await?;

	info!("started http server on http://{addr}");
	axum::serve(listener, routes).await?;

	Ok(())
}

// Runs every hour and cleans up expired revocation rows
fn clean_expired_tokens(pool: &'static PgPool) {
	tokio::spawn(async move {
		let mut interval = tokio::time::interval(std::time::Duration::from_hours(1));
		loop {
			interval.tick().await;
			if let Err(e) = TokenRevocation::purge_expired(pool).await {
				warn!("failed to purge expired token revocations: {e}");
			}
		}
	});
}
