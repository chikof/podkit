//! Registry use cases shared by the JSON API and the web UI: authorize, then scope, then
//! validate, then touch the database.

use axum::http::Method;
use proto::{RegistryAuth, RegistryLogin, RegistryLoginResult};
use sqlx::PgPool;

use super::{LOGIN_CONSTRAINT, NAME_CONSTRAINT, Registry};
use crate::agents::AgentClient;
use crate::applications::StoredRegistryAuth;
use crate::audit::{self, Diff, Target};
use crate::auth::permission::{Action, Resource, authorize};
use crate::db::{is_reference_violation, is_unique_violation};
use crate::error::{AppResult, ServerError};
use crate::ids::{RegistryId, ServerId, TeamId, UserId};
use crate::scope;
use crate::state::AppState;

const MAX_NAME_LEN: usize = 60;
const MAX_PASSWORD_LEN: usize = 4096;

/// What a new registry is made from. The password is encrypted before it is stored and
/// never read back.
pub struct CreateInput {
	pub name: String,
	pub url: String,
	pub username: String,
	pub password: String,
}

/// What an edit changes. The host cannot change: apps pull from it by name. `password` of
/// `None` or empty keeps the stored one.
pub struct UpdateInput {
	pub name: String,
	pub username: String,
	pub password: Option<String>,
}

/// The answer to a login test: what the registry said, never the password.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoginOutcome {
	pub ok: bool,
	pub message: String,
}

fn invalid(what: &str) -> ServerError {
	ServerError::Validation(format!("invalid {what}"))
}

/// The registry host as a pull names it: no scheme, path or credentials, lowercase, with
/// Docker Hub's other names folded into `docker.io`.
pub fn normalize_url(raw: &str) -> AppResult<String> {
	let lowered = raw.trim().to_ascii_lowercase();
	let rest = lowered
		.strip_prefix("https://")
		.or_else(|| lowered.strip_prefix("http://"))
		.unwrap_or(&lowered);
	let host = rest.split(['/', '?', '#']).next().unwrap_or_default();
	let (name, port) = match host.rsplit_once(':') {
		Some((name, port)) if !host.ends_with(']') => (name, Some(port)),
		_ => (host, None),
	};
	let name_ok = !name.is_empty()
		&& name.len() <= 253
		&& !name.starts_with(['.', '-'])
		&& !name.ends_with(['.', '-'])
		&& name
			.chars()
			.all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '[' | ']' | ':'));
	let port_ok =
		port.is_none_or(|p| !p.is_empty() && p.len() <= 5 && p.bytes().all(|b| b.is_ascii_digit()));
	if !name_ok || !port_ok || host.len() > 255 {
		return Err(ServerError::Validation(
			"URL must be a registry host such as ghcr.io or registry.example.com:5000.".to_owned(),
		));
	}
	Ok(match host {
		"index.docker.io" | "registry-1.docker.io" | "registry.hub.docker.com" => {
			"docker.io".to_owned()
		}
		other => other.to_owned(),
	})
}

/// The registry an image reference pulls from, as `docker pull` decides it.
pub fn image_host(reference: &str) -> String {
	match reference.split_once('/') {
		Some((first, _)) if first.contains('.') || first.contains(':') || first == "localhost" => {
			first.to_ascii_lowercase()
		}
		_ => "docker.io".to_owned(),
	}
}

fn text(value: &str, what: &str, max: usize) -> AppResult<()> {
	if value.trim().is_empty() || value.chars().count() > max || value.chars().any(char::is_control)
	{
		return Err(invalid(what));
	}
	Ok(())
}

fn validate_login(name: &str, username: &str, password: Option<&str>) -> AppResult<()> {
	text(name, "name", MAX_NAME_LEN)?;
	text(username, "username", 255)?;
	if password.is_some_and(|p| p.is_empty() || p.len() > MAX_PASSWORD_LEN) {
		return Err(invalid("password"));
	}
	Ok(())
}

fn seal(s: &AppState, password: &str) -> AppResult<Vec<u8>> {
	s.secrets.encrypt(password).map_err(|e| {
		tracing::error!("failed to encrypt registry password: {e}");
		ServerError::Internal
	})
}

fn conflict(e: sqlx::Error) -> ServerError {
	if is_unique_violation(&e, NAME_CONSTRAINT) {
		ServerError::Conflict("a registry with that name already exists".to_owned())
	} else if is_unique_violation(&e, LOGIN_CONSTRAINT) {
		ServerError::Conflict("that registry login is already saved".to_owned())
	} else {
		e.into()
	}
}

