//! Access-rule use cases shared by the web UI: authorize, then scope, then validate, then
//! touch the database. Reading needs `read domain`, every change needs `update domain`: the
//! rules are part of how an application's hostnames are served.

use sqlx::PgPool;

use super::{MAX_USERS, Rules};
use crate::audit::{self, Diff, Target};
use crate::auth::permission::{Action, Resource, authorize};
use crate::error::{AppResult, ServerError};
use crate::ids::{ApplicationId, ProjectId, TeamId, UserId};
use crate::scope;

/// What an application's Access section shows. The users are names only.
#[derive(Debug, Clone)]
pub struct AccessView {
	pub rules: Rules,
	pub users: Vec<String>,
}

/// The form's text: lists as written (one per line, or separated by commas or spaces) and
/// numbers as the browser sends them.
#[derive(Debug, Clone, Default)]
pub struct RulesInput {
	pub allow: String,
	pub deny: String,
	pub rate_average: String,
	pub rate_burst: String,
	pub max_body_mb: String,
	pub security_headers: bool,
}

fn list(raw: &str) -> Vec<String> {
	raw.split(|c: char| c == ',' || c.is_whitespace())
		.filter(|s| !s.is_empty())
		.map(str::to_owned)
		.collect()
}

fn number(raw: &str, what: &str) -> Result<Option<i32>, ServerError> {
	let raw = raw.trim();
	if raw.is_empty() {
		return Ok(None);
	}
	raw.parse::<i32>()
		.map(Some)
		.map_err(|_| ServerError::Validation(format!("{what} must be a whole number")))
}

impl RulesInput {
	/// The rules the form describes, checked.
	///
	/// # Errors
	/// [`ServerError::Validation`] naming the field.
	pub fn parse(&self, internal_only: bool) -> Result<Rules, ServerError> {
		let mut rules = Rules {
			allow_cidrs: list(&self.allow),
			deny_cidrs: list(&self.deny),
			rate_average: number(&self.rate_average, "The rate limit")?,
			rate_burst: number(&self.rate_burst, "The burst")?,
			max_body_mb: number(&self.max_body_mb, "The body size")?,
			security_headers: self.security_headers,
			internal_only,
		};
		rules.validate()?;
		Ok(rules)
	}
}

/// Whether `name` can be a basic-auth username: it travels in a container label as `name:hash`.
#[must_use]
pub fn valid_username(name: &str) -> bool {
	(1..=64).contains(&name.len())
		&& name
			.bytes()
			.all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-' | b'@'))
}

/// The application's access rules and the usernames of its basic auth; needs `read domain`.
pub async fn get(
	db: &PgPool,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	app: ApplicationId,
) -> AppResult<AccessView> {
	authorize(db, user, team, Action::Read, Resource::Domain).await?;
	let app = scope::application(db, team, project, app).await?;
	Ok(AccessView {
		rules: super::rules(db, app.id).await?.unwrap_or_default(),
		users: super::usernames(db, app.id).await?,
	})
}

/// Stores the rules (the switch for internal-only is kept as it is); needs `update domain`.
/// They apply from the next deploy.
pub async fn save(
	db: &PgPool,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	app: ApplicationId,
	input: &RulesInput,
) -> AppResult<()> {
	authorize(db, user, team, Action::Update, Resource::Domain).await?;
	let app = scope::application(db, team, project, app).await?;
	let current = super::rules(db, app.id).await?.unwrap_or_default();
	let new = input.parse(current.internal_only)?;
	super::save_rules(db, app.id, &new).await?;
	let count = |l: &[String]| i64::try_from(l.len()).unwrap_or(i64::MAX);
	audit::log(
		db,
		user,
		team,
		"app.access",
		Target::new("application", app.id.0)
			.named(&app.slug)
			.in_project(project),
		Diff::new()
			.changed(
				"allow",
				count(&current.allow_cidrs),
				count(&new.allow_cidrs),
			)
			.changed("deny", count(&current.deny_cidrs), count(&new.deny_cidrs))
			.changed("rate_average", current.rate_average, new.rate_average)
			.changed("rate_burst", current.rate_burst, new.rate_burst)
			.changed("max_body_mb", current.max_body_mb, new.max_body_mb)
			.changed(
				"security_headers",
				current.security_headers,
				new.security_headers,
			),
	)
	.await;
	Ok(())
}

