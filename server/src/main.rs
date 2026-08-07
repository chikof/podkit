//! podkit's API server: auth, team/project/application management, the
//! deploy pipeline, and the background health monitor that restarts
//! crashed containers.

use std::sync::Arc;

use crypto::age::SecretBox;
use database::PgPool;
use database::connection::{get_db_connection, migrate};
use database::hashing::Argon2PasswordHasher;
use database::models::token_revocations::TokenRevocation;
use database::repositories::application::PgApplicationRepository;
use database::repositories::authorizer::PgAuthorizer;
use database::repositories::custom_domain::PgCustomDomainRepository;
use database::repositories::deployment::PgDeploymentRepository;
use database::repositories::env_var::PgEnvVarRepository;
use database::repositories::project::PgProjectRepository;
use database::repositories::role::PgRoleRepository;
use database::repositories::server::PgServerRepository;
use database::repositories::team::PgTeamRepository;
use database::repositories::team_member::PgTeamMemberRepository;
use database::repositories::user::PgUserRepository;
use podkit_core::domain::application::repository::ApplicationRepository;
use podkit_core::domain::custom_domain::repository::CustomDomainRepository;
use podkit_core::domain::deployment::repository::DeploymentRepository;
use podkit_core::domain::env_var::repository::EnvVarRepository;
use podkit_core::domain::project::repository::ProjectRepository;
use podkit_core::domain::role::repository::RoleRepository;
use podkit_core::domain::server::repository::ServerRepository;
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
mod deploy_pipeline;
mod error;
mod health_monitor;
mod routes;

/// Shared state handed to every route handler: repositories, auth services,
/// and the bits of server config that routes need at request time.
#[derive(Clone)]
pub struct AppState {
	/// Issues and verifies bearer tokens for authenticated requests.
	pub tokens: Arc<TokenService>,
	/// The database connection pool, leaked for the life of the process.
	pub pool: &'static PgPool,
	/// User account storage.
	pub users: Arc<dyn UserRepository>,
	/// Team storage.
	pub teams: Arc<dyn TeamRepository>,
	/// Team membership storage.
	pub team_members: Arc<dyn TeamMemberRepository>,
	/// Role storage.
	pub roles: Arc<dyn RoleRepository>,
	/// Project storage.
	pub projects: Arc<dyn ProjectRepository>,
	/// Server (deployment target) storage.
	pub servers: Arc<dyn ServerRepository>,
	/// Application storage.
	pub applications: Arc<dyn ApplicationRepository>,
	/// Application env var storage.
	pub env_vars: Arc<dyn EnvVarRepository>,
	/// Custom domain storage.
	pub custom_domains: Arc<dyn CustomDomainRepository>,
	/// Deployment storage.
	pub deployments: Arc<dyn DeploymentRepository>,
	/// Checks whether a user is allowed to perform an action on a team.
	pub authorizer: Arc<dyn Authorizer>,
	/// Hashes and verifies user passwords.
	pub password_hasher: Arc<dyn PasswordHasher>,
	/// Encrypts secrets at rest (ssh keys, deploy keys, env var values).
	pub secrets: Arc<SecretBox>,
	/// Port Traefik listens on for plain HTTP ingress.
	pub ingress_port: u16,
	/// Port Traefik listens on for HTTPS ingress.
	pub https_port: u16,
	/// Contact email for ACME certs; `None` disables HTTPS/custom domains.
	pub acme_email: Option<String>,
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

	let secrets = Arc::new(
		SecretBox::from_identity_str(&config.age_secret_key)
			.map_err(|e| crate::error::ServerError::InvalidSecretKey(e.to_string()))?,
	);

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
		servers: Arc::new(PgServerRepository(pool)),
		applications: Arc::new(PgApplicationRepository(pool)),
		env_vars: Arc::new(PgEnvVarRepository(pool)),
		custom_domains: Arc::new(PgCustomDomainRepository(pool)),
		deployments: Arc::new(PgDeploymentRepository(pool)),
		authorizer: Arc::new(PgAuthorizer(pool)),
		password_hasher: Arc::new(Argon2PasswordHasher),
		secrets,
		ingress_port: u16::try_from(config.ingress_port).unwrap_or(80),
		https_port: u16::try_from(config.https_port).unwrap_or(443),
		acme_email: config.acme_email.clone(),
	};

	// run db migrations
	migrate().await?;
	clean_expired_tokens(state.pool);
	health_monitor::spawn(state.clone());

	let routes = routes(state, &config.dashboard_origins).await?;
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
