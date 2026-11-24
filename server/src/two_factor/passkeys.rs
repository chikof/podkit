//! Passkeys (`WebAuthn`) through the `webauthn-rs` crate: a second factor, or a sign-in with
//! no password. Docs: <https://docs.rs/webauthn-rs/0.5.4/webauthn_rs/>; the browser side is
//! <https://www.w3.org/TR/webauthn-3/>.
//!
//! The challenge a ceremony issues is kept server side in an `auth_flows` row (the crate's
//! `danger-allow-state-serialisation` feature lets the state be stored): the browser only
//! holds the flow's random token and is handed the challenge itself. Passkeys here always
//! require user verification (a PIN or biometric), so one is two factors in itself.

use serde_json::json;
use sqlx::PgPool;
use time::OffsetDateTime;
use webauthn_rs::prelude::{
	CreationChallengeResponse, PasskeyAuthentication, PasskeyRegistration, PublicKeyCredential,
	RegisterPublicKeyCredential, RequestChallengeResponse, Url, Uuid, Webauthn, WebauthnBuilder,
};

use crate::audit::{Diff, Target};
use crate::auth::flow::{self, Kind};
use crate::auth::record_everywhere;
use crate::error::{AppResult, ServerError};
use crate::ids::{PasskeyId, UserId};

const MAX_NAME: usize = 60;

/// A registered passkey, as the Security page lists it.
#[derive(Debug, Clone)]
pub struct Passkey {
	pub id: PasskeyId,
	pub name: String,
	pub created_at: OffsetDateTime,
	pub last_used_at: Option<OffsetDateTime>,
}

fn failed(what: &str, e: &dyn std::fmt::Display) -> ServerError {
	tracing::warn!(error = %e, "passkey {what} failed");
	ServerError::Validation(format!(
		"The passkey {what} did not go through. Try again, or use another way to sign in."
	))
}

/// A `Webauthn` for the address the browser is on (`origin`, like `https://podkit.example`).
/// Passkeys are bound to that host name: reaching podkit by another name does not find them.
fn webauthn(origin: &str) -> AppResult<Webauthn> {
	let url = Url::parse(origin).map_err(|e| failed("setup", &e))?;
	let host = url
		.host_str()
		.ok_or_else(|| {
			ServerError::Validation("podkit has no host name to bind passkeys to.".to_owned())
		})?
		.to_owned();
	WebauthnBuilder::new(&host, &url)
		.map(|b| b.rp_name("podkit"))
		.and_then(WebauthnBuilder::build)
		.map_err(|e| failed("setup", &e))
}

/// A stable `WebAuthn` user handle: random-looking, never the email.
fn handle(user: UserId) -> Uuid {
	let mut bytes = [0u8; 16];
	bytes[8..].copy_from_slice(&user.0.to_be_bytes());
	Uuid::from_bytes(bytes)
}

pub async fn list(db: &PgPool, user: UserId) -> Result<Vec<Passkey>, sqlx::Error> {
	sqlx::query_as!(
		Passkey,
		r#"SELECT id AS "id: PasskeyId", name, created_at, last_used_at
		FROM passkeys WHERE user_id = $1 ORDER BY created_at, id"#,
		user.0
	)
	.fetch_all(db)
	.await
}

async fn stored(
	db: &PgPool,
	user: UserId,
) -> Result<Vec<webauthn_rs::prelude::Passkey>, sqlx::Error> {
	let rows = sqlx::query_scalar!("SELECT credential FROM passkeys WHERE user_id = $1", user.0)
		.fetch_all(db)
		.await?;
	Ok(rows
		.into_iter()
		.filter_map(|v| serde_json::from_value(v).ok())
		.collect())
}

/// What a browser needs to start a ceremony: the flow token to send back, and the options
/// for `navigator.credentials`.
#[derive(Debug)]
pub struct Challenge<T> {
	pub flow: String,
	pub options: T,
}