/// The team's registries. The stored password is still encrypted in the result; callers
/// must not render it. Needs `read registry`.
pub async fn list(db: &PgPool, user: UserId, team: TeamId) -> AppResult<Vec<Registry>> {
	authorize(db, user, team, Action::Read, Resource::Registry).await?;
	Ok(super::list_by_team(db, team).await?)
}

/// The team's registries with how many applications and stacks pull with each, for the
/// registries page. Needs `read registry`.
pub async fn list_with_usage(
	db: &PgPool,
	user: UserId,
	team: TeamId,
) -> AppResult<Vec<(Registry, i64)>> {
	authorize(db, user, team, Action::Read, Resource::Registry).await?;
	Ok(super::list_with_usage(db, team).await?)
}

/// Saves a login. Needs `create registry`.
pub async fn create(
	s: &AppState,
	user: UserId,
	team: TeamId,
	mut body: CreateInput,
) -> AppResult<Registry> {
	authorize(&s.db, user, team, Action::Create, Resource::Registry).await?;
	validate_login(&body.name, &body.username, Some(&body.password))?;
	body.url = normalize_url(&body.url)?;
	let sealed = seal(s, &body.password)?;
	let created = insert(&s.db, team, &body, &sealed)
		.await
		.map_err(conflict)?;
	audit::log(
		&s.db,
		user,
		team,
		"registry.create",
		Target::new("registry", created.id).named(&created.name),
		Diff::new()
			.set("name", created.name.clone())
			.set("url", audit::scrub_url(&created.url))
			.set("username", created.username.clone())
			.secret("password"),
	)
	.await;
	Ok(created)
}

async fn insert(
	db: &PgPool,
	team: TeamId,
	body: &CreateInput,
	sealed: &[u8],
) -> Result<Registry, sqlx::Error> {
	sqlx::query_as!(
		Registry,
		r#"
			INSERT INTO registries (id, team_id, name, url, username, password)
			VALUES ($1, $2, $3, $4, $5, $6)
			RETURNING id AS "id: RegistryId", name, url, username,
				password, created_at
		"#,
		RegistryId::generate() as RegistryId,
		team as TeamId,
		body.name.trim(),
		body.url,
		body.username.trim(),
		sealed,
	)
	.fetch_one(db)
	.await
}

/// Renames a registry or changes its username or password. Applications that use it
/// pick the change up on their next deploy. Needs `update registry`.
pub async fn update(
	s: &AppState,
	user: UserId,
	team: TeamId,
	id: RegistryId,
	body: UpdateInput,
) -> AppResult<Registry> {
	authorize(&s.db, user, team, Action::Update, Resource::Registry).await?;
	let registry = scope::registry(&s.db, team, id).await?;
	let password = body.password.as_deref().filter(|p| !p.is_empty());
	validate_login(&body.name, &body.username, password)?;
	let sealed = password.map(|p| seal(s, p)).transpose()?;
	let updated = sqlx::query_as!(
		Registry,
		r#"
			UPDATE registries
			SET name = $2, username = $3, password = COALESCE($4, password)
			WHERE id = $1
			RETURNING id AS "id: RegistryId", name, url, username,
				password, created_at
		"#,
		registry.id as RegistryId,
		body.name.trim(),
		body.username.trim(),
		sealed,
	)
	.fetch_one(&s.db)
	.await
	.map_err(conflict)?;
	let mut diff = Diff::new()
		.changed("name", registry.name.clone(), updated.name.clone())
		.changed(
			"username",
			registry.username.clone(),
			updated.username.clone(),
		);
	if password.is_some() {
		diff = diff.secret("password");
	}
	audit::log(
		&s.db,
		user,
		team,
		"registry.update",
		Target::new("registry", updated.id).named(&updated.name),
		diff,
	)
	.await;
	Ok(updated)
}

/// Deletes a registry nothing pulls with. Needs `delete registry`.
pub async fn remove(
	db: &PgPool,
	user: UserId,
	team: TeamId,
	id: RegistryId,
) -> AppResult<RegistryId> {
	authorize(db, user, team, Action::Delete, Resource::Registry).await?;
	let registry = scope::registry(db, team, id).await?;
	sqlx::query!(
		"DELETE FROM registries WHERE id = $1",
		registry.id as RegistryId
	)
	.execute(db)
	.await
	.map_err(|e| {
		if is_reference_violation(&e) {
			ServerError::Conflict(
				"applications or stacks still pull with this registry; move them first".to_owned(),
			)
		} else {
			e.into()
		}
	})?;
	audit::log(
		db,
		user,
		team,
		"registry.delete",
		Target::new("registry", registry.id).named(&registry.name),
		Diff::new()
			.set("name", registry.name.clone())
			.set("url", audit::scrub_url(&registry.url)),
	)
	.await;
	Ok(registry.id)
}

