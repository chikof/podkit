use crate::domain::shared::ids::{RoleId, TeamId};
use time::OffsetDateTime;

#[derive(Debug, Clone)]
pub struct Role {
	pub id: RoleId,
	/// `None` = built-in/global role, shared across every team.
	pub team_id: Option<TeamId>,
	pub name: String,
	pub is_default: bool,
	pub created_at: OffsetDateTime,
}

impl Role {
	pub fn new(id: RoleId, team_id: Option<TeamId>, name: String, is_default: bool) -> Self {
		Self {
			id,
			team_id,
			name,
			is_default,
			created_at: OffsetDateTime::now_utc(),
		}
	}

	pub fn is_builtin(&self) -> bool {
		self.team_id.is_none()
	}
}
