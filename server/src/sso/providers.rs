//! Identity providers: who may set them up, and what is stored.
//!
//! A provider belongs to the whole instance (`team` is `None`; only the instance
//! administrator, the oldest account, manages those) or to one team (a member with
//! `update team` manages those; a team's provider is invisible from other teams). The
//! client secret is encrypted with the instance secret box and never read back: an edit
//! that leaves the field empty keeps the old one.

use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::PgPool;

use crate::audit::{self, Diff, Target};
use crate::auth::permission::{Action, Resource, authorize};
use crate::auth::record_everywhere;
use crate::error::{AppResult, ServerError};
use crate::ids::{AuthProviderId, RoleId, TeamId, UserId};
use crate::state::AppState;

const MAX_SLUG: usize = 40;
const MAX_NAME: usize = 60;
const DEFAULT_SCOPES: &str = "openid email profile";
/// Constraint violated when a slug is used twice by one owner.
const SLUG_UNIQUE: &str = "auth_providers_slug_key";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
	/// Any OpenID Connect provider with a discovery document.
	Oidc,
	/// GitHub, or a GitHub Enterprise Server.
	Github,
}

impl Kind {
	pub const fn as_str(self) -> &'static str {
		match self {
			Self::Oidc => "oidc",
			Self::Github => "github",
		}
	}

	pub fn parse(raw: &str) -> Option<Self> {
		match raw {
			"oidc" => Some(Self::Oidc),
			"github" => Some(Self::Github),
			_ => None,
		}
	}

	pub const fn label(self) -> &'static str {
		match self {
			Self::Oidc => "OpenID Connect",
			Self::Github => "GitHub",
		}
	}
}

/// Whose provider it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
	Instance,
	Team(TeamId),
}

impl Scope {
	const fn team(self) -> Option<i64> {
		match self {
			Self::Instance => None,
			Self::Team(t) => Some(t.0),
		}
	}
}

/// One line of the group mapping: people in `group` get `role_id` in the team.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupRole {
	pub group: String,
	pub role_id: RoleId,
}

/// A provider, minus its secret.
#[derive(Debug, Clone)]
pub struct Provider {
	pub id: AuthProviderId,
	pub team: Option<TeamId>,
	pub kind: Kind,
	pub slug: String,
	pub name: String,
	pub issuer: Option<String>,
	pub client_id: String,
	pub scopes: String,
	pub jit: bool,
	pub allow_unverified_email: bool,
	pub group_claim: Option<String>,
	pub group_roles: Vec<GroupRole>,
	pub enabled: bool,
}

/// A provider with the secret still sealed, for the sign-in round trip only.
pub struct Sealed {
	pub provider: Provider,
	pub secret: Vec<u8>,
}

impl Sealed {
	/// The client secret in the clear, for one request.
	pub fn open(&self, s: &AppState) -> AppResult<zeroize::Zeroizing<String>> {
		s.secrets
			.decrypt(&self.secret)
			.map(zeroize::Zeroizing::new)
			.map_err(|e| {
				tracing::error!(error = %e, "cannot decrypt a client secret");
				ServerError::Internal
			})
	}
}

struct Row {
	id: i64,
	team_id: Option<i64>,
	kind: String,
	slug: String,
	name: String,
	issuer: Option<String>,
	client_id: String,
	scopes: String,
	jit: bool,
	allow_unverified_email: bool,
	group_claim: Option<String>,
	group_roles: serde_json::Value,
	enabled: bool,
	client_secret: Vec<u8>,
}

impl Row {
	fn seal(self) -> Option<Sealed> {
		Some(Sealed {
			secret: self.client_secret.clone(),
			provider: self.into_provider()?,
		})
	}

