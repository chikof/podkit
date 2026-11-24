//! The second step of a password sign-in: after the password was right for an account with
//! two-step verification, the browser holds a `pk_2fa` cookie naming a short-lived flow and
//! the person proves the second factor to turn it into a session.

use axum::http::HeaderMap;
use serde_json::json;
use tower_cookies::{Cookie, Cookies, cookie::SameSite};
use webauthn_rs::prelude::{PublicKeyCredential, RequestChallengeResponse};

use super::passkeys::{self, Challenge};
use super::service;
use crate::audit::{Diff, Target};
use crate::auth::flow::{self, Kind, MAX_ATTEMPTS};
use crate::auth::session::Method;
use crate::auth::{open_session, record_everywhere};
use crate::error::{AppResult, ServerError};
use crate::ids::UserId;
use crate::state::AppState;
use crate::throttle::{self, Who};

/// The cookie that ties a browser to its second-factor flow.
pub const COOKIE: &str = "pk_2fa";

/// Where someone stands in the second step.
#[derive(Debug, Clone)]
pub struct Pending {
	pub user: UserId,
	/// Where to go once signed in.
	pub next: String,
	pub totp: bool,
	pub passkeys: bool,
}

/// Parks the sign-in of `user`, whose password was right, until they give a second factor.
pub async fn begin(
	s: &AppState,
	cookies: &Cookies,
	headers: &HeaderMap,
	user: UserId,
	next: &str,
) -> AppResult<()> {
	let token = flow::start(
		&s.db,
		Kind::SecondFactor,
		Some(user),
		None,
		&json!({ "next": next }),
	)
	.await?;
	cookies.add(
		Cookie::build((COOKIE, token))
			.http_only(true)
			.secure(crate::auth::reached_over_https(headers))
			.same_site(SameSite::Strict)
			.max_age(flow::TTL)
			.path("/")
			.build(),
	);
	Ok(())
}

fn token(cookies: &Cookies) -> Option<String> {
	cookies.get(COOKIE).map(|c| c.value().to_owned())
}

/// The waiting sign-in this browser has, if any.
pub async fn pending(s: &AppState, cookies: &Cookies) -> AppResult<Option<Pending>> {
	let Some(token) = token(cookies) else {
		return Ok(None);
	};
	let Some(flow) = flow::peek(&s.db, Kind::SecondFactor, &token).await? else {
		return Ok(None);
	};
	let Some(user) = flow.user else {
		return Ok(None);
	};
	let status = service::status(&s.db, user).await?;
	Ok(Some(Pending {
		user,
		next: flow.data["next"].as_str().unwrap_or("/").to_owned(),
		totp: status.totp,
		passkeys: !status.passkeys.is_empty(),
	}))
}

async fn finish(
	s: &AppState,
	cookies: &Cookies,
	headers: &HeaderMap,
	user: UserId,
	token: &str,
) -> AppResult<()> {
	flow::forget(&s.db, token).await?;
	cookies.remove(Cookie::build((COOKIE, "")).path("/").build());
	open_session(s, cookies, headers, user, Method::Password).await?;
	Ok(())
}

async fn refuse(s: &AppState, cookies: &Cookies, user: UserId, token: &str) -> ServerError {
	record_everywhere(
		&s.db,
		user,
		"session.login_failed",
		Target::none("session").named("(second factor)"),
		Diff::new().set("method", "password"),
	)
	.await;
	match flow::miss(&s.db, token).await {
		Ok(true) => ServerError::InvalidCredentials,
		Ok(false) => {
			cookies.remove(Cookie::build((COOKIE, "")).path("/").build());
			ServerError::Validation(format!(
				"Too many wrong codes ({MAX_ATTEMPTS}). Sign in again with your password."
			))
		}
		Err(e) => e.into(),
	}
}

/// A code from the authenticator app or a recovery code. On success the session is open and
/// the page to go to is returned. Five wrong codes end the attempt.
pub async fn submit_code(
	s: &AppState,
	cookies: &Cookies,
	headers: &HeaderMap,
	code: &str,
) -> AppResult<String> {
	let pending = pending(s, cookies)
		.await?
		.ok_or(ServerError::InvalidCredentials)?;
	let token = token(cookies).ok_or(ServerError::InvalidCredentials)?;
	let attempt = throttle::begin(&s.db, Who::User(pending.user)).await?;
	let right = service::check_code(s, pending.user, code).await?;
	attempt.finish(right).await;
	if !right {
		return Err(refuse(s, cookies, pending.user, &token).await);
	}
	finish(s, cookies, headers, pending.user, &token).await?;
	Ok(pending.next)
}

/// The challenge for the person's passkeys, for the second step.
pub async fn passkey_options(
	s: &AppState,
	cookies: &Cookies,
	origin: &str,
) -> AppResult<Challenge<RequestChallengeResponse>> {
	let pending = pending(s, cookies)
		.await?
		.ok_or(ServerError::InvalidCredentials)?;
	passkeys::start_login(&s.db, origin, pending.user, "second_factor")
		.await?
		.ok_or_else(|| ServerError::Validation("You have no passkey.".to_owned()))
}

/// The browser's answer to [`passkey_options`].
pub async fn submit_passkey(
	s: &AppState,
	cookies: &Cookies,
	headers: &HeaderMap,
	origin: &str,
	passkey_flow: &str,
	credential: &PublicKeyCredential,
) -> AppResult<String> {
	let pending = pending(s, cookies)
		.await?
		.ok_or(ServerError::InvalidCredentials)?;
	let token = token(cookies).ok_or(ServerError::InvalidCredentials)?;
	let user = match passkeys::finish_login(
		&s.db,
		origin,
		passkey_flow,
		"second_factor",
		credential,
	)
	.await
	{
		Ok(user) if user == pending.user => user,
		_ => return Err(refuse(s, cookies, pending.user, &token).await),
	};
	finish(s, cookies, headers, user, &token).await?;
	Ok(pending.next)
}

/// The challenge for a sign-in with a passkey and no password: `None` when the email has
/// no passkey (the caller says the same for an unknown email, so nobody can probe for
/// accounts).
pub async fn passwordless_options(
	s: &AppState,
	origin: &str,
	email: &str,
) -> AppResult<Option<Challenge<RequestChallengeResponse>>> {
	let email = crate::auth::normalize_email_lenient(email);
	let Some(user) = crate::auth::users::find_by_email(&s.db, &email).await? else {
		return Ok(None);
	};
	passkeys::start_login(&s.db, origin, user.id, "login").await
}

/// The browser's answer to [`passwordless_options`]: opens the session.
pub async fn passwordless_finish(
	s: &AppState,
	cookies: &Cookies,
	headers: &HeaderMap,
	origin: &str,
	passkey_flow: &str,
	credential: &PublicKeyCredential,
) -> AppResult<()> {
	let user = passkeys::finish_login(&s.db, origin, passkey_flow, "login", credential).await?;
	open_session(s, cookies, headers, user, Method::Passkey).await?;
	Ok(())
}
