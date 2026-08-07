use crate::domain::shared::ids::UserId;
use crate::domain::user::value_objects::{Email, PasswordHash, UserSettings};
use time::OffsetDateTime; // I was considering using Jiff, but I’m not really sure about it...

// garde?.. No
/// A podkit account.
#[derive(Debug, Clone)]
pub struct User {
	/// Unique id of this user.
	pub id: UserId,
	/// Validated, unique email address.
	pub email: Email,
	/// Hashed password. Never the plaintext.
	pub password_hash: PasswordHash,
	/// Name shown in the UI.
	pub display_name: String, // maybe a displayName newtype
	/// Per-user preferences.
	pub settings: UserSettings,
	/// When the account was created.
	pub created_at: OffsetDateTime,
	/// When the account was last updated.
	pub updated_at: OffsetDateTime,
}

// user commands - we may add something like update_password_hash later
impl User {
	/// Creates a new user with default settings, stamping both timestamps to now.
	#[must_use]
	pub fn new(
		id: UserId,
		email: Email,
		password_hash: PasswordHash,
		display_name: String,
	) -> Self {
		let now = OffsetDateTime::now_utc();
		Self {
			id,
			email, // before any fatdevs says anything: This is validated automatically, the core idea is that we only validate once.
			password_hash,
			display_name,
			settings: UserSettings::default(),
			created_at: now,
			updated_at: now, // freshly minted user = no edits yet
		}
	}
}
