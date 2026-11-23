//! Tailscale use cases shared by the JSON API and the web UI: authorize, then scope, then
//! validate, then touch the database.
//!
//! The team's settings need `read team` / `update team`; an application's exposure needs the
//! same permissions as its domains (`read`, `create` and `delete domain`): it is another way
//! to reach the application.

use super::spec;
use super::{Exposure, ExposureStatus, Settings};
use crate::applications::Application;
use crate::audit::{self, Diff, Target};
use crate::auth::permission::{Action, Resource, authorize};
use crate::error::{AppResult, ServerError};
use crate::ids::{ApplicationId, ProjectId, TeamId, UserId};
use crate::jobs::{self, JobKind};
use crate::scope;
use crate::state::AppState;

/// What the settings form sends. `auth_key` is write-only: empty keeps what is stored.
#[derive(Debug, Default)]
pub struct SettingsInput<'a> {
	pub auth_key: &'a str,
	/// Forget the stored key instead of replacing it.
	pub clear_auth_key: bool,
	pub login_server: &'a str,
	pub tags: &'a str,
}

/// The team's Tailscale settings; the auth key itself is never returned.
///
/// # Errors
/// `Forbidden` without `read team`.
pub async fn settings(s: &AppState, user: UserId, team: TeamId) -> AppResult<Settings> {
	authorize(&s.db, user, team, Action::Read, Resource::Team).await?;
	Ok(super::settings(&s.db, team).await?)
}

/// Stores the settings; needs `update team`.
///
/// # Errors
/// `Validation` for a malformed key, login server or tag.
pub async fn save_settings(
	s: &AppState,
	user: UserId,
	team: TeamId,
	input: &SettingsInput<'_>,
) -> AppResult<Settings> {
	authorize(&s.db, user, team, Action::Update, Resource::Team).await?;
	let login_server =
		spec::normalize_login_server(input.login_server).map_err(ServerError::Validation)?;
	let tags = spec::parse_tags(input.tags).map_err(ServerError::Validation)?;
	let new_key = input.auth_key.trim();
	let blob = if new_key.is_empty() {
		None
	} else {
		let key = spec::check_auth_key(new_key).map_err(ServerError::Validation)?;
		Some(s.secrets.encrypt(key).map_err(|e| {
			tracing::error!("failed to encrypt the Tailscale auth key: {e}");
			ServerError::Internal
		})?)
	};
	super::save_settings(&s.db, team, blob.as_deref(), login_server.as_deref(), &tags).await?;
	if input.clear_auth_key && blob.is_none() {
		super::clear_auth_key(&s.db, team).await?;
	}
	let mut diff = Diff::new()
		.set("login_server", login_server.clone().unwrap_or_default())
		.set("tags", tags.join(","));
	if blob.is_some() {
		diff = diff.secret("auth_key");
	} else if input.clear_auth_key {
		diff = diff.set("auth_key", "cleared");
	}
	audit::log(
		&s.db,
		user,
		team,
		"tailscale.settings.update",
		Target::none("tailscale_settings"),
		diff,
	)
	.await;
	Ok(super::settings(&s.db, team).await?)
}

/// An application's exposure with the team's settings, for its page.
#[derive(Debug, Clone)]
pub struct AppView {
	pub exposure: Option<Exposure>,
	pub settings: Settings,
	/// The node name a new exposure would start with.
	pub suggested_hostname: String,
}

/// The application's tailnet exposure; needs `read domain`.
///
/// # Errors
/// `ApplicationNotFound` for an application of another team.
pub async fn view(
	s: &AppState,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	app: ApplicationId,
) -> AppResult<AppView> {
	authorize(&s.db, user, team, Action::Read, Resource::Domain).await?;
	let app = scope::application(&s.db, team, project, app).await?;
	Ok(AppView {
		exposure: super::find(&s.db, app.id).await?,
		settings: super::settings(&s.db, team).await?,
		suggested_hostname: spec::default_hostname(&app.slug),
	})
}

/// Starts serving the application on the tailnet; needs `create domain`. `hostname` is the
/// node's name, the application's slug when empty. Returns right away: the sidecar starts in a
/// job, and the exposure reads `starting` until the node has joined.
///
/// # Errors
/// `Validation` without a stored auth key or with a bad name; `Conflict` while the sidecar
/// is being started or removed.
pub async fn enable(
	s: &AppState,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	app: ApplicationId,
	hostname: &str,
) -> AppResult<Exposure> {
	authorize(&s.db, user, team, Action::Create, Resource::Domain).await?;
	let app = scope::application(&s.db, team, project, app).await?;
	let hostname = match hostname.trim() {
		"" => spec::default_hostname(&app.slug),
		given => given.to_ascii_lowercase(),
	};
	if !spec::valid_hostname(&hostname) {
		return Err(ServerError::Validation(
			"the name must be lowercase letters, digits and dashes, up to 63 characters".to_owned(),
		));
	}
	if !super::settings(&s.db, team).await?.has_auth_key {
		return Err(ServerError::Validation(
			"add a Tailscale auth key in Settings first".to_owned(),
		));
	}
	if let Some(existing) = super::find(&s.db, app.id).await?
		&& matches!(
			existing.status,
			ExposureStatus::Starting | ExposureStatus::Removing
		) && jobs_active(s, app.id).await?
	{
		return Err(ServerError::Conflict(
			"the tailnet sidecar is being changed; try again in a moment".to_owned(),
		));
	}
	super::upsert(&s.db, app.id, &hostname).await?;
	queue(s, JobKind::TailscaleStart, &app).await?;
	realias_running(s, &app, user).await;
	audit::log(
		&s.db,
		user,
		team,
		"tailscale.enable",
		Target::new("application", app.id)
			.named(&app.slug)
			.in_project(project),
		Diff::new().set("hostname", hostname),
	)
	.await;
	super::find(&s.db, app.id)
		.await?
		.ok_or(ServerError::ApplicationNotFound)
}