/// Starts registering a passkey for `user`, named `email` on the authenticator.
pub async fn start_enroll(
	db: &PgPool,
	origin: &str,
	user: UserId,
	email: &str,
	display_name: &str,
) -> AppResult<Challenge<CreationChallengeResponse>> {
	let webauthn = webauthn(origin)?;
	let existing = stored(db, user).await?;
	let exclude = existing
		.iter()
		.map(|p| p.cred_id().clone())
		.collect::<Vec<_>>();
	let (options, state) = webauthn
		.start_passkey_registration(
			handle(user),
			email,
			display_name,
			(!exclude.is_empty()).then_some(exclude),
		)
		.map_err(|e| failed("registration", &e))?;
	let data = json!({ "state": serde_json::to_value(&state).map_err(|_| ServerError::Internal)? });
	let flow = flow::start(db, Kind::PasskeyEnroll, Some(user), None, &data).await?;
	Ok(Challenge { flow, options })
}

/// Finishes the registration and stores the passkey under `name`. Returns the recovery
/// codes when this is the person's first second factor, to be shown once (none otherwise).
pub async fn finish_enroll(
	db: &PgPool,
	origin: &str,
	user: UserId,
	flow_token: &str,
	name: &str,
	credential: &RegisterPublicKeyCredential,
) -> AppResult<Vec<String>> {
	let name = name.trim();
	let name = if name.is_empty() { "Passkey" } else { name };
	if name.chars().count() > MAX_NAME {
		return Err(ServerError::Validation(format!(
			"Name the passkey in {MAX_NAME} characters or fewer."
		)));
	}
	let flow = flow::take(db, Kind::PasskeyEnroll, flow_token)
		.await?
		.filter(|f| f.user == Some(user))
		.ok_or_else(|| {
			ServerError::Validation("That passkey setup has expired. Start again.".to_owned())
		})?;
	let state: PasskeyRegistration =
		serde_json::from_value(flow.data["state"].clone()).map_err(|_| ServerError::Internal)?;
	let passkey = webauthn(origin)?
		.finish_passkey_registration(credential, &state)
		.map_err(|e| failed("registration", &e))?;
	let result = sqlx::query!(
		"INSERT INTO passkeys (id, user_id, name, credential_id, credential)
		VALUES ($1, $2, $3, $4, $5)",
		PasskeyId::generate().0,
		user.0,
		name,
		passkey.cred_id().as_ref(),
		serde_json::to_value(&passkey).map_err(|_| ServerError::Internal)?
	)
	.execute(db)
	.await;
	if let Err(e) = result {
		return Err(
			if crate::db::is_unique_violation(&e, "passkeys_credential_id_key") {
				ServerError::Conflict("That passkey is already registered.".to_owned())
			} else {
				e.into()
			},
		);
	}
	let codes = crate::two_factor::service::ensure_recovery_codes(db, user).await?;
	record_everywhere(
		db,
		user,
		"two_factor.enable",
		Target::none("two_factor").named("passkey"),
		Diff::new().set("method", "passkey").set("name", name),
	)
	.await;
	Ok(codes)
}

/// Removes a passkey of `user`'s. Asks for the password of an account that has one.
pub async fn remove(db: &PgPool, user: UserId, id: PasskeyId, password: &str) -> AppResult<()> {
	crate::two_factor::service::confirm_password(db, user, password).await?;
	let name = sqlx::query_scalar!(
		"DELETE FROM passkeys WHERE id = $1 AND user_id = $2 RETURNING name",
		id.0,
		user.0
	)
	.fetch_optional(db)
	.await?
	.ok_or(ServerError::NotFound)?;
	crate::two_factor::service::drop_recovery_if_unneeded(db, user).await?;
	record_everywhere(
		db,
		user,
		"two_factor.disable",
		Target::none("two_factor").named("passkey"),
		Diff::new().set("method", "passkey").set("name", name),
	)
	.await;
	Ok(())
}

