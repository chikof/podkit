//! The fixtures the sign-in throttle tests of this crate share with the ones in
//! `podkit-ui` that read the pages. They moved here from `throttle::tests`.

use axum::Router;
use axum::http::StatusCode;
use serde_json::{Value, json};
use time::OffsetDateTime;

use crate::ids::UserId;
use crate::state::AppState;
use crate::tests::{PgPool, call, user_id, user_with_team};
use crate::two_factor::{service as two_factor, totp};

pub const PASSWORD: &str = "correct horse battery";

pub fn unix() -> u64 {
	u64::try_from(OffsetDateTime::now_utc().unix_timestamp()).unwrap()
}

pub fn code(secret_b32: &str, offset: i64) -> String {
	let secret = totp::base32_decode(secret_b32).unwrap();
	let step = totp::step_of(unix()).checked_add_signed(offset).unwrap();
	format!("{:06}", totp::code_at(&secret, step, totp::DIGITS))
}

pub async fn login(
	app: &Router,
	email: &str,
	password: &str,
	code: Option<&str>,
) -> (StatusCode, Value) {
	let mut body = json!({ "email": email, "password": password });
	if let Some(code) = code {
		body["code"] = json!(code);
	}
	call(app, "POST", "/api/v1/auth/login", None, Some(body)).await
}

/// An account with the authenticator app on. Returns its secret.
pub async fn with_totp(st: &AppState, app: &Router, name: &str) -> (UserId, String, Vec<String>) {
	user_with_team(app, name).await;
	let user = UserId(user_id(&st.db, name).await);
	let setup = two_factor::begin_totp(st, user).await.unwrap();
	let recovery = two_factor::confirm_totp(st, user, &code(&setup.secret, -1))
		.await
		.unwrap();
	(user, setup.secret, recovery)
}

/// Makes a lock last. The wait after the fifth failure is one second, which a loaded machine
/// can outrun between two requests of a test; doubling the rows makes it half a minute.
pub async fn hold(db: &PgPool) {
	sqlx::query("INSERT INTO auth_failures (subject, at) SELECT subject, NOW() FROM auth_failures")
		.execute(db)
		.await
		.unwrap();
}
