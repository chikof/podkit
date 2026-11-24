//! Private access through Tailscale: an application served only on the team's tailnet.
//!
//! The team stores one auth key (write-only, age-encrypted) and, optionally, a Headscale
//! login server and the tags its sidecars advertise. Exposing an application starts a
//! `tailscale/tailscale` sidecar next to it on its environment's network; the sidecar's
//! `tailscale serve` config proxies HTTPS on the node's `<name>.<tailnet>.ts.net` to the
//! application's stable alias. Removing the exposure logs the node out first, so it does not
//! linger in the tailnet's machine list.
//!
//! Sources (read, not guessed):
//! - the container's environment variables:
//!   <https://github.com/tailscale/tailscale/blob/main/cmd/containerboot/settings.go>
//! - the serve config (`ipn.ServeConfig`), where `${TS_CERT_DOMAIN}` is replaced by the node's
//!   name: <https://github.com/tailscale/tailscale/blob/main/ipn/serve.go> and
//!   <https://github.com/tailscale/tailscale/blob/main/cmd/containerboot/serve.go>
//! - the node's state, from `tailscale status --json` (`BackendState`, `Self.DNSName`):
//!   <https://github.com/tailscale/tailscale/blob/main/ipn/ipnstate/ipnstate.go>
//!
//! The image is BSD-3-Clause licensed (<https://github.com/tailscale/tailscale/blob/main/LICENSE>).

pub mod routes;
pub mod run;
pub mod service;
pub mod spec;

#[cfg(test)]
mod tests;

pub use routes::{application_router, router};

use sqlx::PgExecutor;

use crate::ids::{ApplicationId, TeamId};

/// How a node's state moves while the sidecar starts and stops.
#[derive(Debug, Clone, Copy, PartialEq, Eq, sqlx::Type)]
#[sqlx(type_name = "text", rename_all = "lowercase")]
pub enum ExposureStatus {
	/// The sidecar is starting or has not joined the tailnet yet.
	Starting,
	/// The node is on the tailnet and serving.
	Running,
	/// The sidecar could not start or join; `error` says why.
	Failed,
	/// Being logged out and removed.
	Removing,
}

impl ExposureStatus {
	#[must_use]
	pub const fn as_str(self) -> &'static str {
		match self {
			Self::Starting => "starting",
			Self::Running => "running",
			Self::Failed => "failed",
			Self::Removing => "removing",
		}
	}
}

/// Row of `tailscale_exposures`.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Exposure {
	pub hostname: String,
	pub status: ExposureStatus,
	/// `<hostname>.<tailnet>.ts.net` once the node has joined.
	pub dns_name: Option<String>,
	pub error: Option<String>,
	pub container_id: Option<String>,
}

impl Exposure {
	/// The address to open on a tailnet device, once the node has one.
	#[must_use]
	pub fn url(&self) -> Option<String> {
		self.dns_name.as_ref().map(|name| format!("https://{name}"))
	}
}

/// What a team has set. The auth key itself never leaves the database except into the job that
/// starts a sidecar, so only whether one is stored is here.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Settings {
	pub has_auth_key: bool,
	/// A Headscale (or other control server) URL; `None` joins the Tailscale service.
	pub login_server: Option<String>,
	pub tags: Vec<String>,
}

#[derive(sqlx::FromRow)]
struct SettingsRow {
	has_auth_key: bool,
	login_server: Option<String>,
	tags: Vec<String>,
}

/// The team's settings; the default when nothing is stored.
pub async fn settings(db: impl PgExecutor<'_>, team: TeamId) -> Result<Settings, sqlx::Error> {
	let row = sqlx::query_as::<_, SettingsRow>(
		"SELECT auth_key IS NOT NULL AS has_auth_key, login_server, tags
		FROM tailscale_settings WHERE team_id = $1",
	)
	.bind(team)
	.fetch_optional(db)
	.await?;
	Ok(row.map_or_else(Settings::default, |r| Settings {
		has_auth_key: r.has_auth_key,
		login_server: r.login_server,
		tags: r.tags,
	}))
}