	fn into_provider(self) -> Option<Provider> {
		Some(Provider {
			id: AuthProviderId(self.id),
			team: self.team_id.map(TeamId),
			kind: Kind::parse(&self.kind)?,
			slug: self.slug,
			name: self.name,
			issuer: self.issuer,
			client_id: self.client_id,
			scopes: self.scopes,
			jit: self.jit,
			allow_unverified_email: self.allow_unverified_email,
			group_claim: self.group_claim,
			group_roles: serde_json::from_value(self.group_roles).unwrap_or_default(),
			enabled: self.enabled,
		})
	}
}

/// Whether `user` is the instance administrator: the oldest account.
pub async fn is_instance_admin(db: &PgPool, user: UserId) -> Result<bool, sqlx::Error> {
	let first = sqlx::query_scalar!("SELECT id FROM users ORDER BY created_at, id LIMIT 1")
		.fetch_optional(db)
		.await?;
	Ok(first == Some(user.0))
}

/// Refuses anyone who may not manage the providers of `scope`.
async fn manage(db: &PgPool, user: UserId, scope: Scope) -> AppResult<()> {
	match scope {
		Scope::Team(team) => authorize(db, user, team, Action::Update, Resource::Team).await,
		Scope::Instance => {
			if is_instance_admin(db, user).await? {
				Ok(())
			} else {
				Err(ServerError::Forbidden)
			}
		}
	}
}

/// The providers of `scope`; managing them is a precondition, so a person who may not
/// sees nothing of them.
pub async fn list(db: &PgPool, user: UserId, scope: Scope) -> AppResult<Vec<Provider>> {
	manage(db, user, scope).await?;
	let rows = match scope.team() {
		Some(team) => {
			sqlx::query_as!(
				Row,
				"SELECT id, team_id, kind, slug, name, issuer, client_id, scopes, jit,
					allow_unverified_email, group_claim, group_roles, enabled, client_secret
				FROM auth_providers WHERE team_id = $1 ORDER BY name, id",
				team
			)
			.fetch_all(db)
			.await?
		}
		None => {
			sqlx::query_as!(
				Row,
				"SELECT id, team_id, kind, slug, name, issuer, client_id, scopes, jit,
					allow_unverified_email, group_claim, group_roles, enabled, client_secret
				FROM auth_providers WHERE team_id IS NULL ORDER BY name, id"
			)
			.fetch_all(db)
			.await?
		}
	};
	Ok(rows.into_iter().filter_map(Row::into_provider).collect())
}

/// One provider of `scope`, for editing. A provider of another scope answers as if it did
/// not exist.
pub async fn find(
	db: &PgPool,
	user: UserId,
	scope: Scope,
	id: AuthProviderId,
) -> AppResult<Provider> {
	manage(db, user, scope).await?;
	sealed_in(db, scope, id)
		.await?
		.map(|s| s.provider)
		.ok_or(ServerError::NotFound)
}

async fn sealed_in(
	db: &PgPool,
	scope: Scope,
	id: AuthProviderId,
) -> Result<Option<Sealed>, sqlx::Error> {
	let row = match scope.team() {
		Some(team) => {
			sqlx::query_as!(
				Row,
				"SELECT id, team_id, kind, slug, name, issuer, client_id, scopes, jit,
					allow_unverified_email, group_claim, group_roles, enabled, client_secret
				FROM auth_providers WHERE id = $1 AND team_id = $2",
				id.0,
				team
			)
			.fetch_optional(db)
			.await?
		}
		None => {
			sqlx::query_as!(
				Row,
				"SELECT id, team_id, kind, slug, name, issuer, client_id, scopes, jit,
					allow_unverified_email, group_claim, group_roles, enabled, client_secret
				FROM auth_providers WHERE id = $1 AND team_id IS NULL",
				id.0
			)
			.fetch_optional(db)
			.await?
		}
	};
	Ok(row.and_then(Row::seal))
}

/// An enabled provider by id, whoever owns it: the sign-in routes are public.
pub async fn find_enabled(db: &PgPool, id: AuthProviderId) -> AppResult<Sealed> {
	sqlx::query_as!(
		Row,
		"SELECT id, team_id, kind, slug, name, issuer, client_id, scopes, jit,
			allow_unverified_email, group_claim, group_roles, enabled, client_secret
		FROM auth_providers WHERE id = $1 AND enabled",
		id.0
	)
	.fetch_optional(db)
	.await?
	.and_then(Row::seal)
	.ok_or(ServerError::NotFound)
}