/// Stops serving the application on the tailnet: the node is logged out, the sidecar and its
/// state volume are removed; needs `delete domain`. Returns right away; the row stays,
/// reading `removing`, until the job has finished.
///
/// # Errors
/// `Conflict` while the sidecar is being started or removed.
pub async fn disable(
	s: &AppState,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	app: ApplicationId,
) -> AppResult<Option<Exposure>> {
	authorize(&s.db, user, team, Action::Delete, Resource::Domain).await?;
	let app = scope::application(&s.db, team, project, app).await?;
	let Some(existing) = super::find(&s.db, app.id).await? else {
		return Ok(None);
	};
	if existing.status == ExposureStatus::Removing && jobs_active(s, app.id).await? {
		return Err(ServerError::Conflict(
			"the tailnet sidecar is already being removed".to_owned(),
		));
	}
	super::set_status(&s.db, app.id, ExposureStatus::Removing, None).await?;
	queue(s, JobKind::TailscaleRemove, &app).await?;
	audit::log(
		&s.db,
		user,
		team,
		"tailscale.disable",
		Target::new("application", app.id)
			.named(&app.slug)
			.in_project(project),
		Diff::new().set("hostname", existing.hostname),
	)
	.await;
	Ok(super::find(&s.db, app.id).await?)
}

async fn jobs_active(s: &AppState, app: ApplicationId) -> Result<bool, sqlx::Error> {
	Ok(
		jobs::active_exists(&s.db, JobKind::TailscaleStart, app.0).await?
			|| jobs::active_exists(&s.db, JobKind::TailscaleRemove, app.0).await?,
	)
}

async fn queue(s: &AppState, kind: JobKind, app: &Application) -> Result<(), sqlx::Error> {
	jobs::enqueue(
		s,
		kind,
		app.server_id,
		app.id.0,
		format!("tailscale:{}", app.id),
	)
	.await?;
	Ok(())
}

/// A running application only answers to [`spec::target_alias`] once it is redeployed with
/// it, so it is restarted from its current image. A failure to queue the restart is only
/// logged: the alias arrives with the next deploy, and the page says so.
async fn realias_running(s: &AppState, app: &Application, user: UserId) {
	let running = match crate::deployments::latest_running(&s.db, app.id).await {
		Ok(running) => running,
		Err(e) => {
			tracing::warn!(
				application = app.id.0,
				"cannot tell whether the application runs: {e}"
			);
			return;
		}
	};
	let Some(running) = running else { return };
	if let Err(e) = crate::deployments::queue(s, app, Some(user), Some(&running)).await {
		tracing::warn!(
			application = app.id.0,
			"cannot restart the application for its tailnet alias: {e}"
		);
	}
}

/// Whether the application should answer to its tailnet alias: it has an exposure.
pub async fn wants_alias(db: &sqlx::PgPool, app: ApplicationId) -> Result<bool, sqlx::Error> {
	Ok(super::find(db, app).await?.is_some())
}

/// Before an application is deleted: logs its node out and removes the sidecar, best effort,
/// because the row's cascade would otherwise leave both behind on the server.
pub async fn before_app_removed(s: &AppState, team: TeamId, app: &Application) {
	let exposure = match super::find(&s.db, app.id).await {
		Ok(Some(exposure)) => exposure,
		Ok(None) => return,
		Err(e) => {
			tracing::warn!(
				application = app.id.0,
				"cannot read the tailnet exposure: {e}"
			);
			return;
		}
	};
	let server = match scope::server(&s.db, team, app.server_id).await {
		Ok(server) => server,
		Err(e) => {
			tracing::warn!(
				application = app.id.0,
				"cannot load the server to remove the sidecar: {e}"
			);
			return;
		}
	};
	let client = match s.agents.client(s, &server).await {
		Ok(client) => client,
		Err(e) => {
			tracing::warn!(
				application = app.id.0,
				"cannot reach the agent to remove the sidecar: {e}"
			);
			return;
		}
	};
	let notes = super::run::teardown(&client, app.id, exposure.container_id.as_deref()).await;
	for note in notes {
		tracing::info!(application = app.id.0, "tailnet sidecar removal: {note}");
	}
}
