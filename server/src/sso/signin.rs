//! The sign-in round trip with an identity provider, and what becomes of its answer.
//!
//! 1. [`begin`] makes a flow (state, nonce, PKCE verifier), ties it to the browser with a
//!    cookie and returns the provider's address.
//! 2. [`callback`] checks `state` against the cookie and the flow, trades the code, validates
//!    the answer and [`resolve`]s who it is: a linked account signs in; an email that an
//!    account already has asks for that account's password ([`finish_link`]) before the
//!    provider is linked; an unknown email creates an account when the provider allows it
//!    and the registration policy does.

use axum::http::HeaderMap;
use serde_json::json;
use tower_cookies::{Cookie, Cookies, cookie::SameSite};
use zeroize::Zeroizing;

use super::providers::{self, GroupRole, Kind, Provider, Sealed};
use super::{Identity, github, oidc};
use crate::audit::{self, Diff, Target};
use crate::auth::flow::{self, Kind as FlowKind, MAX_ATTEMPTS};
use crate::auth::session::Method;
use crate::auth::{open_session, record_everywhere, service, users};
use crate::error::{AppResult, ServerError};
use crate::ids::{AuthProviderId, IdentityId, RoleId, TeamId, UserId};
use crate::state::{AppState, Registration};

/// Ties the browser to the OIDC flow it started. `Lax`, not `Strict`: the provider
/// sends the browser back from another site, and a strict cookie would not come along.
pub const FLOW_COOKIE: &str = "pk_sso";
/// Ties the browser to the wait for a password to link an account.
pub const LINK_COOKIE: &str = "pk_link";

/// The longest name an account holds (`users.name`).
const MAX_NAME: usize = 40;

fn refused(why: &str) -> ServerError {
	ServerError::Validation(why.to_owned())
}

/// What the person is told when the provider's answer cannot be trusted; the cause is in
/// the log.
fn untrusted(cause: &str) -> ServerError {
	tracing::warn!(cause, "identity provider sign-in refused");
	refused(
		"The identity provider's answer could not be verified. Try again, or ask an administrator.",
	)
}

fn cookie(
	name: &'static str,
	value: String,
	headers: &HeaderMap,
	same_site: SameSite,
) -> Cookie<'static> {
	Cookie::build((name, value))
		.http_only(true)
		.secure(crate::auth::reached_over_https(headers))
		.same_site(same_site)
		.max_age(flow::TTL)
		.path("/")
		.build()
}

fn clear(cookies: &Cookies, name: &'static str) {
	cookies.remove(Cookie::build((name, "")).path("/").build());
}

/// Where the provider sends the browser back to.
pub fn redirect_uri(s: &AppState, headers: &HeaderMap, id: AuthProviderId) -> String {
	format!(
		"{}/sso/{id}/callback",
		crate::auth::origin::external_base(&s.config, headers)
	)
}

fn random_nonce() -> String {
	crypto::generate_token(16)
}

/// Starts a sign-in: the address to send the browser to. `next` is where to go afterwards
/// and must already be a safe local path.
pub async fn begin(
	s: &AppState,
	cookies: &Cookies,
	headers: &HeaderMap,
	id: AuthProviderId,
	next: &str,
) -> AppResult<String> {
	let sealed = providers::find_enabled(&s.db, id).await?;
	let p = &sealed.provider;
	let redirect = redirect_uri(s, headers, p.id);
	let pkce = oidc::pkce();
	let nonce = random_nonce();
	let state = flow::start(
		&s.db,
		FlowKind::Oidc,
		None,
		Some(p.id),
		&json!({ "verifier": pkce.verifier, "nonce": nonce, "next": next }),
	)
	.await?;
	let url = match p.kind {
		Kind::Oidc => {
			let issuer = p.issuer.as_deref().ok_or(ServerError::Internal)?;
			let disc = oidc::discover(&s.http, issuer)
				.await
				.map_err(|e| untrusted(&e))?;
			oidc::authorization_url(&oidc::AuthRequest {
				endpoint: &disc.authorization_endpoint,
				client_id: &p.client_id,
				redirect_uri: &redirect,
				scope: &p.scopes,
				state: &state,
				nonce: Some(&nonce),
				challenge: &pkce.challenge,
			})
		}
		Kind::Github => github::authorization_url(
			&github::endpoints(p.issuer.as_deref()),
			&p.client_id,
			&redirect,
			&state,
			&pkce,
		),
	}
	.map_err(|e| untrusted(&e))?;
	cookies.add(cookie(FLOW_COOKIE, state, headers, SameSite::Lax));
	Ok(url)
}