/// A sign-in button.
#[derive(Debug, Clone)]
pub struct Button {
	pub id: AuthProviderId,
	pub name: String,
	pub kind: Kind,
}

/// The buttons for the sign-in page: the instance's providers, and those of the team named
/// by `team_slug` when there is one (`/login?team=acme`).
pub async fn buttons(db: &PgPool, team_slug: Option<&str>) -> Result<Vec<Button>, sqlx::Error> {
	let rows = sqlx::query!(
		"SELECT p.id, p.name, p.kind FROM auth_providers p
		LEFT JOIN teams t ON t.id = p.team_id
		WHERE p.enabled AND (p.team_id IS NULL OR t.slug = $1)
		ORDER BY p.team_id IS NOT NULL, p.name, p.id",
		team_slug
	)
	.fetch_all(db)
	.await?;
	Ok(rows
		.into_iter()
		.filter_map(|r| {
			Some(Button {
				id: AuthProviderId(r.id),
				name: r.name,
				kind: Kind::parse(&r.kind)?,
			})
		})
		.collect())
}

/// What the form sends.
#[derive(Debug, Clone)]
pub struct Input {
	pub kind: Kind,
	pub name: String,
	pub slug: String,
	pub issuer: String,
	pub client_id: String,
	/// Empty on an edit keeps the stored secret.
	pub client_secret: String,
	pub scopes: String,
	pub jit: bool,
	pub allow_unverified_email: bool,
	pub group_claim: String,
	/// `group = Role name`, one per line.
	pub group_roles: String,
	pub enabled: bool,
}

fn bad(message: &str) -> ServerError {
	ServerError::Validation(message.to_owned())
}

fn http_url(raw: &str) -> Option<String> {
	let url = url::Url::parse(raw.trim()).ok()?;
	(matches!(url.scheme(), "http" | "https") && url.host_str().is_some())
		.then(|| url.as_str().trim_end_matches('/').to_owned())
}

/// Reads `group = Role name` lines against the roles `team` can assign.
async fn parse_group_roles(
	db: &PgPool,
	team: Option<TeamId>,
	text: &str,
) -> AppResult<Vec<GroupRole>> {
	let lines: Vec<&str> = text
		.lines()
		.map(str::trim)
		.filter(|l| !l.is_empty())
		.collect();
	if lines.is_empty() {
		return Ok(Vec::new());
	}
	let Some(team) = team else {
		return Err(bad(
			"Group mapping is for a team's own provider: roles belong to a team.",
		));
	};
	let roles = crate::members::list_roles(db, team).await?;
	let mut out = Vec::new();
	for line in lines {
		let (group, role) = line
			.split_once('=')
			.map(|(g, r)| (g.trim(), r.trim()))
			.filter(|(g, r)| !g.is_empty() && !r.is_empty())
			.ok_or_else(|| bad("Write each group mapping as: group = Role name"))?;
		let role = roles
			.iter()
			.find(|r| r.name.eq_ignore_ascii_case(role))
			.ok_or_else(|| {
				ServerError::Validation(format!("This team has no role named {role}."))
			})?;
		out.push(GroupRole {
			group: group.to_owned(),
			role_id: role.id,
		});
	}
	Ok(out)
}

/// The mapping as the form shows it: `group = Role name` per line.
pub async fn group_roles_text(
	db: &PgPool,
	team: TeamId,
	mapping: &[GroupRole],
) -> AppResult<String> {
	let roles = crate::members::list_roles(db, team).await?;
	Ok(mapping
		.iter()
		.filter_map(|m| {
			let role = roles.iter().find(|r| r.id == m.role_id)?;
			Some(format!("{} = {}", m.group, role.name))
		})
		.collect::<Vec<_>>()
		.join("\n"))
}