async fn agent_for(s: &AppState, team: TeamId, server: Option<ServerId>) -> AppResult<AgentClient> {
	let server = if let Some(id) = server {
		scope::server(&s.db, team, id).await?
	} else {
		let all = crate::servers::list_by_team(&s.db, team).await?;
		let local = all.iter().position(|s| s.is_local).unwrap_or(0);
		all.into_iter()
			.nth(local)
			.ok_or(ServerError::ServerNotFound)?
	};
	s.agents.client(s, &server).await
}

async fn login(
	s: &AppState,
	team: TeamId,
	server: Option<ServerId>,
	url: &str,
	username: &str,
	password: &str,
) -> AppResult<LoginOutcome> {
	let client = agent_for(s, team, server).await?;
	let body = RegistryLogin {
		username: username.to_owned(),
		password: password.to_owned(),
		server: url.to_owned(),
	};
	let answer: RegistryLoginResult = client
		.json(Method::POST, "/v1/registry-login", Some(&body))
		.await?;
	Ok(LoginOutcome {
		ok: answer.ok,
		message: answer.message,
	})
}

/// Logs in to a saved registry from one of the team's servers (its local one when
/// `server` is `None`). Needs `update registry`.
pub async fn test(
	s: &AppState,
	user: UserId,
	team: TeamId,
	id: RegistryId,
	server: Option<ServerId>,
) -> AppResult<LoginOutcome> {
	authorize(&s.db, user, team, Action::Update, Resource::Registry).await?;
	let registry = scope::registry(&s.db, team, id).await?;
	let password = s.secrets.decrypt(&registry.password).map_err(|e| {
		tracing::error!(
			registry = registry.id.0,
			"registry password decrypt failed: {e}"
		);
		ServerError::Internal
	})?;
	let outcome = login(
		s,
		team,
		server,
		&registry.url,
		&registry.username,
		&password,
	)
	.await?;
	audit::log(
		&s.db,
		user,
		team,
		"registry.test",
		Target::new("registry", registry.id).named(&registry.name),
		Diff::new()
			.set("server_id", server.map(|s| s.0))
			.set("ok", outcome.ok),
	)
	.await;
	Ok(outcome)
}

/// Tries a login before it is saved: the same checks as [`create`], then one login from the
/// agent. Nothing is stored. Needs `create registry`.
pub async fn test_new(
	s: &AppState,
	user: UserId,
	team: TeamId,
	server: Option<ServerId>,
	mut body: CreateInput,
) -> AppResult<LoginOutcome> {
	authorize(&s.db, user, team, Action::Create, Resource::Registry).await?;
	validate_login(&body.name, &body.username, Some(&body.password))?;
	body.url = normalize_url(&body.url)?;
	login(
		s,
		team,
		server,
		&body.url,
		body.username.trim(),
		&body.password,
	)
	.await
}

/// The credentials a deploy sends the agent for a saved registry. No permission check:
/// callers are deploy jobs acting for a resource they already resolved.
pub async fn auth_for(s: &AppState, id: RegistryId) -> AppResult<RegistryAuth> {
	let registry = super::find_by_id(&s.db, id)
		.await?
		.ok_or(ServerError::RegistryNotFound)?;
	let password = s.secrets.decrypt(&registry.password).map_err(|e| {
		tracing::error!(
			registry = registry.id.0,
			"registry password decrypt failed: {e}"
		);
		ServerError::Internal
	})?;
	Ok(RegistryAuth {
		username: registry.username,
		password,
		server: Some(registry.url),
	})
}

/// Checks a registry may be used for `image`: it is the team's own, and it is the registry
/// the image is pulled from, so its login is never sent anywhere else.
pub async fn pick_for_image(
	db: &PgPool,
	team: TeamId,
	id: RegistryId,
	image: &str,
) -> AppResult<Registry> {
	let registry = scope::registry(db, team, id).await?;
	if image_host(image) != registry.url {
		return Err(ServerError::Validation(format!(
			"that registry is for {}, but the image is pulled from {}",
			registry.url,
			image_host(image)
		)));
	}
	Ok(registry)
}

/// Checks a registry is the team's own, for stacks, whose services may pull from several
/// hosts: the agent sends the login only to a registry that asks for it.
pub async fn pick(db: &PgPool, team: TeamId, id: RegistryId) -> AppResult<Registry> {
	scope::registry(db, team, id).await
}

