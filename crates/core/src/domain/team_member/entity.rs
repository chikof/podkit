use crate::domain::shared::ids::{RoleId, TeamId, TeamMemberId, UserId};
use time::OffsetDateTime;

#[derive(Debug, Clone)]
pub struct TeamMember {
	pub id: TeamMemberId,
	pub team_id: TeamId,
	pub user_id: UserId,
	pub role_id: RoleId,
	pub joined_at: OffsetDateTime,
}

impl TeamMember {
	pub fn new(id: TeamMemberId, team_id: TeamId, user_id: UserId, role_id: RoleId) -> Self {
		Self {
			id,
			team_id,
			user_id,
			role_id,
			joined_at: OffsetDateTime::now_utc(),
		}
	}

	pub fn change_role(&mut self, role_id: RoleId) {
		self.role_id = role_id;
	}
}
