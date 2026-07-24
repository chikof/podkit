use crate::domain::shared::ids::{ProjectId, TeamId};
use time::OffsetDateTime;

#[derive(Debug, Clone)]
pub struct Project {
	pub id: ProjectId,
	pub team_id: TeamId,
	pub name: String,
	pub slug: String,
	pub created_at: OffsetDateTime,
	pub updated_at: OffsetDateTime,
}

impl Project {
	pub fn new(id: ProjectId, team_id: TeamId, name: String, slug: String) -> Self {
		let now = OffsetDateTime::now_utc();
		Self {
			id,
			team_id,
			name,
			slug,
			created_at: now,
			updated_at: now,
		}
	}
}
