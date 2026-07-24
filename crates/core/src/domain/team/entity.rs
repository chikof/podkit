use crate::domain::shared::ids::TeamId;
use time::OffsetDateTime;

#[derive(Debug, Clone)]
pub struct Team {
	pub id: TeamId,
	pub name: String,
	pub slug: String,
	pub logo: String,
	pub created_at: OffsetDateTime,
	pub updated_at: OffsetDateTime,
}

impl Team {
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
