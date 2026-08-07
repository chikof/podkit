use crate::domain::shared::ids::{ProjectId, TeamId};
use time::OffsetDateTime;

/// Groups a team's applications, e.g. by product or environment.
#[derive(Debug, Clone)]
pub struct Project {
	/// Unique id of this project.
	pub id: ProjectId,
	/// The team that owns this project.
	pub team_id: TeamId,
	/// Display name.
	pub name: String,
	/// URL-safe unique identifier, scoped to the team.
	pub slug: String,
	/// When the project was created.
	pub created_at: OffsetDateTime,
	/// When the project was last updated.
	pub updated_at: OffsetDateTime,
}

impl Project {
	/// Creates a new project, stamping both timestamps to now.
	#[must_use]
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