struct Clean {
	kind: Kind,
	name: String,
	slug: String,
	issuer: Option<String>,
	client_id: String,
	scopes: String,
	group_claim: Option<String>,
	group_roles: Vec<GroupRole>,
}

async fn clean(db: &PgPool, scope: Scope, input: &Input) -> AppResult<Clean> {
	let name = input.name.trim();
	if name.is_empty() || name.chars().count() > MAX_NAME {
		return Err(ServerError::Validation(format!(
			"Name must be 1 to {MAX_NAME} characters."
		)));
	}
	let slug = input.slug.trim();
	if slug.is_empty()
		|| slug.len() > MAX_SLUG
		|| !slug
			.bytes()
			.all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
		|| slug.starts_with('-')
		|| slug.ends_with('-')
	{
		return Err(ServerError::Validation(format!(
			"Slug must be 1 to {MAX_SLUG} lowercase letters, digits or hyphens."
		)));
	}
	if input.client_id.trim().is_empty() {
		return Err(bad("Enter the client ID the provider gave you."));
	}
	let issuer = input.issuer.trim();
	let issuer = match input.kind {
		Kind::Oidc => Some(
			http_url(issuer)
				.ok_or_else(|| bad("Enter the issuer URL, like https://auth.example.com"))?,
		),
		Kind::Github if issuer.is_empty() => None,
		Kind::Github => Some(http_url(issuer).ok_or_else(|| {
			bad("Leave the address empty for github.com, or enter your GitHub Enterprise address.")
		})?),
	};
	let scopes = match input.kind {
		Kind::Oidc => {
			let mut parts: Vec<&str> = input.scopes.split_whitespace().collect();
			if parts.is_empty() {
				parts = DEFAULT_SCOPES.split(' ').collect();
			}
			if !parts.contains(&"openid") {
				return Err(bad("An OpenID Connect provider needs the openid scope."));
			}
			parts.join(" ")
		}
		Kind::Github => super::github::SCOPES.to_owned(),
	};
	let group_claim = input.group_claim.trim();
	let group_roles = parse_group_roles(
		db,
		match scope {
			Scope::Team(t) => Some(t),
			Scope::Instance => None,
		},
		&input.group_roles,
	)
	.await?;
	if !group_roles.is_empty() && (group_claim.is_empty() || input.kind == Kind::Github) {
		return Err(bad(
			"Group mapping needs the name of the claim that lists groups, and an OpenID Connect provider.",
		));
	}
	Ok(Clean {
		kind: input.kind,
		name: name.to_owned(),
		slug: slug.to_owned(),
		issuer,
		client_id: input.client_id.trim().to_owned(),
		scopes,
		group_claim: (!group_claim.is_empty()).then(|| group_claim.to_owned()),
		group_roles,
	})
}

fn seal(s: &AppState, secret: &str) -> AppResult<Vec<u8>> {
	s.secrets.encrypt(secret).map_err(|e| {
		tracing::error!(error = %e, "cannot encrypt a client secret");
		ServerError::Internal
	})
}

fn taken(e: sqlx::Error) -> ServerError {
	if crate::db::is_unique_violation(&e, SLUG_UNIQUE) {
		ServerError::Conflict("A provider with that slug already exists.".to_owned())
	} else {
		e.into()
	}
}

async fn log(
	s: &AppState,
	user: UserId,
	scope: Scope,
	action: &str,
	name: &str,
	id: i64,
	diff: Diff,
) {
	let target = Target::new("auth_provider", id).named(name);
	match scope {
		Scope::Team(team) => audit::log(&s.db, user, team, action, target, diff).await,
		// The log is per team: an instance-wide change goes to the administrator's teams.
		Scope::Instance => record_everywhere(&s.db, user, action, target, diff).await,
	}
}

