use time::OffsetDateTime; // I was considering using Jiff, but I’m not really sure about it...
use crate::domain::shared::ids::UserId;
use crate::domain::user::value_objects::{Email, PasswordHash, UserSettings};

// garde?.. No
#[derive(Debug, Clone)]
pub struct User {
    pub id: UserId,
    pub email: Email,
    pub password_hash: PasswordHash,
    pub display_name: String, // maybe a displayName newtype
    pub settings: UserSettings,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

// user commands - we may add something like update_password_hash later
impl User {
    pub fn new(id: UserId, email: Email, password_hash: PasswordHash, display_name: String) -> Self {
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