/// What the provider sent back.
#[derive(Debug, Default)]
pub struct Callback {
	pub code: Option<String>,
	pub state: Option<String>,
	/// The provider's own `error` (the person said no, or it failed).
	pub error: Option<String>,
}

/// What a callback led to.
#[derive(Debug, PartialEq, Eq)]
pub enum Landing {
	/// A session is open; go here.
	SignedIn(String),
	/// An account has this email: it must be unlocked with its password first.
	LinkNeeded,
}

/// Finishes the round trip.
pub async fn callback(
	s: &AppState,
	cookies: &Cookies,
	headers: &HeaderMap,
	id: AuthProviderId,
	answer: Callback,
) -> AppResult<Landing> {
	let cookie_state = cookies.get(FLOW_COOKIE).map(|c| c.value().to_owned());
	clear(cookies, FLOW_COOKIE);
	if let Some(error) = answer.error {
		tracing::info!(provider = %id, error, "identity provider returned an error");
		return Err(refused("The identity provider did not sign you in."));
	}
	// The cookie and the query must carry the same state, and the flow must be ours: a
	// callback someone else started (login CSRF) has no matching cookie.
	let (Some(state), Some(cookie_state), Some(code)) = (answer.state, cookie_state, answer.code)
	else {
		return Err(refused(
			"That sign-in link is not valid any more. Start again from the sign-in page.",
		));
	};
	if !crypto::constant_time_eq(&state, &cookie_state) {
		return Err(refused(
			"That sign-in link is not valid any more. Start again from the sign-in page.",
		));
	}
	let flow = flow::take(&s.db, FlowKind::Oidc, &state)
		.await?
		.filter(|f| f.provider == Some(id))
		.ok_or_else(|| {
			refused("That sign-in link has expired. Start again from the sign-in page.")
		})?;
	let sealed = providers::find_enabled(&s.db, id).await?;
	let verifier = flow.data["verifier"]
		.as_str()
		.unwrap_or_default()
		.to_owned();
	let nonce = flow.data["nonce"].as_str().unwrap_or_default().to_owned();
	let next = flow.data["next"].as_str().unwrap_or("/").to_owned();

	let identity = fetch_identity(s, headers, &sealed, &code, &verifier, &nonce)
		.await
		.map_err(|e| untrusted(&e))?;
	match resolve(s, &sealed.provider, &identity).await? {
		Resolved::User(user) => {
			apply_groups(s, &sealed.provider, user, &identity.groups).await?;
			open_session(s, cookies, headers, user, Method::Sso).await?;
			Ok(Landing::SignedIn(next))
		}
		Resolved::Link(user) => {
			let token = flow::start(
				&s.db,
				FlowKind::Link,
				Some(user),
				Some(sealed.provider.id),
				&json!({
					"subject": identity.subject, "email": identity.email, "name": identity.name,
					"groups": identity.groups, "next": next,
				}),
			)
			.await?;
			cookies.add(cookie(LINK_COOKIE, token, headers, SameSite::Strict));
			Ok(Landing::LinkNeeded)
		}
	}
}