/// Adds a provider.
pub async fn create(
	s: &AppState,
	user: UserId,
	scope: Scope,
	input: &Input,
) -> AppResult<Provider> {
	manage(&s.db, user, scope).await?;
	let c = clean(&s.db, scope, input).await?;
	if input.client_secret.trim().is_empty() {
		return Err(bad("Enter the client secret the provider gave you."));
	}
	let id = AuthProviderId::generate();
	sqlx::query!(
		"INSERT INTO auth_providers (id, team_id, kind, slug, name, issuer, client_id, client_secret,
			scopes, jit, allow_unverified_email, group_claim, group_roles, enabled)
		VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14)",
		id.0,
		scope.team(),
		c.kind.as_str(),
		c.slug,
		c.name,
		c.issuer,
		c.client_id,
		seal(s, input.client_secret.trim())?,
		c.scopes,
		input.jit,
		input.allow_unverified_email,
		c.group_claim,
		json!(c.group_roles),
		input.enabled
	)
	.execute(&s.db)
	.await
	.map_err(taken)?;
	log(
		s,
		user,
		scope,
		"auth_provider.create",
		&c.name,
		id.0,
		Diff::new()
			.set("kind", c.kind.as_str())
			.set("issuer", c.issuer.clone().unwrap_or_default())
			.set("client_id", c.client_id.clone())
			.secret("client_secret")
			.set("jit", input.jit),
	)
	.await;
	find(&s.db, user, scope, id).await
}

/// Changes a provider. An empty `client_secret` keeps the stored one.
pub async fn update(
	s: &AppState,
	user: UserId,
	scope: Scope,
	id: AuthProviderId,
	input: &Input,
) -> AppResult<Provider> {
	manage(&s.db, user, scope).await?;
	let before = sealed_in(&s.db, scope, id)
		.await?
		.ok_or(ServerError::NotFound)?
		.provider;
	let c = clean(&s.db, scope, input).await?;
	if c.kind != before.kind {
		return Err(bad("A provider keeps its kind. Add a new one instead."));
	}
	let new_secret = input.client_secret.trim();
	sqlx::query!(
		"UPDATE auth_providers SET slug = $2, name = $3, issuer = $4, client_id = $5,
			client_secret = COALESCE($6, client_secret), scopes = $7, jit = $8,
			allow_unverified_email = $9, group_claim = $10, group_roles = $11, enabled = $12
		WHERE id = $1",
		id.0,
		c.slug,
		c.name,
		c.issuer,
		c.client_id,
		if new_secret.is_empty() {
			None
		} else {
			Some(seal(s, new_secret)?)
		},
		c.scopes,
		input.jit,
		input.allow_unverified_email,
		c.group_claim,
		json!(c.group_roles),
		input.enabled
	)
	.execute(&s.db)
	.await
	.map_err(taken)?;
	let mut diff = Diff::new()
		.changed("name", before.name.clone(), c.name.clone())
		.changed("slug", before.slug.clone(), c.slug.clone())
		.changed(
			"issuer",
			before.issuer.clone().unwrap_or_default(),
			c.issuer.clone().unwrap_or_default(),
		)
		.changed("client_id", before.client_id.clone(), c.client_id.clone())
		.changed("jit", before.jit, input.jit)
		.changed("enabled", before.enabled, input.enabled);
	if !new_secret.is_empty() {
		diff = diff.secret("client_secret");
	}
	log(s, user, scope, "auth_provider.update", &c.name, id.0, diff).await;
	find(&s.db, user, scope, id).await
}

/// Removes a provider and every sign-in linked to it. Accounts made through it stay.
pub async fn delete(s: &AppState, user: UserId, scope: Scope, id: AuthProviderId) -> AppResult<()> {
	manage(&s.db, user, scope).await?;
	let before = sealed_in(&s.db, scope, id)
		.await?
		.ok_or(ServerError::NotFound)?
		.provider;
	sqlx::query!("DELETE FROM auth_providers WHERE id = $1", id.0)
		.execute(&s.db)
		.await?;
	log(
		s,
		user,
		scope,
		"auth_provider.delete",
		&before.name,
		id.0,
		Diff::new(),
	)
	.await;
	Ok(())
}
