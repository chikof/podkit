use crate::domain::shared::ids::{RoleId, TeamId};
use time::OffsetDateTime;

/// A named set of permissions a team member can be assigned.
#[derive(Debug, Clone)]
pub struct Role {
	/// Unique id of this role.
	pub id: RoleId,
	/// `None` = built-in/global role, shared across every team.
	pub team_id: Option<TeamId>,
	/// Display name, e.g. "admin" or "viewer".
	pub name: String,
	/// Whether this role is assigned by default to new team members.
	pub is_default: bool,
	/// When the role was created.
	pub created_at: OffsetDateTime,
}

impl Role {
	/// Creates a new role, stamping `created_at` to now.
	#[must_use]
	pub fn new(id: RoleId, team_id: Option<TeamId>, name: String, is_default: bool) -> Self {
		Self {
			id,
			team_id,
			name,
			is_default,
			created_at: OffsetDateTime::now_utc(),
		}
	}

	/// True for built-in/global roles, shared across every team.
	#[must_use]
	pub fn is_builtin(&self) -> bool {
		self.team_id.is_none()
	}
}