/// Takes the application off the public ingress, or puts it back; needs `update domain`.
/// Applies from the next deploy.
pub async fn set_internal(
	db: &PgPool,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	app: ApplicationId,
	internal: bool,
) -> AppResult<()> {
	authorize(db, user, team, Action::Update, Resource::Domain).await?;
	let app = scope::application(db, team, project, app).await?;
	super::set_internal(db, app.id, internal).await?;
	audit::log(
		db,
		user,
		team,
		"app.internal_only",
		Target::new("application", app.id.0)
			.named(&app.slug)
			.in_project(project),
		Diff::new().set("internal_only", internal),
	)
	.await;
	Ok(())
}

/// Adds a basic-auth user, or sets a new password for the one of that name; needs
/// `update domain`. Only the bcrypt hash is kept. Applies from the next deploy.
pub async fn set_password(
	db: &PgPool,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	app: ApplicationId,
	username: &str,
	password: &str,
) -> AppResult<()> {
	authorize(db, user, team, Action::Update, Resource::Domain).await?;
	let app = scope::application(db, team, project, app).await?;
	let username = username.trim();
	if !valid_username(username) {
		return Err(ServerError::Validation(
			"The username can use letters, digits and _ . - @, up to 64 characters".to_owned(),
		));
	}
	// bcrypt reads at most 72 bytes; refusing more beats silently ignoring the rest.
	if password.len() < 8 || password.len() > 72 || password.chars().any(char::is_control) {
		return Err(ServerError::Validation(
			"The password must be 8 to 72 characters".to_owned(),
		));
	}
	let existing = super::usernames(db, app.id).await?;
	if !existing.iter().any(|u| u == username) && super::user_count(db, app.id).await? >= MAX_USERS
	{
		return Err(ServerError::Validation(format!(
			"An application can have {MAX_USERS} basic-auth users"
		)));
	}
	let hash = hash_password(password).await?;
	super::upsert_user(db, app.id, username, &hash).await?;
	audit::log(
		db,
		user,
		team,
		"app.access_user.set",
		Target::new("application", app.id.0)
			.named(&app.slug)
			.in_project(project),
		Diff::new().set("username", username.to_owned()),
	)
	.await;
	Ok(())
}

/// Removes a basic-auth user; needs `update domain`. Applies from the next deploy.
pub async fn remove_user(
	db: &PgPool,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	app: ApplicationId,
	username: &str,
) -> AppResult<()> {
	authorize(db, user, team, Action::Update, Resource::Domain).await?;
	let app = scope::application(db, team, project, app).await?;
	if !super::delete_user(db, app.id, username).await? {
		return Err(ServerError::Validation("There is no such user".to_owned()));
	}
	audit::log(
		db,
		user,
		team,
		"app.access_user.remove",
		Target::new("application", app.id.0)
			.named(&app.slug)
			.in_project(project),
		Diff::new().set("username", username.to_owned()),
	)
	.await;
	Ok(())
}

/// bcrypt in the `$2y$` form Traefik's htpasswd reader accepts
/// (<https://doc.traefik.io/traefik/middlewares/http/basicauth/>: "MD5, SHA1 and bcrypt").
/// Cost 10: Traefik checks the hash on every request that carries a login.
async fn hash_password(password: &str) -> AppResult<String> {
	let password = password.to_owned();
	tokio::task::spawn_blocking(move || {
		bcrypt::hash_with_result(password, 10)
			.map(|parts| parts.format_for_version(bcrypt::Version::TwoY))
	})
	.await
	.map_err(|e| {
		tracing::error!("password hashing stopped: {e}");
		ServerError::Internal
	})?
	.map_err(|e| {
		tracing::error!("password hashing failed: {e}");
		ServerError::Internal
	})
}
