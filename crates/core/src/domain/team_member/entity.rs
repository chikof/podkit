use crate::domain::shared::ids::{RoleId, TeamId, TeamMemberId, UserId};
use time::OffsetDateTime;

/// Links a user to a team with a single role.
#[derive(Debug, Clone)]
pub struct TeamMember {
	/// Unique id of this membership row.
	pub id: TeamMemberId,
	/// The team joined.
	pub team_id: TeamId,
	/// The user who joined.
	pub user_id: UserId,
	/// The role held within this team.
	pub role_id: RoleId,
	/// When the user joined the team.
	pub joined_at: OffsetDateTime,
}

impl TeamMember {
	/// Creates a new membership, stamping `joined_at` to now.
	#[must_use]
	pub fn new(id: TeamMemberId, team_id: TeamId, user_id: UserId, role_id: RoleId) -> Self {
		Self {
			id,
			team_id,
			user_id,
			role_id,
			joined_at: OffsetDateTime::now_utc(),
		}
	}

	/// Reassigns the member's role.
	pub fn change_role(&mut self, role_id: RoleId) {
		self.role_id = role_id;
	}
}