async fn fetch_identity(
	s: &AppState,
	headers: &HeaderMap,
	sealed: &Sealed,
	code: &str,
	verifier: &str,
	nonce: &str,
) -> Result<Identity, String> {
	let p = &sealed.provider;
	let secret = sealed
		.open(s)
		.map_err(|_| "cannot read the client secret".to_owned())?;
	let redirect = redirect_uri(s, headers, p.id);
	match p.kind {
		Kind::Github => {
			let ep = github::endpoints(p.issuer.as_deref());
			let token = github::exchange(
				&s.http,
				&ep,
				&p.client_id,
				&secret,
				&redirect,
				code,
				verifier,
			)
			.await?;
			github::identity(&s.http, &ep, &token).await
		}
		Kind::Oidc => {
			let issuer = p.issuer.as_deref().ok_or("the provider has no issuer")?;
			let disc = oidc::discover(&s.http, issuer).await?;
			let tokens = oidc::exchange(
				&s.http,
				&disc,
				&p.client_id,
				&secret,
				&redirect,
				code,
				verifier,
			)
			.await?;
			let id_token = tokens
				.id_token
				.ok_or("the token endpoint returned no ID token")?;
			let mut claims =
				oidc::verify_id_token(&s.http, &disc, &p.client_id, nonce, &id_token).await?;
			// Some providers leave the email out of the ID token and give it at userinfo.
			if claims.email.is_none()
				&& let (Some(endpoint), Some(access)) =
					(&disc.userinfo_endpoint, &tokens.access_token)
			{
				let more = oidc::userinfo(&s.http, endpoint, access, &claims.subject).await?;
				claims.email = more.email;
				claims.email_verified = claims.email_verified.or(more.email_verified);
				claims.name = claims.name.or(more.name);
				for (k, v) in more.all {
					claims.all.entry(k).or_insert(v);
				}
			}
			let groups = p
				.group_claim
				.as_deref()
				.map(|c| claims.list(c))
				.unwrap_or_default();
			Ok(Identity {
				name: claims
					.name
					.clone()
					.or_else(|| claims.email.clone())
					.unwrap_or_else(|| claims.subject.clone()),
				subject: claims.subject,
				email: claims.email,
				email_verified: claims.email_verified.unwrap_or(false),
				groups,
			})
		}
	}
}

/// Whose the identity is.
#[derive(Debug, PartialEq, Eq)]
pub enum Resolved {
	/// A linked account, or a new one made now.
	User(UserId),
	/// The email is an existing account's; it has not been linked yet.
	Link(UserId),
}

fn clip_name(name: &str) -> String {
	let name: String = name.trim().chars().take(MAX_NAME).collect();
	if name.is_empty() {
		"Member".to_owned()
	} else {
		name
	}
}

/// Decides whose `identity` is. Never links an existing account on the email alone.
pub async fn resolve(
	s: &AppState,
	provider: &Provider,
	identity: &Identity,
) -> AppResult<Resolved> {
	let linked = sqlx::query_scalar!(
		"SELECT user_id FROM user_identities WHERE provider_id = $1 AND subject = $2",
		provider.id.0,
		identity.subject
	)
	.fetch_optional(&s.db)
	.await?;
	if let Some(user) = linked {
		return Ok(Resolved::User(UserId(user)));
	}

	let email = identity
		.email
		.as_deref()
		.filter(|_| {
			// A team administrator's provider cannot vouch for an unverified email.
			identity.email_verified || (provider.allow_unverified_email && provider.team.is_none())
		})
		.ok_or_else(|| {
			refused("The provider did not give a verified email address, so podkit cannot tell which account is yours.")
		})?;
	let email = crate::auth::validate_email(email)
		.map_err(|_| refused("The provider gave an email address podkit cannot use."))?
		.normalize();

	if let Some(existing) = users::find_by_email(&s.db, &email).await? {
		// Matching on the email alone would let any provider an administrator can set up
		// sign in as anyone: the person proves the account is theirs first.
		return Ok(Resolved::Link(existing.id));
	}

	if !provider.jit {
		return Err(ServerError::Validation(format!(
			"No account uses {email}, and {} does not create accounts. Ask an administrator.",
			provider.name
		)));
	}
	if !super::domains::allows_new_account(&s.db, provider.id, provider.team, &email).await? {
		return Err(refused(&format!(
			"{} cannot create an account for {email}: its email domain is not approved for this provider. Ask an administrator.",
			provider.name
		)));
	}
	if !service::registration_allowed(&s.db, s.config.registration).await? {
		return Err(ServerError::RegistrationClosed);
	}
	let user = create_account(s, &clip_name(&identity.name), &email).await?;
	link(s, provider.id, user, identity).await?;
	record_everywhere(
		&s.db,
		user,
		"user.create",
		Target::new("user", user).named(email),
		Diff::new().set("via", provider.name.clone()),
	)
	.await;
	Ok(Resolved::User(user))
}

