//! The fixtures the two-step sign-in tests of this crate share with the ones in
//! `podkit-ui` that read the pages. They moved here from `two_factor::tests`.

use time::OffsetDateTime;

use crate::ids::UserId;
use crate::state::AppState;
use crate::tests::{PgPool, user_id};
use crate::two_factor::service::{self};
use crate::two_factor::totp;

pub const PASSWORD: &str = "correct horse battery";

pub const ORIGIN: &str = "https://podkit.test";

pub fn unix() -> u64 {
	u64::try_from(OffsetDateTime::now_utc().unix_timestamp()).unwrap()
}

/// The code the app shows `offset` 30-second steps from now.
pub fn code(secret_b32: &str, offset: i64) -> String {
	let secret = totp::base32_decode(secret_b32).unwrap();
	let step = totp::step_of(unix()).checked_add_signed(offset).unwrap();
	format!("{:06}", totp::code_at(&secret, step, totp::DIGITS))
}

/// Turns the authenticator app on for `user`. Returns the secret (base32) and the recovery
/// codes. The confirming code is the previous step's, so the current one and the next are
/// still unspent.
pub async fn enrol(st: &AppState, user: UserId) -> (String, Vec<String>) {
	let setup = service::begin_totp(st, user).await.unwrap();
	let codes = service::confirm_totp(st, user, &code(&setup.secret, -1))
		.await
		.unwrap();
	(setup.secret, codes)
}

pub async fn uid(db: &PgPool, name: &str) -> UserId {
	UserId(user_id(db, name).await)
}

pub async fn audit_actions(db: &PgPool) -> Vec<String> {
	sqlx::query_scalar("SELECT action FROM audit_events ORDER BY id")
		.fetch_all(db)
		.await
		.unwrap()
}