/// The encrypted auth key, for the job that starts a sidecar.
pub async fn auth_key_blob(
	db: impl PgExecutor<'_>,
	team: TeamId,
) -> Result<Option<Vec<u8>>, sqlx::Error> {
	sqlx::query_scalar::<_, Option<Vec<u8>>>(
		"SELECT auth_key FROM tailscale_settings WHERE team_id = $1",
	)
	.bind(team)
	.fetch_optional(db)
	.await
	.map(Option::flatten)
}

/// Stores the settings. `auth_key` is the new encrypted key, or `None` to keep the stored one.
pub async fn save_settings(
	db: impl PgExecutor<'_>,
	team: TeamId,
	auth_key: Option<&[u8]>,
	login_server: Option<&str>,
	tags: &[String],
) -> Result<(), sqlx::Error> {
	sqlx::query(
		"INSERT INTO tailscale_settings (team_id, auth_key, login_server, tags)
		VALUES ($1, $2, $3, $4)
		ON CONFLICT (team_id) DO UPDATE SET
			auth_key = COALESCE($2, tailscale_settings.auth_key),
			login_server = $3, tags = $4, updated_at = NOW()",
	)
	.bind(team)
	.bind(auth_key)
	.bind(login_server)
	.bind(tags)
	.execute(db)
	.await?;
	Ok(())
}

/// Forgets the stored auth key; the other settings stay.
pub async fn clear_auth_key(db: impl PgExecutor<'_>, team: TeamId) -> Result<(), sqlx::Error> {
	sqlx::query(
		"UPDATE tailscale_settings SET auth_key = NULL, updated_at = NOW() WHERE team_id = $1",
	)
	.bind(team)
	.execute(db)
	.await?;
	Ok(())
}

/// The application's exposure, if it has one. No team check: callers have scoped the
/// application already.
pub async fn find(
	db: impl PgExecutor<'_>,
	application: ApplicationId,
) -> Result<Option<Exposure>, sqlx::Error> {
	sqlx::query_as::<_, Exposure>(
		"SELECT hostname, status, dns_name, error, container_id
		FROM tailscale_exposures WHERE application_id = $1",
	)
	.bind(application)
	.fetch_optional(db)
	.await
}

/// Records an exposure about to be started, replacing a failed one.
pub async fn upsert(
	db: impl PgExecutor<'_>,
	application: ApplicationId,
	hostname: &str,
) -> Result<(), sqlx::Error> {
	sqlx::query(
		"INSERT INTO tailscale_exposures (application_id, hostname) VALUES ($1, $2)
		ON CONFLICT (application_id) DO UPDATE SET
			hostname = $2, status = 'starting', error = NULL, updated_at = NOW()",
	)
	.bind(application)
	.bind(hostname)
	.execute(db)
	.await?;
	Ok(())
}

pub async fn set_status(
	db: impl PgExecutor<'_>,
	application: ApplicationId,
	status: ExposureStatus,
	error: Option<&str>,
) -> Result<(), sqlx::Error> {
	sqlx::query(
		"UPDATE tailscale_exposures SET status = $2, error = $3, updated_at = NOW()
		WHERE application_id = $1",
	)
	.bind(application)
	.bind(status)
	.bind(error)
	.execute(db)
	.await?;
	Ok(())
}

pub async fn set_running(
	db: impl PgExecutor<'_>,
	application: ApplicationId,
	dns_name: &str,
) -> Result<(), sqlx::Error> {
	sqlx::query(
		"UPDATE tailscale_exposures SET status = 'running', dns_name = $2, error = NULL,
			updated_at = NOW() WHERE application_id = $1",
	)
	.bind(application)
	.bind(dns_name)
	.execute(db)
	.await?;
	Ok(())
}

pub async fn set_container(
	db: impl PgExecutor<'_>,
	application: ApplicationId,
	container_id: &str,
) -> Result<(), sqlx::Error> {
	sqlx::query(
		"UPDATE tailscale_exposures SET container_id = $2, updated_at = NOW()
		WHERE application_id = $1",
	)
	.bind(application)
	.bind(container_id)
	.execute(db)
	.await?;
	Ok(())
}

pub async fn delete(
	db: impl PgExecutor<'_>,
	application: ApplicationId,
) -> Result<(), sqlx::Error> {
	sqlx::query("DELETE FROM tailscale_exposures WHERE application_id = $1")
		.bind(application)
		.execute(db)
		.await?;
	Ok(())
}