/// Makes the account of a person who has no password: its password is random and nobody
/// is told it. Follows the registration policy's atomic rule for a first-user instance.
async fn create_account(s: &AppState, name: &str, email: &str) -> AppResult<UserId> {
	let unknown = crypto::hash(Zeroizing::new(crypto::generate_token(32)))
		.await
		.map_err(|e| {
			tracing::error!(error = %format!("{e:#}"), "password hashing failed");
			ServerError::Internal
		})?;
	let made = match s.config.registration {
		Registration::Open => users::insert(&s.db, name, email, &unknown).await.map(Some),
		Registration::FirstUser => users::insert_if_no_users(&s.db, name, email, &unknown).await,
		Registration::Closed => return Err(ServerError::RegistrationClosed),
	}
	.map_err(|e| {
		if crate::db::is_unique_violation(&e, users::EMAIL_UNIQUE) {
			ServerError::EmailTaken
		} else {
			e.into()
		}
	})?;
	let user = made.ok_or(ServerError::RegistrationClosed)?;
	sqlx::query!(
		"UPDATE users SET has_password = FALSE WHERE id = $1",
		user.id.0
	)
	.execute(&s.db)
	.await?;
	Ok(user.id)
}

async fn link(
	s: &AppState,
	provider: AuthProviderId,
	user: UserId,
	identity: &Identity,
) -> Result<(), sqlx::Error> {
	sqlx::query!(
		"INSERT INTO user_identities (id, provider_id, user_id, subject, email)
		VALUES ($1, $2, $3, $4, $5) ON CONFLICT (provider_id, subject) DO NOTHING",
		IdentityId::generate().0,
		provider.0,
		user.0,
		identity.subject,
		identity.email
	)
	.execute(&s.db)
	.await?;
	Ok(())
}

/// The built-in role a person gets when a team's provider adds them and no group says
/// otherwise.
async fn default_role(db: &sqlx::PgPool) -> Result<RoleId, sqlx::Error> {
	sqlx::query_scalar!(
		r#"SELECT id AS "id: RoleId" FROM roles WHERE team_id IS NULL AND is_default ORDER BY id LIMIT 1"#
	)
	.fetch_one(db)
	.await
}

/// The role the groups map to: the first mapping line whose group the person is in, so the
/// administrator lists the most privileged group first.
fn mapped_role(mapping: &[GroupRole], groups: &[String]) -> Option<RoleId> {
	mapping
		.iter()
		.find(|m| groups.iter().any(|g| g == &m.group))
		.map(|m| m.role_id)
}

