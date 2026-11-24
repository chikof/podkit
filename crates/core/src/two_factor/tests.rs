//! Two-step verification end to end: an authenticator app and recovery codes at sign-in (JSON
//! API and web), passkeys through a soft authenticator, the team policy and its grace
//! period, and the session list. Every test name holds `two_factor`, so `-- two_factor`
//! runs them (and the unit tests next to `totp` and `service`).

#![allow(
	clippy::many_single_char_names,
	clippy::too_many_lines,
	reason = "a table of scenarios reads best straight down"
)]

use axum::Router;
use axum::http::StatusCode;
use serde_json::{Value, json};

use super::service::{self, RECOVERY_CODES};
use crate::state::AppState;
use crate::tests::two_factor::{ORIGIN, PASSWORD, audit_actions, code, enrol, uid};
use crate::tests::{PgPool, call, db_test, state, user_with_team};

async fn world(db: PgPool) -> (AppState, Router) {
	let mut st = state(db);
	let mut config = (*st.config).clone();
	config.public_url = Some(ORIGIN.to_owned());
	st.config = std::sync::Arc::new(config);
	let app = crate::routes::routes(st.clone()).await.unwrap();
	(st, app)
}

// --- the JSON API ----------------------------------------------------------------------

#[db_test]
async fn two_factor_api_login_needs_a_code_once_the_app_is_on(db: PgPool) {
	let (st, app) = world(db.clone()).await;
	user_with_team(&app, "ann").await;
	user_with_team(&app, "bob").await;
	let ann = uid(&db, "ann").await;
	let (secret, recovery) = enrol(&st, ann).await;
	assert_eq!(recovery.len(), RECOVERY_CODES);

	let login = |body: Value| {
		let app = app.clone();
		async move { call(&app, "POST", "/api/v1/auth/login", None, Some(body)).await }
	};
	let with = |c: &str| json!({ "email": "ann@example.com", "password": PASSWORD, "code": c });

	// No code: told to send one, no token.
	let (status, body) = login(json!({ "email": "ann@example.com", "password": PASSWORD })).await;
	assert_eq!(status, StatusCode::UNAUTHORIZED);
	assert!(body.get("token").is_none(), "{body}");
	// A wrong one.
	let (status, body) = login(with("000001")).await;
	assert!(
		status == StatusCode::UNAUTHORIZED && body.get("token").is_none(),
		"{status} {body}"
	);
	// A right one, once.
	let now = code(&secret, 0);
	let (status, body) = login(with(&now)).await;
	assert_eq!(status, StatusCode::OK, "{body}");
	assert!(body["token"].is_string());
	let (status, _) = login(with(&now)).await;
	assert_eq!(
		status,
		StatusCode::UNAUTHORIZED,
		"a code is spent by its first use"
	);
	// The next step's code is fine: the clock may be a little off.
	let (status, _) = login(with(&code(&secret, 1))).await;
	assert_eq!(status, StatusCode::OK);
	// Far from now is not.
	let (status, _) = login(with(&code(&secret, 5))).await;
	assert_eq!(status, StatusCode::UNAUTHORIZED);

	// A recovery code stands in for the app, once.
	let (status, _) = login(with(&recovery[0])).await;
	assert_eq!(status, StatusCode::OK);
	let (status, _) = login(with(&recovery[0])).await;
	assert_eq!(
		status,
		StatusCode::UNAUTHORIZED,
		"a recovery code works once"
	);
	assert_eq!(service::status(&db, ann).await.unwrap().recovery_left, 9);

	// Someone without the app signs in as before.
	let (status, _) = call(
		&app,
		"POST",
		"/api/v1/auth/login",
		None,
		Some(json!({ "email": "bob@example.com", "password": PASSWORD })),
	)
	.await;
	assert_eq!(status, StatusCode::OK);

	let actions = audit_actions(&db).await;
	assert!(
		actions.iter().any(|a| a == "session.login_failed"),
		"{actions:?}"
	);
	assert!(
		actions.iter().any(|a| a == "two_factor.enable"),
		"{actions:?}"
	);
}

/// A passkey saved by webauthn-rs 0.5.4 (OpenSSL) still loads and serialises to the same JSON in
/// webauthn-rs 0.6 (RustCrypto): stored credentials survive the upgrade.
#[test]
fn two_factor_passkey_saved_by_webauthn_rs_0_5_loads_unchanged() {
	let stored = json!({
		"cred": {
			"cred_id": "87Q8EPkrnz-qtL1qKxlYoJytnfidzIzODFKbNjntocM",
			"cred": {
				"type_": "ES256",
				"key": { "EC_EC2": {
					"curve": "SECP256R1",
					"x": "udJX-Kj1RBOkoz-ZeKMylfycLQjO_kILdnH728gzUpI",
					"y": "BERKFdokLWefcjGoDfzc0VlcLWDaI4rJWy3BWX338Yo"
				} }
			},
			"counter": 0,
			"transports": null,
			"user_verified": true,
			"backup_eligible": false,
			"backup_state": false,
			"registration_policy": "required",
			"extensions": {
				"cred_protect": "Ignored",
				"hmac_create_secret": "NotRequested",
				"appid": "NotRequested",
				"cred_props": "Ignored"
			},
			"attestation": { "data": "Self_", "metadata": "None" },
			"attestation_format": "packed"
		}
	});
	let key: webauthn_rs::prelude::Passkey = serde_json::from_value(stored.clone()).unwrap();
	assert_eq!(key.cred_id().len(), 32);
	assert_eq!(serde_json::to_value(&key).unwrap(), stored);
}