/// Moves every application's own registry login into the team's saved registries, one per
/// host and username. Applications are pointed at the saved registry and their private
/// copy is dropped. A login whose password cannot be read is left as it is. Safe to run
/// again; returns how many applications moved.
pub async fn backfill(s: &AppState) -> AppResult<usize> {
	let rows = sqlx::query!(
		r#"
			SELECT a.id, p.team_id, a.image AS "image!", a.registry_auth AS "registry_auth!"
			FROM applications a JOIN projects p ON p.id = a.project_id
			WHERE a.registry_auth IS NOT NULL AND a.registry_id IS NULL AND a.image IS NOT NULL
			ORDER BY a.id
		"#
	)
	.fetch_all(&s.db)
	.await?;
	let mut moved = 0;
	for row in rows {
		let stored = s
			.secrets
			.decrypt(&row.registry_auth)
			.ok()
			.and_then(|json| serde_json::from_str::<StoredRegistryAuth>(&json).ok());
		let Some(stored) = stored else {
			tracing::warn!(
				application = row.id,
				"registry login not readable; left as is"
			);
			continue;
		};
		let url = image_host(&row.image);
		let mut tx = s.db.begin().await?;
		let found = sqlx::query_scalar!(
			"SELECT id FROM registries WHERE team_id = $1 AND url = $2 AND username = $3",
			row.team_id,
			url,
			stored.username,
		)
		.fetch_optional(&mut *tx)
		.await?;
		let registry = if let Some(id) = found {
			id
		} else {
			let sealed = seal(s, &stored.password)?;
			let name = free_name(&mut tx, row.team_id, &url, &stored.username).await?;
			let id = RegistryId::generate().0;
			sqlx::query!(
				"INSERT INTO registries (id, team_id, name, url, username, password)
				VALUES ($1, $2, $3, $4, $5, $6)",
				id,
				row.team_id,
				name,
				url,
				stored.username,
				sealed,
			)
			.execute(&mut *tx)
			.await?;
			id
		};
		sqlx::query!(
			"UPDATE applications SET registry_id = $2, registry_auth = NULL WHERE id = $1",
			row.id,
			registry,
		)
		.execute(&mut *tx)
		.await?;
		tx.commit().await?;
		moved += 1;
	}
	Ok(moved)
}

/// `host (user)`, with a number added when the team already has a registry of that name.
async fn free_name(
	tx: &mut sqlx::PgConnection,
	team: i64,
	url: &str,
	username: &str,
) -> AppResult<String> {
	let base: String = format!("{url} ({username})")
		.chars()
		.take(MAX_NAME_LEN - 4)
		.collect();
	let mut name = base.clone();
	let mut n = 1;
	while sqlx::query_scalar!(
		r#"SELECT EXISTS (SELECT 1 FROM registries WHERE team_id = $1 AND name = $2) AS "taken!""#,
		team,
		name,
	)
	.fetch_one(&mut *tx)
	.await?
	{
		n += 1;
		name = format!("{base} {n}");
	}
	Ok(name)
}

#[cfg(test)]
mod unit {
	use super::*;

	#[test]
	fn urls_are_reduced_to_a_host() {
		for (raw, want) in [
			("ghcr.io", Some("ghcr.io")),
			("  GHCR.io  ", Some("ghcr.io")),
			("https://ghcr.io/", Some("ghcr.io")),
			(
				"https://registry.example.com:5000/v2/",
				Some("registry.example.com:5000"),
			),
			("http://127.0.0.1:5000", Some("127.0.0.1:5000")),
			("https://index.docker.io/v1/", Some("docker.io")),
			("registry-1.docker.io", Some("docker.io")),
			("", None),
			("https://", None),
			("user:pw@ghcr.io", None),
			("ghcr.io:abc", None),
			("ghcr io", None),
			("-bad.example.com", None),
			("exa_mple.com", None),
		] {
			assert_eq!(normalize_url(raw).ok().as_deref(), want, "{raw:?}");
		}
	}

	#[test]
	fn image_hosts_follow_docker() {
		for (image, host) in [
			("alpine:3", "docker.io"),
			("traefik/whoami", "docker.io"),
			("ghcr.io/me/app:1", "ghcr.io"),
			("GHCR.io/me/app:1", "ghcr.io"),
			("localhost:5000/app", "localhost:5000"),
			("localhost/app", "localhost"),
			("127.0.0.1:5000/app:1", "127.0.0.1:5000"),
		] {
			assert_eq!(image_host(image), host, "{image}");
		}
	}
}