/// Puts a person who signed in through a team's provider in that team, and keeps their role
/// in step with their groups. A provider of the instance changes no team.
///
/// With a group mapping, a person in none of the mapped groups is not added (and an existing
/// member keeps their role). An Owner is never demoted when they are the team's last one.
pub async fn apply_groups(
	s: &AppState,
	provider: &Provider,
	user: UserId,
	groups: &[String],
) -> AppResult<()> {
	let Some(team) = provider.team else {
		return Ok(());
	};
	let mapped = mapped_role(&provider.group_roles, groups);
	let member = crate::members::find(&s.db, team, user).await?;
	let ctx = audit::by(user, team);
	match (member, mapped) {
		(None, Some(role)) => {
			crate::members::ensure_assignable(&s.db, team, role).await?;
			crate::members::insert(&s.db, team, user, role).await?;
			audit::record(
				&s.db,
				&ctx,
				"member.add",
				Target::new("member", user),
				Diff::new().set("role_id", role.0),
			)
			.await;
		}
		(None, None) if provider.group_roles.is_empty() => {
			let role = default_role(&s.db).await?;
			crate::members::insert(&s.db, team, user, role).await?;
			audit::record(
				&s.db,
				&ctx,
				"member.add",
				Target::new("member", user),
				Diff::new().set("role_id", role.0),
			)
			.await;
		}
		(Some(member), Some(role)) if member.role_id != role => {
			if last_owner(&s.db, team, member.role_id).await? {
				return Ok(());
			}
			crate::members::ensure_assignable(&s.db, team, role).await?;
			crate::members::set_role(&s.db, team, user, role).await?;
			audit::record(
				&s.db,
				&ctx,
				"member.update",
				Target::new("member", user),
				Diff::new().change("role_id", member.role_id.0, role.0),
			)
			.await;
		}
		_ => {}
	}
	Ok(())
}

/// Whether `role` is Owner and the team has no other Owner.
async fn last_owner(db: &sqlx::PgPool, team: TeamId, role: RoleId) -> Result<bool, sqlx::Error> {
	sqlx::query_scalar!(
		r#"
			SELECT (
				EXISTS (SELECT 1 FROM roles WHERE id = $2 AND team_id IS NULL AND name = 'Owner')
				AND (SELECT COUNT(*) FROM team_members WHERE team_id = $1 AND role_id = $2) <= 1
			) AS "last!"
		"#,
		team.0,
		role.0
	)
	.fetch_one(db)
	.await
}

/// The provider and email a waiting link is about, for the page that asks for the password.
#[derive(Debug, Clone)]
pub struct LinkPrompt {
	pub provider: String,
	pub email: Option<String>,
}

/// The waiting link this browser has, if any.
pub async fn pending_link(s: &AppState, cookies: &Cookies) -> AppResult<Option<LinkPrompt>> {
	let Some(token) = cookies.get(LINK_COOKIE).map(|c| c.value().to_owned()) else {
		return Ok(None);
	};
	let Some(flow) = flow::peek(&s.db, FlowKind::Link, &token).await? else {
		return Ok(None);
	};
	let Some(provider) = flow.provider else {
		return Ok(None);
	};
	let name = sqlx::query_scalar!("SELECT name FROM auth_providers WHERE id = $1", provider.0)
		.fetch_optional(&s.db)
		.await?;
	Ok(name.map(|provider| LinkPrompt {
		provider,
		email: flow.data["email"].as_str().map(str::to_owned),
	}))
}

/// What a correct password for a link led to.
#[derive(Debug, PartialEq, Eq)]
pub enum Linked {
	/// Signed in; go here.
	SignedIn(String),
	/// The account has a second factor to give first.
	SecondFactor,
}