/// Starts a sign-in with one of `user`'s passkeys. `purpose` is stored with the flow
/// (`"login"` for the passwordless sign-in, `"second_factor"` after a password).
/// `None` when the person has no passkey.
pub async fn start_login(
	db: &PgPool,
	origin: &str,
	user: UserId,
	purpose: &str,
) -> AppResult<Option<Challenge<RequestChallengeResponse>>> {
	let keys = stored(db, user).await?;
	if keys.is_empty() {
		return Ok(None);
	}
	let (options, state) = webauthn(origin)?
		.start_passkey_authentication(&keys)
		.map_err(|e| failed("sign-in", &e))?;
	let data = json!({
		"state": serde_json::to_value(&state).map_err(|_| ServerError::Internal)?,
		"purpose": purpose,
	});
	let flow = flow::start(db, Kind::PasskeyLogin, Some(user), None, &data).await?;
	Ok(Some(Challenge { flow, options }))
}

/// Finishes a passkey sign-in and returns whose it was. The signature counter of the
/// stored credential moves forward, so a cloned authenticator is noticed.
pub async fn finish_login(
	db: &PgPool,
	origin: &str,
	flow_token: &str,
	purpose: &str,
	credential: &PublicKeyCredential,
) -> AppResult<UserId> {
	let flow = flow::take(db, Kind::PasskeyLogin, flow_token)
		.await?
		.filter(|f| f.data["purpose"] == purpose)
		.ok_or(ServerError::InvalidCredentials)?;
	let user = flow.user.ok_or(ServerError::InvalidCredentials)?;
	let state: PasskeyAuthentication =
		serde_json::from_value(flow.data["state"].clone()).map_err(|_| ServerError::Internal)?;
	// A refused assertion is a wrong guess like a wrong code: counted, and locks the account.
	let attempt = crate::throttle::begin(db, crate::throttle::Who::User(user)).await?;
	let result = match webauthn(origin)?.finish_passkey_authentication(credential, &state) {
		Ok(result) => {
			attempt.ok().await;
			result
		}
		Err(e) => {
			tracing::info!(error = %e, "passkey sign-in refused");
			attempt.fail().await;
			return Err(ServerError::InvalidCredentials);
		}
	};

	let rows = sqlx::query!(
		"SELECT id, credential FROM passkeys WHERE user_id = $1",
		user.0
	)
	.fetch_all(db)
	.await?;
	for row in rows {
		let Ok(mut key) = serde_json::from_value::<webauthn_rs::prelude::Passkey>(row.credential)
		else {
			continue;
		};
		if key.cred_id() != result.cred_id() {
			continue;
		}
		if key.update_credential(&result) == Some(true) {
			sqlx::query!(
				"UPDATE passkeys SET credential = $2, last_used_at = NOW() WHERE id = $1",
				row.id,
				serde_json::to_value(&key).map_err(|_| ServerError::Internal)?
			)
			.execute(db)
			.await?;
		} else {
			sqlx::query!(
				"UPDATE passkeys SET last_used_at = NOW() WHERE id = $1",
				row.id
			)
			.execute(db)
			.await?;
		}
	}
	Ok(user)
}

/// What the browser needs to keep a password manager's passkeys in step with ours (the
/// Signal API, see `passkey.js`): the relying party id, the account's handle and the
/// credential ids, all as unpadded base64url.
#[derive(Debug, Clone)]
pub struct SyncInfo {
	pub rp_id: String,
	pub user_id: String,
	/// Space separated.
	pub credential_ids: String,
}

pub async fn sync_info(db: &PgPool, origin: &str, user: UserId) -> AppResult<SyncInfo> {
	use base64::Engine as _;
	use base64::engine::general_purpose::URL_SAFE_NO_PAD;
	let rp_id = Url::parse(origin)
		.ok()
		.and_then(|u| u.host_str().map(str::to_owned))
		.unwrap_or_default();
	let ids = sqlx::query_scalar!(
		"SELECT credential_id FROM passkeys WHERE user_id = $1 ORDER BY id",
		user.0
	)
	.fetch_all(db)
	.await?;
	Ok(SyncInfo {
		rp_id,
		user_id: URL_SAFE_NO_PAD.encode(handle(user).as_bytes()),
		credential_ids: ids
			.iter()
			.map(|id| URL_SAFE_NO_PAD.encode(id))
			.collect::<Vec<_>>()
			.join(" "),
	})
}
