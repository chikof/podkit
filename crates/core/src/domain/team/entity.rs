use crate::domain::shared::ids::TeamId;
use time::OffsetDateTime;

/// The top-level tenancy boundary: owns projects, servers, and members.
#[derive(Debug, Clone)]
pub struct Team {
	/// Unique id of this team.
	pub id: TeamId,
	/// Display name.
	pub name: String,
	/// URL-safe unique identifier.
	pub slug: String,
	/// Logo URL or path.
	pub logo: String,
	/// When the team was created.
	pub created_at: OffsetDateTime,
	/// When the team was last updated.
	pub updated_at: OffsetDateTime,
}

impl Team {
	/// Creates a new team, stamping both timestamps to now.
	#[must_use]
	pub fn new(id: TeamId, name: String, slug: String, logo: String) -> Self {
		let now = OffsetDateTime::now_utc();
		Self {
			id,
			name,
			slug,
			logo,
			created_at: now,
			updated_at: now,
		}
	}
}