/// Links the provider to the account once its password is right, then signs in. Five wrong
/// passwords end the attempt. An account with no password of its own cannot be linked this
/// way.
pub async fn finish_link(
	s: &AppState,
	cookies: &Cookies,
	headers: &HeaderMap,
	password: &str,
) -> AppResult<Linked> {
	let token = cookies
		.get(LINK_COOKIE)
		.map(|c| c.value().to_owned())
		.ok_or_else(|| {
			refused("That link request has expired. Start again from the sign-in page.")
		})?;
	let flow = flow::peek(&s.db, FlowKind::Link, &token)
		.await?
		.ok_or_else(|| {
			refused("That link request has expired. Start again from the sign-in page.")
		})?;
	let (Some(user), Some(provider)) = (flow.user, flow.provider) else {
		return Err(ServerError::Internal);
	};
	let row = sqlx::query!(
		"SELECT password_hash, has_password FROM users WHERE id = $1",
		user.0
	)
	.fetch_one(&s.db)
	.await?;
	if !row.has_password {
		flow::forget(&s.db, &token).await?;
		clear(cookies, LINK_COOKIE);
		return Err(refused(
			"That account was made by a provider and has no password to prove it with. Sign in with the provider you used before.",
		));
	}
	let ok = crypto::verify(Zeroizing::new(password.to_owned()), row.password_hash)
		.await
		.map_err(|e| {
			tracing::error!(error = %format!("{e:#}"), "password check failed");
			ServerError::Internal
		})?;
	if !ok {
		return if flow::miss(&s.db, &token).await? {
			Err(ServerError::InvalidCredentials)
		} else {
			clear(cookies, LINK_COOKIE);
			Err(ServerError::Validation(format!(
				"Too many wrong passwords ({MAX_ATTEMPTS}). Start again from the sign-in page."
			)))
		};
	}

	flow::forget(&s.db, &token).await?;
	clear(cookies, LINK_COOKIE);
	let identity = Identity {
		subject: flow.data["subject"].as_str().unwrap_or_default().to_owned(),
		email: flow.data["email"].as_str().map(str::to_owned),
		email_verified: true,
		name: flow.data["name"].as_str().unwrap_or_default().to_owned(),
		groups: flow.data["groups"]
			.as_array()
			.map(|a| {
				a.iter()
					.filter_map(|g| g.as_str().map(str::to_owned))
					.collect()
			})
			.unwrap_or_default(),
	};
	let sealed = providers::find_enabled(&s.db, provider).await?;
	link(s, provider, user, &identity).await?;
	record_everywhere(
		&s.db,
		user,
		"identity.link",
		Target::new("identity", provider.0).named(sealed.provider.name.clone()),
		Diff::new(),
	)
	.await;
	apply_groups(s, &sealed.provider, user, &identity.groups).await?;
	let next = flow.data["next"].as_str().unwrap_or("/").to_owned();
	if crate::two_factor::service::enrolled(&s.db, user).await? {
		crate::two_factor::challenge::begin(s, cookies, headers, user, &next).await?;
		return Ok(Linked::SecondFactor);
	}
	open_session(s, cookies, headers, user, Method::Password).await?;
	Ok(Linked::SignedIn(next))
}

/// A sign-in linked to an account, for the Security page.
#[derive(Debug, Clone)]
pub struct Linkage {
	pub id: IdentityId,
	pub provider: String,
	pub email: Option<String>,
}

pub async fn linkages(db: &sqlx::PgPool, user: UserId) -> Result<Vec<Linkage>, sqlx::Error> {
	sqlx::query_as!(
		Linkage,
		r#"
			SELECT i.id AS "id: IdentityId", p.name AS provider, i.email
			FROM user_identities i JOIN auth_providers p ON p.id = i.provider_id
			WHERE i.user_id = $1 ORDER BY i.created_at, i.id
		"#,
		user.0
	)
	.fetch_all(db)
	.await
}

/// Removes a linked sign-in of `user`'s. An account with no password would be locked out
/// without it, so that is refused.
pub async fn unlink(db: &sqlx::PgPool, user: UserId, id: IdentityId) -> AppResult<()> {
	let has_password = sqlx::query_scalar!("SELECT has_password FROM users WHERE id = $1", user.0)
		.fetch_one(db)
		.await?;
	if !has_password {
		return Err(refused(
			"This account has no password, so its provider sign-in cannot be removed.",
		));
	}
	let name = sqlx::query_scalar!(
		"DELETE FROM user_identities i USING auth_providers p
		WHERE i.id = $1 AND i.user_id = $2 AND p.id = i.provider_id RETURNING p.name",
		id.0,
		user.0
	)
	.fetch_optional(db)
	.await?
	.ok_or(ServerError::NotFound)?;
	record_everywhere(
		db,
		user,
		"identity.unlink",
		Target::new("identity", id.0).named(name),
		Diff::new(),
	)
	.await;
	Ok(())
}
